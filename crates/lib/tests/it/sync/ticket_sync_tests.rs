//! Integration tests for the DatabaseTicket sync API.
//!
//! Tests `create_ticket`, `sync_with_ticket`, `bootstrap_with_ticket`,
//! `try_addresses_concurrently`, and `request_database_access`.

use eidetica::{
    auth::Permission,
    entry::ID,
    store::DocStore,
    sync::{
        Address, DatabaseTicket,
        transports::{http::HttpTransport, iroh::IrohTransport},
    },
};
use iroh::RelayMode;

use super::helpers::*;

/// Start server -> `create_ticket` -> verify database ID and HTTP address present ->
/// serialize to URL -> parse back -> verify fields match.
#[tokio::test]
async fn test_create_ticket_round_trip() {
    let (_server_instance, _server_user, _server_key_id, server_database, tree_id, server_sync) =
        setup_public_sync_enabled_server("server_user", "server_key", "test_database").await;

    // Start the server so `create_ticket` has addresses to report
    let _server_addr = start_sync_server(&server_sync).await;

    // Create a ticket for the database
    let ticket = server_sync
        .create_ticket(server_database.root_id())
        .await
        .expect("create_ticket should succeed");

    // Verify database ID
    assert_eq!(
        ticket.database_id(),
        &tree_id,
        "Ticket should contain the correct database ID"
    );

    // Verify at least one HTTP address is present
    assert!(
        !ticket.addresses().is_empty(),
        "Ticket should have at least one address hint"
    );
    assert!(
        ticket
            .addresses()
            .iter()
            .any(|a| a.transport_type == "http"),
        "Ticket should contain an HTTP address hint"
    );

    // Serialize to URL and parse back
    let url = ticket.to_string();
    assert!(
        url.starts_with("eidetica:?db="),
        "URL should have eidetica magnet-style format"
    );

    let parsed: DatabaseTicket = url
        .parse()
        .expect("Ticket URL should round-trip successfully");

    assert_eq!(
        parsed.database_id(),
        ticket.database_id(),
        "Parsed ticket database ID should match original"
    );
    assert_eq!(
        parsed.addresses().len(),
        ticket.addresses().len(),
        "Parsed ticket should have same number of addresses"
    );
    for (original, parsed_addr) in ticket.addresses().iter().zip(parsed.addresses().iter()) {
        assert_eq!(
            original.transport_type, parsed_addr.transport_type,
            "Transport type should match"
        );
        assert_eq!(
            original.address, parsed_addr.address,
            "Address should match"
        );
    }

    // Cleanup
    server_sync.stop_server().await.unwrap();
}

/// Server with data -> `create_ticket` -> client `sync_with_ticket` -> verify data
/// arrived (tips non-empty, root entry exists).
#[tokio::test]
async fn test_sync_with_ticket_happy_path() {
    // Setup server with data
    let (_server_instance, _server_user, _server_key_id, server_database, tree_id, server_sync) =
        setup_public_sync_enabled_server("server_user", "server_key", "test_database").await;

    // Add test data
    {
        let tx = server_database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("messages").await.unwrap();
        store.set("msg1", "Hello via ticket!").await.unwrap();
        tx.commit().await.unwrap();
    }

    // Start server and create ticket
    let _server_addr = start_sync_server(&server_sync).await;
    let ticket = server_sync
        .create_ticket(&tree_id)
        .await
        .expect("create_ticket should succeed");

    // Setup client
    let (client_instance, _client_user, _client_key_id, client_sync) =
        setup_sync_enabled_client("client_user", "client_key").await;

    // Verify client doesn't have the database initially
    assert!(
        !client_instance.has_database(&tree_id).await,
        "Client should not have the database initially"
    );

    // Register HTTP transport on client and sync via ticket
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    client_sync
        .sync_with_ticket(&ticket)
        .await
        .expect("sync_with_ticket should succeed");

    client_sync.flush().await.ok();

    // Verify root entry exists on client
    let root = client_instance
        .backend()
        .get(&tree_id)
        .await
        .expect("Client should have the root entry after sync");
    assert_eq!(root.id(), tree_id);

    // Verify tips are non-empty
    let tips = client_instance
        .backend()
        .get_tips(&tree_id)
        .await
        .expect("Client should have tips");
    assert!(
        !tips.is_empty(),
        "Client should have non-empty tips after sync"
    );

    // Cleanup
    server_sync.stop_server().await.unwrap();
}

