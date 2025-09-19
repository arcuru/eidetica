//! Tests for the manual bootstrap approval flow.
//!
//! This test suite verifies the complete manual approval workflow for bootstrap requests,
//! including storing pending requests, listing them, and approving/rejecting them.

use super::helpers::*;
use eidetica::{
    auth::{Permission as AuthPermission, settings::AuthSettings},
    constants::SETTINGS,
    store::DocStore,
    sync::{
        RequestStatus,
        handler::{SyncHandler, SyncHandlerImpl},
        protocol::{SyncRequest, SyncResponse, SyncTreeRequest},
    },
};

#[tokio::test]
async fn test_manual_approval_stores_pending_request() {
    let (_instance, _database, sync, tree_id) = setup_manual_approval_server();
    let sync_handler = create_test_sync_handler(&sync);

    // Create a bootstrap request that should be stored as pending
    let sync_request = create_bootstrap_request(
        &tree_id,
        "ed25519:test_requesting_key",
        "laptop_key",
        AuthPermission::Write(5),
    );

    // Handle the request
    let response = sync_handler.handle_request(&sync_request).await;
    let request_id = assert_bootstrap_pending(&response);
    println!("✅ Bootstrap request stored as pending: {}", request_id);

    // Verify the request was stored in sync database
    assert_request_stored(&sync, 1);

    let pending_requests = sync.pending_bootstrap_requests().unwrap();
    let (_, stored_request) = &pending_requests[0];
    assert_eq!(stored_request.tree_id, tree_id);
    assert_eq!(
        stored_request.requesting_pubkey,
        "ed25519:test_requesting_key"
    );
    assert_eq!(stored_request.requesting_key_name, "laptop_key");
    assert_eq!(
        stored_request.requested_permission,
        AuthPermission::Write(5)
    );
    assert!(matches!(stored_request.status, RequestStatus::Pending));

    println!("✅ Pending request correctly stored in sync database");
}

#[tokio::test]
async fn test_auto_approve_still_works() {
    let (_instance, _database, sync, tree_id) = setup_auto_approval_server();
    let sync_handler = create_test_sync_handler(&sync);

    // Create a bootstrap request that should be auto-approved
    let sync_request = create_bootstrap_request(
        &tree_id,
        "ed25519:test_requesting_key",
        "laptop_key",
        AuthPermission::Write(5),
    );

    // Handle the request
    let response = sync_handler.handle_request(&sync_request).await;

    // Should return Bootstrap (auto-approved)
    match response {
        SyncResponse::Bootstrap(bootstrap_response) => {
            assert_eq!(bootstrap_response.tree_id, tree_id);
            assert!(bootstrap_response.key_approved);
            assert_eq!(
                bootstrap_response.granted_permission,
                Some(AuthPermission::Write(5))
            );
            println!("✅ Bootstrap request auto-approved successfully");
        }
        other => panic!("Expected Bootstrap, got: {:?}", other),
    }

    // Should have no pending requests since it was auto-approved
    assert_request_stored(&sync, 0);

    println!("✅ Auto-approval still works when policy allows it");
}

#[tokio::test]
async fn test_approve_bootstrap_request() {
    let (_instance, database, mut sync, tree_id) = setup_manual_approval_server();

    // Server already has admin key "server_admin" from setup_manual_approval_server

    // Create sync handler and submit bootstrap request
    let sync_handler = create_test_sync_handler(&sync);
    let request_id = create_pending_bootstrap_request(
        &sync_handler,
        &tree_id,
        "ed25519:test_requesting_key",
        "laptop_key",
        AuthPermission::Write(5),
    )
    .await;

    // Verify request is pending
    assert_request_stored(&sync, 1);

    // Approve the request using server admin key
    approve_request(&mut sync, &request_id, "server_admin")
        .expect("Failed to approve bootstrap request");

    println!("✅ Bootstrap request approved successfully");

    // Verify request is now approved
    let (_, approved_request) = sync
        .get_bootstrap_request(&request_id)
        .expect("Failed to get bootstrap request")
        .expect("Bootstrap request not found");

    match approved_request.status {
        RequestStatus::Approved { approved_by, .. } => {
            assert_eq!(approved_by, "server_admin");
        }
        other => panic!("Expected Approved status, got: {:?}", other),
    }

    // Verify the key was added to the target database
    let transaction = database
        .new_transaction()
        .expect("Failed to create transaction");
    let settings_store = transaction
        .get_store::<DocStore>(SETTINGS)
        .expect("Failed to create settings store");
    let auth_doc = settings_store
        .get_node("auth")
        .expect("Failed to get auth settings");
    let auth_settings = AuthSettings::from_doc(auth_doc);
    let added_key = auth_settings
        .get_key("laptop_key")
        .expect("Auth key not found")
        .expect("Failed to parse auth key");

    assert_eq!(added_key.pubkey, "ed25519:test_requesting_key");
    assert_eq!(added_key.permissions, AuthPermission::Write(5));
    assert_eq!(added_key.status, eidetica::auth::types::KeyStatus::Active);

    println!("✅ Requesting key successfully added to target database");

    // No more pending requests
    let pending_requests = sync
        .pending_bootstrap_requests()
        .expect("Failed to list pending requests");
    assert_eq!(pending_requests.len(), 0);
}

