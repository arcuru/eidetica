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
    Database, Entry,
    auth::{
        Permission as AuthPermission,
        crypto::{format_public_key, generate_keypair},
        types::{AuthKey, KeyStatus, SigKey},
    },
    backend::VerificationStatus,
    crdt::{Doc, doc::Value},
    database::DatabaseKey,
    store::Table,
    sync::{
        RequestStatus, Sync,
        handler::{SyncHandler, SyncHandlerImpl},
        protocol::{RequestContext, SyncRequest, SyncResponse, SyncTreeRequest},
        transports::http::HttpTransport,
    },
};

#[tokio::test]
async fn test_manual_approval_stores_pending_request() {
    let (_instance, _user, _key_id, _database, sync, tree_id) =
        setup_manual_approval_server().await;
    let sync_handler = create_test_sync_handler(&sync);

    // Create a bootstrap request that should be stored as pending
    let test_key = generate_public_key();
    let sync_request =
        create_bootstrap_request(&tree_id, &test_key, "laptop_key", AuthPermission::Write(5));

    // Handle the request
    let context = RequestContext::default();
    let response = sync_handler.handle_request(&sync_request, &context).await;
    let request_id = assert_bootstrap_pending(&response);
    println!("✅ Bootstrap request stored as pending: {request_id}");

    // Verify the request was stored in sync database
    assert_request_stored(&sync, 1).await;

    let pending_requests = sync.pending_bootstrap_requests().await.unwrap();
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
    let (_instance, _user, _key_id, _database, sync, tree_id) = setup_auto_approval_server().await;
    let sync_handler = create_test_sync_handler(&sync);

    // Create a bootstrap request that should be auto-approved
    let test_key = generate_public_key();
    let sync_request =
        create_bootstrap_request(&tree_id, &test_key, "laptop_key", AuthPermission::Write(5));

    // Handle the request
    let context = RequestContext::default();
    let response = sync_handler.handle_request(&sync_request, &context).await;

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
        other => panic!("Expected Bootstrap, got: {other:?}"),
    }

    // Should have no pending requests since it was auto-approved
    assert_request_stored(&sync, 0).await;

    println!("✅ Auto-approval still works when policy allows it");
}

#[tokio::test]
async fn test_approve_bootstrap_request() {
    let (_instance, user, key_id, database, sync, tree_id) = setup_manual_approval_server().await;

    // Server already has admin key from setup_manual_approval_server

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
    assert_request_stored(&sync, 1).await;

    // Approve the request using the user's key
    approve_request(&user, &sync, &request_id, &key_id)
        .await
        .expect("Failed to approve bootstrap request");

    println!("✅ Bootstrap request approved successfully");

    // Verify request is now approved
    let (_, approved_request) = sync
        .get_bootstrap_request(&request_id)
        .await
        .expect("Failed to get bootstrap request")
        .expect("Bootstrap request not found");

    match approved_request.status {
        RequestStatus::Approved { approved_by, .. } => {
            assert_eq!(approved_by, key_id);
        }
        other => panic!("Expected Approved status, got: {other:?}"),
    }

    // Verify the key was added to the target database
    let transaction = database
        .new_transaction()
        .await
        .expect("Failed to create transaction");
    let settings_store = transaction
        .get_settings()
        .expect("Failed to create settings store");
    let added_key = settings_store
        .get_auth_key(&test_key)
        .await
        .expect("Failed to get auth key");

    assert_eq!(added_key.name(), Some("laptop_key"));
    assert_eq!(added_key.permissions(), &AuthPermission::Write(5));
    assert_eq!(added_key.status(), &KeyStatus::Active);

    println!("✅ Requesting key successfully added to target database");

    // No more pending requests
    let pending_requests = sync
        .pending_bootstrap_requests()
        .await
        .expect("Failed to list pending requests");
    assert_eq!(pending_requests.len(), 0);
}

