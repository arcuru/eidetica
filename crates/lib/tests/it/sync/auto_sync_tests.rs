//! Tests for automatic sync-on-commit functionality.
//!
//! These tests verify that the global callback registered in Instance::enable_sync()
//! correctly triggers sync operations based on combined settings.
//!
//! Note: Since we can't directly test the private callback registration from integration tests,
//! we test the observable behavior: that enable_sync() works and doesn't crash when commits occur.

use crate::helpers::test_instance;
use eidetica::{
    crdt::Doc,
    user::types::{DatabasePreferences, SyncSettings},
};

/// Test that commits work correctly when sync is enabled
/// This verifies the callback doesn't break normal operation
#[test]
fn test_commits_work_with_sync_enabled() {
    let instance = test_instance();
    instance.enable_sync().expect("Enable sync");

    // Create user and database
    instance.create_user("testuser", None).expect("Create user");
    let mut user = instance.login_user("testuser", None).expect("Login user");

    let mut db_settings = Doc::new();
    db_settings.set_string("name", "test_db");
    let key_id = user.get_default_key().expect("Get default key");
    let db = user
        .create_database(db_settings, &key_id)
        .expect("Create database");
    let db_id = db.root_id().clone();

    // Add database with sync enabled
    user.add_database(DatabasePreferences {
        database_id: db_id.clone(),
        key_id: key_id.clone(),
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: true,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .expect("Add database");

    // Register user with sync
    let sync = instance.sync().expect("Sync should exist");
    sync.sync_user(user.user_uuid(), user.user_database().root_id())
        .expect("Register user");

    std::thread::sleep(std::time::Duration::from_millis(50));

    // Make a write - callback should be invoked but handle gracefully (no transport)
    let tx = db.new_transaction().expect("Create transaction");
    let store = tx
        .get_store::<eidetica::store::DocStore>("data")
        .expect("Get store");
    let mut doc = Doc::new();
    doc.set_string("message", "Test");
    store.set("key1", doc).expect("Set doc");

    let entry_id = tx.commit().expect("Commit should succeed");

    // Verify the entry was actually committed
    let fetched = instance.backend().get(&entry_id).expect("Get entry");
    assert_eq!(fetched.id(), &entry_id, "Entry should exist in backend");
}

/// Test that multiple commits work correctly
#[test]
fn test_multiple_commits_with_sync_enabled() {
    let instance = test_instance();
    instance.enable_sync().expect("Enable sync");

    instance.create_user("testuser", None).expect("Create user");
    let mut user = instance.login_user("testuser", None).expect("Login user");

    let mut db_settings = Doc::new();
    db_settings.set_string("name", "test_db");
    let key_id = user.get_default_key().expect("Get default key");
    let db = user
        .create_database(db_settings, &key_id)
        .expect("Create database");

    // Make 5 writes - all should succeed
    for i in 0..5 {
        let tx = db.new_transaction().expect("Create transaction");
        let store = tx
            .get_store::<eidetica::store::DocStore>("data")
            .expect("Get store");
        let mut doc = Doc::new();
        doc.set_string("message", format!("Message {}", i));
        store.set(format!("key{}", i), doc).expect("Set doc");
        tx.commit()
            .unwrap_or_else(|_| panic!("Commit {} should succeed", i));
    }
}

/// Test that commits work when sync_enabled=false
#[test]
fn test_commits_with_sync_disabled() {
    let instance = test_instance();
    instance.enable_sync().expect("Enable sync");

    instance.create_user("testuser", None).expect("Create user");
    let mut user = instance.login_user("testuser", None).expect("Login user");

    let mut db_settings = Doc::new();
    db_settings.set_string("name", "test_db");
    let key_id = user.get_default_key().expect("Get default key");
    let db = user
        .create_database(db_settings, &key_id)
        .expect("Create database");
    let db_id = db.root_id().clone();

    // Add database with sync disabled
    user.add_database(DatabasePreferences {
        database_id: db_id.clone(),
        key_id: key_id.clone(),
        sync_settings: SyncSettings {
            sync_enabled: false,
            sync_on_commit: true,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .expect("Add database");

    // Commit should work fine
    let tx = db.new_transaction().expect("Create transaction");
    let store = tx
        .get_store::<eidetica::store::DocStore>("data")
        .expect("Get store");
    let mut doc = Doc::new();
    doc.set_string("message", "Sync disabled");
    store.set("key1", doc).expect("Set doc");
    tx.commit().expect("Commit should succeed");
}

/// Test that commits work when sync_on_commit=false
#[test]
fn test_commits_with_sync_on_commit_disabled() {
    let instance = test_instance();
    instance.enable_sync().expect("Enable sync");

    instance.create_user("testuser", None).expect("Create user");
    let mut user = instance.login_user("testuser", None).expect("Login user");

    let mut db_settings = Doc::new();
    db_settings.set_string("name", "test_db");
    let key_id = user.get_default_key().expect("Get default key");
    let db = user
        .create_database(db_settings, &key_id)
        .expect("Create database");
    let db_id = db.root_id().clone();

    // Add database with sync_on_commit=false
    user.add_database(DatabasePreferences {
        database_id: db_id.clone(),
        key_id: key_id.clone(),
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: Some(3600),
            properties: Default::default(),
        },
    })
    .expect("Add database");

    // Commit should work fine
    let tx = db.new_transaction().expect("Create transaction");
    let store = tx
        .get_store::<eidetica::store::DocStore>("data")
        .expect("Get store");
    let mut doc = Doc::new();
    doc.set_string("message", "Sync on commit disabled");
    store.set("key1", doc).expect("Set doc");
    tx.commit().expect("Commit should succeed");
}

/// Test that commits work before transport is enabled
#[test]
fn test_commits_before_transport_enabled() {
    let instance = test_instance();
    instance.enable_sync().expect("Enable sync");

    instance.create_user("testuser", None).expect("Create user");
    let mut user = instance.login_user("testuser", None).expect("Login user");

    let mut db_settings = Doc::new();
    db_settings.set_string("name", "test_db");
    let key_id = user.get_default_key().expect("Get default key");
    let db = user
        .create_database(db_settings, &key_id)
        .expect("Create database");

    // Don't enable transport, but commits should still work
    let tx = db.new_transaction().expect("Create transaction");
    let store = tx
        .get_store::<eidetica::store::DocStore>("data")
        .expect("Get store");
    let mut doc = Doc::new();
    doc.set_string("message", "No transport");
    store.set("key1", doc).expect("Set doc");
    tx.commit()
        .expect("Commit should succeed even without transport");
}

/// Test that commits work with multiple databases
#[test]
fn test_commits_with_multiple_databases() {
    let instance = test_instance();
    instance.enable_sync().expect("Enable sync");

    instance.create_user("testuser", None).expect("Create user");
    let mut user = instance.login_user("testuser", None).expect("Login user");
    let key_id = user.get_default_key().expect("Get default key");

    // Create two databases
    let mut db1_settings = Doc::new();
    db1_settings.set_string("name", "db1");
    let db1 = user
        .create_database(db1_settings, &key_id)
        .expect("Create db1");

    let mut db2_settings = Doc::new();
    db2_settings.set_string("name", "db2");
    let db2 = user
        .create_database(db2_settings, &key_id)
        .expect("Create db2");

    // Write to both
    let tx1 = db1.new_transaction().expect("Create transaction");
    let store1 = tx1
        .get_store::<eidetica::store::DocStore>("data")
        .expect("Get store");
    let mut doc1 = Doc::new();
    doc1.set_string("message", "DB1");
    store1.set("key1", doc1).expect("Set doc");
    tx1.commit().expect("Commit db1");

    let tx2 = db2.new_transaction().expect("Create transaction");
    let store2 = tx2
        .get_store::<eidetica::store::DocStore>("data")
        .expect("Get store");
    let mut doc2 = Doc::new();
    doc2.set_string("message", "DB2");
    store2.set("key1", doc2).expect("Set doc");
    tx2.commit().expect("Commit db2");
}

/// Test that sync can be enabled multiple times (idempotent)
#[test]
fn test_enable_sync_is_idempotent() {
    let instance = test_instance();

    instance.enable_sync().expect("First enable_sync");
    instance
        .enable_sync()
        .expect("Second enable_sync should be no-op");
    instance
        .enable_sync()
        .expect("Third enable_sync should be no-op");

    // Should still work
    assert!(instance.sync().is_some(), "Sync should exist");
}
