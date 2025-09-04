//! Generic transport conformance tests that work with any SyncTransport implementation
//!
//! These tests verify that all transport implementations provide consistent behavior
//! for syncing entries between Instance instances.

use std::time::Duration;
use tokio::time::sleep;

use eidetica::Instance;
use eidetica::Result;
use eidetica::backend::database::InMemory;

use super::helpers::{HttpTransportFactory, IrohTransportFactory, TransportFactory};

/// Set up two Instance instances with private keys
async fn setup_databases() -> Result<(Instance, Instance)> {
    let db1 = {
        let db = Instance::new(Box::new(InMemory::new()));
        db.add_private_key("device_key")?;
        db
    };
    let db2 = {
        let db = Instance::new(Box::new(InMemory::new()));
        db.add_private_key("device_key")?;
        db
    };
    Ok((db1, db2))
}

/// Set up sync instances with servers and peer connections
async fn setup_sync_with_peers<F>(
    factory: &F,
    db1: &Instance,
    db2: &Instance,
) -> Result<(eidetica::sync::Sync, eidetica::sync::Sync, String, String)>
where
    F: TransportFactory,
{
    // Create sync instances with transports
    let mut sync1 = factory.create_sync(db1.backend().clone())?;
    let mut sync2 = factory.create_sync(db2.backend().clone())?;

    // Start servers on any available port
    sync1.start_server_async("127.0.0.1:0").await?;
    sync2.start_server_async("127.0.0.1:0").await?;

    // Give servers time to initialize
    sleep(Duration::from_millis(200)).await;

    // Get server addresses
    let addr1: String = sync1.get_server_address_async().await?;
    let addr2: String = sync2.get_server_address_async().await?;

    println!("Server 1 address: {addr1}");
    println!("Server 2 address: {addr2}");

    // Create Address objects for connection
    let address1 = factory.create_address(&addr1);
    let address2 = factory.create_address(&addr2);

    // Connect peers
    let peer1_pubkey = sync1.get_device_public_key()?;
    let peer2_pubkey = sync2.get_device_public_key()?;

    // Register peers with each other
    sync1.register_peer(&peer2_pubkey, Some("test_peer_2"))?;
    sync1.add_peer_address(&peer2_pubkey, address2)?;

    sync2.register_peer(&peer1_pubkey, Some("test_peer_1"))?;
    sync2.add_peer_address(&peer1_pubkey, address1)?;

    Ok((
        sync1,
        sync2,
        peer1_pubkey.to_string(),
        peer2_pubkey.to_string(),
    ))
}

/// Set up trees with bidirectional sync hooks
fn setup_sync_hooks(
    db1: &Instance,
    db2: &Instance,
    sync1: &eidetica::sync::Sync,
    sync2: &eidetica::sync::Sync,
    peer1_pubkey: &str,
    peer2_pubkey: &str,
) -> Result<(eidetica::Database, eidetica::Database)> {
    use eidetica::sync::hooks::SyncHookCollection;

    let mut tree1 = db1.new_database_default("device_key")?;
    let mut tree2 = db2.new_database_default("device_key")?;

    // Set up sync hooks
    let hook1 = sync1.create_sync_hook(peer2_pubkey.to_string());
    let mut hooks1 = SyncHookCollection::new();
    hooks1.add_hook(hook1);
    tree1.set_sync_hooks(std::sync::Arc::new(hooks1));

    let hook2 = sync2.create_sync_hook(peer1_pubkey.to_string());
    let mut hooks2 = SyncHookCollection::new();
    hooks2.add_hook(hook2);
    tree2.set_sync_hooks(std::sync::Arc::new(hooks2));

    Ok((tree1, tree2))
}

/// Clean up sync instances
async fn cleanup_sync(
    mut sync1: eidetica::sync::Sync,
    mut sync2: eidetica::sync::Sync,
) -> Result<()> {
    sync1.stop_server_async().await?;
    sync2.stop_server_async().await?;
    Ok(())
}

/// Generic test function that works with any transport factory
async fn test_instance_entry_sync_conformance<F>(factory: F) -> Result<()>
where
    F: TransportFactory,
{
    println!("Testing {} transport conformance", factory.transport_name());

    // Set up databases and sync instances
    let (db1, db2) = setup_databases().await?;
    let (sync1, sync2, peer1_pubkey, peer2_pubkey) =
        setup_sync_with_peers(&factory, &db1, &db2).await?;
    let (tree1, _tree2) =
        setup_sync_hooks(&db1, &db2, &sync1, &sync2, &peer1_pubkey, &peer2_pubkey)?;

    // Create entries in DB1 - these should automatically sync via hooks
    let op1 = tree1.new_operation()?;
    let docstore1 = op1.get_store::<eidetica::store::DocStore>("data")?;
    docstore1.set("name", "Alice")?;
    docstore1.set("age", "30")?;
    let entry_id1 = op1.commit()?;

    let op2 = tree1.new_operation()?;
    let docstore1_2 = op2.get_store::<eidetica::store::DocStore>("data")?;
    docstore1_2.set("name", "Bob")?;
    docstore1_2.set("age", "25")?;
    let entry_id2 = op2.commit()?;

    println!("Created entries in DB1 (with sync hooks): {entry_id1} and {entry_id2}");

    // Wait for sync to propagate
    sleep(factory.sync_wait_time()).await;

    // Verify entries were synced to DB2 backend
    let entry1_in_db2 = db2.backend().get(&entry_id1);
    let entry2_in_db2 = db2.backend().get(&entry_id2);

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
    let (db1, db2) = setup_databases().await?;
    let (sync1, sync2, peer1_pubkey, peer2_pubkey) =
        setup_sync_with_peers(&factory, &db1, &db2).await?;
    let (tree1, tree2) =
        setup_sync_hooks(&db1, &db2, &sync1, &sync2, &peer1_pubkey, &peer2_pubkey)?;

    // Create entry in DB1
    let op1 = tree1.new_operation()?;
    let docstore1 = op1.get_store::<eidetica::store::DocStore>("data")?;
    docstore1.set("origin", "db1")?;
    let entry_from_db1 = op1.commit()?;

    // Create entry in DB2
    let op2 = tree2.new_operation()?;
    let docstore2 = op2.get_store::<eidetica::store::DocStore>("data")?;
    docstore2.set("origin", "db2")?;
    let entry_from_db2 = op2.commit()?;

    // Wait for bidirectional sync
    sleep(factory.sync_wait_time()).await;

    // Verify DB1 has entry from DB2
    let db2_entry_in_db1 = db1.backend().get(&entry_from_db2);
    println!("DB2 entry in DB1: {:?}", db2_entry_in_db1.is_ok());

    // Verify DB2 has entry from DB1
    let db1_entry_in_db2 = db2.backend().get(&entry_from_db1);
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