/// Manually build ticket with one bad address (`127.0.0.1:1`) and one good (real
/// server). `sync_with_ticket` succeeds via the good address despite the bad one.
#[tokio::test]
async fn test_sync_with_ticket_mixed_addresses() {
    // Setup server with data
    let (_server_instance, _server_user, _server_key_id, server_database, tree_id, server_sync) =
        setup_public_sync_enabled_server("server_user", "server_key", "test_database").await;

    // Add test data
    {
        let tx = server_database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("data").await.unwrap();
        store.set("key", "value").await.unwrap();
        tx.commit().await.unwrap();
    }

    // Start server and get actual address
    let server_addr = start_sync_server(&server_sync).await;

    // Build ticket manually with one bad and one good address
    let ticket = DatabaseTicket::with_addresses(
        tree_id.clone(),
        vec![
            Address::http("127.0.0.1:1"), // Unreachable
            server_addr,                  // Good address
        ],
    );

    // Setup client
    let (client_instance, _client_user, _client_key_id, client_sync) =
        setup_sync_enabled_client("client_user", "client_key").await;

    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Sync should succeed via the good address
    client_sync
        .sync_with_ticket(&ticket)
        .await
        .expect("sync_with_ticket should succeed with at least one good address");

    client_sync.flush().await.ok();

    // Verify data arrived
    let root = client_instance
        .backend()
        .get(&tree_id)
        .await
        .expect("Client should have the root entry");
    assert_eq!(root.id(), tree_id);

    // Cleanup
    server_sync.stop_server().await.unwrap();
}

/// Ticket with only unreachable addresses (`127.0.0.1:1`, `127.0.0.1:2`).
/// `sync_with_ticket` returns error.
#[tokio::test]
async fn test_sync_with_ticket_all_addresses_fail() {
    let (_instance, sync) = setup().await;

    sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    sync.accept_connections().await.unwrap();

    let fake_tree_id =
        ID::new("sha256:0000000000000000000000000000000000000000000000000000000000000000");

    let ticket = DatabaseTicket::with_addresses(
        fake_tree_id,
        vec![Address::http("127.0.0.1:1"), Address::http("127.0.0.1:2")],
    );

    let (_client_instance, client_sync) = setup().await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    let result = client_sync.sync_with_ticket(&ticket).await;

    assert!(
        result.is_err(),
        "sync_with_ticket should fail when all addresses are unreachable"
    );

    // Cleanup
    sync.stop_server().await.unwrap();
}

/// Ticket with no address hints. `sync_with_ticket` returns `InvalidAddress` error
/// containing "no address hints".
#[tokio::test]
async fn test_sync_with_ticket_empty_addresses() {
    let (_instance, sync) = setup().await;

    sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    sync.accept_connections().await.unwrap();

    let fake_tree_id =
        ID::new("sha256:0000000000000000000000000000000000000000000000000000000000000000");

    // Ticket with no addresses
    let ticket = DatabaseTicket::new(fake_tree_id);

    let result = sync.sync_with_ticket(&ticket).await;

    assert!(
        result.is_err(),
        "sync_with_ticket should fail with no addresses"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.to_lowercase().contains("no address hints"),
        "Error should mention 'no address hints', got: {err_msg}"
    );

    // Cleanup
    sync.stop_server().await.unwrap();
}

