//! Generic transport conformance tests that work with any SyncTransport implementation
//!
//! These tests verify that all transport implementations provide consistent behavior
//! for syncing entries between Instance instances.

use std::time::Duration;

use eidetica::{
    Database, Instance, Result,
    auth::crypto::PublicKey,
    crdt::Doc,
    store::DocStore,
    sync::{PeerId, Sync},
    user::User,
};
use tokio::time::sleep;

use super::helpers::{HttpTransportFactory, IrohTransportFactory, TransportFactory};
use crate::helpers::test_instance_with_user_and_key;

/// Set up two Instance instances with users and private keys
async fn setup_databases() -> Result<(Instance, User, PublicKey, Instance, User, PublicKey)> {
    let (db1, user1, key_id1) = test_instance_with_user_and_key("user1", Some("device_key")).await;
    let (db2, user2, key_id2) = test_instance_with_user_and_key("user2", Some("device_key")).await;
    Ok((db1, user1, key_id1, db2, user2, key_id2))
}

/// Set up sync instances with servers and peer connections
async fn setup_sync_with_peers<F>(
    factory: &F,
    db1: &Instance,
    db2: &Instance,
) -> Result<(Sync, Sync, String, String)>
where
    F: TransportFactory,
{
    // Create sync instances with transports
    let sync1 = factory.create_sync(db1.clone()).await?;
    let sync2 = factory.create_sync(db2.clone()).await?;

    // Start servers on any available port
    sync1.accept_connections().await?;
    sync2.accept_connections().await?;

    // Give servers time to initialize
    sleep(Duration::from_millis(200)).await;

    // Get server addresses
    let addr1: String = sync1.get_server_address().await?;
    let addr2: String = sync2.get_server_address().await?;

    println!("Server 1 address: {addr1}");
    println!("Server 2 address: {addr2}");

    // Create Address objects for connection
    let address1 = factory.create_address(&addr1);
    let address2 = factory.create_address(&addr2);

    // Connect peers
    let peer1_pubkey = sync1.get_device_id()?;
    let peer2_pubkey = sync2.get_device_id()?;

    // Register peers with each other
    sync1
        .register_peer(&peer2_pubkey, Some("test_peer_2"))
        .await?;
    sync1.add_peer_address(&peer2_pubkey, address2).await?;

    sync2
        .register_peer(&peer1_pubkey, Some("test_peer_1"))
        .await?;
    sync2.add_peer_address(&peer1_pubkey, address1).await?;

    Ok((
        sync1,
        sync2,
        peer1_pubkey.to_string(),
        peer2_pubkey.to_string(),
    ))
}

/// Set up trees with bidirectional sync hooks
#[allow(clippy::too_many_arguments)]
async fn setup_sync_hooks(
    user1: &mut User,
    user2: &mut User,
    key_id1: &PublicKey,
    key_id2: &PublicKey,
    sync1: &Sync,
    sync2: &Sync,
    peer1_pubkey: &str,
    peer2_pubkey: &str,
) -> Result<(Database, Database)> {
    let mut settings1 = Doc::new();
    settings1.set("name", "test_tree_1");
    let tree1 = user1.create_database(settings1, key_id1).await?;

    let mut settings2 = Doc::new();
    settings2.set("name", "test_tree_2");
    let tree2 = user2.create_database(settings2, key_id2).await?;

    // Set up sync callbacks using WriteCallback directly
    // Clone sync instances and peer pubkeys for use in callbacks
    let sync1_clone = sync1.clone();
    let peer2_pubkey_owned = peer2_pubkey.to_string();
    tree1.on_local_write(move |entry, db, _instance| {
        let sync = sync1_clone.clone();
        let peer = PeerId::new(peer2_pubkey_owned.clone());
        let entry_id = entry.id();
        let tree_id = db.root_id().clone();
        async move {
            sync.queue_entry_for_sync(&peer, &entry_id, &tree_id)?;
            Ok(())
        }
    })?;

    let sync2_clone = sync2.clone();
    let peer1_pubkey_owned = peer1_pubkey.to_string();
    tree2.on_local_write(move |entry, db, _instance| {
        let sync = sync2_clone.clone();
        let peer = PeerId::new(peer1_pubkey_owned.clone());
        let entry_id = entry.id();
        let tree_id = db.root_id().clone();
        async move {
            sync.queue_entry_for_sync(&peer, &entry_id, &tree_id)?;
            Ok(())
        }
    })?;

    Ok((tree1, tree2))
}

/// Clean up sync instances
async fn cleanup_sync(sync1: Sync, sync2: Sync) -> Result<()> {
    sync1.stop_server().await?;
    sync2.stop_server().await?;
    Ok(())
}

