//! End-to-end test for automatic sync-on-commit.
//!
//! This test verifies that writes to one instance automatically sync to another
//! without any manual hook registration.

use crate::helpers::test_instance;
use eidetica::{
    crdt::Doc,
    sync::peer_types::Address,
    user::types::{SyncSettings, TrackedDatabase},
};
use std::time::Duration;
use tokio::time::sleep;

/// Test that writes automatically sync between two instances
#[tokio::test]
async fn test_auto_sync_between_instances() -> eidetica::Result<()> {
    println!("\n=== Testing Automatic Sync Between Two Instances ===\n");

    // Create two instances
    let instance1 = test_instance().await;
    let instance2 = test_instance().await;

    // Enable sync on both - this registers the automatic sync callback
    instance1.enable_sync().await?;
    instance2.enable_sync().await?;

    let sync1 = instance1.sync().expect("Sync1 should exist");
    let sync2 = instance2.sync().expect("Sync2 should exist");

    // Set up HTTP transport
    sync1.enable_http_transport().await?;
    sync2.enable_http_transport().await?;

    // Start server on sync2
    sync2.start_server("127.0.0.1:0").await?;
    let server_addr = sync2.get_server_address().await?;
    sleep(Duration::from_millis(100)).await;

    // Get peer public keys
    let peer1_pubkey = sync1.get_device_public_key().await?;
    let peer2_pubkey = sync2.get_device_public_key().await?;

    println!("Instance 1 pubkey: {peer1_pubkey}");
    println!("Instance 2 pubkey: {peer2_pubkey}");

    // Register peers with each other
    let server_address = Address::http(server_addr);
    sync1
        .register_peer(&peer2_pubkey, Some("instance2"))
        .await?;
    sync1
        .add_peer_address(&peer2_pubkey, server_address)
        .await?;

    sync2
        .register_peer(&peer1_pubkey, Some("instance1"))
        .await?;

    // Create user on instance1
    instance1.create_user("alice", None).await?;
    let mut user1 = instance1.login_user("alice", None).await?;

    // Create a database
    let mut db_settings = Doc::new();
    db_settings.set("name", "shared_notes");
    let key_id = user1.get_default_key()?;
    let db1 = user1.create_database(db_settings, &key_id).await?;
    let db_id = db1.root_id().clone();

    println!("Created database with ID: {db_id}");

    // Configure sync settings for this database with sync_on_commit enabled
    // This automatically registers the user with sync and updates combined settings
    user1
        .track_database(TrackedDatabase {
            database_id: db_id.clone(),
            key_id: key_id.clone(),
            sync_settings: SyncSettings {
                sync_enabled: true,
                sync_on_commit: true, // THIS is what triggers auto-sync
                interval_seconds: None,
                properties: Default::default(),
            },
        })
        .await?;

    // Add peer2 as a sync target for this database
    sync1.add_tree_sync(&peer2_pubkey, &db_id).await?;

    println!("Configured sync settings and peer mapping");

    println!("\n--- Writing to instance1 ---");

    // Write to database on instance1 - should automatically sync to instance2!
    let tx = db1.new_transaction().await?;
    let store = tx.get_store::<eidetica::store::DocStore>("notes").await?;
    let mut note = Doc::new();
    note.set("title", "Meeting Notes");
    note.set("content", "Discuss automatic sync implementation");
    note.set("author", "alice");
    store.set("note1", note).await?;

    let entry_id = tx.commit().await?;
    println!("Committed entry {entry_id} to instance1");

    // Flush sync queue to send entries immediately
    println!("\n--- Flushing sync queue ---");
    sync1.flush().await?;

    // Verify entry was automatically synced to instance2's backend
    println!("\n--- Checking instance2 backend ---");
    let synced_entry_result = instance2.backend().get(&entry_id).await;

    assert!(
        synced_entry_result.is_ok(),
        "Entry should have automatically synced to instance2 backend"
    );

    let synced_entry = synced_entry_result?;
    assert_eq!(
        synced_entry.id(),
        &entry_id,
        "Synced entry ID should match original"
    );

    println!("✅ SUCCESS! Entry {entry_id} automatically synced from instance1 to instance2");
    println!("   - No manual hook registration required");
    println!("   - Triggered by Instance::enable_sync() callback");
    println!("   - Controlled by sync_on_commit=true setting");

    Ok(())
}

