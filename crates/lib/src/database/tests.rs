//! Tests for the database module.

use std::sync::{Arc, Mutex};

use super::*;
use crate::{
    auth::crypto::generate_keypair, backend::database::InMemory, instance::WriteSource,
    store::DocStore,
};

#[tokio::test]
async fn test_find_sigkeys_returns_sorted_by_permission() -> Result<()> {
    // Create instance
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await?;

    // Generate a test key
    let (signing_key, public_key) = generate_keypair();

    // Create database (Database::create bootstraps signing key as Admin(0))
    let db = Database::create(&instance, signing_key, Doc::new()).await?;

    // Add global Write(10) key via follow-up transaction (bootstrap key stays at Admin(0))
    let txn = db.new_transaction().await?;
    let settings_store = txn.get_settings()?;
    settings_store
        .set_global_auth_key(AuthKey::active(None, Permission::Write(10)))
        .await?;
    txn.commit().await?;

    // Call find_sigkeys
    let results = Database::find_sigkeys(&instance, db.root_id(), &public_key).await?;

    // Verify we got 2 entries (direct key + global)
    assert_eq!(results.len(), 2, "Should find direct key and global option");

    // Verify they're sorted by permission, highest first
    // Admin(0) > Write(10)
    assert_eq!(
        results[0].1,
        Permission::Admin(0),
        "First should be Admin(0) from bootstrap key"
    );
    assert_eq!(
        results[1].1,
        Permission::Write(10),
        "Second should be Write(10) from global"
    );

    // Verify the SigKey types
    assert!(
        results[0].0.has_pubkey_hint(&public_key),
        "First should be direct pubkey hint"
    );
    assert!(results[1].0.is_global(), "Second should be global hint");

    Ok(())
}

#[tokio::test]
async fn test_create_bootstraps_signing_key_as_admin_zero() -> Result<()> {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await?;

    let (signing_key, signing_pubkey) = generate_keypair();

    // Create database (signing key is bootstrapped as Admin(0))
    let db = Database::create(&instance, signing_key, Doc::new()).await?;

    // Verify the signing key was bootstrapped as Admin(0)
    let results = Database::find_sigkeys(&instance, db.root_id(), &signing_pubkey).await?;
    assert_eq!(results.len(), 1, "Signing key should be present in auth");
    assert_eq!(
        results[0].1,
        Permission::Admin(0),
        "Signing key should be Admin(0)"
    );

    Ok(())
}

#[tokio::test]
async fn test_create_rejects_preconfigured_auth() -> Result<()> {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await?;

    let (signing_key, _) = generate_keypair();

    let (_, other_pubkey) = generate_keypair();

    // Pre-configure auth in settings — this should be rejected
    let mut settings = Doc::new();
    settings.set("name", "test_reject");

    let mut auth_settings = AuthSettings::new();
    auth_settings.add_key(
        &other_pubkey,
        AuthKey::active(Some("other_user"), Permission::Write(5)),
    )?;
    settings.set("auth", auth_settings.as_doc().clone());

    // Database::create should return an error
    let result = Database::create(&instance, signing_key, settings).await;
    assert!(result.is_err(), "Should reject preconfigured auth");

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("must not contain auth configuration"),
        "Error should mention auth configuration, got: {err_msg}"
    );

    Ok(())
}

// ===== Write Callback Tests =====

/// Helper: create an Instance + Database for callback tests
async fn setup_callback_test() -> (Instance, Database) {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (signing_key, _) = generate_keypair();
    let db = Database::create(&instance, signing_key, Doc::new())
        .await
        .unwrap();
    (instance, db)
}