#[tokio::test]
async fn test_reject_bootstrap_request() {
    let (_instance, database, mut sync, _tree_id) = setup_manual_approval_server();
    let tree_id = database.root_id().clone();

    // Create sync handler
    let sync_handler = SyncHandlerImpl::new(
        sync.backend().clone(),
        "_device_key",
        sync.sync_tree_root_id().clone(),
    );

    // Create a bootstrap request that will be stored as pending
    let sync_request = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![], // Empty tips = bootstrap needed
        requesting_key: Some("ed25519:test_requesting_key".to_string()),
        requesting_key_name: Some("laptop_key".to_string()),
        requested_permission: Some(AuthPermission::Write(5)),
    });

    // Handle the request to store it as pending
    let response = sync_handler.handle_request(&sync_request).await;
    let request_id = match response {
        SyncResponse::BootstrapPending { request_id, .. } => request_id,
        other => panic!("Expected BootstrapPending, got: {:?}", other),
    };

    // Verify request is pending
    let pending_requests = sync
        .pending_bootstrap_requests()
        .expect("Failed to list pending requests");
    assert_eq!(pending_requests.len(), 1);

    // Reject the request
    sync.reject_bootstrap_request(&request_id, "_device_key")
        .expect("Failed to reject bootstrap request");

    println!("✅ Bootstrap request rejected successfully");

    // Verify request is now rejected
    let (_, rejected_request) = sync
        .get_bootstrap_request(&request_id)
        .expect("Failed to get bootstrap request")
        .expect("Bootstrap request not found");

    match rejected_request.status {
        RequestStatus::Rejected { rejected_by, .. } => {
            assert_eq!(rejected_by, "_device_key");
        }
        other => panic!("Expected Rejected status, got: {:?}", other),
    }

    // Verify the key was NOT added to the target database
    let transaction = database
        .new_transaction()
        .expect("Failed to create transaction");
    let settings_store = transaction
        .get_store::<DocStore>(SETTINGS)
        .expect("Failed to create settings store");
    let key_result = match settings_store.get_node("auth") {
        Ok(auth_doc) => {
            let auth_settings = AuthSettings::from_doc(auth_doc);
            auth_settings.get_key("laptop_key")
        }
        Err(_) => None, // No auth settings means no keys
    };
    assert!(
        key_result.is_none(),
        "Key should not have been added to database"
    );

    println!("✅ Requesting key correctly NOT added to database after rejection");

    // No more pending requests
    let pending_requests = sync
        .pending_bootstrap_requests()
        .expect("Failed to list pending requests");
    assert_eq!(pending_requests.len(), 0);
}

#[tokio::test]
async fn test_list_bootstrap_requests_by_status() {
    let (_instance, database, mut sync, _tree_id) = setup_manual_approval_server();
    let tree_id = database.root_id().clone();

    // Server already has admin key "server_admin" from setup_manual_approval_server

    // Create sync handler
    let sync_handler = SyncHandlerImpl::new(
        sync.backend().clone(),
        "_device_key",
        sync.sync_tree_root_id().clone(),
    );

    // Create and store a bootstrap request
    let sync_request = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![],
        requesting_key: Some("ed25519:test_key".to_string()),
        requesting_key_name: Some("test_key".to_string()),
        requested_permission: Some(AuthPermission::Write(5)),
    });

    let response = sync_handler.handle_request(&sync_request).await;
    let request_id = match response {
        SyncResponse::BootstrapPending { request_id, .. } => request_id,
        other => panic!("Expected BootstrapPending, got: {:?}", other),
    };

    // Approve the request
    sync.approve_bootstrap_request(&request_id, "server_admin")
        .expect("Failed to approve request");

    // Try to approve again - should fail
    let result = sync.approve_bootstrap_request(&request_id, "server_admin");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Invalid request state")
    );

    // Try to reject already approved request - should fail
    let result = sync.reject_bootstrap_request(&request_id, "server_admin");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Invalid request state")
    );

    println!("✅ Double approval/rejection properly prevented");
}