/// Test bidirectional automatic sync
#[tokio::test]
async fn test_bidirectional_auto_sync() -> eidetica::Result<()> {
    println!("\n=== Testing Bidirectional Automatic Sync ===\n");

    // Create two instances
    let instance1 = test_instance().await;
    let instance2 = test_instance().await;

    instance1.enable_sync().await?;
    instance2.enable_sync().await?;

    let sync1 = instance1.sync().unwrap();
    let sync2 = instance2.sync().unwrap();

    // Set up HTTP transport
    sync1.enable_http_transport().await?;
    sync2.enable_http_transport().await?;

    // Start servers on both instances
    sync1.start_server("127.0.0.1:0").await?;
    sync2.start_server("127.0.0.1:0").await?;

    let server1_addr = sync1.get_server_address().await?;
    let server2_addr = sync2.get_server_address().await?;

    sleep(Duration::from_millis(100)).await;

    let peer1_pubkey = sync1.get_device_public_key().await?;
    let peer2_pubkey = sync2.get_device_public_key().await?;

    // Register peers bidirectionally
    let address1 = Address::http(server1_addr);
    let address2 = Address::http(server2_addr);

    sync1
        .register_peer(&peer2_pubkey, Some("instance2"))
        .await?;
    sync1.add_peer_address(&peer2_pubkey, address2).await?;

    sync2
        .register_peer(&peer1_pubkey, Some("instance1"))
        .await?;
    sync2.add_peer_address(&peer1_pubkey, address1).await?;

    // Create users on both instances
    instance1.create_user("alice", None).await?;
    let mut user1 = instance1.login_user("alice", None).await?;

    instance2.create_user("bob", None).await?;
    let mut user2 = instance2.login_user("bob", None).await?;

    // Create database on instance1
    let mut db_settings = Doc::new();
    db_settings.set("name", "collaboration_space");
    let key1 = user1.get_default_key()?;
    let db1 = user1.create_database(db_settings.clone(), &key1).await?;
    let db1_id = db1.root_id().clone();

    // Create database on instance2 (in real scenario, would bootstrap from instance1)
    let key2 = user2.get_default_key()?;
    let db2 = user2.create_database(db_settings, &key2).await?;
    let db2_id = db2.root_id().clone();

    // Configure sync on both instances
    // track_database() automatically registers users with sync and updates combined settings
    user1
        .track_database(TrackedDatabase {
            database_id: db1_id.clone(),
            key_id: key1.clone(),
            sync_settings: SyncSettings {
                sync_enabled: true,
                sync_on_commit: true,
                interval_seconds: None,
                properties: Default::default(),
            },
        })
        .await?;

    user2
        .track_database(TrackedDatabase {
            database_id: db2_id.clone(),
            key_id: key2.clone(),
            sync_settings: SyncSettings {
                sync_enabled: true,
                sync_on_commit: true,
                interval_seconds: None,
                properties: Default::default(),
            },
        })
        .await?;

    // Configure peer mappings
    sync1.add_tree_sync(&peer2_pubkey, &db1_id).await?;
    sync2.add_tree_sync(&peer1_pubkey, &db2_id).await?;

    println!("--- Alice writes on instance1 ---");
    let tx1 = db1.new_transaction().await?;
    let store1 = tx1
        .get_store::<eidetica::store::DocStore>("messages")
        .await?;
    let mut msg1 = Doc::new();
    msg1.set("from", "alice");
    msg1.set("text", "Hello from instance1!");
    store1.set("alice_msg", msg1).await?;
    let entry1_id = tx1.commit().await?;
    println!("Alice committed entry: {entry1_id}");

    println!("\n--- Bob writes on instance2 ---");
    let tx2 = db2.new_transaction().await?;
    let store2 = tx2
        .get_store::<eidetica::store::DocStore>("messages")
        .await?;
    let mut msg2 = Doc::new();
    msg2.set("from", "bob");
    msg2.set("text", "Hello from instance2!");
    store2.set("bob_msg", msg2).await?;
    let entry2_id = tx2.commit().await?;
    println!("Bob committed entry: {entry2_id}");

    // Flush both sync queues for bidirectional sync
    sync1.flush().await?;
    sync2.flush().await?;

    println!("\n--- Verifying bidirectional sync ---");

    // Verify Alice's entry synced to instance2
    let alice_entry_on_2 = instance2.backend().get(&entry1_id).await?;
    assert_eq!(
        alice_entry_on_2.id(),
        &entry1_id,
        "Alice's entry should sync to instance2"
    );
    println!("✅ Alice's entry synced: instance1 → instance2");

    // Verify Bob's entry synced to instance1
    let bob_entry_on_1 = instance1.backend().get(&entry2_id).await?;
    assert_eq!(
        bob_entry_on_1.id(),
        &entry2_id,
        "Bob's entry should sync to instance1"
    );
    println!("✅ Bob's entry synced: instance2 → instance1");

    println!("\n✅ SUCCESS! Bidirectional automatic sync working!");

    Ok(())
}

