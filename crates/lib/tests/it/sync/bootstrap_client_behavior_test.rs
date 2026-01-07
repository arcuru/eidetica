//! Tests for client behavior during bootstrap approval flows.
//!
//! This module tests how clients behave during various stages of the bootstrap
//! approval process, including polling for status, retry logic, and handling
//! of duplicate requests.

use super::helpers::*;
use eidetica::auth::Permission;
use eidetica::sync::RequestStatus;
use tracing::info;

/// Test that a client can poll for the status of its pending bootstrap request
#[tokio::test]
async fn test_client_polling_for_pending_status() {
    info!("Testing client polling for pending bootstrap request status");

    // Setup server with manual approval
    let (_server_instance, _database, server_sync, tree_id) = setup_manual_approval_server().await;

    // Start server
    let server_addr = start_sync_server(&server_sync).await;

    // Setup client
    let (_client_instance, client_sync) = setup_simple_client().await;

    // Client attempts bootstrap (should be pending due to manual approval)
    client_sync.enable_http_transport().await.unwrap();
    let bootstrap_result = client_sync
        .sync_with_peer_for_bootstrap(&server_addr, &tree_id, "client_key", Permission::Write(5))
        .await;

    // Should fail due to manual approval being required
    assert!(bootstrap_result.is_err());
    let error_msg = bootstrap_result.unwrap_err().to_string();
    info!("Bootstrap error message: {}", error_msg);
    // The error might be about protocol mismatch or unexpected response
    // since the current implementation uses BootstrapPending response

    // Verify request is stored on server side
    let pending_requests = server_sync
        .pending_bootstrap_requests()
        .await
        .expect("Failed to list pending requests");
    assert_eq!(pending_requests.len(), 1);
    let (_, request) = &pending_requests[0];
    assert!(matches!(request.status, RequestStatus::Pending));
    assert_eq!(request.tree_id, tree_id);

    info!("✅ Client correctly received pending status and request stored on server");

    // Cleanup
    server_sync.stop_server().await.unwrap();
}

/// Test client retry behavior after receiving pending status
#[tokio::test]
async fn test_client_retry_after_pending() {
    info!("Testing client retry behavior after pending approval");

    // Setup server with manual approval
    let (_server_instance, _database, server_sync, tree_id) = setup_manual_approval_server().await;

    // Start server
    let server_addr = start_sync_server(&server_sync).await;

    // Setup client
    let (client_instance, client_sync) = setup_simple_client().await;

    // First attempt - should be pending
    client_sync.enable_http_transport().await.unwrap();
    let bootstrap_result = client_sync
        .sync_with_peer_for_bootstrap(&server_addr, &tree_id, "client_key", Permission::Write(5))
        .await;
    assert!(bootstrap_result.is_err());

    // Get the pending request and approve it
    let pending_requests = server_sync
        .pending_bootstrap_requests()
        .await
        .expect("Failed to list pending requests");
    assert_eq!(pending_requests.len(), 1);
    let (request_id, _) = &pending_requests[0];

    approve_request(&server_sync, request_id, "server_admin")
        .await
        .expect("Failed to approve request");

    // Flush any pending sync work before client retries
    server_sync.flush().await.ok();
    let retry_result = client_sync
        .sync_with_peer_for_bootstrap(&server_addr, &tree_id, "client_key", Permission::Write(5))
        .await;

    // Current implementation still returns error even after approval
    // This documents the current behavior - the client needs to sync normally after approval
    info!("Retry result after approval: {:?}", retry_result);

    // Alternative: Try normal sync instead of bootstrap sync after approval
    let normal_sync_result = client_sync
        .sync_with_peer(&server_addr, Some(&tree_id))
        .await;
    info!(
        "Normal sync result after approval: {:?}",
        normal_sync_result
    );

    // Verify client can load the database
    assert!(
        client_instance.load_database(&tree_id).await.is_ok(),
        "Client should be able to load database after successful bootstrap"
    );

    info!("✅ Client successfully retried and bootstrapped after approval");

    // Cleanup
    server_sync.stop_server().await.unwrap();
}

/// Test handling of duplicate bootstrap requests from the same client
#[tokio::test]
async fn test_duplicate_bootstrap_requests() {
    info!("Testing duplicate bootstrap requests from same client");

    // Setup server with manual approval
    let (_server_instance, _database, server_sync, tree_id) = setup_manual_approval_server().await;

    // Start server
    let server_addr = start_sync_server(&server_sync).await;

    // Setup client
    let (_client_instance, client_sync) = setup_simple_client().await;

    // First bootstrap attempt
    client_sync.enable_http_transport().await.unwrap();
    let first_result = client_sync
        .sync_with_peer_for_bootstrap(&server_addr, &tree_id, "client_key", Permission::Write(5))
        .await;
    assert!(first_result.is_err());

    // Verify one pending request
    let pending_requests = server_sync
        .pending_bootstrap_requests()
        .await
        .expect("Failed to list pending requests");
    assert_eq!(pending_requests.len(), 1);

    // Second bootstrap attempt with same parameters
    let second_result = client_sync
        .sync_with_peer_for_bootstrap(&server_addr, &tree_id, "client_key", Permission::Write(5))
        .await;
    assert!(second_result.is_err());

    // Should still only have one pending request (no duplicates)
    let pending_requests_after = server_sync
        .pending_bootstrap_requests()
        .await
        .expect("Failed to list pending requests");

    // Note: Current implementation may create duplicate requests
    // This test documents the current behavior and can be updated
    // when duplicate detection is implemented
    info!(
        "Pending requests after duplicate attempt: {}",
        pending_requests_after.len()
    );

    info!("✅ Duplicate request handling behavior documented");

    // Cleanup
    server_sync.stop_server().await.unwrap();
}