#[tokio::test]
async fn test_duplicate_bootstrap_requests_same_client() {
    let (_instance, database, sync, _tree_id_from_setup) = setup_manual_approval_server();
    let tree_id = database.root_id().clone();

    // Create sync handler
    let sync_handler = SyncHandlerImpl::new(
        sync.backend().clone(),
        "_device_key",
        sync.sync_tree_root_id().clone(),
    );

    // Create first bootstrap request
    let sync_request1 = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![], // Empty tips = bootstrap needed
        requesting_key: Some("ed25519:test_requesting_key".to_string()),
        requesting_key_name: Some("laptop_key".to_string()),
        requested_permission: Some(AuthPermission::Write(5)),
    });

    // Handle first request
    let response1 = sync_handler.handle_request(&sync_request1).await;
    let request_id1 = match response1 {
        SyncResponse::BootstrapPending { request_id, .. } => request_id,
        other => panic!("Expected BootstrapPending, got: {:?}", other),
    };

    // Create identical second bootstrap request
    let sync_request2 = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![], // Empty tips = bootstrap needed
        requesting_key: Some("ed25519:test_requesting_key".to_string()),
        requesting_key_name: Some("laptop_key".to_string()),
        requested_permission: Some(AuthPermission::Write(5)),
    });

    // Handle second identical request
    let response2 = sync_handler.handle_request(&sync_request2).await;
    let request_id2 = match response2 {
        SyncResponse::BootstrapPending { request_id, .. } => request_id,
        other => panic!("Expected BootstrapPending, got: {:?}", other),
    };

    // Check how many pending requests we have
    let pending_requests = sync
        .pending_bootstrap_requests()
        .expect("Failed to list pending requests");

    // Document current behavior - may create duplicates or reuse existing
    println!(
        "Number of pending requests after duplicate submission: {}",
        pending_requests.len()
    );
    println!("First request ID: {}", request_id1);
    println!("Second request ID: {}", request_id2);

    // Verify at least one request exists
    assert!(
        !pending_requests.is_empty(),
        "Should have at least one pending request"
    );

    // Verify all requests have correct details
    for (_, request) in &pending_requests {
        assert_eq!(request.tree_id, tree_id);
        assert_eq!(request.requesting_pubkey, "ed25519:test_requesting_key");
        assert_eq!(request.requesting_key_name, "laptop_key");
        assert_eq!(request.requested_permission, AuthPermission::Write(5));
        assert!(matches!(request.status, RequestStatus::Pending));
    }

    println!("✅ Duplicate request handling behavior documented");
}

#[tokio::test]
async fn test_approval_with_nonexistent_request_id() {
    let (_instance, _database, mut sync, _tree_id) = setup_manual_approval_server();

    // Try to approve a request that doesn't exist
    let result = sync.approve_bootstrap_request("nonexistent_request_id", "_device_key");

    assert!(
        result.is_err(),
        "Approval should fail for non-existent request"
    );
    let error_msg = result.unwrap_err().to_string();
    println!("Approval error for non-existent request: {}", error_msg);
    assert!(
        error_msg.contains("Request not found") || error_msg.contains("not found"),
        "Error should indicate request not found: {}",
        error_msg
    );

    // Try to reject a request that doesn't exist
    let result = sync.reject_bootstrap_request("nonexistent_request_id", "_device_key");

    assert!(
        result.is_err(),
        "Rejection should fail for non-existent request"
    );
    let error_msg = result.unwrap_err().to_string();
    println!("Rejection error for non-existent request: {}", error_msg);
    assert!(
        error_msg.contains("Request not found") || error_msg.contains("not found"),
        "Error should indicate request not found: {}",
        error_msg
    );

    println!("✅ Non-existent request ID properly handled");
}

#[tokio::test]
async fn test_malformed_permission_requests() {
    let (_instance, database, sync, _tree_id_from_setup) = setup_manual_approval_server();
    let tree_id = database.root_id().clone();

    // Create sync handler
    let sync_handler = SyncHandlerImpl::new(
        sync.backend().clone(),
        "_device_key",
        sync.sync_tree_root_id().clone(),
    );

    // Test with various permission configurations to ensure they're handled properly
    let permission_tests = vec![
        (AuthPermission::Read, "Read permission"),
        (AuthPermission::Write(0), "Write permission with priority 0"),
        (
            AuthPermission::Write(u32::MAX),
            "Write permission with max priority",
        ),
        (AuthPermission::Admin(0), "Admin permission with priority 0"),
        (
            AuthPermission::Admin(u32::MAX),
            "Admin permission with max priority",
        ),
    ];

    for (permission, description) in &permission_tests {
        let sync_request = SyncRequest::SyncTree(SyncTreeRequest {
            tree_id: tree_id.clone(),
            our_tips: vec![],
            requesting_key: Some("ed25519:test_key".to_string()),
            requesting_key_name: Some(format!("key_for_{}", description.replace(" ", "_"))),
            requested_permission: Some(permission.clone()),
        });

        let response = sync_handler.handle_request(&sync_request).await;

        match response {
            SyncResponse::BootstrapPending { .. } => {
                println!("✅ {} correctly stored as pending", description);
            }
            other => panic!(
                "Expected BootstrapPending for {}, got: {:?}",
                description, other
            ),
        }
    }

    // Verify all requests were stored
    let pending_requests = sync
        .pending_bootstrap_requests()
        .expect("Failed to list pending requests");
    assert_eq!(
        pending_requests.len(),
        permission_tests.len(),
        "Should have stored all permission test requests"
    );

    println!("✅ All permission formats correctly processed");
}