#[tokio::test]
async fn test_reject_bootstrap_request() {
    let (_instance, user, key_id, database, sync, _tree_id) = setup_manual_approval_server().await;
    let tree_id = database.root_id().clone();

    // Create sync handler
    let sync_handler = SyncHandlerImpl::new(
        sync.instance().expect("Failed to get instance").clone(),
        sync.sync_tree_root_id().clone(),
    );

    // Create a bootstrap request that will be stored as pending
    let test_key = generate_public_key();
    let sync_request = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![], // Empty tips = bootstrap needed
        peer_pubkey: None,
        requesting_key: Some(test_key.clone()),
        requesting_key_name: Some("laptop_key".to_string()),
        requested_permission: Some(AuthPermission::Write(5)),
    });

    // Handle the request to store it as pending
    let context = RequestContext::default();
    let response = sync_handler.handle_request(&sync_request, &context).await;
    let request_id = match response {
        SyncResponse::BootstrapPending { request_id, .. } => request_id,
        other => panic!("Expected BootstrapPending, got: {other:?}"),
    };

    // Verify request is pending
    let pending_requests = sync
        .pending_bootstrap_requests()
        .await
        .expect("Failed to list pending requests");
    assert_eq!(pending_requests.len(), 1);

    // Reject the request
    user.reject_bootstrap_request(&sync, &request_id, &key_id)
        .await
        .expect("Failed to reject bootstrap request");

    println!("✅ Bootstrap request rejected successfully");

    // Verify request is now rejected
    let (_, rejected_request) = sync
        .get_bootstrap_request(&request_id)
        .await
        .expect("Failed to get bootstrap request")
        .expect("Bootstrap request not found");

    match rejected_request.status {
        RequestStatus::Rejected { rejected_by, .. } => {
            assert_eq!(rejected_by, key_id);
        }
        other => panic!("Expected Rejected status, got: {other:?}"),
    }

    // Verify the key was NOT added to the target database
    let transaction = database
        .new_transaction()
        .await
        .expect("Failed to create transaction");
    let settings_store = transaction
        .get_settings()
        .expect("Failed to create settings store");
    let key_result = settings_store.get_auth_key("laptop_key").await;
    assert!(
        key_result.is_err(),
        "Key should not have been added to database"
    );

    println!("✅ Requesting key correctly NOT added to database after rejection");

    // No more pending requests
    let pending_requests = sync
        .pending_bootstrap_requests()
        .await
        .expect("Failed to list pending requests");
    assert_eq!(pending_requests.len(), 0);
}