/// Auto-approve server -> `create_ticket` -> client
/// `request_database_access` with ticket -> verify database bootstrapped with auth.
#[tokio::test]
async fn test_bootstrap_with_ticket_authenticated() {
    // Setup server with auto-approve
    let (_server_instance, _server_user, _server_key_id, server_database, tree_id, server_sync) =
        setup_sync_enabled_server_with_auto_approve("server_user", "server_key", "test_database")
            .await;

    // Add test data
    {
        let tx = server_database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("messages").await.unwrap();
        store.set("msg1", "Authenticated content").await.unwrap();
        tx.commit().await.unwrap();
    }

    // Start server and create ticket
    let _server_addr = start_sync_server(&server_sync).await;
    let ticket = server_sync
        .create_ticket(&tree_id)
        .await
        .expect("create_ticket should succeed");

    // Setup client
    let (_client_instance, client_user, client_key_id, client_sync) =
        setup_sync_enabled_client("client_user", "client_key").await;

    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Use User API to request access via ticket
    client_user
        .request_database_access(&client_sync, &ticket, &client_key_id, Permission::Write(5))
        .await
        .expect("request_database_access should succeed");

    client_sync.flush().await.ok();

    // Verify the client has the database root
    let client_instance = client_sync.instance().expect("Should have instance");
    let root = client_instance
        .backend()
        .get(&tree_id)
        .await
        .expect("Client should have the root entry after bootstrap");
    assert_eq!(root.id(), tree_id);

    // Verify tips exist
    let tips = client_instance
        .backend()
        .get_tips(&tree_id)
        .await
        .expect("Client should have tips");
    assert!(
        !tips.is_empty(),
        "Client should have non-empty tips after authenticated bootstrap"
    );

    // Cleanup
    server_sync.stop_server().await.unwrap();
}

/// Register transport but don't start server -> `create_ticket` -> ticket has correct
/// database ID but empty addresses (since `get_all_server_addresses` filters on
/// `is_server_running()`).
#[tokio::test]
async fn test_create_ticket_no_servers_running() {
    let (_server_instance, _server_user, _server_key_id, server_database, tree_id, server_sync) =
        setup_public_sync_enabled_server("server_user", "server_key", "test_database").await;

    // Register transport but do NOT start the server
    server_sync
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();

    // Create ticket — should have the correct database ID but no addresses
    let ticket = server_sync
        .create_ticket(server_database.root_id())
        .await
        .expect("create_ticket should succeed even without running servers");

    assert_eq!(
        ticket.database_id(),
        &tree_id,
        "Ticket should contain the correct database ID"
    );
    assert!(
        ticket.addresses().is_empty(),
        "Ticket should have no addresses when no server is running, but got: {:?}",
        ticket.addresses()
    );
}

/// Register both HTTP and Iroh transports, start servers, then `create_ticket` ->
/// ticket contains one HTTP and one Iroh address hint, and the URL round-trips.
#[tokio::test]
async fn test_create_ticket_multiple_transports() {
    let (_server_instance, _server_user, _server_key_id, server_database, tree_id, server_sync) =
        setup_public_sync_enabled_server("server_user", "server_key", "test_database").await;

    // Register both HTTP and Iroh (relay-disabled for fast local test)
    server_sync
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    server_sync
        .register_transport(
            "iroh",
            IrohTransport::builder().relay_mode(RelayMode::Disabled),
        )
        .await
        .unwrap();
    server_sync.accept_connections().await.unwrap();

    // Create ticket — should contain addresses from both transports
    let ticket = server_sync
        .create_ticket(server_database.root_id())
        .await
        .expect("create_ticket should succeed");

    assert_eq!(ticket.database_id(), &tree_id);
    assert!(
        ticket.addresses().len() >= 2,
        "Ticket should have at least 2 addresses (HTTP + Iroh), got: {:?}",
        ticket.addresses()
    );

    let has_http = ticket
        .addresses()
        .iter()
        .any(|a| a.transport_type == "http");
    let has_iroh = ticket
        .addresses()
        .iter()
        .any(|a| a.transport_type == "iroh");
    assert!(has_http, "Ticket should contain an HTTP address hint");
    assert!(has_iroh, "Ticket should contain an Iroh address hint");

    // Round-trip the URL
    let url = ticket.to_string();
    assert!(url.contains("pr=http:"));
    assert!(url.contains("pr=iroh:"));

    let parsed: DatabaseTicket = url
        .parse()
        .expect("Ticket URL should round-trip successfully");
    assert_eq!(parsed.database_id(), ticket.database_id());
    assert_eq!(parsed.addresses().len(), ticket.addresses().len());

    for (original, parsed_addr) in ticket.addresses().iter().zip(parsed.addresses().iter()) {
        assert_eq!(original.transport_type, parsed_addr.transport_type);
        assert_eq!(original.address, parsed_addr.address);
    }

    // Cleanup
    server_sync.stop_server().await.unwrap();
}