#[tokio::test]
async fn test_local_write_callback_fires() {
    let (_instance, db) = setup_callback_test().await;

    type EventRecord = (Vec<crate::entry::ID>, WriteSource);
    let events: Arc<Mutex<Vec<EventRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = events.clone();

    let _cb = db
        .on_write(move |event, _db| {
            let prev_tips = event.previous_tips().to_vec();
            let source = event.source();
            events_clone.lock().unwrap().push((prev_tips, source));
            async { Ok(()) }
        })
        .await
        .unwrap();

    // First commit
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("key", "value1").await.unwrap();
    let id1 = txn.commit().await.unwrap();

    // Second commit
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("key", "value2").await.unwrap();
    let _id2 = txn.commit().await.unwrap();

    let recorded = events.lock().unwrap();
    assert_eq!(recorded.len(), 2, "callback should fire once per commit");
    assert_eq!(recorded[0].1, WriteSource::Local);
    assert_eq!(recorded[1].1, WriteSource::Local);
    // Second callback's previous_tips should contain id1
    assert!(
        recorded[1].0.contains(&id1),
        "previous_tips for second commit should contain the first commit's ID"
    );
}

#[tokio::test]
async fn test_local_write_event_contains_single_entry() {
    let (_instance, db) = setup_callback_test().await;

    let entry_counts: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let counts_clone = entry_counts.clone();

    let _cb = db
        .on_write(move |event, _db| {
            counts_clone.lock().unwrap().push(event.entries().len());
            async { Ok(()) }
        })
        .await
        .unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v").await.unwrap();
    txn.commit().await.unwrap();

    let counts = entry_counts.lock().unwrap();
    assert_eq!(counts.len(), 1);
    assert_eq!(
        counts[0], 1,
        "local write events should contain exactly one entry"
    );
}

#[tokio::test]
async fn test_remote_write_callback_fires_via_put_remote_entries() {
    use crate::backend::VerificationStatus;
    let (instance, db) = setup_callback_test().await;

    let remote_events: Arc<Mutex<Vec<(usize, WriteSource)>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = remote_events.clone();

    let _cb = db
        .on_write(move |event, _db| {
            let source = event.source();
            let count = event.entries().len();
            let events = events_clone.clone();
            async move {
                if source == WriteSource::Remote {
                    events.lock().unwrap().push((count, source));
                }
                Ok(())
            }
        })
        .await
        .unwrap();

    // Commit locally — should NOT record (callback filters to remote only).
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("key", "local").await.unwrap();
    let local_id = txn.commit().await.unwrap();

    assert!(
        remote_events.lock().unwrap().is_empty(),
        "remote-only filter should drop local commits"
    );

    // Simulate remote sync: revert two entries to `Unverified` so they
    // re-enter the verify pipeline, then `put_remote_entries` (no-op
    // store for existing entries; runs verify which promotes them and
    // fires one batched `Verified` event).
    //
    // The fire-on-Verified contract means subscribers don't see "we
    // received a re-ingestion of an already-Verified entry" — only
    // "an entry just settled to Verified." We trigger that by
    // forcibly putting the entries back into the Unverified state.
    let backend = instance.require_local_engine().unwrap();
    let root_id = db.root_id().clone();
    backend
        .update_verification_status(&root_id, VerificationStatus::Unverified)
        .await
        .unwrap();
    backend
        .update_verification_status(&local_id, VerificationStatus::Unverified)
        .await
        .unwrap();

    let entry = instance.get(&root_id).await.unwrap();
    let local_entry = instance.get(&local_id).await.unwrap();
    instance
        .put_remote_entries(&root_id, vec![entry, local_entry])
        .await
        .unwrap();

    let events = remote_events.lock().unwrap();
    assert_eq!(
        events.len(),
        1,
        "verify should promote both entries and fire once"
    );
    assert_eq!(
        events[0].0, 2,
        "batch should contain both promoted entries"
    );
    assert_eq!(events[0].1, WriteSource::Remote);
}