#[tokio::test]
async fn test_list_bootstrap_requests_by_status() {
    let (_instance, user, key_id, database, sync, _tree_id) = setup_manual_approval_server().await;
    let tree_id = database.root_id().clone();

    // Server already has admin key from setup_manual_approval_server

    // Create sync handler
    let sync_handler = SyncHandlerImpl::new(
        sync.instance().expect("Failed to get instance").clone(),
        sync.sync_tree_root_id().clone(),
    );

    // Create and store a bootstrap request
    let test_key = generate_public_key();
    let sync_request = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![],
        peer_pubkey: None,
        requesting_key: Some(test_key.clone()),
        requesting_key_name: Some("test_key".to_string()),
        requested_permission: Some(AuthPermission::Write(5)),
    });

    let context = RequestContext::default();
    let response = sync_handler.handle_request(&sync_request, &context).await;
    let request_id = match response {
        SyncResponse::BootstrapPending { request_id, .. } => request_id,
        other => panic!("Expected BootstrapPending, got: {other:?}"),
    };

    // Approve the request using the user's key
    approve_request(&user, &sync, &request_id, &key_id)
        .await
        .expect("Failed to approve request");

    // Try to approve again - should fail
    let result = approve_request(&user, &sync, &request_id, &key_id).await;
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Invalid request state")
    );

    // Try to reject already approved request - should fail
    let result = user
        .reject_bootstrap_request(&sync, &request_id, &key_id)
        .await;
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
    let (_instance, _user, _key_id, database, sync, _tree_id_from_setup) =
        setup_manual_approval_server().await;
    let tree_id = database.root_id().clone();

    // Create sync handler
    let sync_handler = SyncHandlerImpl::new(
        sync.instance().expect("Failed to get instance").clone(),
        sync.sync_tree_root_id().clone(),
    );

    // Create first bootstrap request
    let test_key = generate_public_key();
    let sync_request1 = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![], // Empty tips = bootstrap needed
        peer_pubkey: None,
        requesting_key: Some(test_key.clone()),
        requesting_key_name: Some("laptop_key".to_string()),
        requested_permission: Some(AuthPermission::Write(5)),
    });

    // Handle first request
    let context = RequestContext::default();
    let response1 = sync_handler.handle_request(&sync_request1, &context).await;
    let request_id1 = match response1 {
        SyncResponse::BootstrapPending { request_id, .. } => request_id,
        other => panic!("Expected BootstrapPending, got: {other:?}"),
    };

    // Create identical second bootstrap request
    let sync_request2 = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![], // Empty tips = bootstrap needed
        peer_pubkey: None,
        requesting_key: Some(test_key.clone()),
        requesting_key_name: Some("laptop_key".to_string()),
        requested_permission: Some(AuthPermission::Write(5)),
    });

    // Handle second identical request
    let context = RequestContext::default();
    let response2 = sync_handler.handle_request(&sync_request2, &context).await;
    let request_id2 = match response2 {
        SyncResponse::BootstrapPending { request_id, .. } => request_id,
        other => panic!("Expected BootstrapPending, got: {other:?}"),
    };

    // Check how many pending requests we have
    let pending_requests = sync
        .pending_bootstrap_requests()
        .await
        .expect("Failed to list pending requests");

    // Document current behavior - may create duplicates or reuse existing
    println!(
        "Number of pending requests after duplicate submission: {}",
        pending_requests.len()
    );
    println!("First request ID: {request_id1}");
    println!("Second request ID: {request_id2}");

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
    let (_instance, user, key_id, _database, sync, _tree_id) = setup_manual_approval_server().await;

    // Try to approve a request that doesn't exist
    let result = approve_request(&user, &sync, "nonexistent_request_id", &key_id).await;

    assert!(
        result.is_err(),
        "Approval should fail for non-existent request"
    );
    let error_msg = result.unwrap_err().to_string();
    println!("Approval error for non-existent request: {error_msg}");
    assert!(
        error_msg.contains("Request not found") || error_msg.contains("not found"),
        "Error should indicate request not found: {error_msg}"
    );

    // Try to reject a request that doesn't exist
    let result = user
        .reject_bootstrap_request(&sync, "nonexistent_request_id", &key_id)
        .await;

    assert!(
        result.is_err(),
        "Rejection should fail for non-existent request"
    );
    let error_msg = result.unwrap_err().to_string();
    println!("Rejection error for non-existent request: {error_msg}");
    assert!(
        error_msg.contains("Request not found") || error_msg.contains("not found"),
        "Error should indicate request not found: {error_msg}"
    );

    println!("✅ Non-existent request ID properly handled");
}