/// Test that automatic sync works when enable_sync is called AFTER user setup
/// This tests the initialize_user_settings() path
#[tokio::test]
async fn test_enable_sync_after_user_setup() -> eidetica::Result<()> {
    println!("\n=== Testing Enable Sync After User Setup ===\n");

    // Phase 1: Create instance WITHOUT enabling sync first
    let instance1 = test_instance().await;
    let instance2 = test_instance().await;

    // Create user and add database preferences BEFORE enabling sync
    instance1.create_user("alice", None).await?;
    let mut user1 = instance1.login_user("alice", None).await?;

    let mut db_settings = Doc::new();
    db_settings.set("name", "notes");
    let key_id = user1.get_default_key()?;
    let db1 = user1.create_database(db_settings, &key_id).await?;
    let db_id = db1.root_id().clone();

    // Add database preferences - but sync isn't enabled yet!
    user1
        .track_database(TrackedDatabase {
            database_id: db_id.clone(),
            key_id: key_id.clone(),
            sync_settings: SyncSettings {
                sync_enabled: true,
                sync_on_commit: true,
                interval_seconds: None,
                properties: Default::default(),
            },
        })
        .await?;

    println!("✅ User and preferences created (sync not enabled yet)");

    // Phase 2: NOW enable sync - this should call initialize_user_settings()
    // and pick up the existing user preferences
    instance1.enable_sync().await?;
    instance2.enable_sync().await?;

    let sync1 = instance1.sync().unwrap();
    let sync2 = instance2.sync().unwrap();

    // Set up HTTP transport
    sync1.enable_http_transport().await?;
    sync2.enable_http_transport().await?;

    sync2.start_server("127.0.0.1:0").await?;
    let server_addr = sync2.get_server_address().await?;
    sleep(Duration::from_millis(100)).await;

    let peer1_pubkey = sync1.get_device_public_key().await?;
    let peer2_pubkey = sync2.get_device_public_key().await?;

    sync1
        .register_peer(&peer2_pubkey, Some("instance2"))
        .await?;
    sync1
        .add_peer_address(&peer2_pubkey, Address::http(server_addr))
        .await?;

    sync2
        .register_peer(&peer1_pubkey, Some("instance1"))
        .await?;

    // Add peer mapping
    sync1.add_tree_sync(&peer2_pubkey, &db_id).await?;

    println!("✅ Sync enabled and configured");

    // Phase 3: Write to database - should sync even though we enabled sync AFTER user setup
    let tx = db1.new_transaction().await?;
    let store = tx.get_store::<eidetica::store::DocStore>("notes").await?;
    let mut note = Doc::new();
    note.set("content", "Test note");
    store.set("note1", note).await?;
    let entry_id = tx.commit().await?;

    println!("Committed entry {entry_id} after enabling sync");

    // Flush sync queue
    sync1.flush().await?;

    // Verify sync worked
    let synced_entry = instance2.backend().get(&entry_id).await;
    assert!(
        synced_entry.is_ok(),
        "Sync should work when enable_sync() is called after user setup (tests initialize_user_settings)"
    );

    println!("✅ SUCCESS! Sync works when enabled after user setup");
    println!("   - This validates initialize_user_settings() is working");

    Ok(())
}

