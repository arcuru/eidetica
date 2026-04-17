//! Tests for the database module.

use std::sync::{Arc, Mutex};

use super::*;
use crate::{
    auth::crypto::generate_keypair,
    backend::database::InMemory,
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

    db.on_local_write(move |event, _db, _instance| {
        let prev_tips = event.previous_tips().to_vec();
        let source = event.source();
        events_clone.lock().unwrap().push((prev_tips, source));
        Box::pin(async move { Ok(()) })
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

    db.on_local_write(move |event, _db, _instance| {
        counts_clone.lock().unwrap().push(event.entries().len());
        Box::pin(async move { Ok(()) })
    })
    .unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v").await.unwrap();
    txn.commit().await.unwrap();

    let counts = entry_counts.lock().unwrap();
    assert_eq!(counts.len(), 1);
    assert_eq!(counts[0], 1, "local write events should contain exactly one entry");
}

#[tokio::test]
async fn test_local_write_entry_convenience_method() {
    let (_instance, db) = setup_callback_test().await;

    let had_entry: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
    let flag = had_entry.clone();

    db.on_local_write(move |event, _db, _instance| {
        *flag.lock().unwrap() = Some(event.entry().is_some());
        Box::pin(async move { Ok(()) })
    })
    .unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v").await.unwrap();
    txn.commit().await.unwrap();

    assert_eq!(*had_entry.lock().unwrap(), Some(true));
}

#[tokio::test]
async fn test_remote_write_callback_fires_via_put_remote_entries() {
    let (instance, db) = setup_callback_test().await;

    let remote_events: Arc<Mutex<Vec<(usize, WriteSource)>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = remote_events.clone();

    db.on_remote_write(move |event, _db, _instance| {
        events_clone
            .lock()
            .unwrap()
            .push((event.entries().len(), event.source()));
        Box::pin(async move { Ok(()) })
    })
    .unwrap();

    // Commit locally — should NOT fire remote callback
    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("key", "local").await.unwrap();
    let local_id = txn.commit().await.unwrap();

    assert!(
        remote_events.lock().unwrap().is_empty(),
        "on_remote_write should not fire for local commits"
    );

    // Simulate remote sync via put_remote_entries
    let entry = instance.get(db.root_id()).await.unwrap();
    let local_entry = instance.get(&local_id).await.unwrap();
    instance
        .put_remote_entries(db.root_id(), vec![entry, local_entry])
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

    db.on_remote_write(move |event, _db, _instance| {
        log_clone
            .lock()
            .unwrap()
            .push(event.previous_tips().to_vec());
        Box::pin(async move { Ok(()) })
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
        .put_remote_entries(db.root_id(), vec![entry])
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

    db.on_remote_write(move |_event, _db, _instance| {
        *count_clone.lock().unwrap() += 1;
        Box::pin(async move { Ok(()) })
    })
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
    db.on_local_write(move |_event, _db, _instance| {
        Box::pin(async move {
            Err(crate::Error::Io(std::io::Error::other("test error")))
        })
    })
    .unwrap();

    // Second callback should still execute
    db.on_local_write(move |_event, _db, _instance| {
        *flag.lock().unwrap() = true;
        Box::pin(async move { Ok(()) })
    })
    .unwrap();

    let txn = db.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v").await.unwrap();
    txn.commit().await.unwrap();

    assert!(*second_fired.lock().unwrap());
}