#[tokio::test]
async fn test_remote_write_previous_tips() {
    use crate::backend::VerificationStatus;
    let (instance, db) = setup_callback_test().await;

    let prev_tips_log: Arc<Mutex<Vec<Vec<crate::entry::ID>>>> = Arc::new(Mutex::new(Vec::new()));
    let log_clone = prev_tips_log.clone();

    let _cb = db
        .on_write(move |event, _db| {
            let source = event.source();
            let prev = event.previous_tips().to_vec();
            let log = log_clone.clone();
            async move {
                if source == WriteSource::Remote {
                    log.lock().unwrap().push(prev);
                }
                Ok(())
            }
        })
        .await
        .unwrap();

    // Commit locally to advance tips.
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v").await.unwrap();
    let local_id = txn.commit().await.unwrap();

    // Revert the just-committed entry to `Unverified` and put_remote_entries
    // it. The verify pass will re-promote it and fire one batched event
    // with the same entries.
    let backend = instance.require_local_engine().unwrap();
    backend
        .update_verification_status(&local_id, VerificationStatus::Unverified)
        .await
        .unwrap();
    let entry = instance.get(&local_id).await.unwrap();
    instance
        .put_remote_entries(db.root_id(), vec![entry])
        .await
        .unwrap();

    let log = prev_tips_log.lock().unwrap();
    assert_eq!(log.len(), 1);
    assert!(
        log[0].contains(&local_id),
        "previous_tips should reflect raw tips at the start of the verify pass; got {:?}",
        log[0]
    );
}

#[tokio::test]
async fn test_empty_remote_batch_does_not_fire_callback() {
    let (instance, db) = setup_callback_test().await;

    let fire_count: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let count_clone = fire_count.clone();

    let _cb = db
        .on_write(move |_event, _db| {
            *count_clone.lock().unwrap() += 1;
            async { Ok(()) }
        })
        .await
        .unwrap();

    instance
        .put_remote_entries(db.root_id(), vec![])
        .await
        .unwrap();

    assert_eq!(*fire_count.lock().unwrap(), 0);
}

#[tokio::test]
async fn test_callback_error_does_not_block_other_callbacks() {
    let (_instance, db) = setup_callback_test().await;

    let second_fired: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let flag = second_fired.clone();

    // First callback always errors
    let _cb1 = db
        .on_write(move |_event, _db| async {
            Err(crate::Error::Io(std::io::Error::other("test error")))
        })
        .await
        .unwrap();

    // Second callback should still execute
    let _cb2 = db
        .on_write(move |_event, _db| {
            *flag.lock().unwrap() = true;
            async { Ok(()) }
        })
        .await
        .unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v").await.unwrap();
    txn.commit().await.unwrap();

    assert!(*second_fired.lock().unwrap());
}

#[tokio::test]
async fn test_drop_write_callback_unregisters() {
    let (_instance, db) = setup_callback_test().await;

    let fire_count: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let count_clone = fire_count.clone();

    let cb = db
        .on_write(move |_event, _db| {
            *count_clone.lock().unwrap() += 1;
            async { Ok(()) }
        })
        .await
        .unwrap();

    // Fires while handle is alive
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    txn.commit().await.unwrap();
    assert_eq!(*fire_count.lock().unwrap(), 1);

    // Drop the handle — callback unregisters
    drop(cb);

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v2").await.unwrap();
    txn.commit().await.unwrap();
    assert_eq!(
        *fire_count.lock().unwrap(),
        1,
        "callback should not fire after WriteCallback is dropped"
    );
}

#[tokio::test]
async fn test_drop_only_unregisters_that_callback() {
    let (_instance, db) = setup_callback_test().await;

    let cb1_count: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let cb2_count: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));

    let cb1_clone = cb1_count.clone();
    let cb1 = db
        .on_write(move |_event, _db| {
            *cb1_clone.lock().unwrap() += 1;
            async { Ok(()) }
        })
        .await
        .unwrap();

    let cb2_clone = cb2_count.clone();
    let _cb2 = db
        .on_write(move |_event, _db| {
            *cb2_clone.lock().unwrap() += 1;
            async { Ok(()) }
        })
        .await
        .unwrap();

    // Both fire on the first commit
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    txn.commit().await.unwrap();
    assert_eq!(*cb1_count.lock().unwrap(), 1);
    assert_eq!(*cb2_count.lock().unwrap(), 1);

    drop(cb1);

    // Only cb2 fires after cb1 is dropped
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v2").await.unwrap();
    txn.commit().await.unwrap();
    assert_eq!(*cb1_count.lock().unwrap(), 1);
    assert_eq!(*cb2_count.lock().unwrap(), 2);
}