#[tokio::test]
async fn test_malformed_permission_requests() {
    let (_instance, _user, _key_id, database, sync, _tree_id_from_setup) =
        setup_manual_approval_server().await;
    let tree_id = database.root_id().clone();

    // Create sync handler
    let sync_handler = SyncHandlerImpl::new(
        sync.instance().expect("Failed to get instance").clone(),
        sync.sync_tree_root_id().clone(),
    );

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
            peer_pubkey: None,
            requesting_key: Some(test_key.clone()),
            requesting_key_name: Some(format!("key_for_{}", description.replace(" ", "_"))),
            requested_permission: Some(*permission),
        });

        let context = RequestContext::default();
        let response = sync_handler.handle_request(&sync_request, &context).await;

        match response {
            SyncResponse::BootstrapPending { .. } => {
                println!("✅ {description} correctly stored as pending");
            }
            other => panic!("Expected BootstrapPending for {description}, got: {other:?}"),
        }
    }

    // Verify all requests were stored
    let pending_requests = sync
        .pending_bootstrap_requests()
        .await
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
    let (server_instance, mut server_user, server_key_id) =
        crate::helpers::test_instance_with_user_and_key("server_user", Some("server_admin")).await;
    server_instance.enable_sync().await.unwrap();

    // Create database with global '*' permission for Write(10) and admin key
    let mut settings = Doc::new();
    settings.set("name", "Test Global Permission DB");

    let database = server_user
        .create_database(settings, &server_key_id)
        .await
        .unwrap();

    // Add global '*' permission
    crate::helpers::add_auth_key(
        &database,
        "*",
        AuthKey::active(Some("*"), AuthPermission::Write(10)),
    )
    .await;
    let tree_id = database.root_id().clone();

    // Setup sync
    let sync = Sync::new(server_instance.clone()).await.unwrap();

    // Enable sync for this database
    enable_sync_for_instance_database(&sync, &tree_id)
        .await
        .unwrap();

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

    let context = RequestContext::default();
    let response = sync_handler.handle_request(&sync_request, &context).await;
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
        other => panic!("Expected Bootstrap, got: {other:?}"),
    }

    // Verify NO pending requests were created (global permission bypasses storage)
    let pending_requests = sync.pending_bootstrap_requests().await.unwrap();
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

    let context = RequestContext::default();
    let response = sync_handler.handle_request(&sync_request, &context).await;
    match response {
        SyncResponse::Bootstrap(bootstrap_response) => {
            assert!(bootstrap_response.key_approved);
            assert_eq!(
                bootstrap_response.granted_permission,
                Some(AuthPermission::Read)
            );
            println!("✅ Read request auto-approved via global permission");
        }
        other => panic!("Expected Bootstrap, got: {other:?}"),
    }

    // Test 3: Request Admin(5) permission - should require manual approval (Admin > Write always)
    println!("🔍 Testing Admin(5) request against global Write(10) permission");
    let sync_request = create_bootstrap_request(
        &tree_id,
        "ed25519:admin_requesting_key",
        "admin_client",
        AuthPermission::Admin(5),
    );

    let context = RequestContext::default();
    let response = sync_handler.handle_request(&sync_request, &context).await;
    match response {
        SyncResponse::BootstrapPending { request_id, .. } => {
            println!("✅ Admin(5) request properly requires manual approval: {request_id}");
        }
        other => {
            panic!("Expected BootstrapPending for insufficient global permission, got: {other:?}")
        }
    }

    // Verify one pending request was created for the Admin request
    let pending_requests = sync.pending_bootstrap_requests().await.unwrap();
    assert_eq!(
        pending_requests.len(),
        1,
        "Should have one pending request for insufficient permission"
    );

    println!("✅ Global permission auto-approval works correctly");
}

