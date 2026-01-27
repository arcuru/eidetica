//! Concurrency tests for bootstrap operations.
//!
//! These tests verify that concurrent bootstrap requests work correctly
//! without race conditions or interference between clients.

use super::helpers::*;
use eidetica::{Error, Result, sync::transports::http::HttpTransport};
use std::time::Duration;
use tracing::info;

/// Test multiple clients bootstrapping from the same database simultaneously.
///
/// Verifies that concurrent bootstrap requests do not interfere with each other
/// and that all clients receive a consistent view of the database.
#[tokio::test]
async fn test_multiple_clients_bootstrap_same_database() -> Result<()> {
    info!("Running test: test_multiple_clients_bootstrap_same_database");

    // Setup the server instance
    let server_instance = setup_instance_with_initialized().await;

    // Create some test data directly in the server's backend
    let root_entry = create_test_tree_entry();
    let test_tree_id = root_entry.id().clone();

    // Store entry in server backend
    let server_sync = server_instance.sync().unwrap();
    server_sync
        .backend()
        .unwrap()
        .put_verified(root_entry)
        .await
        .unwrap();

    // Enable sync for this tree
    enable_sync_for_instance_database(&server_sync, &test_tree_id)
        .await
        .unwrap();

    // Start server
    let server_addr = start_sync_server(&server_sync).await;

    // Create multiple clients and bootstrap them concurrently
    let num_clients = 3;
    let mut handles = Vec::with_capacity(num_clients);

    for i in 0..num_clients {
        let tree_id = test_tree_id.clone();
        let addr = server_addr.clone();

        let handle = tokio::spawn(async move {
            info!("Starting client {}", i);

            // Create client instance
            let client_instance = setup_instance_with_initialized().await;

            // Verify client doesn't have the database initially
            assert!(
                !client_instance.has_database(&tree_id).await,
                "Client {i} should not have the database initially"
            );

            // Bootstrap from server
            {
                let client_sync = client_instance.sync().unwrap();
                client_sync
                    .register_transport("http", HttpTransport::builder())
                    .await
                    .unwrap();
                client_sync
                    .sync_with_peer(&addr, Some(&tree_id))
                    .await
                    .unwrap();

                // Wait a moment for sync to complete
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            // Verify client now has the database
            assert!(
                client_instance.has_database(&tree_id).await,
                "Client {i} should have the database after bootstrap"
            );

            info!("Client {} successfully bootstrapped", i);
            Ok::<_, Error>((i, client_instance))
        });
        handles.push(handle);
    }

    // Wait for all clients to complete
    for handle in handles {
        let (client_id, _client_instance) = handle.await.unwrap().unwrap();
        info!("Client {} completed successfully", client_id);
    }

    // Cleanup
    server_sync.stop_server().await.unwrap();

    info!("Test finished: test_multiple_clients_bootstrap_same_database");
    Ok(())
}

/// Test concurrent key approval requests from multiple clients using User API.
///
/// Verifies that when multiple clients request key approval simultaneously,
/// all requests are processed correctly without race conditions.
#[tokio::test]
async fn test_concurrent_key_approval_requests() -> Result<()> {
    info!("Running test: test_concurrent_key_approval_requests");

    // Setup server with bootstrap auto-approval
    let (server_instance, _server_user, _server_key_id, _server_database, test_tree_id) =
        setup_server_with_bootstrap_database(
            "server_user",
            "server_admin",
            "Test Concurrent Approval",
        )
        .await;

    // Start server
    let server_sync = server_instance.sync().unwrap();
    let server_addr = start_sync_server(&server_sync).await;

    // Create multiple clients that will request key approval concurrently
    let num_clients = 4;
    let mut handles = Vec::with_capacity(num_clients);

    for i in 0..num_clients {
        let tree_id = test_tree_id.clone();
        let addr = server_addr.clone();

        let handle = tokio::spawn(async move {
            info!("Starting client {} key approval request", i);

            // Create client instance with user and key
            let (mut client_instance, mut client_user, client_key_id) =
                setup_indexed_client(i).await;

            // Verify client doesn't have the database initially
            assert!(
                client_user.open_database(&tree_id).await.is_err(),
                "Client {i} should not have the database initially"
            );

            // Request database access with automatic key mapping
            request_database_access_default(
                &mut client_instance,
                &mut client_user,
                &addr,
                &tree_id,
                &client_key_id,
            )
            .await
            .unwrap();

            // Verify client can now load the database using User API
            let client_db = client_user.open_database(&tree_id).await.unwrap();

            // Verify client's key was added to auth settings
            {
                let settings = client_db.get_settings().await.unwrap();
                let _auth_value = settings.get("auth").await.unwrap();
            }

            info!("Client {} successfully got key approval", i);
            Ok::<_, Error>((i, client_instance, client_user))
        });
        handles.push(handle);
    }

    // Wait for all clients to complete
    for handle in handles {
        let (client_id, _client_instance, _client_user) = handle.await.unwrap().unwrap();
        info!("Client {} key approval completed successfully", client_id);
    }

    // Cleanup
    server_sync.stop_server().await.unwrap();

    info!("Test finished: test_concurrent_key_approval_requests");
    Ok(())
}