/// Test that automatic sync works after instance restart (login without add_database)
#[tokio::test]
async fn test_auto_sync_after_restart() -> eidetica::Result<()> {
    println!("\n=== Testing Auto-Sync After Instance Restart ===\n");

    // Phase 1: Initial setup - create user and add database preferences
    let instance1 = test_instance().await;
    let instance2 = test_instance().await;

    instance1.enable_sync().await?;
    instance2.enable_sync().await?;

    let sync1 = instance1.sync().unwrap();
    let sync2 = instance2.sync().unwrap();

    // Set up HTTP transport
    sync1.enable_http_transport().await?;
    sync2.enable_http_transport().await?;

    sync2.start_server("127.0.0.1:0").await?;
    let server_addr = sync2.get_server_address().await?;
    sleep(Duration::from_millis(100)).await;

    let peer1_pubkey = sync1.get_device_public_key().await?;
    let peer2_pubkey = sync2.get_device_public_key().await?;

    sync1
        .register_peer(&peer2_pubkey, Some("instance2"))
        .await?;
    sync1
        .add_peer_address(&peer2_pubkey, Address::http(server_addr))
        .await?;

    sync2
        .register_peer(&peer1_pubkey, Some("instance1"))
        .await?;

    // Create user and configure database preferences
    instance1.create_user("alice", None).await?;
    let mut user1 = instance1.login_user("alice", None).await?;

    let mut db_settings = Doc::new();
    db_settings.set("name", "persistent_notes");
    let key_id = user1.get_default_key()?;
    let db1 = user1.create_database(db_settings, &key_id).await?;
    let db_id = db1.root_id().clone();

    // Add database preferences (this registers user and updates settings)
    user1
        .track_database(TrackedDatabase {
            database_id: db_id.clone(),
            key_id: key_id.clone(),
            sync_settings: SyncSettings {
                sync_enabled: true,
                sync_on_commit: true,
                interval_seconds: None,
                properties: Default::default(),
            },
        })
        .await?;

    sync1.add_tree_sync(&peer2_pubkey, &db_id).await?;

    // Write an entry to verify initial sync works
    let tx = db1.new_transaction().await?;
    let store = tx.get_store::<eidetica::store::DocStore>("notes").await?;
    let mut note = Doc::new();
    note.set("content", "Initial write");
    store.set("note1", note).await?;
    let entry1_id = tx.commit().await?;

    // Flush sync queue
    sync1.flush().await?;

    // Verify initial sync worked
    assert!(
        instance2.backend().get(&entry1_id).await.is_ok(),
        "Initial sync should work"
    );

    println!("✅ Initial sync working");

    // Drop user session to simulate logout
    drop(user1);

    println!("\n--- Simulating Instance Restart ---");

    // Phase 2: Simulate restart - login again WITHOUT calling add_database
    // This tests that login-time registration and settings update work
    let user1_relogin = instance1.login_user("alice", None).await?;

    println!("✅ User logged in after 'restart'");

    // Re-open the database (preferences still exist from before)
    let db1_after_restart = user1_relogin.open_database(&db_id).await?;

    // Write another entry - sync should still work even though we didn't call add_database
    let tx = db1_after_restart.new_transaction().await?;
    let store = tx.get_store::<eidetica::store::DocStore>("notes").await?;
    let mut note = Doc::new();
    note.set("content", "After restart write");
    store.set("note2", note).await?;
    let entry2_id = tx.commit().await?;

    println!("Committed entry {entry2_id} after restart");

    // Flush sync queue
    sync1.flush().await?;

    // Verify sync still works after restart
    let synced_entry = instance2.backend().get(&entry2_id).await;
    assert!(
        synced_entry.is_ok(),
        "Sync should work after restart without calling add_database again"
    );

    println!("✅ SUCCESS! Auto-sync works after restart (login-time registration)");

    Ok(())
}