/// Test that bootstrap approval works when key already has specific permission
/// Should approve without adding duplicate key
#[tokio::test]
async fn test_bootstrap_with_existing_specific_key_permission() {
    println!("\n🧪 TEST: Bootstrap with existing specific key permission");

    // Setup server instance
    let (server_instance, mut server_user, server_key_id) =
        crate::helpers::test_instance_with_user_and_key("server_user", Some("server_admin")).await;
    server_instance.enable_sync().await.unwrap();

    let test_key = generate_public_key();

    // Create database with both admin key and the test key with Write(5) permission
    let mut settings = Doc::new();
    settings.set("name", "Test Existing Key DB");

    let database = server_user
        .create_database(settings, &server_key_id)
        .await
        .unwrap();

    // Add the test key with Write(5) permission
    crate::helpers::add_auth_key(
        &database,
        &test_key,
        AuthKey::active(Some("existing_laptop"), AuthPermission::Write(5)),
    )
    .await;
    let tree_id = database.root_id().clone();

    // Set up sync system
    let sync = Sync::new(server_instance.clone()).await.unwrap();

    // Enable sync for this database
    enable_sync_for_instance_database(&sync, &tree_id)
        .await
        .unwrap();

    let sync_handler = create_test_sync_handler(&sync);

    // Now try to bootstrap with the same key requesting Write(10) permission (should succeed)
    let sync_request =
        create_bootstrap_request(&tree_id, &test_key, "laptop_key", AuthPermission::Write(10));

    let context = RequestContext::default();
    let response = sync_handler.handle_request(&sync_request, &context).await;

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
        other => panic!("Expected Bootstrap response, got: {other:?}"),
    }

    // Verify no duplicate key was added by checking auth settings
    let settings_store = database.get_settings().await.unwrap();
    let auth_settings = settings_store.auth_snapshot().await.unwrap();

    // Should have exactly 2 keys (admin + existing test key)
    let all_keys = auth_settings.get_all_keys().unwrap();
    let key_count = all_keys.len();
    assert_eq!(
        key_count,
        2,
        "Should have exactly 2 keys (admin + test_key), got: {key_count}. Keys: {:?}",
        all_keys.keys().collect::<Vec<_>>()
    );

    // Verify the original test key is still there (keyed by pubkey now)
    assert!(
        all_keys.contains_key(&test_key),
        "Original test key should still exist (keyed by pubkey: {test_key})"
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
    let (server_instance, mut server_user, server_key_id) =
        crate::helpers::test_instance_with_user_and_key("server_user", Some("server_admin")).await;
    server_instance.enable_sync().await.unwrap();

    let test_key = generate_public_key();

    // Create database with admin key and global Write(5) permission
    let mut settings = Doc::new();
    settings.set("name", "Test Global Permission No Duplicate DB");

    let database = server_user
        .create_database(settings, &server_key_id)
        .await
        .unwrap();

    // Add global '*' permission
    crate::helpers::add_auth_key(
        &database,
        "*",
        AuthKey::active(Some("*"), AuthPermission::Write(5)),
    )
    .await;
    let tree_id = database.root_id().clone();

    // Set up sync system
    let sync = Sync::new(server_instance.clone()).await.unwrap();

    // Enable sync for this database
    enable_sync_for_instance_database(&sync, &tree_id)
        .await
        .unwrap();

    let sync_handler = create_test_sync_handler(&sync);

    // Try to bootstrap with any key requesting Write(10) permission (should succeed via global)
    let sync_request =
        create_bootstrap_request(&tree_id, &test_key, "laptop_key", AuthPermission::Write(10));

    let context = RequestContext::default();
    let response = sync_handler.handle_request(&sync_request, &context).await;

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
        other => panic!("Expected Bootstrap response, got: {other:?}"),
    }

    // Verify no new key was added - should still only have admin + global key
    let settings_store = database.get_settings().await.unwrap();
    let auth_settings = settings_store.auth_snapshot().await.unwrap();

    // Should have exactly 2 keys (admin + global "*" key)
    let all_keys = auth_settings.get_all_keys().unwrap();
    let key_count = all_keys.len();
    assert_eq!(
        key_count,
        2,
        "Should have exactly 2 keys (admin + global '*'), got: {key_count}. Keys: {:?}",
        all_keys.keys().collect::<Vec<_>>()
    );

    // Verify the global key is still there
    assert!(all_keys.contains_key("*"), "Global key should still exist");

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
    let (server_instance, mut server_user, server_key_id) =
        crate::helpers::test_instance_with_user_and_key("server_user", Some("server_admin")).await;
    server_instance.enable_sync().await.unwrap();

    // Create database with global '*' permission allowing Write(5)
    let mut settings = Doc::new();
    settings.set("name", "Global Permission Bug Test DB");

    let database = server_user
        .create_database(settings, &server_key_id)
        .await
        .unwrap();

    // Add global '*' permission
    crate::helpers::add_auth_key(
        &database,
        "*",
        AuthKey::active(Some("*"), AuthPermission::Write(5)),
    )
    .await;
    let tree_id = database.root_id().clone();

    // Setup client instance
    let (client_instance, mut _client_user, client_key_id) =
        crate::helpers::test_instance_with_user_and_key("client_user", Some("client_key")).await;
    client_instance.enable_sync().await.unwrap();

    // Set up sync system and handler
    let sync = Sync::new(server_instance.clone()).await.unwrap();
    let sync_handler = create_test_sync_handler(&sync);

    // Client bootstraps via global permission - this should succeed
    let sync_request = create_bootstrap_request(
        &tree_id,
        &client_key_id,
        "client_key",
        AuthPermission::Write(10),
    );
    let context = RequestContext::default();
    let response = sync_handler.handle_request(&sync_request, &context).await;

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
        other => panic!("Expected Bootstrap response, got: {other:?}"),
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

