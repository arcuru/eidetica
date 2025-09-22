//! Tests for the manual bootstrap approval flow.
//!
//! This test suite verifies the complete manual approval workflow for bootstrap requests,
//! including storing pending requests, listing them, and approving/rejecting them.

use super::helpers::*;

/// Generate a valid test public key
fn generate_public_key() -> String {
    let (_, verifying_key) = generate_keypair();
    format_public_key(&verifying_key)
}
use eidetica::{
    auth::{
        Permission as AuthPermission,
        crypto::{format_public_key, generate_keypair},
    },
    crdt::Doc,
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
    let test_key = generate_public_key();
    let sync_request =
        create_bootstrap_request(&tree_id, &test_key, "laptop_key", AuthPermission::Write(5));

    // Handle the request
    let response = sync_handler.handle_request(&sync_request).await;
    let request_id = assert_bootstrap_pending(&response);
    println!("✅ Bootstrap request stored as pending: {}", request_id);

    // Verify the request was stored in sync database
    assert_request_stored(&sync, 1);

    let pending_requests = sync.pending_bootstrap_requests().unwrap();
    let (_, stored_request) = &pending_requests[0];
    assert_eq!(stored_request.tree_id, tree_id);
    assert_eq!(stored_request.requesting_pubkey, test_key);
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
    let test_key = generate_public_key();
    let sync_request =
        create_bootstrap_request(&tree_id, &test_key, "laptop_key", AuthPermission::Write(5));

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
    let test_key = generate_public_key();
    let request_id = create_pending_bootstrap_request(
        &sync_handler,
        &tree_id,
        &test_key,
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
        .get_settings()
        .expect("Failed to create settings store");
    let added_key = settings_store
        .get_auth_key("laptop_key")
        .expect("Failed to get auth key");

    assert_eq!(added_key.pubkey(), &test_key);
    assert_eq!(added_key.permissions(), &AuthPermission::Write(5));
    assert_eq!(
        added_key.status(),
        &eidetica::auth::types::KeyStatus::Active
    );

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
    let sync_handler =
        SyncHandlerImpl::new(sync.backend().clone(), sync.sync_tree_root_id().clone());

    // Create a bootstrap request that will be stored as pending
    let test_key = generate_public_key();
    let sync_request = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![], // Empty tips = bootstrap needed
        requesting_key: Some(test_key.clone()),
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
        .get_settings()
        .expect("Failed to create settings store");
    let key_result = settings_store.get_auth_key("laptop_key");
    assert!(
        key_result.is_err(),
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
    let sync_handler =
        SyncHandlerImpl::new(sync.backend().clone(), sync.sync_tree_root_id().clone());

    // Create and store a bootstrap request
    let test_key = generate_public_key();
    let sync_request = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![],
        requesting_key: Some(test_key.clone()),
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
    let sync_handler =
        SyncHandlerImpl::new(sync.backend().clone(), sync.sync_tree_root_id().clone());

    // Create first bootstrap request
    let test_key = generate_public_key();
    let sync_request1 = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![], // Empty tips = bootstrap needed
        requesting_key: Some(test_key.clone()),
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
        requesting_key: Some(test_key.clone()),
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
        assert_eq!(request.requesting_pubkey, test_key);
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
    let sync_handler =
        SyncHandlerImpl::new(sync.backend().clone(), sync.sync_tree_root_id().clone());

    // Generate a test key to use for all permission tests
    let test_key = generate_public_key();

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
            requesting_key: Some(test_key.clone()),
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

#[tokio::test]
async fn test_bootstrap_with_global_permission_auto_approval() {
    println!("\n🧪 TEST: Bootstrap with global permission auto-approval");

    // Setup server instance
    let server_instance = setup_instance_with_initialized();
    let server_key = "server_admin";
    server_instance.add_private_key(server_key).unwrap();

    let server_pubkey = server_instance
        .get_formatted_public_key(server_key)
        .unwrap();

    // Create database with global '*' permission for Write(10) and admin key
    let mut settings = Doc::new();
    settings.set_string("name", "Test Global Permission DB");

    let mut auth_doc = eidetica::crdt::Doc::new();

    // Add admin key for database creation
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

    // Add global '*' permission with Write(10) access
    auth_doc
        .set_json(
            "*",
            serde_json::json!({
                "pubkey": "*",
                "permissions": {"Write": 10},
                "status": "Active"
            }),
        )
        .unwrap();

    settings.set_doc("auth", auth_doc);

    let database = server_instance.new_database(settings, server_key).unwrap();
    let tree_id = database.root_id().clone();

    // Setup sync
    let sync = eidetica::sync::Sync::new(server_instance.backend().clone()).unwrap();
    let sync_handler = create_test_sync_handler(&sync);

    // Test 1: Request Write(15) permission - should be auto-approved via global permission
    // Note: Lower priority numbers = higher permissions, so Write(15) < Write(10) in permission level
    println!("🔍 Testing Write(15) request against global Write(10) permission");
    let sync_request = create_bootstrap_request(
        &tree_id,
        "ed25519:client_requesting_key",
        "client_key",
        AuthPermission::Write(15),
    );

    let response = sync_handler.handle_request(&sync_request).await;
    match response {
        SyncResponse::Bootstrap(bootstrap_response) => {
            assert_eq!(bootstrap_response.tree_id, tree_id);
            assert!(bootstrap_response.key_approved);
            assert_eq!(
                bootstrap_response.granted_permission,
                Some(AuthPermission::Write(15))
            );
            println!("✅ Write(15) request auto-approved via global permission");
        }
        other => panic!("Expected Bootstrap, got: {:?}", other),
    }

    // Verify NO pending requests were created (global permission bypasses storage)
    let pending_requests = sync.pending_bootstrap_requests().unwrap();
    assert_eq!(
        pending_requests.len(),
        0,
        "Global permission should not create pending requests"
    );

    // Test 2: Request Read permission - should also be auto-approved (Read < Write in permission level)
    println!("🔍 Testing Read request against global Write(10) permission");
    let sync_request = create_bootstrap_request(
        &tree_id,
        "ed25519:another_client_key",
        "another_client",
        AuthPermission::Read,
    );

    let response = sync_handler.handle_request(&sync_request).await;
    match response {
        SyncResponse::Bootstrap(bootstrap_response) => {
            assert!(bootstrap_response.key_approved);
            assert_eq!(
                bootstrap_response.granted_permission,
                Some(AuthPermission::Read)
            );
            println!("✅ Read request auto-approved via global permission");
        }
        other => panic!("Expected Bootstrap, got: {:?}", other),
    }

    // Test 3: Request Admin(5) permission - should require manual approval (Admin > Write always)
    println!("🔍 Testing Admin(5) request against global Write(10) permission");
    let sync_request = create_bootstrap_request(
        &tree_id,
        "ed25519:admin_requesting_key",
        "admin_client",
        AuthPermission::Admin(5),
    );

    let response = sync_handler.handle_request(&sync_request).await;
    match response {
        SyncResponse::BootstrapPending { request_id, .. } => {
            println!(
                "✅ Admin(5) request properly requires manual approval: {}",
                request_id
            );
        }
        other => panic!(
            "Expected BootstrapPending for insufficient global permission, got: {:?}",
            other
        ),
    }

    // Verify one pending request was created for the Admin request
    let pending_requests = sync.pending_bootstrap_requests().unwrap();
    assert_eq!(
        pending_requests.len(),
        1,
        "Should have one pending request for insufficient permission"
    );

    println!("✅ Global permission auto-approval works correctly");
}

#[tokio::test]
async fn test_global_permission_overrides_manual_policy() {
    println!("\n🧪 TEST: Global permission overrides manual approval policy");

    // Setup server instance
    let server_instance = setup_instance_with_initialized();
    let server_key = "server_admin";
    server_instance.add_private_key(server_key).unwrap();

    let server_pubkey = server_instance
        .get_formatted_public_key(server_key)
        .unwrap();

    // Create database with manual approval policy (bootstrap_auto_approve: false)
    // but also global '*' permission
    let mut settings = eidetica::crdt::Doc::new();
    settings.set_string("name", "Test Manual Policy with Global Permission");

    let mut auth_doc = eidetica::crdt::Doc::new();

    // Add admin key for database creation
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

    // Add global '*' permission with Write(10) access
    auth_doc
        .set_json(
            "*",
            serde_json::json!({
                "pubkey": "*",
                "permissions": {"Write": 10},
                "status": "Active"
            }),
        )
        .unwrap();

    // Explicitly set bootstrap policy to require manual approval (false)
    let mut policy_doc = Doc::new();
    policy_doc
        .set_json("bootstrap_auto_approve", false)
        .unwrap();
    auth_doc.set_doc("policy", policy_doc);

    settings.set_doc("auth", auth_doc);

    let database = server_instance.new_database(settings, server_key).unwrap();
    let tree_id = database.root_id().clone();

    // Setup sync
    let sync = eidetica::sync::Sync::new(server_instance.backend().clone()).unwrap();
    let sync_handler = create_test_sync_handler(&sync);

    // Test 1: Request Read permission - should be auto-approved despite manual policy
    println!("🔍 Testing Read request with manual policy but global Write(10) permission");
    let sync_request = create_bootstrap_request(
        &tree_id,
        "ed25519:client_requesting_key",
        "client_key",
        AuthPermission::Read,
    );

    let response = sync_handler.handle_request(&sync_request).await;
    match response {
        SyncResponse::Bootstrap(bootstrap_response) => {
            assert_eq!(bootstrap_response.tree_id, tree_id);
            assert!(bootstrap_response.key_approved);
            assert_eq!(
                bootstrap_response.granted_permission,
                Some(AuthPermission::Read)
            );
            println!("✅ Read request auto-approved via global permission despite manual policy");
        }
        other => panic!(
            "Expected Bootstrap (global permission override), got: {:?}",
            other
        ),
    }

    // Verify NO pending requests were created (global permission bypasses manual policy)
    let pending_requests = sync.pending_bootstrap_requests().unwrap();
    assert_eq!(
        pending_requests.len(),
        0,
        "Global permission should override manual policy and not create pending requests"
    );

    // Test 2: Request Write(5) permission - should require manual approval (Write(5) > Write(10) in permission level)
    println!("🔍 Testing Write(5) request exceeding global Write(10) permission");
    let sync_request = create_bootstrap_request(
        &tree_id,
        "ed25519:write_requesting_key",
        "write_client",
        AuthPermission::Write(5),
    );

    let response = sync_handler.handle_request(&sync_request).await;
    match response {
        SyncResponse::BootstrapPending { request_id, .. } => {
            println!(
                "✅ Write(5) request properly requires manual approval (exceeds global permission): {}",
                request_id
            );
        }
        other => panic!(
            "Expected BootstrapPending for insufficient global permission, got: {:?}",
            other
        ),
    }

    // Verify one pending request was created for the Write request
    let pending_requests = sync.pending_bootstrap_requests().unwrap();
    assert_eq!(
        pending_requests.len(),
        1,
        "Should have one pending request for permission exceeding global permission"
    );

    println!("✅ Global permission correctly overrides manual policy when sufficient");
}

/// Test that bootstrap approval works when key already has specific permission
/// Should approve without adding duplicate key
#[tokio::test]
async fn test_bootstrap_with_existing_specific_key_permission() {
    println!("\n🧪 TEST: Bootstrap with existing specific key permission");

    // Setup server instance
    let server_instance = setup_instance_with_initialized();
    let server_key = "server_admin";
    server_instance.add_private_key(server_key).unwrap();

    let server_pubkey = server_instance
        .get_formatted_public_key(server_key)
        .unwrap();

    let test_key = generate_public_key();

    // Create database with both admin key and the test key with Write(5) permission
    let mut settings = eidetica::crdt::Doc::new();
    settings.set_string("name", "Test Existing Key DB");

    let mut auth_doc = eidetica::crdt::Doc::new();

    // Add admin key for database creation
    auth_doc
        .set_json(
            server_key,
            serde_json::json!({
                "pubkey": server_pubkey,
                "permissions": {"Admin": 1},
                "status": "Active"
            }),
        )
        .unwrap();

    // Add the test key with Write(5) permission
    auth_doc
        .set_json(
            "existing_laptop",
            serde_json::json!({
                "pubkey": test_key,
                "permissions": {"Write": 5},
                "status": "Active"
            }),
        )
        .unwrap();

    settings.set_doc("auth", auth_doc);

    let database = server_instance.new_database(settings, server_key).unwrap();
    let tree_id = database.root_id().clone();

    // Set up sync system
    let sync = eidetica::sync::Sync::new(database.backend().clone()).unwrap();
    let sync_handler = create_test_sync_handler(&sync);

    // Now try to bootstrap with the same key requesting Write(10) permission (should succeed)
    let sync_request =
        create_bootstrap_request(&tree_id, &test_key, "laptop_key", AuthPermission::Write(10));

    let response = sync_handler.handle_request(&sync_request).await;

    match response {
        SyncResponse::Bootstrap(bootstrap_response) => {
            // Should get approved sync response, not pending
            assert!(bootstrap_response.key_approved);
            assert_eq!(
                bootstrap_response.granted_permission,
                Some(AuthPermission::Write(10)) // Should get requested permission since existing allows it
            );
            println!("✅ Bootstrap approved via existing specific key permission");
        }
        other => panic!("Expected Bootstrap response, got: {:?}", other),
    }

    // Verify no duplicate key was added by checking auth settings
    let transaction = database.new_transaction().unwrap();
    let settings = transaction.get_settings().unwrap();
    let auth_doc = settings.get_auth_doc_for_validation().unwrap();

    // Should have exactly 2 keys (admin + existing test key)
    let key_count = auth_doc.keys().count();
    assert_eq!(
        key_count, 2,
        "Should not add duplicate key when permission already exists"
    );

    // Verify the original key is still there by checking the auth doc directly
    assert!(
        auth_doc.contains_key("existing_laptop"),
        "Original key should still exist"
    );

    println!(
        "✅ Bootstrap with existing specific key permission works correctly without duplicate"
    );
}

/// Test that bootstrap approval works when key has global permission
/// Should approve without adding new key
#[tokio::test]
async fn test_bootstrap_with_existing_global_permission_no_duplicate() {
    println!("\n🧪 TEST: Bootstrap with existing global permission - no duplicate key");

    // Setup server instance
    let server_instance = setup_instance_with_initialized();
    let server_key = "server_admin";
    server_instance.add_private_key(server_key).unwrap();

    let server_pubkey = server_instance
        .get_formatted_public_key(server_key)
        .unwrap();

    let test_key = generate_public_key();

    // Create database with admin key and global Write(5) permission
    let mut settings = eidetica::crdt::Doc::new();
    settings.set_string("name", "Test Global Permission No Duplicate DB");

    let mut auth_doc = eidetica::crdt::Doc::new();

    // Add admin key for database creation
    auth_doc
        .set_json(
            server_key,
            serde_json::json!({
                "pubkey": server_pubkey,
                "permissions": {"Admin": 1},
                "status": "Active"
            }),
        )
        .unwrap();

    // Add global '*' permission with Write(5)
    auth_doc
        .set_json(
            "*",
            serde_json::json!({
                "pubkey": "*",
                "permissions": {"Write": 5},
                "status": "Active"
            }),
        )
        .unwrap();

    settings.set_doc("auth", auth_doc);

    let database = server_instance.new_database(settings, server_key).unwrap();
    let tree_id = database.root_id().clone();

    // Set up sync system
    let sync = eidetica::sync::Sync::new(database.backend().clone()).unwrap();
    let sync_handler = create_test_sync_handler(&sync);

    // Try to bootstrap with any key requesting Write(10) permission (should succeed via global)
    let sync_request =
        create_bootstrap_request(&tree_id, &test_key, "laptop_key", AuthPermission::Write(10));

    let response = sync_handler.handle_request(&sync_request).await;

    match response {
        SyncResponse::Bootstrap(bootstrap_response) => {
            // Should get approved sync response, not pending
            assert!(bootstrap_response.key_approved);
            assert_eq!(
                bootstrap_response.granted_permission,
                Some(AuthPermission::Write(10)) // Should get requested permission since global allows it
            );
            println!("✅ Bootstrap approved via existing global permission");
        }
        other => panic!("Expected Bootstrap response, got: {:?}", other),
    }

    // Verify no new key was added - should still only have admin + global key
    let transaction = database.new_transaction().unwrap();
    let settings = transaction.get_settings().unwrap();
    let auth_doc = settings.get_auth_doc_for_validation().unwrap();

    // Should have exactly 2 keys (admin + global "*" key)
    let key_count = auth_doc.keys().count();
    assert_eq!(
        key_count, 2,
        "Should not add new key when global permission exists"
    );

    // Verify the global key is still there
    assert!(auth_doc.contains_key("*"), "Global key should still exist");

    println!(
        "✅ Bootstrap with existing global permission works correctly without adding duplicate key"
    );
}

/// Test that demonstrates client-side key discovery issue: clients approved via global
/// permission need a way to discover which SigKey to use for creating entries.
///
/// Current Behavior:
/// - Server approves bootstrap via global "*" permission without adding a per-device key
/// - Client successfully bootstraps and can read from the database
/// - When client attempts to create entries, it must choose which SigKey to use:
///   - Using their device key name (e.g., "client_key") will fail validation
///   - Using "*" as the SigKey works correctly (with pubkey field populated)
/// - However, the client has no programmatic way to discover this requirement
///
/// The Issue:
/// This is a client-side API/UX design issue. Clients need a mechanism to:
/// 1. Query the database's auth settings after bootstrap approval
/// 2. Determine whether their access comes from global "*" or a specific key
/// 3. Select the appropriate SigKey for entry creation based on that discovery
///
/// Potential Solutions:
/// - Client-side helper API: `database.discover_auth_key()` that queries auth settings
///   and returns the appropriate SigKey to use ("*" for global, specific key name otherwise)
/// - Bootstrap response enhancement: Include which key authorized the client
/// - Documentation: Clear guidance on when to use "*" vs device-specific keys
///
/// This test is intentionally ignored until the client-side key discovery mechanism is implemented.
#[ignore]
#[tokio::test]
async fn test_bootstrap_global_permission_client_cannot_create_entries_bug() {
    println!("\n🧪 TEST: Global permission bootstrap client entry creation bug");

    // Setup server instance with global permission
    let server_instance = setup_instance_with_initialized();
    let server_key = "server_admin";
    server_instance.add_private_key(server_key).unwrap();

    let server_pubkey = server_instance
        .get_formatted_public_key(server_key)
        .unwrap();

    // Create database with global '*' permission allowing Write(5)
    let mut settings = eidetica::crdt::Doc::new();
    settings.set_string("name", "Global Permission Bug Test DB");

    let mut auth_doc = eidetica::crdt::Doc::new();

    // Add admin key
    auth_doc
        .set_json(
            server_key,
            serde_json::json!({
                "pubkey": server_pubkey,
                "permissions": {"Admin": 1},
                "status": "Active"
            }),
        )
        .unwrap();

    // Add global '*' permission
    auth_doc
        .set_json(
            "*",
            serde_json::json!({
                "pubkey": "*",
                "permissions": {"Write": 5},
                "status": "Active"
            }),
        )
        .unwrap();

    settings.set_doc("auth", auth_doc);
    let database = server_instance.new_database(settings, server_key).unwrap();
    let tree_id = database.root_id().clone();

    // Setup client instance
    let client_instance = setup_instance_with_initialized();
    let client_key = "client_key";
    client_instance.add_private_key(client_key).unwrap();

    let client_pubkey = client_instance
        .get_formatted_public_key(client_key)
        .unwrap();

    // Set up sync system and handler
    let sync = eidetica::sync::Sync::new(database.backend().clone()).unwrap();
    let sync_handler = create_test_sync_handler(&sync);

    // Client bootstraps via global permission - this should succeed
    let sync_request = create_bootstrap_request(
        &tree_id,
        &client_pubkey,
        client_key,
        AuthPermission::Write(10),
    );
    let response = sync_handler.handle_request(&sync_request).await;

    // Verify bootstrap succeeded
    match response {
        SyncResponse::Bootstrap(bootstrap_response) => {
            assert!(
                bootstrap_response.key_approved,
                "Bootstrap should succeed via global permission"
            );
            assert_eq!(
                bootstrap_response.granted_permission,
                Some(AuthPermission::Write(10))
            );
            println!("✅ Client successfully bootstrapped via global permission");
        }
        other => panic!("Expected Bootstrap response, got: {:?}", other),
    }

    // CLIENT-SIDE KEY DISCOVERY ISSUE:
    // The client cannot programmatically discover which SigKey to use for entry creation.
    // This test demonstrates that clients need an API to query auth settings and
    // determine whether to use "*" (for global permissions) or their device key name.

    println!("📋 CLIENT-SIDE ISSUE: No API for discovering which SigKey to use");
    println!("   Client approved via global permission but lacks key discovery mechanism");
    println!("   Needs: database.discover_auth_key() or similar client-side API");

    // When the client-side key discovery mechanism is implemented, this test should
    // demonstrate its usage for determining the correct SigKey.

    // For now, we intentionally fail here to document the missing client-side API
    // and avoid moving `response` a second time (which would not compile).
    panic!(
        "❌ CLIENT-SIDE API MISSING: No mechanism for key discovery! \
        Client needs a way to query auth settings and determine which SigKey to use \
        for entry creation. Expected: database.discover_auth_key() returning '*' for global permissions."
    );
}