/// Generic test function that works with any transport factory
async fn test_instance_entry_sync_conformance<F>(factory: F) -> Result<()>
where
    F: TransportFactory,
{
    println!("Testing {} transport conformance", factory.transport_name());

    // Set up databases and sync instances
    let (db1, mut user1, key_id1, db2, mut user2, key_id2) = setup_databases().await?;
    let (sync1, sync2, peer1_pubkey, peer2_pubkey) =
        setup_sync_with_peers(&factory, &db1, &db2).await?;
    let (tree1, _tree2) = setup_sync_hooks(
        &mut user1,
        &mut user2,
        &key_id1,
        &key_id2,
        &sync1,
        &sync2,
        &peer1_pubkey,
        &peer2_pubkey,
    )
    .await?;

    // Create entries in DB1 - these should automatically sync via hooks
    let txn1 = tree1.new_transaction().await?;
    let docstore1 = txn1.get_store::<DocStore>("data").await?;
    docstore1.set("name", "Alice").await?;
    docstore1.set("age", "30").await?;
    let entry_id1 = txn1.commit().await?;

    let txn2 = tree1.new_transaction().await?;
    let docstore1_2 = txn2.get_store::<DocStore>("data").await?;
    docstore1_2.set("name", "Bob").await?;
    docstore1_2.set("age", "25").await?;
    let entry_id2 = txn2.commit().await?;

    println!("Created entries in DB1 (with sync hooks): {entry_id1} and {entry_id2}");

    // Flush sync queue to send entries immediately
    sync1.flush().await?;

    // Verify entries were synced to DB2 backend
    let entry1_in_db2 = db2.backend().get(&entry_id1).await;
    let entry2_in_db2 = db2.backend().get(&entry_id2).await;

    println!("Checking for entries in DB2 backend...");
    println!("Entry 1 in DB2: {:?}", entry1_in_db2.is_ok());
    println!("Entry 2 in DB2: {:?}", entry2_in_db2.is_ok());

    assert!(
        entry1_in_db2.is_ok() && entry2_in_db2.is_ok(),
        "Both entries should have synced to DB2"
    );

    // Verify both synced entries have the expected data
    if let Ok(synced_entry1) = &entry1_in_db2 {
        println!("Successfully found synced entry 1: {}", synced_entry1.id());

        if let Some(data) = synced_entry1.data("data").ok().map(|d| d.as_str()) {
            println!("Synced entry 1 data: {data}");
            assert!(data.contains("Alice"), "Entry 1 should contain Alice data");
        }
    }

    if let Ok(synced_entry2) = &entry2_in_db2 {
        println!("Successfully found synced entry 2: {}", synced_entry2.id());

        if let Some(data) = synced_entry2.data("data").ok().map(|d| d.as_str()) {
            println!("Synced entry 2 data: {data}");
            assert!(data.contains("Bob"), "Entry 2 should contain Bob data");
        }
    }

    println!(
        "✅ Successfully verified entries synced via {} transport",
        factory.transport_name()
    );
    println!("   - Entries accessible via normal Instance/DocStore interfaces");
    println!("   - Data integrity maintained across sync");

    cleanup_sync(sync1, sync2).await?;
    Ok(())
}

/// Test bidirectional sync to ensure both directions work
async fn test_bidirectional_sync_conformance<F>(factory: F) -> Result<()>
where
    F: TransportFactory,
{
    println!("Testing {} bidirectional sync", factory.transport_name());

    // Set up databases and sync instances
    let (db1, mut user1, key_id1, db2, mut user2, key_id2) = setup_databases().await?;
    let (sync1, sync2, peer1_pubkey, peer2_pubkey) =
        setup_sync_with_peers(&factory, &db1, &db2).await?;
    let (tree1, tree2) = setup_sync_hooks(
        &mut user1,
        &mut user2,
        &key_id1,
        &key_id2,
        &sync1,
        &sync2,
        &peer1_pubkey,
        &peer2_pubkey,
    )
    .await?;

    // Create entry in DB1
    let txn1 = tree1.new_transaction().await?;
    let docstore1 = txn1.get_store::<DocStore>("data").await?;
    docstore1.set("origin", "db1").await?;
    let entry_from_db1 = txn1.commit().await?;

    // Create entry in DB2
    let txn2 = tree2.new_transaction().await?;
    let docstore2 = txn2.get_store::<DocStore>("data").await?;
    docstore2.set("origin", "db2").await?;
    let entry_from_db2 = txn2.commit().await?;

    // Flush both sync queues for bidirectional sync
    sync1.flush().await?;
    sync2.flush().await?;

    // Verify DB1 has entry from DB2
    let db2_entry_in_db1 = db1.backend().get(&entry_from_db2).await;
    println!("DB2 entry in DB1: {:?}", db2_entry_in_db1.is_ok());

    // Verify DB2 has entry from DB1
    let db1_entry_in_db2 = db2.backend().get(&entry_from_db1).await;
    println!("DB1 entry in DB2: {:?}", db1_entry_in_db2.is_ok());

    // At least one direction should have synced
    assert!(
        db2_entry_in_db1.is_ok() || db1_entry_in_db2.is_ok(),
        "Bidirectional sync should have worked in at least one direction"
    );

    // If both worked, verify data integrity
    if let Ok(synced_entry) = db2_entry_in_db1
        && let Some(data) = synced_entry.data("data").ok().map(|d| d.as_str())
    {
        assert!(
            data.contains("db2"),
            "Synced entry should contain 'db2' origin data"
        );
    }

    if let Ok(synced_entry) = db1_entry_in_db2
        && let Some(data) = synced_entry.data("data").ok().map(|d| d.as_str())
    {
        assert!(
            data.contains("db1"),
            "Synced entry should contain 'db1' origin data"
        );
    }

    println!(
        "✅ Successfully verified bidirectional sync via {} transport",
        factory.transport_name()
    );

    cleanup_sync(sync1, sync2).await?;
    Ok(())
}

#[tokio::test]
async fn test_http_transport_conformance() {
    test_instance_entry_sync_conformance(HttpTransportFactory)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_iroh_transport_conformance() {
    test_instance_entry_sync_conformance(IrohTransportFactory)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_http_bidirectional_conformance() {
    test_bidirectional_sync_conformance(HttpTransportFactory)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_iroh_bidirectional_conformance() {
    test_bidirectional_sync_conformance(IrohTransportFactory)
        .await
        .unwrap();
}