#[tokio::test]
async fn test_remote_callback_receives_only_stored_entries() {
    use crate::backend::VerificationStatus;
    let (instance, db) = setup_callback_test().await;

    let captured: Arc<Mutex<Vec<crate::entry::ID>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = captured.clone();

    db.on_write(move |event, _db| {
        let ids: Vec<_> = event.entries().iter().map(|e| e.id()).collect();
        let source = event.source();
        let captured = captured_clone.clone();
        async move {
            if source == WriteSource::Remote {
                captured.lock().unwrap().extend(ids);
            }
            Ok(())
        }
    })
    .await
    .unwrap()
    .detach();

    // Build two entries via local commits, then revert their verification
    // status so the next `put_remote_entries` triggers a verify pass that
    // re-promotes them and fires a single `Verified` event containing
    // both.
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    let id1 = txn.commit().await.unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v2").await.unwrap();
    let id2 = txn.commit().await.unwrap();

    let backend = instance.require_local_engine().unwrap();
    backend
        .update_verification_status(&id1, VerificationStatus::Unverified)
        .await
        .unwrap();
    backend
        .update_verification_status(&id2, VerificationStatus::Unverified)
        .await
        .unwrap();

    let entry1 = instance.get(&id1).await.unwrap();
    let entry2 = instance.get(&id2).await.unwrap();

    instance
        .put_remote_entries(db.root_id(), vec![entry1, entry2])
        .await
        .unwrap();

    // verify processes in topo order (parents-before-children). The
    // captured set must contain both ids; order in the WriteEvent is the
    // verify-pass promotion order, which is topo-sorted by `get_tree`.
    let captured_ids = captured.lock().unwrap();
    assert_eq!(captured_ids.len(), 2);
    assert!(
        captured_ids.contains(&id1) && captured_ids.contains(&id2),
        "captured entries must include both ids; got {captured_ids:?}",
    );
}

#[tokio::test]
async fn test_detached_write_callback_outlives_handle() {
    let (_instance, db) = setup_callback_test().await;

    let fire_count: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let count_clone = fire_count.clone();

    db.on_write(move |_event, _db| {
        *count_clone.lock().unwrap() += 1;
        async { Ok(()) }
    })
    .await
    .unwrap()
    .detach();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v").await.unwrap();
    txn.commit().await.unwrap();
    assert_eq!(*fire_count.lock().unwrap(), 1);
}

// Regression test for the per-tree `tree_locks` serialization in
// `Instance::put_entry` / `put_remote_entries`. Two concurrent writers on the
// same tree must observe a serial `previous_tips` chain — one callback's
// `previous_tips` must include the other event's entry. Without the lock,
// both writers snapshot tips before either persists, and neither callback
// reflects the other write.
//
// Uses `tokio::spawn` (not `join!`) on a multi-thread runtime to get real
// parallelism — `join!` polls both futures cooperatively from the same task
// and would not expose the race even when the lock is removed. Repeats the
// concurrent-pair scenario for many iterations because the race window in
// the in-memory backend is narrow; with the lock every iteration must
// serialize, so any single un-serialized iteration fails the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_writes_serialize_previous_tips() {
    const ITERATIONS: usize = 100;

    for iter in 0..ITERATIONS {
        let (_instance, db) = setup_callback_test().await;

        type EventRecord = (Vec<crate::entry::ID>, Vec<crate::entry::ID>);
        let events: Arc<Mutex<Vec<EventRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();

        let _cb = db
            .on_write(move |event, _db| {
                let prev = event.previous_tips().to_vec();
                let ids: Vec<_> = event.entries().iter().map(|e| e.id()).collect();
                events_clone.lock().unwrap().push((prev, ids));
                async { Ok(()) }
            })
            .await
            .unwrap();

        let db_a = db.clone();
        let db_b = db.clone();
        let h1 = tokio::spawn(async move {
            let txn = db_a.new_transaction().await.unwrap();
            let store = txn.get_store::<DocStore>("d").await.unwrap();
            store.set("k1", "v1").await.unwrap();
            txn.commit().await.unwrap()
        });
        let h2 = tokio::spawn(async move {
            let txn = db_b.new_transaction().await.unwrap();
            let store = txn.get_store::<DocStore>("d").await.unwrap();
            store.set("k2", "v2").await.unwrap();
            txn.commit().await.unwrap()
        });
        let id1 = h1.await.unwrap();
        let id2 = h2.await.unwrap();

        let recorded = events.lock().unwrap();
        assert_eq!(
            recorded.len(),
            2,
            "iter {iter}: both writes should fire callbacks"
        );

        let serialized = recorded
            .iter()
            .any(|(prev, _)| prev.contains(&id1) || prev.contains(&id2));
        assert!(
            serialized,
            "iter {iter}: concurrent writes must produce a serial previous_tips chain; got events: {:?}",
            *recorded
        );
    }
}

