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
            let prev_tips = event.previous_tips().tips().to_vec();
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
async fn test_local_write_event_brackets_one_entry() {
    let (_instance, db) = setup_callback_test().await;

    let added_counts: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let counts_clone = added_counts.clone();

    let _cb = db
        .on_write(move |event, db| {
            let prev = event.previous_tips().clone();
            let post = event.post_tips().clone();
            let db = db.clone();
            let counts = counts_clone.clone();
            async move {
                let ids = db.ids_added(&prev, &post).await?;
                counts.lock().unwrap().push(ids.len());
                Ok(())
            }
        })
        .await
        .unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v").await.unwrap();
    txn.commit().await.unwrap();

    let counts = added_counts.lock().unwrap();
    assert_eq!(counts.len(), 1);
    assert_eq!(
        counts[0], 1,
        "local write should bracket exactly one new entry"
    );
}

#[tokio::test]
async fn test_remote_write_callback_fires_via_put_remote_entries() {
    use crate::backend::VerificationStatus;
    let (instance, db) = setup_callback_test().await;

    let remote_events: Arc<Mutex<Vec<WriteSource>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = remote_events.clone();

    let _cb = db
        .on_write(move |event, _db| {
            let source = event.source();
            let events = events_clone.clone();
            async move {
                if source == WriteSource::Remote {
                    events.lock().unwrap().push(source);
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
    //
    // Note: under the cursor-only WriteEvent shape, the callback's
    // cursor was already advanced past these entries by the prior
    // local-commit fire. The verify-promotion fire therefore lands
    // with `previous_tips == post_tips` (the DAG didn't change — only
    // verification status did). `ids_added` would correctly return
    // empty for this fire. We assert the fire *happens* with the
    // expected source; cursor-diff enumeration is covered by the
    // dedicated `ids_added` tests.
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
        "verify should fire one batched Remote event for the promotion"
    );
    assert_eq!(events[0], WriteSource::Remote);
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
            let prev = event.previous_tips().tips().to_vec();
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
async fn test_remote_callback_catches_up_promoted_entries_via_ids_added() {
    // Verifies the canonical sync-ingest shape: a callback registered with
    // a stale cursor catches up on every entry promoted by the subsequent
    // verify pass via `ids_added(prev, post)`.
    //
    // Sequence:
    //   1. Two entries are committed locally (no callback yet — no fires,
    //      no cursor moves).
    //   2. Both are demoted to Unverified, simulating the state the daemon
    //      would observe after raw sync ingest (entries on disk, not yet
    //      settled).
    //   3. A callback is registered with `on_write_at_tips(vec![root_id])`
    //      — the cursor anchored at the pre-sync frontier.
    //   4. `put_remote_entries` runs the verify pass, which promotes both
    //      entries and fires once. `ids_added(prev=cursor=[root], post=raw_tips)`
    //      yields exactly the two promoted entries.
    use crate::backend::VerificationStatus;
    let (instance, db) = setup_callback_test().await;

    // Two local commits (no callback, no fires).
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    let id1 = txn.commit().await.unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v2").await.unwrap();
    let id2 = txn.commit().await.unwrap();

    // Demote both to Unverified.
    let backend = instance.require_local_engine().unwrap();
    backend
        .update_verification_status(&id1, VerificationStatus::Unverified)
        .await
        .unwrap();
    backend
        .update_verification_status(&id2, VerificationStatus::Unverified)
        .await
        .unwrap();

    // Now register the callback with the cursor anchored at the root —
    // i.e. the pre-sync frontier from the callback's perspective.
    let root_id = db.root_id().clone();
    let captured: Arc<Mutex<Vec<crate::entry::ID>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = captured.clone();

    db.on_write_at_tips(Snapshot::new(vec![root_id.clone()]), move |event, db| {
        let prev = event.previous_tips().clone();
        let post = event.post_tips().clone();
        let source = event.source();
        let db = db.clone();
        let captured = captured_clone.clone();
        async move {
            if source == WriteSource::Remote {
                let ids = db.ids_added(&prev, &post).await?;
                captured.lock().unwrap().extend(ids);
            }
            Ok(())
        }
    })
    .await
    .unwrap()
    .detach();

    // Replay the entries through the remote-ingest path. The backend already
    // holds them, so the put is a no-op; verify is what promotes them and
    // fires the Remote event.
    let entry1 = instance.get(&id1).await.unwrap();
    let entry2 = instance.get(&id2).await.unwrap();
    instance
        .put_remote_entries(&root_id, vec![entry1, entry2])
        .await
        .unwrap();

    let captured_ids = captured.lock().unwrap();
    assert!(
        captured_ids.contains(&id1) && captured_ids.contains(&id2),
        "ids_added must surface both promoted entries; got {captured_ids:?}",
    );
}

// Regression: a write callback may re-enter its tree's lock without
// deadlocking. The dispatch path holds `tree_lock(root_id)` only across
// the synchronous cursor-advance + spawn phase; the user closures are
// awaited *after* the guard is dropped. Before that split, `put_entry`
// (and `verify`) awaited the callback while still holding the lock, so a
// callback that re-entered `tree_lock` blocked forever against the
// dispatcher that was waiting on it.
//
// The re-entry is driven by an explicit `db.verify().await` inside the
// callback. `verify()` unconditionally acquires `tree_lock`, so this is
// the cleanest deterministic trigger for the exact lock re-acquire that a
// tip read (`Database::snapshot`) reaches transitively via the
// access-time auto-verify hook — no special DAG shape required. On the
// pre-fix code the commit hangs and the timeout fires.
//
// Multi-thread runtime so the spawned callback task can run on a
// different worker than the firing path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_callback_reentrant_tree_lock_local_commit_no_deadlock() {
    let (_instance, db) = setup_callback_test().await;

    let _cb = db
        .on_write(move |_event, db| {
            let db = db.clone();
            async move {
                // Re-enters tree_lock(root_id). Must not deadlock against
                // the `put_entry` that fired us.
                let _ = db.verify().await;
                Ok(())
            }
        })
        .await
        .unwrap();

    let db_for_commit = db.clone();
    let commit = async move {
        let txn = db_for_commit.new_transaction().await.unwrap();
        let store = txn.get_store::<DocStore>("data").await.unwrap();
        store.set("k", "v").await.unwrap();
        txn.commit().await.unwrap();
    };

    tokio::time::timeout(std::time::Duration::from_secs(10), commit)
        .await
        .expect("local commit + re-entrant callback must not deadlock");
}

// Companion to the above for the verify-promotion fire path: when
// `put_remote_entries` runs the verify pass, it promotes the ingested
// entry and fires the callback *while holding tree_lock*. A callback that
// re-enters the lock (here, again via `verify()`) must not deadlock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_callback_reentrant_tree_lock_verify_promote_no_deadlock() {
    use crate::backend::VerificationStatus;
    let (instance, db) = setup_callback_test().await;

    // One committed entry, demoted to Unverified to mimic raw sync ingest.
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v").await.unwrap();
    let id1 = txn.commit().await.unwrap();
    let backend = instance.require_local_engine().unwrap();
    backend
        .update_verification_status(&id1, VerificationStatus::Unverified)
        .await
        .unwrap();

    let root_id = db.root_id().clone();
    let _cb = db
        .on_write_at_tips(Snapshot::new(vec![root_id.clone()]), move |_event, db| {
            let db = db.clone();
            async move {
                let _ = db.verify().await;
                Ok(())
            }
        })
        .await
        .unwrap();

    // verify() promotes id1 and fires the callback under tree_lock; the
    // callback's own verify() re-acquires that lock.
    let entry1 = instance.get(&id1).await.unwrap();
    let fire = instance.put_remote_entries(&root_id, vec![entry1]);

    tokio::time::timeout(std::time::Duration::from_secs(10), fire)
        .await
        .expect("verify-promotion fire + re-entrant callback must not deadlock")
        .unwrap();
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

        type EventRecord = (Snapshot, Vec<crate::entry::ID>);
        let events: Arc<Mutex<Vec<EventRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();

        let _cb = db
            .on_write(move |event, db| {
                let prev = event.previous_tips().clone();
                let post = event.post_tips().clone();
                let db = db.clone();
                let evs = events_clone.clone();
                async move {
                    let ids = db.ids_added(&prev, &post).await?;
                    evs.lock().unwrap().push((prev, ids));
                    Ok(())
                }
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
            .any(|(prev, _)| prev.tips().contains(&id1) || prev.tips().contains(&id2));
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
    let tips_t0 = db.snapshot().await.unwrap();

    // First commit. cb1 will be registered at T0 *before* this commit
    // exists, so cb1's cursor stays at T0; cb2 is registered after,
    // anchored at T1.
    let cb1_events: Arc<Mutex<Vec<Snapshot>>> = Arc::new(Mutex::new(Vec::new()));
    let cb1_events_clone = cb1_events.clone();
    let _cb1 = db
        .on_write_at_tips(tips_t0.clone(), move |event, _db| {
            let prev = event.previous_tips().clone();
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

    let tips_t1 = db.snapshot().await.unwrap();
    assert!(
        tips_t1.tips().contains(&id1),
        "tips_t1 must include the just-committed entry"
    );

    // Register cb2 at T1 — its cursor anchors here, distinct from cb1's.
    let cb2_events: Arc<Mutex<Vec<Snapshot>>> = Arc::new(Mutex::new(Vec::new()));
    let cb2_events_clone = cb2_events.clone();
    let _cb2 = db
        .on_write_at_tips(tips_t1.clone(), move |event, _db| {
            let prev = event.previous_tips().clone();
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
        cb1_recorded[1].tips().contains(&id1),
        "cb1's second fire's prev should reflect the post-commit-1 cursor; got {:?}",
        cb1_recorded[1]
    );

    // cb2: one fire (registered after commit-1), with prev=T1 (initial
    // cursor) — independent of cb1's cursor history.
    assert_eq!(
        cb2_recorded.len(),
        1,
        "cb2 should fire once (post-register)"
    );
    assert!(
        cb2_recorded[0].tips().contains(&id1),
        "cb2's first fire's prev should equal its initial cursor (T1, which contains id1); got {:?}",
        cb2_recorded[0]
    );
    assert!(
        !cb2_recorded[0].tips().contains(&id2),
        "cb2's first fire's prev must NOT yet contain id2 (the entry it is being notified about)"
    );
}

// ===== ids_added DAG-diff helper =====

#[tokio::test]
async fn test_ids_added_empty_when_cursors_equal() {
    let (_instance, db) = setup_callback_test().await;
    let tips = db.snapshot().await.unwrap();
    let added = db.ids_added(&tips, &tips).await.unwrap();
    assert!(added.is_empty(), "equal cursors should yield empty diff");
}

#[tokio::test]
async fn test_ids_added_single_commit() {
    let (_instance, db) = setup_callback_test().await;
    let prev = db.snapshot().await.unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    let id1 = txn.commit().await.unwrap();

    let post = db.snapshot().await.unwrap();
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
    let prev = db.snapshot().await.unwrap();

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

    let post = db.snapshot().await.unwrap();
    let added = db.ids_added(&prev, &post).await.unwrap();

    assert_eq!(added.len(), 3, "three commits should add three entries");
    let pos1 = added.iter().position(|i| i == &id1).expect("id1 in added");
    let pos2 = added.iter().position(|i| i == &id2).expect("id2 in added");
    let pos3 = added.iter().position(|i| i == &id3).expect("id3 in added");
    assert!(
        pos1 < pos2,
        "id1 (parent) must precede id2 (child) in topo order"
    );
    assert!(
        pos2 < pos3,
        "id2 (parent) must precede id3 (child) in topo order"
    );
}

#[tokio::test]
async fn test_ids_added_skips_entries_before_cursor() {
    let (_instance, db) = setup_callback_test().await;

    // Commit one entry, then snapshot the cursor *after* it.
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    let id1 = txn.commit().await.unwrap();
    let after_first = db.snapshot().await.unwrap();

    // Two more commits past that cursor.
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v2").await.unwrap();
    let id2 = txn.commit().await.unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v3").await.unwrap();
    let id3 = txn.commit().await.unwrap();

    let post = db.snapshot().await.unwrap();
    let added = db.ids_added(&after_first, &post).await.unwrap();

    assert!(!added.contains(&id1), "entry at cursor must be excluded");
    assert!(
        added.contains(&id2),
        "post-cursor entry id2 must be included"
    );
    assert!(
        added.contains(&id3),
        "post-cursor entry id3 must be included"
    );
    assert_eq!(added.len(), 2, "exactly the two post-cursor entries");
}

#[tokio::test]
async fn test_ids_added_empty_previous_returns_full_closure() {
    let (_instance, db) = setup_callback_test().await;

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    let id1 = txn.commit().await.unwrap();

    let post = db.snapshot().await.unwrap();
    let added = db.ids_added(&Snapshot::EMPTY, &post).await.unwrap();

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

/// Regression: a **backward** cursor bracket — `previous_tips` strictly newer
/// than `post_tips` — must not expand into the full ancestor closure.
///
/// `ids_added` walks parents from `post_tips` and halts at members of
/// `previous_tips`. When `previous_tips` is newer, its members are
/// *descendants* of the walk's starting point, so the walk never reaches the
/// boundary and runs all the way to the root — reporting the entire history as
/// "added". The doc comment on `ids_added` claims a stale `previous_tips` "can
/// only over-report ... the conservative direction"; that reasoning covers a
/// stale-behind cursor, not one that is ahead of `post_tips`, where the
/// over-report degenerates to O(full history). On a connected instance every
/// step is a permission-checked wire round-trip, so this is a fetch storm.
///
/// Nothing was *added* going backward, so the correct answer is empty.
#[tokio::test]
async fn test_ids_added_backward_bracket_does_not_report_full_history() {
    let (_instance, db) = setup_callback_test().await;

    let mut committed = Vec::new();
    let mut early: Option<Snapshot> = None;
    for i in 0..6 {
        let txn = db.new_transaction().await.unwrap();
        let store = txn.get_store::<DocStore>("data").await.unwrap();
        store.set("k", format!("v{i}")).await.unwrap();
        committed.push(txn.commit().await.unwrap());
        if i == 1 {
            early = Some(db.snapshot().await.unwrap());
        }
    }
    let early = early.unwrap();
    let late = db.snapshot().await.unwrap();
    assert_ne!(early, late, "test setup: cursors must differ");

    // Backward bracket: previous = the LATER cursor, post = the EARLIER one.
    let added = db.ids_added(&late, &early).await.unwrap();

    assert!(
        added.is_empty(),
        "backward bracket must report nothing added; got {} ids for a {}-commit tree \
         (full-history walk: root + every entry)",
        added.len(),
        committed.len()
    );
}

/// Regression: entries that have **not** passed local validation must not
/// appear inside an event bracket.
///
/// `Instance::put_entry` computes `post_tips` from `Instance::snapshot`, and
/// `Database::verify` fires from `raw_tips` — both are the **raw** backend
/// snapshot, whose contract is purely structural (entries with no children in
/// the tree), with no verification filter. Cursors seeded by `on_write` come
/// from the same raw snapshot. So a bracket can span a still-`Unverified`
/// entry, and a subscriber expanding it via `ids_added` enumerates that entry
/// — and can then fetch its body, since `GetEntry` gates read permission only.
///
/// This contradicts the contract stated in `Instance::put_entry`'s own doc
/// ("a subscriber's accumulated state can only ever rest on entries that have
/// passed local validation") and on `ids_added` ("callers driven by an event
/// observe only Verified IDs in practice").
///
/// Second-order consequence, not asserted here: the cursor advances *past* the
/// unverified entry, so when it is later promoted (or fails), that transition
/// falls inside the boundary and is never reported to the subscriber.
///
/// KNOWN-FAILING. The defect is real and reproduced; the fix is deliberately
/// deferred because it turns on how verified/unverified state should be
/// represented in cursors at all — filtering brackets to the Verified frontier
/// changes what every cursor points at (and sync's global hook depends on both
/// ends being raw tips), whereas filtering at enumeration is narrower but
/// leaves the cursor model inconsistent. Tracked as its own task. The fix
/// commit removes this marker.
#[should_panic = "ids_added enumerated an Unverified entry"]
#[tokio::test]
async fn test_ids_added_excludes_unverified_entries() {
    let (instance, db) = setup_callback_test().await;

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    let _id1 = txn.commit().await.unwrap();

    // Cursor sits after the first (verified) commit.
    let cursor = db.snapshot().await.unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v2").await.unwrap();
    let id2 = txn.commit().await.unwrap();

    // Force id2 back to Unverified: models a sync-ingested entry whose local
    // validation is still pending (settings not yet available, partial sync).
    instance
        .demote_to_unverified(db.root_id(), &id2)
        .await
        .unwrap();

    let post = db.snapshot().await.unwrap();
    let added = db.ids_added(&cursor, &post).await.unwrap();

    assert!(
        !added.contains(&id2),
        "ids_added enumerated an Unverified entry ({id2}); the settled-state-only \
         contract says a subscriber never observes unvalidated entries"
    );
}

/// Regression: a `Verified` entry committed on top of an `Unverified` parent
/// must not permanently hide that parent from every future verify pass.
///
/// The targeted-walk `verify()` stops descending at any `Verified` entry,
/// relying on a prefix-closure invariant ("a `Verified` entry hiding an
/// `Unverified` descendant cannot occur") that is maintained only by cascading
/// demote. But `Transaction::commit` stores `Verified` unconditionally — it
/// validates signature and permissions, not parent status — so anchoring a
/// commit in the unverified region via `new_transaction_at` breaks the
/// invariant directly. Once that happens the walk treats the child as a
/// settled boundary and the unverified ancestors become unreachable: they can
/// never be promoted, and can never be reported as failed.
///
/// The pre-`7e593065ca` full-walk verify retried the whole tree each pass, so
/// it recovered from this; the targeted walk does not.
///
/// IGNORED — this construction does not reach the scenario. `demote_to_unverified`
/// followed by a tips-reading commit lets the access-time auto-verify hook
/// re-promote the parent before the child is written, so the precondition
/// asserts below fail (parent is `Verified` again, not `Unverified`) and the
/// hazardous shape is never built. A real repro needs an entry that *cannot* be
/// promoted — i.e. one whose pinned `_settings` are genuinely absent, as after a
/// partial sync — rather than one that is merely marked down. Until that exists,
/// this finding is UNPROVEN, not refuted.
#[ignore = "construction healed by auto-verify; needs a genuinely-unpromotable entry"]
#[tokio::test]
async fn test_verify_reaches_unverified_ancestor_behind_verified_child() {
    let (instance, db) = setup_callback_test().await;

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    let id1 = txn.commit().await.unwrap();

    // Force id1 Unverified: models an entry whose validation is still pending.
    instance
        .demote_to_unverified(db.root_id(), &id1)
        .await
        .unwrap();

    // Commit a child anchored on the now-unverified tip.
    let snapshot = db.snapshot().await.unwrap();
    let txn = db.new_transaction_at(&snapshot).await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v2").await.unwrap();
    let id2 = txn.commit().await.unwrap();

    // Precondition: the test must actually have built the hazardous shape —
    // a Verified child whose parent is the Unverified entry. If the commit
    // path re-anchored elsewhere, or stored the child Unverified, this test
    // proves nothing about the walk.
    let engine = instance.require_local_engine().unwrap();
    let id2_entry = engine.get(&id2).await.unwrap();
    assert!(
        id2_entry.parents().unwrap_or_default().contains(&id1),
        "precondition: child must be anchored on the unverified parent"
    );
    assert_eq!(
        engine.get_verification_status(&id2).await.unwrap(),
        VerificationStatus::Verified,
        "precondition: child must be stored Verified atop an Unverified parent"
    );
    assert_eq!(
        engine.get_verification_status(&id1).await.unwrap(),
        VerificationStatus::Unverified,
        "precondition: parent must still be Unverified before the verify pass"
    );

    // A verify pass must still be able to reach and settle id1.
    db.verify().await.unwrap();

    let status = instance
        .require_local_engine()
        .unwrap()
        .get_verification_status(&id1)
        .await
        .unwrap();
    assert_ne!(
        status,
        VerificationStatus::Unverified,
        "verify() could not reach id1 behind a Verified child; the entry is \
         permanently stranded in the Unverified region"
    );
}