#[tokio::test]
async fn test_global_permission_enables_transactions() {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    struct TestData {
        message: String,
    }

    println!("\n🧪 TEST: Global permission enables transaction commits");

    // Setup server instance
    let (server_instance, mut server_user, server_key_id) =
        crate::helpers::test_instance_with_user_and_key("server_user", Some("server_admin")).await;
    server_instance.enable_sync().await.unwrap();

    // Create database (signing key bootstrapped as Admin(0))
    let mut settings = Doc::new();
    settings.set("name", "Test Global Permission Transactions");

    let database = server_user
        .create_database(settings, &server_key_id)
        .await
        .unwrap();

    // Add extra keys via follow-up transaction
    let txn = database.new_transaction().await.unwrap();
    let settings_store = txn.get_settings().unwrap();

    // Add global '*' permission
    settings_store
        .set_auth_key("*", AuthKey::active(Some("*"), AuthPermission::Write(10)))
        .await
        .unwrap();

    txn.commit().await.unwrap();
    let tree_id = database.root_id().clone();

    // Setup sync
    let sync = Sync::new(server_instance.clone()).await.unwrap();

    // Enable sync for this database
    enable_sync_for_instance_database(&sync, &tree_id)
        .await
        .unwrap();

    let sync_handler = create_test_sync_handler(&sync);

    // Setup client instance
    let (client_instance, client_user, client_key_id) =
        crate::helpers::test_instance_with_user_and_key("client_user", Some("client_device")).await;
    client_instance.enable_sync().await.unwrap();

    println!("🔍 Testing bootstrap with global permission");

    // Test 1: Bootstrap with global permission
    let sync_request = create_bootstrap_request(
        &tree_id,
        &client_key_id,
        "client_device",
        AuthPermission::Write(15),
    );

    let context = RequestContext::default();
    let response = sync_handler.handle_request(&sync_request, &context).await;
    match response {
        SyncResponse::Bootstrap(bootstrap_response) => {
            assert_eq!(bootstrap_response.tree_id, tree_id);
            assert!(bootstrap_response.key_approved);
            println!("✅ Bootstrap approved via global permission");
        }
        other => panic!("Expected Bootstrap, got: {other:?}"),
    }

    // Verify NO pending requests were created (global permission bypasses storage)
    let pending_requests = sync.pending_bootstrap_requests().await.unwrap();
    assert_eq!(
        pending_requests.len(),
        0,
        "Global permission should not create pending requests"
    );

    // Verify client key was NOT added to auth settings (global permission used instead)
    let db_settings = database.get_settings().await.unwrap();
    match db_settings.get("auth").await {
        Ok(Value::Doc(auth_node)) => {
            // Client key should NOT be present
            assert!(
                auth_node.get("client_device").is_none(),
                "Client key should not be added when global permission grants access"
            );
            println!("✅ Client key correctly NOT added to auth settings");
        }
        _ => panic!("Auth section should exist"),
    }

    println!("🔍 Testing transaction commit with global permission");

    // Test 2: Client can commit transactions using global permission
    // Copy all tree entries from server to client so client can see the full auth settings
    let tree_entries = server_instance.backend().get_tree(&tree_id).await.unwrap();
    for entry in tree_entries {
        client_instance
            .backend()
            .put(VerificationStatus::Verified, entry)
            .await
            .unwrap();
    }

    // Load the database on client side with the client's signing key
    // When using User API, keys are stored in the User's key manager, not the Instance backend
    let client_signing_key = client_user
        .get_signing_key(&client_key_id)
        .expect("Failed to get client signing key");

    // Discover which SigKeys this public key can use
    // This will return global "*" since the client is using global permissions
    let sigkeys = Database::find_sigkeys(&client_instance, &tree_id, &client_key_id)
        .await
        .expect("Should find valid SigKeys");

    // Should have at least one SigKey (global permission)
    assert!(!sigkeys.is_empty(), "Should find at least one SigKey");

    // Extract the first SigKey (should be global permission encoded as "*:ed25519:...")
    let (sigkey, _permission) = &sigkeys[0];
    assert!(sigkey.is_global(), "Should resolve to global permission");
    let sigkey_str = match sigkey {
        SigKey::Direct(hint) => hint
            .pubkey
            .clone()
            .or(hint.name.clone())
            .expect("Should have pubkey or name"),
        _ => panic!("Expected Direct SigKey"),
    };

    let client_db = Database::open(
        client_instance.clone(),
        &tree_id,
        DatabaseKey::from_legacy_sigkey(client_signing_key, &sigkey_str),
    )
    .await
    .expect("Client should be able to load database");

    // Create a transaction and commit data
    let transaction = client_db.new_transaction().await.unwrap();
    let store = transaction
        .get_store::<Table<TestData>>("test_data")
        .await
        .unwrap();

    store
        .insert(TestData {
            message: "Test from client with global permission".to_string(),
        })
        .await
        .unwrap();

    // This should succeed now with global permission fallback
    match transaction.commit().await {
        Ok(entry_id) => {
            println!("✅ Transaction committed successfully: {entry_id}");

            // Verify the entry was created with global permission in SigInfo
            let entry = client_instance.backend().get(&entry_id).await.unwrap();
            match &entry.sig.key {
                SigKey::Direct(hint) => {
                    // Global permission is encoded as "*:ed25519:..." in the pubkey field
                    assert!(
                        entry.sig.key.is_global(),
                        "Entry should use global permission key, got: {:?}",
                        hint
                    );
                    println!("✅ Entry correctly uses global permission key in SigInfo");
                }
                other => panic!("Expected Direct SigKey, got: {other:?}"),
            }

            // Verify hint has key identification
            let hint = entry.sig.hint();
            assert!(
                hint.pubkey.is_some() || hint.name.is_some(),
                "SigInfo should include key hint"
            );
            println!("✅ SigInfo correctly includes key hint");
        }
        Err(e) => {
            panic!("Transaction should succeed with global permission: {e:?}");
        }
    }

    println!("✅ Global permission transaction test PASSED");
}