/// Two callbacks on the same tree, registered at different cursors,
/// each see *their own* `previous_tips` on the next fire. This locks
/// in the per-callback cursor semantics introduced by the cursor
/// refactor (private brain note:
/// write-callback-cursor-refactor-plan.md).
#[tokio::test]
async fn test_per_callback_cursor_independent_previous_tips() {
    let (_instance, db) = setup_callback_test().await;

    // Tips at T0 (before any commit).
    let tips_t0 = db.get_tips().await.unwrap();

    // First commit. cb1 will be registered at T0 *before* this commit
    // exists, so cb1's cursor stays at T0; cb2 is registered after,
    // anchored at T1.
    let cb1_events: Arc<Mutex<Vec<Vec<crate::entry::ID>>>> = Arc::new(Mutex::new(Vec::new()));
    let cb1_events_clone = cb1_events.clone();
    let _cb1 = db
        .on_write_at_tips(tips_t0.clone(), move |event, _db| {
            let prev = event.previous_tips().to_vec();
            let evs = cb1_events_clone.clone();
            async move {
                evs.lock().unwrap().push(prev);
                Ok(())
            }
        })
        .await
        .unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    let id1 = txn.commit().await.unwrap();

    let tips_t1 = db.get_tips().await.unwrap();
    assert!(
        tips_t1.contains(&id1),
        "tips_t1 must include the just-committed entry"
    );

    // Register cb2 at T1 — its cursor anchors here, distinct from cb1's.
    let cb2_events: Arc<Mutex<Vec<Vec<crate::entry::ID>>>> = Arc::new(Mutex::new(Vec::new()));
    let cb2_events_clone = cb2_events.clone();
    let _cb2 = db
        .on_write_at_tips(tips_t1.clone(), move |event, _db| {
            let prev = event.previous_tips().to_vec();
            let evs = cb2_events_clone.clone();
            async move {
                evs.lock().unwrap().push(prev);
                Ok(())
            }
        })
        .await
        .unwrap();

    // Second commit. cb1's first event has prev=T1 (its cursor advanced
    // when commit-1 fired). cb2's first event has prev=T1 (its initial
    // cursor). Wait — cb1's *first* fire was for commit-1: prev=T0,
    // entries=[id1], cursor advances to T1. cb1's *second* fire (this
    // commit) should have prev=T1.
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v2").await.unwrap();
    let id2 = txn.commit().await.unwrap();

    let cb1_recorded = cb1_events.lock().unwrap().clone();
    let cb2_recorded = cb2_events.lock().unwrap().clone();

    // cb1: two fires. First with prev=T0 (initial cursor), second with
    // prev=T1 (cursor advanced after first fire).
    assert_eq!(cb1_recorded.len(), 2, "cb1 should fire twice");
    assert_eq!(
        cb1_recorded[0], tips_t0,
        "cb1's first fire's prev should equal its initial cursor (T0)"
    );
    assert!(
        cb1_recorded[1].contains(&id1),
        "cb1's second fire's prev should reflect the post-commit-1 cursor; got {:?}",
        cb1_recorded[1]
    );

    // cb2: one fire (registered after commit-1), with prev=T1 (initial
    // cursor) — independent of cb1's cursor history.
    assert_eq!(cb2_recorded.len(), 1, "cb2 should fire once (post-register)");
    assert!(
        cb2_recorded[0].contains(&id1),
        "cb2's first fire's prev should equal its initial cursor (T1, which contains id1); got {:?}",
        cb2_recorded[0]
    );
    assert!(
        !cb2_recorded[0].contains(&id2),
        "cb2's first fire's prev must NOT yet contain id2 (the entry it is being notified about)"
    );
}

