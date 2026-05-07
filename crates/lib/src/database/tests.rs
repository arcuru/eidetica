//! Tests for the database module.

use std::sync::{Arc, Mutex};

use super::*;
use crate::{
    auth::crypto::generate_keypair,
    backend::{VerificationStatus, database::InMemory},
    instance::WriteSource,
    store::DocStore,
};

#[tokio::test]
async fn test_find_sigkeys_returns_sorted_by_permission() -> Result<()> {
    // Create instance
    let instance = Instance::open(Box::new(InMemory::new())).await?;

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
    let instance = Instance::open(Box::new(InMemory::new())).await?;

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
    let instance = Instance::open(Box::new(InMemory::new())).await?;

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
    let instance = Instance::open(Box::new(InMemory::new())).await.unwrap();
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
        .unwrap();

    // Commit locally — should NOT record (callback filters to remote only)
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("key", "local").await.unwrap();
    let local_id = txn.commit().await.unwrap();

    assert!(
        remote_events.lock().unwrap().is_empty(),
        "remote-only filter should drop local commits"
    );

    // Simulate remote sync via put_remote_entries
    let entry = instance.get(db.root_id()).await.unwrap();
    let local_entry = instance.get(&local_id).await.unwrap();
    instance
        .put_remote_entries(
            db.root_id(),
            VerificationStatus::Verified,
            vec![entry, local_entry],
        )
        .await
        .unwrap();

    let events = remote_events.lock().unwrap();
    assert_eq!(events.len(), 1, "should fire exactly once for the batch");
    assert_eq!(events[0].0, 2, "batch should contain both entries");
    assert_eq!(events[0].1, WriteSource::Remote);
}

#[tokio::test]
async fn test_remote_write_previous_tips() {
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
        .unwrap();

    // Commit locally to advance tips
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v").await.unwrap();
    let local_id = txn.commit().await.unwrap();

    // Simulate remote write
    let entry = instance.get(&local_id).await.unwrap();
    instance
        .put_remote_entries(db.root_id(), VerificationStatus::Verified, vec![entry])
        .await
        .unwrap();

    let log = prev_tips_log.lock().unwrap();
    assert_eq!(log.len(), 1);
    assert!(
        log[0].contains(&local_id),
        "previous_tips should reflect state before the remote batch"
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
        .unwrap();

    instance
        .put_remote_entries(db.root_id(), VerificationStatus::Verified, vec![])
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
        .unwrap();

    // Second callback should still execute
    let _cb2 = db
        .on_write(move |_event, _db| {
            *flag.lock().unwrap() = true;
            async { Ok(()) }
        })
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
        .unwrap();

    let cb2_clone = cb2_count.clone();
    let _cb2 = db
        .on_write(move |_event, _db| {
            *cb2_clone.lock().unwrap() += 1;
            async { Ok(()) }
        })
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
    .unwrap()
    .detach();

    // Build entries via local commits, then re-feed them as a remote batch.
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v1").await.unwrap();
    let id1 = txn.commit().await.unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v2").await.unwrap();
    let id2 = txn.commit().await.unwrap();

    let entry1 = instance.get(&id1).await.unwrap();
    let entry2 = instance.get(&id2).await.unwrap();

    instance
        .put_remote_entries(
            db.root_id(),
            VerificationStatus::Verified,
            vec![entry1, entry2],
        )
        .await
        .unwrap();

    // The callback's WriteEvent.entries() must contain exactly the entries
    // that were stored — no more, no less, in input order. This pins the
    // contract: storage failures inside put_remote_entries are silent skips,
    // and the callback only sees `stored_entries`. (The in-memory backend
    // doesn't make it easy to inject a partial-store failure; we exercise the
    // success path here and rely on code review for the failure branch.)
    let captured_ids = captured.lock().unwrap();
    assert_eq!(captured_ids.len(), 2);
    assert_eq!(captured_ids[0], id1);
    assert_eq!(captured_ids[1], id2);
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
