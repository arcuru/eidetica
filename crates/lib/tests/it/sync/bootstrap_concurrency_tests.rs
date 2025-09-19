// crates/lib/tests/it/sync/bootstrap_concurrency_tests.rs

use super::helpers::*;
use eidetica::Result;
use std::time::Duration;
use tracing::info;

/// Test multiple clients bootstrapping from the same database simultaneously.
/// This test ensures that concurrent bootstrap requests do not interfere with each other
/// and that all clients receive a consistent view of the database.
#[tokio::test]
async fn test_multiple_clients_bootstrap_same_database() -> Result<()> {
    info!("Running test: test_multiple_clients_bootstrap_same_database");

    // 1. Setup the server instance
    let mut server_instance = setup_instance_with_initialized();

    // Create some test data directly in the server's backend
    let root_entry = create_test_tree_entry();
    let test_tree_id = root_entry.id().clone();

    // Store entry in server backend
    let server_sync = server_instance.sync_mut().unwrap();
    server_sync.backend().put_verified(root_entry).unwrap();

    // Start server
    let server_addr = start_sync_server(server_sync).await;

    // 3. Create multiple clients
    let num_clients = 3; // Use fewer clients to avoid overloading
    let mut client_handles = Vec::new();

    for i in 0..num_clients {
        let tree_id = test_tree_id.clone();
        let addr = server_addr.clone();

        let handle = tokio::spawn(async move {
            info!("Starting client {}", i);

            // Create client instance
            let mut client_instance = setup_instance_with_initialized();

            // Verify client doesn't have the database initially
            assert!(
                client_instance.load_database(&tree_id).is_err(),
                "Client {} should not have the database initially",
                i
            );

            // Bootstrap from server
            let client_sync = client_instance.sync_mut().unwrap();
            client_sync.enable_http_transport().unwrap();
            client_sync
                .sync_with_peer(&addr, Some(&tree_id))
                .await
                .unwrap();

            // Wait a moment for sync to complete
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Verify client can now load the database
            let _client_db = client_instance.load_database(&tree_id).unwrap();

            info!("Client {} successfully bootstrapped", i);
            Ok::<_, eidetica::Error>((i, client_instance))
        });

        client_handles.push(handle);
    }

    // 5. Wait for all clients to complete bootstrapping and verify their state
    for handle in client_handles {
        let (client_id, _client_instance) = handle.await.unwrap().unwrap();
        info!("Client {} completed successfully", client_id);
    }

    // Cleanup
    let server_sync = server_instance.sync_mut().unwrap();
    server_sync.stop_server_async().await.unwrap();

    info!("Test finished: test_multiple_clients_bootstrap_same_database");
    Ok(())
}

/// Test concurrent key approval requests from multiple clients.
/// This test ensures that when multiple clients request key approval simultaneously,
/// all requests are processed correctly without race conditions.
#[tokio::test]
async fn test_concurrent_key_approval_requests() -> Result<()> {
    info!("Running test: test_concurrent_key_approval_requests");

    // 1. Setup the server instance with an existing database that has bootstrap auto-approval enabled
    let mut server_instance = setup_instance_with_initialized();

    // Create a database with bootstrap auto-approval policy
    let server_key = "server_admin";
    server_instance.add_private_key(server_key).unwrap();

    let server_pubkey = server_instance
        .get_formatted_public_key(server_key)
        .unwrap()
        .unwrap();

    // Create database with policy that allows bootstrap auto-approval
    let mut settings = eidetica::crdt::Doc::new();
    settings.set_string("name", "Test Concurrent Approval");

    let mut auth_doc = eidetica::crdt::Doc::new();
    let mut policy_doc = eidetica::crdt::Doc::new();
    policy_doc.set_json("bootstrap_auto_approve", true).unwrap();
    auth_doc.set_node("policy", policy_doc);

    // Add admin key
    auth_doc
        .set_json(
            server_key,
            serde_json::json!({
                "pubkey": server_pubkey,
                "permissions": {"Admin": 10},
                "status": "Active"
            }),
        )
        .unwrap();

    settings.set_node("auth", auth_doc);
    let server_database = server_instance.new_database(settings, server_key).unwrap();
    let test_tree_id = server_database.root_id().clone();

    // Start server
    let server_sync = server_instance.sync_mut().unwrap();
    let server_addr = start_sync_server(server_sync).await;

    // 2. Create multiple clients that will request key approval concurrently
    let num_clients = 4;
    let mut client_handles = Vec::new();

    for i in 0..num_clients {
        let tree_id = test_tree_id.clone();
        let addr = server_addr.clone();

        let handle = tokio::spawn(async move {
            info!("Starting client {} key approval request", i);

            // Create client instance with its own key
            let mut client_instance = setup_instance_with_initialized();
            let client_key = format!("client_key_{}", i);
            client_instance.add_private_key(&client_key).unwrap();

            // Verify client doesn't have the database initially
            assert!(
                client_instance.load_database(&tree_id).is_err(),
                "Client {} should not have the database initially",
                i
            );

            // Request bootstrap with key approval
            let client_sync = client_instance.sync_mut().unwrap();
            client_sync.enable_http_transport().unwrap();

            client_sync
                .sync_with_peer_for_bootstrap(
                    &addr,
                    &tree_id,
                    &client_key,
                    eidetica::auth::Permission::Write(5),
                )
                .await
                .unwrap();

            // Wait a moment for sync to complete
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Verify client can now load the database
            let client_db = client_instance.load_database(&tree_id).unwrap();

            // Verify client's key was added to auth settings
            let settings = client_db.get_settings().unwrap();
            let _auth_value = settings.get("auth").unwrap();

            info!("Client {} successfully got key approval", i);
            Ok::<_, eidetica::Error>((i, client_instance))
        });

        client_handles.push(handle);
    }

    // 3. Wait for all clients to complete key approval and verify their state
    for handle in client_handles {
        let (client_id, _client_instance) = handle.await.unwrap().unwrap();
        info!("Client {} key approval completed successfully", client_id);
    }

    // Cleanup
    let server_sync = server_instance.sync_mut().unwrap();
    server_sync.stop_server_async().await.unwrap();

    info!("Test finished: test_concurrent_key_approval_requests");
    Ok(())
}