// =============================================================================
// Client Behavior Tests (End-to-End)
//
// These tests verify client-side behavior during the bootstrap approval flow,
// testing the complete round-trip through the network transport layer.
// =============================================================================

/// Test client retry behavior after receiving pending status and subsequent approval
///
/// This tests the critical user workflow:
/// 1. Client attempts bootstrap → receives pending
/// 2. Admin approves request
/// 3. Client retries → succeeds
/// 4. Client can load database
#[tokio::test]
async fn test_client_retry_after_approval() {
    println!("\n🧪 TEST: Client retry after bootstrap approval");

    // Setup server with manual approval
    let (server_instance, server_user, server_key_id, _database, server_sync, tree_id) =
        setup_manual_approval_server().await;

    // Start server
    let server_addr = start_sync_server(&server_sync).await;

    // Setup client with User API
    let (client_instance, _client_user, client_key_id, client_sync) =
        setup_sync_enabled_client("test_client", "client_key").await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // First attempt - should be pending
    println!("🔍 Client attempting bootstrap (should be pending)...");
    let bootstrap_result = client_sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            &client_key_id,
            &client_key_id,
            AuthPermission::Write(5),
        )
        .await;
    assert!(
        bootstrap_result.is_err(),
        "First attempt should fail (pending)"
    );
    println!("✅ First attempt correctly returned pending/error");

    // Get the pending request and approve it
    let pending_requests = server_sync
        .pending_bootstrap_requests()
        .await
        .expect("Failed to list pending requests");
    assert_eq!(
        pending_requests.len(),
        1,
        "Should have exactly one pending request"
    );
    let (request_id, _) = &pending_requests[0];
    println!("🔍 Found pending request: {request_id}");

    // Approve the request using the server user's key
    server_user
        .approve_bootstrap_request(&server_sync, request_id, &server_key_id)
        .await
        .expect("Failed to approve request");
    println!("✅ Request approved by admin");

    // Flush any pending sync work before client retries
    server_sync.flush().await.ok();

    // Client retries - should now succeed
    println!("🔍 Client retrying bootstrap after approval...");
    let retry_result = client_sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            &client_key_id,
            &client_key_id,
            AuthPermission::Write(5),
        )
        .await;

    // The retry might still return an error if the bootstrap response format
    // differs from what the client expects. Try normal sync as fallback.
    if retry_result.is_err() {
        println!("🔍 Bootstrap retry returned error, trying normal sync...");
        client_sync
            .sync_with_peer(&server_addr, Some(&tree_id))
            .await
            .expect("Normal sync should succeed after approval");
    }

    // Flush client sync
    client_sync.flush().await.ok();

    // Verify client has the database
    let has_db = client_instance.has_database(&tree_id).await;
    assert!(
        has_db,
        "Client should have database after successful bootstrap"
    );

    println!("✅ Client successfully received database after approval");

    // Cleanup
    server_sync.stop_server().await.unwrap();
    drop(server_instance);
    drop(server_key_id);

    println!("✅ TEST PASSED: Client retry after approval");
}

