//! Tests for multi-transport support in the sync system.
//!
//! These tests verify that multiple transports can be enabled simultaneously
//! and that the system routes requests appropriately.

use std::time::Duration;

use eidetica::{
    Database, Result,
    store::DocStore,
    sync::{
        peer_types::Address,
        transports::{http::HttpTransport, iroh::IrohTransport},
    },
};
use iroh::RelayMode;

use super::helpers::setup;

/// Test that multiple HTTP transports can be enabled (though unusual, validates the mechanism)
#[tokio::test]
async fn test_enable_http_transport_twice_adds_transport() -> Result<()> {
    let (_instance, sync) = setup().await;

    // Enable first HTTP transport
    sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;

    // Enable second HTTP transport - should still succeed (adds to existing)
    sync.register_transport("http2", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;

    // Start server on all transports
    sync.accept_connections().await?;

    // Get all server addresses - should have entries
    let addresses = sync.get_all_server_addresses().await?;
    assert!(
        !addresses.is_empty(),
        "Should have at least one server address"
    );

    // Each HTTP transport will bind to its own port
    // (In practice you'd use different transport types like HTTP + Iroh)

    sync.stop_server().await?;

    Ok(())
}

/// Test that transport routing works correctly
#[tokio::test]
async fn test_transport_routing_by_address_type() -> Result<()> {
    let (_instance, sync) = setup().await;

    // Enable HTTP transport
    sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;

    // Start server
    sync.accept_connections().await?;

    // Get the server address
    let addr = sync.get_server_address().await?;
    assert!(!addr.is_empty(), "Should have a server address");

    // Get all addresses
    let all_addresses = sync.get_all_server_addresses().await?;
    assert_eq!(all_addresses.len(), 1, "Should have exactly one transport");
    assert_eq!(all_addresses[0].0, "http", "Should be HTTP transport type");

    sync.stop_server().await?;

    Ok(())
}

/// Test server start/stop with multiple transports
#[tokio::test]
async fn test_server_lifecycle_with_multiple_transports() -> Result<()> {
    let (_instance, sync) = setup().await;

    // Enable two transports
    sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;
    sync.register_transport("http2", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;

    // Start servers on all transports
    sync.accept_connections().await?;

    // Verify servers are running
    let addresses = sync.get_all_server_addresses().await?;
    assert!(!addresses.is_empty(), "Should have running servers");

    // Stop all servers
    sync.stop_server().await?;

    // Verify no more addresses available
    let result = sync.get_server_address().await;
    assert!(
        result.is_err(),
        "Should have no server addresses after stop"
    );

    Ok(())
}

/// Test that data synced via one transport is available via another.
///
/// This test sets up a server with both HTTP and Iroh transports, then:
/// 1. HTTP client bootstraps the database from the server via HTTP
/// 2. HTTP client adds data and syncs it back to server via HTTP
/// 3. Iroh client retrieves the data from the server via Iroh
/// 4. Verifies the data flowed through correctly (HTTP → Server → Iroh)
///
/// This follows the pattern from `test_collaborative_database_with_sync_and_global_permissions`
/// in `user/integration_tests.rs` which demonstrates correct sync_with_peer usage.
#[tokio::test]
async fn test_http_and_iroh_sync_interoperability() -> Result<()> {
    // Server with a sync-enabled database (wildcard permissions for testing)
    let (server_instance, _user, _key_id, _server_database, server_sync, tree_id) =
        super::helpers::setup_global_wildcard_server().await;

    // Enable both HTTP and Iroh transports on server
    server_sync
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;
    server_sync
        .register_transport(
            "iroh",
            IrohTransport::builder().relay_mode(RelayMode::Disabled),
        )
        .await?;

    // Start server
    server_sync.accept_connections().await?;

    // Allow endpoints to initialize
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Get server addresses for both transports
    let server_addresses = server_sync.get_all_server_addresses().await?;
    assert_eq!(
        server_addresses.len(),
        2,
        "Server should have both HTTP and Iroh addresses"
    );

    let http_addr = server_addresses
        .iter()
        .find(|(t, _)| t == "http")
        .map(|(_, a)| a.clone())
        .expect("Should have HTTP address");

    let iroh_addr = server_addresses
        .iter()
        .find(|(t, _)| t == "iroh")
        .map(|(_, a)| a.clone())
        .expect("Should have Iroh address");

    println!("Server HTTP address: {http_addr}");
    println!("Server Iroh address: {iroh_addr}");
    println!("Server tree_id: {tree_id}");

    let server_pubkey = server_sync.get_device_id()?;

    // === LEG 1: HTTP client bootstraps and adds data via HTTP ===
    println!("\n--- LEG 1: HTTP client syncs data TO server via HTTP ---");

    let (http_client_instance, http_client_sync) = setup().await;
    http_client_sync
        .register_transport("http", HttpTransport::builder())
        .await?;

    // Register server as peer
    http_client_sync
        .register_peer(&server_pubkey, Some("server"))
        .await?;
    http_client_sync
        .add_peer_address(&server_pubkey, Address::http(&http_addr))
        .await?;

    // HTTP client bootstraps the database from server
    println!("HTTP client bootstrapping database from server...");
    http_client_sync
        .sync_with_peer(&http_addr, Some(&tree_id))
        .await
        .expect("HTTP client should bootstrap from server");

    // Verify HTTP client has the tree
    assert!(
        http_client_instance.has_database(&tree_id).await,
        "HTTP client should have the tree after bootstrap"
    );
    println!("✅ HTTP client bootstrapped database from server");

    // HTTP client opens the database using the wildcard permission ("*")
    // The wildcard allows any key to write, so we use the client's device key
    // but sign as "*" to use the global permission
    let http_client_db = Database::open(
        http_client_instance.clone(),
        &tree_id,
        http_client_instance.device_key().clone(),
        "*".to_string(), // Use wildcard permission
    )
    .await?;

    let entry_id = {
        let tx = http_client_db.new_transaction().await?;
        let store = tx.get_store::<DocStore>("multi_transport_data").await?;
        store.set("test_key", "http_to_iroh_test_value").await?;
        store.set("source", "http_client").await?;
        tx.commit().await?
    };
    println!("✅ HTTP client created entry: {entry_id}");

    // HTTP client syncs changes back to server
    println!("HTTP client syncing changes to server...");
    http_client_sync
        .sync_with_peer(&http_addr, Some(&tree_id))
        .await
        .expect("HTTP client should sync changes to server");

    // Flush any pending sync work
    http_client_sync.flush().await.ok();

    // Verify server received the entry
    assert!(
        server_instance.has_entry(&entry_id).await,
        "Server should have received the entry via HTTP"
    );
    println!("✅ Server received entry via HTTP");

    // === LEG 2: Iroh client retrieves data via Iroh ===
    println!("\n--- LEG 2: Iroh client syncs data FROM server via Iroh ---");

    let (iroh_client_instance, iroh_client_sync) = setup().await;
    iroh_client_sync
        .register_transport(
            "iroh",
            IrohTransport::builder().relay_mode(RelayMode::Disabled),
        )
        .await?;

    // Register server as peer with Iroh address
    iroh_client_sync
        .register_peer(&server_pubkey, Some("server"))
        .await?;
    iroh_client_sync
        .add_peer_address(&server_pubkey, Address::iroh(&iroh_addr))
        .await?;

    // Verify entry is NOT on Iroh client yet
    assert!(
        !iroh_client_instance.has_entry(&entry_id).await,
        "Entry should NOT be on Iroh client yet"
    );

    // Iroh client syncs from server via Iroh transport
    println!("Iroh client syncing from server via Iroh...");
    iroh_client_sync
        .sync_with_peer(&iroh_addr, Some(&tree_id))
        .await
        .expect("Should be able to sync from server via Iroh");

    // Flush any pending sync work
    iroh_client_sync.flush().await.ok();

    // Verify Iroh client received the entry
    let synced_entry = iroh_client_instance
        .backend()
        .get(&entry_id)
        .await
        .expect("Iroh client should have received the entry via Iroh transport");

    println!(
        "✅ Iroh client received entry {} via Iroh transport",
        synced_entry.id()
    );

    // Verify data integrity by checking the subtree data
    let data = synced_entry.data("multi_transport_data")?.as_str();
    assert!(
        data.contains("http_to_iroh_test_value"),
        "Synced entry should contain expected data, got: {data}"
    );
    assert!(
        data.contains("http_client"),
        "Synced entry should show it came from http_client, got: {data}"
    );
    println!("✅ Data integrity verified!");

    // Cleanup
    server_sync.stop_server().await?;

    println!("\n✅ Multi-transport HTTP→Server→Iroh test passed!");
    Ok(())
}

/// Test that client-only transports (no bind address) succeed without starting a server
#[tokio::test]
async fn test_client_only_transports_succeed_without_server() -> Result<()> {
    let (_instance, sync) = setup().await;

    // Add two HTTP transports WITHOUT bind addresses - these are client-only
    // HttpTransport::new() creates a transport with no pre-configured bind address
    let http1 = HttpTransport::new()?;
    let http2 = HttpTransport::new()?;
    sync.add_transport("http1", Box::new(http1)).await?;
    sync.add_transport("http2", Box::new(http2)).await?;

    // accept_connections() should succeed - client-only transports are valid
    // They just don't start a server (no-op), which is not an error
    let result = sync.accept_connections().await;

    assert!(
        result.is_ok(),
        "Client-only transports should not cause errors: {:?}",
        result.err()
    );

    Ok(())
}

/// Test that start_all_servers succeeds when at least one transport starts successfully
#[tokio::test]
async fn test_start_all_servers_succeeds_when_one_passes() -> Result<()> {
    let (_instance, sync) = setup().await;

    // Add one HTTP transport WITH a bind address (will succeed)
    let http_good = HttpTransport::builder().bind("127.0.0.1:0").build_sync()?;
    sync.add_transport("http_good", Box::new(http_good)).await?;

    // Add one HTTP transport WITHOUT a bind address (will fail)
    let http_bad = HttpTransport::new()?;
    sync.add_transport("http_bad", Box::new(http_bad)).await?;

    // accept_connections() should succeed because at least one transport started
    let result = sync.accept_connections().await;
    assert!(
        result.is_ok(),
        "Should succeed when at least one transport starts, got: {:?}",
        result.err()
    );

    // Verify the successful transport is running by getting its address
    let addresses = sync.get_all_server_addresses().await?;
    assert_eq!(addresses.len(), 1, "Should have exactly one running server");
    assert_eq!(
        addresses[0].0, "http_good",
        "The running server should be http_good"
    );

    // Cleanup
    sync.stop_server().await?;

    Ok(())
}