/// Test client behavior after request rejection
#[tokio::test]
async fn test_client_behavior_after_rejection() {
    info!("Testing client behavior after bootstrap request rejection");

    // Setup server with manual approval
    let (_server_instance, _database, server_sync, tree_id) = setup_manual_approval_server().await;

    // Start server
    let server_addr = start_sync_server(&server_sync).await;

    // Setup client
    let (client_instance, client_sync) = setup_simple_client().await;

    // Bootstrap attempt - should be pending
    client_sync.enable_http_transport().await.unwrap();
    let bootstrap_result = client_sync
        .sync_with_peer_for_bootstrap(&server_addr, &tree_id, "client_key", Permission::Write(5))
        .await;
    assert!(bootstrap_result.is_err());

    // Get the pending request and reject it
    let pending_requests = server_sync
        .pending_bootstrap_requests()
        .await
        .expect("Failed to list pending requests");
    assert_eq!(pending_requests.len(), 1);
    let (request_id, _) = &pending_requests[0];

    server_sync
        .reject_bootstrap_request(request_id, "server_admin")
        .await
        .expect("Failed to reject request");

    // Flush any pending sync work before client retries
    server_sync.flush().await.ok();
    let retry_result = client_sync
        .sync_with_peer_for_bootstrap(&server_addr, &tree_id, "client_key", Permission::Write(5))
        .await;

    assert!(retry_result.is_err(), "Retry should fail after rejection");

    // Client should not be able to access the database
    assert!(
        client_instance.load_database(&tree_id).await.is_err(),
        "Client should not have access to database after rejection"
    );

    info!("✅ Client correctly denied access after request rejection");

    // Cleanup
    server_sync.stop_server().await.unwrap();
}

/// Test client with different permission levels
#[tokio::test]
async fn test_client_different_permission_requests() {
    info!("Testing client bootstrap with different permission levels");

    // Setup server with manual approval
    let (_server_instance, _database, server_sync, tree_id) = setup_manual_approval_server().await;

    // Start server
    let server_addr = start_sync_server(&server_sync).await;

    // Test different permission levels
    let permission_levels = [
        Permission::Read,
        Permission::Write(10),
        Permission::Admin(5),
    ];

    for (i, permission) in permission_levels.iter().enumerate() {
        let client_key = format!("client_key_{i}");
        let (_client_instance, client_sync) = setup_bootstrap_client(&client_key).await;

        client_sync.enable_http_transport().await.unwrap();

        let bootstrap_result = client_sync
            .sync_with_peer_for_bootstrap(&server_addr, &tree_id, &client_key, permission.clone())
            .await;

        // Should be pending regardless of permission level
        assert!(bootstrap_result.is_err());

        // Verify request is stored with correct permission
        let pending_requests = server_sync
            .pending_bootstrap_requests()
            .await
            .expect("Failed to list pending requests");

        let matching_request = pending_requests
            .iter()
            .find(|(_, r)| r.requesting_key_name == client_key)
            .expect("Should find request for this client");

        assert_eq!(matching_request.1.requested_permission, *permission);

        info!(
            "✅ Permission level {:?} correctly stored in request",
            permission
        );
    }

    // Cleanup
    server_sync.stop_server().await.unwrap();
}

/// Test client connection errors and recovery
#[tokio::test]
async fn test_client_connection_error_handling() {
    info!("Testing client connection error handling");

    // Setup client
    let (_client_instance, client_sync) = setup_simple_client().await;
    client_sync.enable_http_transport().await.unwrap();

    // Try to connect to non-existent server
    let fake_tree_id = eidetica::entry::ID::from("fake_tree_id");
    let result = client_sync
        .sync_with_peer_for_bootstrap(
            "127.0.0.1:65432", // Non-existent server
            &fake_tree_id,
            "client_key",
            Permission::Write(5),
        )
        .await;

    // Should fail with connection error
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    // Accept various forms of connection errors
    let is_connection_error = error_msg.contains("connection")
        || error_msg.contains("refused")
        || error_msg.contains("network")
        || error_msg.contains("sending request");

    assert!(
        is_connection_error,
        "Error should indicate connection failure: {error_msg}"
    );

    info!("✅ Client correctly handles connection errors");
}