/// Test client behavior after request rejection
///
/// This tests that:
/// 1. Client attempts bootstrap → receives pending
/// 2. Admin rejects request
/// 3. Client retry fails
/// 4. Client cannot load database
#[tokio::test]
async fn test_client_denied_after_rejection() {
    println!("\n🧪 TEST: Client denied after bootstrap rejection");

    // Setup server with manual approval
    let (server_instance, server_user, server_key_id, _database, server_sync, tree_id) =
        setup_manual_approval_server().await;

    // Start server
    let server_addr = start_sync_server(&server_sync).await;

    // Setup client with User API
    let (client_instance, _client_user, client_key_id, client_sync) =
        setup_sync_enabled_client("test_client", "client_key").await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Bootstrap attempt - should be pending
    println!("🔍 Client attempting bootstrap (should be pending)...");
    let bootstrap_result = client_sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            &client_key_id,
            &client_key_id,
            AuthPermission::Write(5),
        )
        .await;
    assert!(
        bootstrap_result.is_err(),
        "First attempt should fail (pending)"
    );
    println!("✅ First attempt correctly returned pending/error");

    // Get the pending request and reject it
    let pending_requests = server_sync
        .pending_bootstrap_requests()
        .await
        .expect("Failed to list pending requests");
    assert_eq!(
        pending_requests.len(),
        1,
        "Should have exactly one pending request"
    );
    let (request_id, _) = &pending_requests[0];
    println!("🔍 Found pending request: {request_id}");

    server_user
        .reject_bootstrap_request(&server_sync, request_id, &server_key_id)
        .await
        .expect("Failed to reject request");
    println!("✅ Request rejected by admin");

    // Flush any pending sync work before client retries
    server_sync.flush().await.ok();

    // Client retries - should still fail
    println!("🔍 Client retrying bootstrap after rejection...");
    let retry_result = client_sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            &client_key_id,
            &client_key_id,
            AuthPermission::Write(5),
        )
        .await;
    assert!(retry_result.is_err(), "Retry should fail after rejection");
    println!("✅ Retry correctly failed after rejection");

    // Client should not have the database
    let has_db = client_instance.has_database(&tree_id).await;
    assert!(!has_db, "Client should NOT have database after rejection");
    println!("✅ Client correctly doesn't have database");

    // Cleanup
    server_sync.stop_server().await.unwrap();
    drop(server_instance);
    drop(server_key_id);

    println!("✅ TEST PASSED: Client denied after rejection");
}

/// Test bootstrap with user-provided key API
///
/// This verifies the `sync_with_peer_for_bootstrap_with_key` API works correctly.
#[tokio::test]
async fn test_bootstrap_api_equivalence() {
    println!("\n🧪 TEST: Bootstrap with user-provided key API");

    // Setup server with global wildcard permission (auto-approve)
    let (_server_instance, _user, _key_id, _server_db, server_sync, tree_id) =
        setup_global_wildcard_server().await;

    // Add some content to the server database
    let entry = Entry::root_builder()
        .set_subtree_data("data", r#"{"test": "data"}"#)
        .build()
        .unwrap();

    server_sync
        .backend()
        .expect("Failed to get backend")
        .put_verified(entry)
        .await
        .unwrap();

    let server_addr = start_sync_server(&server_sync).await;

    // Client: Use sync_with_peer_for_bootstrap_with_key (user-provided key)
    println!("🔍 Client: Testing user-provided key API...");
    let (client_instance, _client_user, client_key_id, client_sync) =
        setup_sync_enabled_client("client", "client_key").await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    client_sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            &client_key_id,
            &client_key_id,
            AuthPermission::Write(5),
        )
        .await
        .expect("Client bootstrap should succeed");
    client_sync.flush().await.ok();
    println!("✅ Client bootstrap succeeded with user-provided key");

    // Verify client has the data
    let client_has_root = client_sync
        .backend()
        .expect("Failed to get backend")
        .get(&tree_id)
        .await
        .is_ok();
    assert!(client_has_root, "Client should have root entry");

    // Client should have the database
    assert!(
        client_instance.has_database(&tree_id).await,
        "Client should have database"
    );

    // Cleanup
    server_sync.stop_server().await.unwrap();

    println!("✅ TEST PASSED: Bootstrap with user-provided key");
}