// ===== ids_added DAG-diff helper =====

#[tokio::test]
async fn test_ids_added_empty_when_cursors_equal() {
    let (_instance, db) = setup_callback_test().await;
    let tips = db.get_tips().await.unwrap();
    let added = db.ids_added(&tips, &tips).await.unwrap();
    assert!(added.is_empty(), "equal cursors should yield empty diff");
}

#[tokio::test]
async fn test_ids_added_single_commit() {
    let (_instance, db) = setup_callback_test().await;
    let prev = db.get_tips().await.unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    let id1 = txn.commit().await.unwrap();

    let post = db.get_tips().await.unwrap();
    let added = db.ids_added(&prev, &post).await.unwrap();

    assert_eq!(
        added,
        vec![id1.clone()],
        "single commit should add exactly the new entry"
    );
}

#[tokio::test]
async fn test_ids_added_multi_commit_topo_order() {
    let (_instance, db) = setup_callback_test().await;
    let prev = db.get_tips().await.unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    let id1 = txn.commit().await.unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v2").await.unwrap();
    let id2 = txn.commit().await.unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v3").await.unwrap();
    let id3 = txn.commit().await.unwrap();

    let post = db.get_tips().await.unwrap();
    let added = db.ids_added(&prev, &post).await.unwrap();

    assert_eq!(added.len(), 3, "three commits should add three entries");
    let pos1 = added.iter().position(|i| i == &id1).expect("id1 in added");
    let pos2 = added.iter().position(|i| i == &id2).expect("id2 in added");
    let pos3 = added.iter().position(|i| i == &id3).expect("id3 in added");
    assert!(pos1 < pos2, "id1 (parent) must precede id2 (child) in topo order");
    assert!(pos2 < pos3, "id2 (parent) must precede id3 (child) in topo order");
}

#[tokio::test]
async fn test_ids_added_skips_entries_before_cursor() {
    let (_instance, db) = setup_callback_test().await;

    // Commit one entry, then snapshot the cursor *after* it.
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    let id1 = txn.commit().await.unwrap();
    let after_first = db.get_tips().await.unwrap();

    // Two more commits past that cursor.
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v2").await.unwrap();
    let id2 = txn.commit().await.unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v3").await.unwrap();
    let id3 = txn.commit().await.unwrap();

    let post = db.get_tips().await.unwrap();
    let added = db.ids_added(&after_first, &post).await.unwrap();

    assert!(!added.contains(&id1), "entry at cursor must be excluded");
    assert!(added.contains(&id2), "post-cursor entry id2 must be included");
    assert!(added.contains(&id3), "post-cursor entry id3 must be included");
    assert_eq!(added.len(), 2, "exactly the two post-cursor entries");
}

#[tokio::test]
async fn test_ids_added_empty_previous_returns_full_closure() {
    let (_instance, db) = setup_callback_test().await;

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    let id1 = txn.commit().await.unwrap();

    let post = db.get_tips().await.unwrap();
    let added = db.ids_added(&[], &post).await.unwrap();

    // With empty cursor, every ancestor reachable from post_tips is "added",
    // which for a fresh database is the root + every committed entry.
    assert!(
        added.contains(&id1),
        "empty cursor should include all reachable entries; missing id1"
    );
    assert!(
        added.contains(db.root_id()),
        "empty cursor should include the root entry"
    );
}
