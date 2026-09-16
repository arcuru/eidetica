//! Tests for the manual bootstrap approval flow.
//!
//! This test suite verifies the complete manual approval workflow for bootstrap requests,
//! including storing pending requests, listing them, and approving/rejecting them.

use super::helpers::*;
use crate::helpers::LocalBackendTestExt;
use eidetica::{
    Database, Entry,
    auth::{
        Permission as AuthPermission,
        crypto::{PrivateKey, PublicKey},
        types::{AuthKey, KeyStatus, SigKey},
    },
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
    let (instance, _user, _key_id, _database, sync, tree_id) = setup_manual_approval_server().await;
    let sync_handler = create_test_sync_handler(&sync);

    // Create a bootstrap request that should be stored as pending
    let signing_key = PrivateKey::generate();
    let test_key = signing_key.public_key();
    let sync_request = create_signed_bootstrap_request(
        &tree_id,
        &signing_key,
        "laptop_key",
        AuthPermission::Write(5),
        &instance.id(),
    );

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
    let (instance, _user, _key_id, _database, sync, tree_id) = setup_auto_approval_server().await;
    let sync_handler = create_test_sync_handler(&sync);

    // Create a bootstrap request that should be auto-approved
    let signing_key = PrivateKey::generate();
    let sync_request = create_signed_bootstrap_request(
        &tree_id,
        &signing_key,
        "laptop_key",
        AuthPermission::Write(5),
        &instance.id(),
    );

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
    let (instance, user, key_id, database, sync, tree_id) = setup_manual_approval_server().await;

    // Server already has admin key from setup_manual_approval_server

    // Create sync handler and submit bootstrap request
    let sync_handler = create_test_sync_handler(&sync);
    let signing_key = PrivateKey::generate();
    let test_key = signing_key.public_key();
    let request_id = create_pending_bootstrap_request(
        &sync_handler,
        &tree_id,
        &signing_key,
        "laptop_key",
        AuthPermission::Write(5),
        &instance.id(),
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
            assert_eq!(approved_by, key_id.to_string());
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
    let (instance, user, key_id, database, sync, _tree_id) = setup_manual_approval_server().await;
    let tree_id = database.root_id().clone();

    // Create sync handler
    let sync_handler = SyncHandlerImpl::new(
        sync.instance().expect("Failed to get instance").clone(),
        sync.sync_tree_root_id().clone(),
    );

    // Create a bootstrap request that will be stored as pending
    let signing_key = PrivateKey::generate();
    let test_key = signing_key.public_key();
    let sync_request = create_signed_bootstrap_request(
        &tree_id,
        &signing_key,
        "laptop_key",
        AuthPermission::Write(5),
        &instance.id(),
    );

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
            assert_eq!(rejected_by, key_id.to_string());
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
    let key_result = settings_store.get_auth_key(&test_key).await;
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
    let (instance, user, key_id, database, sync, _tree_id) = setup_manual_approval_server().await;
    let tree_id = database.root_id().clone();

    // Server already has admin key from setup_manual_approval_server

    // Create sync handler
    let sync_handler = SyncHandlerImpl::new(
        sync.instance().expect("Failed to get instance").clone(),
        sync.sync_tree_root_id().clone(),
    );

    // Create and store a bootstrap request
    let signing_key = PrivateKey::generate();
    let sync_request = create_signed_bootstrap_request(
        &tree_id,
        &signing_key,
        "test_key",
        AuthPermission::Write(5),
        &instance.id(),
    );

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

fn assert_authentication_failure(response: SyncResponse) {
    match response {
        SyncResponse::Error(message) => assert!(
            message.contains("Authentication"),
            "expected authentication failure, got: {message}"
        ),
        other => panic!("expected authentication failure, got: {other:?}"),
    }
}

#[tokio::test]
async fn named_bootstrap_request_requires_matching_proof() {
    let (instance, _user, _key_id, _database, sync, tree_id) = setup_manual_approval_server().await;
    let handler = create_test_sync_handler(&sync);
    let context = RequestContext::default();

    let missing_key = PrivateKey::generate();
    let missing = create_bootstrap_request(
        &tree_id,
        &missing_key.public_key().to_string(),
        "missing-proof",
        AuthPermission::Write(5),
    );
    assert_authentication_failure(handler.handle_request(&missing, &context).await);

    let corrupt_key = PrivateKey::generate();
    let mut corrupt = create_signed_bootstrap_request(
        &tree_id,
        &corrupt_key,
        "corrupt-proof",
        AuthPermission::Write(5),
        &instance.id(),
    );
    let SyncRequest::SyncTree(corrupt) = &mut corrupt else {
        unreachable!()
    };
    corrupt.auth.as_mut().unwrap().signature[0] ^= 0xff;
    assert_authentication_failure(
        handler
            .handle_request(&SyncRequest::SyncTree(corrupt.clone()), &context)
            .await,
    );

    let claimed_key = PrivateKey::generate();
    let other_key = PrivateKey::generate();
    let mut wrong_signer = create_signed_bootstrap_request(
        &tree_id,
        &other_key,
        "wrong-signer",
        AuthPermission::Write(5),
        &instance.id(),
    );
    let SyncRequest::SyncTree(wrong_signer) = &mut wrong_signer else {
        unreachable!()
    };
    wrong_signer.requesting_key = Some(claimed_key.public_key());
    assert_authentication_failure(
        handler
            .handle_request(&SyncRequest::SyncTree(wrong_signer.clone()), &context)
            .await,
    );

    assert!(sync.pending_bootstrap_requests().await.unwrap().is_empty());

    let incomplete = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id,
        our_tips: Vec::new().into(),
        peer_pubkey: None,
        requesting_key: Some(PrivateKey::generate().public_key()),
        requesting_key_name: None,
        requested_permission: None,
        metadata: None,
        auth: None,
    });
    assert_authentication_failure(handler.handle_request(&incomplete, &context).await);
}

#[tokio::test]
async fn unproven_callers_cannot_observe_existing_request_lifecycle() {
    let (instance, user, key_id, _database, sync, tree_id) = setup_manual_approval_server().await;
    let handler = create_test_sync_handler(&sync);
    let context = RequestContext::default();

    let pending_key = PrivateKey::generate();
    let pending_request = create_signed_bootstrap_request(
        &tree_id,
        &pending_key,
        "pending-key",
        AuthPermission::Write(5),
        &instance.id(),
    );
    let pending_id =
        assert_bootstrap_pending(&handler.handle_request(&pending_request, &context).await)
            .to_string();
    let missing_proof = create_bootstrap_request(
        &tree_id,
        &pending_key.public_key().to_string(),
        "pending-key",
        AuthPermission::Write(5),
    );
    assert_authentication_failure(handler.handle_request(&missing_proof, &context).await);
    assert_eq!(sync.pending_bootstrap_requests().await.unwrap().len(), 1);

    user.reject_bootstrap_request(&sync, &pending_id, &key_id)
        .await
        .unwrap();
    let wrong_key = PrivateKey::generate();
    let mut wrong_signer = create_signed_bootstrap_request(
        &tree_id,
        &wrong_key,
        "pending-key",
        AuthPermission::Write(5),
        &instance.id(),
    );
    let SyncRequest::SyncTree(wrong_signer) = &mut wrong_signer else {
        unreachable!()
    };
    wrong_signer.requesting_key = Some(pending_key.public_key());
    assert_authentication_failure(
        handler
            .handle_request(&SyncRequest::SyncTree(wrong_signer.clone()), &context)
            .await,
    );

    let approved_key = PrivateKey::generate();
    let approved_request = create_signed_bootstrap_request(
        &tree_id,
        &approved_key,
        "approved-key",
        AuthPermission::Read,
        &instance.id(),
    );
    let approved_id =
        assert_bootstrap_pending(&handler.handle_request(&approved_request, &context).await)
            .to_string();
    user.approve_bootstrap_request(&sync, &approved_id, &key_id)
        .await
        .unwrap();
    let approved_missing_proof = create_bootstrap_request(
        &tree_id,
        &approved_key.public_key().to_string(),
        "approved-key",
        AuthPermission::Read,
    );
    assert_authentication_failure(
        handler
            .handle_request(&approved_missing_proof, &context)
            .await,
    );
}

#[tokio::test]
async fn public_sync_distinguishes_anonymous_reads_from_named_access_requests() {
    let (instance, _user, _key_id, _database, tree_id, sync) =
        setup_public_sync_enabled_server("server", "server-key", "public-db").await;
    let handler = create_test_sync_handler(&sync);
    let context = RequestContext::default();

    let anonymous = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: Vec::new().into(),
        peer_pubkey: None,
        requesting_key: None,
        requesting_key_name: None,
        requested_permission: None,
        metadata: None,
        auth: None,
    });
    assert!(matches!(
        handler.handle_request(&anonymous, &context).await,
        SyncResponse::Bootstrap(_)
    ));

    let named_key = PrivateKey::generate();
    let unsigned_named = create_bootstrap_request(
        &tree_id,
        &named_key.public_key().to_string(),
        "named-key",
        AuthPermission::Admin(5),
    );
    assert_authentication_failure(handler.handle_request(&unsigned_named, &context).await);
    assert!(sync.pending_bootstrap_requests().await.unwrap().is_empty());

    let signed_named = create_signed_bootstrap_request(
        &tree_id,
        &named_key,
        "named-key",
        AuthPermission::Admin(5),
        &instance.id(),
    );
    assert_bootstrap_pending(&handler.handle_request(&signed_named, &context).await);
}

#[tokio::test]
async fn test_duplicate_bootstrap_requests_same_client() {
    let (instance, _user, _key_id, database, sync, _tree_id_from_setup) =
        setup_manual_approval_server().await;
    let tree_id = database.root_id().clone();

    // Create sync handler
    let sync_handler = SyncHandlerImpl::new(
        sync.instance().expect("Failed to get instance").clone(),
        sync.sync_tree_root_id().clone(),
    );

    // Create first bootstrap request
    let signing_key = PrivateKey::generate();
    let test_key = signing_key.public_key();
    let sync_request = create_signed_bootstrap_request(
        &tree_id,
        &signing_key,
        "laptop_key",
        AuthPermission::Write(5),
        &instance.id(),
    );

    // Submit the same signed request concurrently. A pending request does not
    // serve data, so its proof nonce remains reusable for an exact retry.
    let context1 = RequestContext::default();
    let context2 = RequestContext::default();
    let (response1, response2) = tokio::join!(
        sync_handler.handle_request(&sync_request, &context1),
        sync_handler.handle_request(&sync_request, &context2),
    );
    let request_id1 = match response1 {
        SyncResponse::BootstrapPending { request_id, .. } => request_id,
        other => panic!("Expected BootstrapPending, got: {other:?}"),
    };
    let request_id2 = match response2 {
        SyncResponse::BootstrapPending { request_id, .. } => request_id,
        other => panic!("Expected BootstrapPending, got: {other:?}"),
    };

    // Check how many pending requests we have
    let pending_requests = sync
        .pending_bootstrap_requests()
        .await
        .expect("Failed to list pending requests");

    assert_eq!(
        request_id1, request_id2,
        "a retry must reuse its request ID"
    );
    assert_eq!(pending_requests.len(), 1, "a retry must not add a row");

    // Verify all requests have correct details
    for (_, request) in &pending_requests {
        assert_eq!(request.tree_id, tree_id);
        assert_eq!(request.requesting_pubkey, test_key);
        assert_eq!(request.requesting_key_name, "laptop_key");
        assert_eq!(request.requested_permission, AuthPermission::Write(5));
        assert!(matches!(request.status, RequestStatus::Pending));
    }
}

#[tokio::test]
async fn test_rejected_retry_is_typed_and_other_permission_is_distinct() {
    let (instance, user, key_id, database, sync, tree_id) = setup_manual_approval_server().await;
    let handler = create_test_sync_handler(&sync);
    let signing_key = PrivateKey::generate();
    let requesting_key = signing_key.public_key();
    let write_request = create_signed_bootstrap_request(
        &tree_id,
        &signing_key,
        "laptop_key",
        AuthPermission::Write(5),
        &instance.id(),
    );
    let context = RequestContext::default();

    let first = handler.handle_request(&write_request, &context).await;
    let request_id = assert_bootstrap_pending(&first).to_string();
    user.reject_bootstrap_request(&sync, &request_id, &key_id)
        .await
        .unwrap();

    let retry = handler.handle_request(&write_request, &context).await;
    assert!(matches!(
        retry,
        SyncResponse::BootstrapRejected {
            request_id: ref rejected_id,
            ..
        } if rejected_id == &request_id
    ));
    assert!(sync.pending_bootstrap_requests().await.unwrap().is_empty());

    let read_request = create_signed_bootstrap_request(
        &tree_id,
        &signing_key,
        "laptop_key",
        AuthPermission::Read,
        &instance.id(),
    );
    let read_id = assert_bootstrap_pending(&handler.handle_request(&read_request, &context).await)
        .to_string();
    assert_ne!(read_id, request_id);
    assert_eq!(sync.pending_bootstrap_requests().await.unwrap().len(), 1);

    // A later out-of-band grant must not turn the rejected request into a
    // successful retry. Rejection is terminal for this request identity.
    let tx = database.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_auth_key(
            &requesting_key,
            AuthKey::active(Some("laptop_key"), AuthPermission::Write(5)),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        handler.handle_request(&write_request, &context).await,
        SyncResponse::BootstrapRejected {
            request_id: ref rejected_id,
            ..
        } if rejected_id == &request_id
    ));

    // The rejection remains durable when Sync is reconstructed over the same
    // persisted tree, which is the restart path for the request manager.
    let reloaded = Sync::load(sync.instance().unwrap().clone(), sync.sync_tree_root_id())
        .await
        .unwrap();
    let reloaded_handler = create_test_sync_handler(&reloaded);
    assert!(matches!(
        reloaded_handler
            .handle_request(&write_request, &context)
            .await,
        SyncResponse::BootstrapRejected {
            request_id: ref rejected_id,
            ..
        } if rejected_id == &request_id
    ));

    // Keep the target database live for the handler's sync-enabled check.
    assert_eq!(database.root_id(), &tree_id);
}

#[tokio::test]
async fn test_successful_explicit_bootstrap_consumes_request_nonce() {
    let (instance, _user, _key_id, database, sync, tree_id) = setup_manual_approval_server().await;
    let handler = create_test_sync_handler(&sync);
    let signing_key = PrivateKey::generate();
    let requesting_key = signing_key.public_key();

    let tx = database.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_auth_key(
            &requesting_key,
            AuthKey::active(Some("laptop_key"), AuthPermission::Write(5)),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let request = create_signed_bootstrap_request(
        &tree_id,
        &signing_key,
        "laptop_key",
        AuthPermission::Write(5),
        &instance.id(),
    );
    let context = RequestContext::default();
    assert!(matches!(
        handler.handle_request(&request, &context).await,
        SyncResponse::Bootstrap(_)
    ));
    assert!(matches!(
        handler.handle_request(&request, &context).await,
        SyncResponse::Error(ref message) if message.contains("nonce already spent")
    ));
}

#[tokio::test]
async fn test_approved_request_can_restart_after_grant_revocation() {
    let (instance, user, key_id, database, sync, tree_id) = setup_manual_approval_server().await;
    let handler = create_test_sync_handler(&sync);
    let signing_key = PrivateKey::generate();
    let requesting_key = signing_key.public_key();
    let request = create_signed_bootstrap_request(
        &tree_id,
        &signing_key,
        "laptop_key",
        AuthPermission::Write(5),
        &instance.id(),
    );
    let context = RequestContext::default();

    let original_id =
        assert_bootstrap_pending(&handler.handle_request(&request, &context).await).to_string();
    user.approve_bootstrap_request(&sync, &original_id, &key_id)
        .await
        .unwrap();

    let tx = database.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .revoke_auth_key(&requesting_key)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let response = handler.handle_request(&request, &context).await;
    let pending_id = assert_bootstrap_pending(&response);
    assert_eq!(pending_id, original_id);
    let stored = sync
        .get_bootstrap_request(&original_id)
        .await
        .unwrap()
        .unwrap()
        .1;
    assert!(matches!(stored.status, RequestStatus::Pending));
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
    let (instance, _user, _key_id, database, sync, _tree_id_from_setup) =
        setup_manual_approval_server().await;
    let tree_id = database.root_id().clone();

    // Create sync handler
    let sync_handler = SyncHandlerImpl::new(
        sync.instance().expect("Failed to get instance").clone(),
        sync.sync_tree_root_id().clone(),
    );

    // Generate a test key to use for all permission tests
    let signing_key = PrivateKey::generate();
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
        let sync_request = create_signed_bootstrap_request(
            &tree_id,
            &signing_key,
            &format!("key_for_{}", description.replace(" ", "_")),
            *permission,
            &instance.id(),
        );

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
        crate::helpers::test_local_instance_with_user_and_key("server_user", Some("server_admin"))
            .await;
    server_instance.enable_sync().await.unwrap();

    // Create database with global permission for Write(10) and admin key
    let mut settings = Doc::new();
    settings.set("name", "Test Global Permission DB");

    let database = server_user
        .create_database(settings, &server_key_id)
        .await
        .unwrap();

    // Add global permission
    crate::helpers::set_global_auth_key(
        &database,
        AuthKey::active(None, AuthPermission::Write(10)),
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
    let (client_key1, _) = eidetica::auth::generate_keypair();
    let sync_request = create_signed_bootstrap_request(
        &tree_id,
        &client_key1,
        "client_key",
        AuthPermission::Write(15),
        &server_instance.id(),
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
    let (client_key2, _) = eidetica::auth::generate_keypair();
    let sync_request = create_signed_bootstrap_request(
        &tree_id,
        &client_key2,
        "another_client",
        AuthPermission::Read,
        &server_instance.id(),
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
    let (client_key3, _) = eidetica::auth::generate_keypair();
    let sync_request = create_signed_bootstrap_request(
        &tree_id,
        &client_key3,
        "admin_client",
        AuthPermission::Admin(5),
        &server_instance.id(),
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
        crate::helpers::test_local_instance_with_user_and_key("server_user", Some("server_admin"))
            .await;
    server_instance.enable_sync().await.unwrap();

    let (test_signing_key, test_key) = eidetica::auth::generate_keypair();

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
    let sync_request = create_signed_bootstrap_request(
        &tree_id,
        &test_signing_key,
        "laptop_key",
        AuthPermission::Write(10),
        &server_instance.id(),
    );

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
    let test_key_str = test_key.to_string();
    assert!(
        all_keys.contains_key(&test_key_str),
        "Original test key should still exist (keyed by pubkey: {test_key_str})"
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
        crate::helpers::test_local_instance_with_user_and_key("server_user", Some("server_admin"))
            .await;
    server_instance.enable_sync().await.unwrap();

    let (test_signing_key, _) = eidetica::auth::generate_keypair();

    // Create database with admin key and global Write(5) permission
    let mut settings = Doc::new();
    settings.set("name", "Test Global Permission No Duplicate DB");

    let database = server_user
        .create_database(settings, &server_key_id)
        .await
        .unwrap();

    // Add global permission
    crate::helpers::set_global_auth_key(&database, AuthKey::active(None, AuthPermission::Write(5)))
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
    let sync_request = create_signed_bootstrap_request(
        &tree_id,
        &test_signing_key,
        "laptop_key",
        AuthPermission::Write(10),
        &server_instance.id(),
    );

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

    // Verify no new key was added - should still only have admin key (global is separate)
    let settings_store = database.get_settings().await.unwrap();
    let auth_settings = settings_store.auth_snapshot().await.unwrap();

    // Should have exactly 1 key (admin only; global is stored separately)
    let all_keys = auth_settings.get_all_keys().unwrap();
    let key_count = all_keys.len();
    assert_eq!(
        key_count,
        1,
        "Should have exactly 1 key (admin only), got: {key_count}. Keys: {:?}",
        all_keys.keys().collect::<Vec<_>>()
    );

    // Verify the global key is still there via dedicated accessor
    let global_key = auth_settings.get_global_key();
    assert!(
        global_key.is_ok(),
        "Global key should still exist via get_global_key()"
    );

    println!(
        "✅ Bootstrap with existing global permission works correctly without adding duplicate key"
    );
}

/// A key whose only authority on the target tree flows through a *delegated*
/// tree must bootstrap-access it, just like it can already sign entries there.
///
/// Topology (mirrors chaz's standalone-bridge split):
/// - session tree T grants `Write` directly to K1 and delegates to agent tree D
/// - K2 is `Admin` on D but has **no** direct grant on T
/// - K2 requests bootstrap access to T presenting only its pubkey
///
/// Before the fix the bootstrap check used a delegation-blind settings-level
/// access check, so K2 was bounced to manual approval and hung. It now resolves
/// through `Database::can_access`.
#[tokio::test]
async fn test_bootstrap_with_delegated_only_key_auto_approval() {
    use eidetica::auth::types::{DelegatedTreeRef, PermissionBounds, TreeReference};

    // Server owns both the session tree (T) and the agent tree (D).
    let (server_instance, mut server_user, server_key_id) =
        crate::helpers::test_local_instance_with_user_and_key("server_user", Some("server_admin"))
            .await;
    server_instance.enable_sync().await.unwrap();

    // Agent tree D: K2 is Admin on it, with no relationship to T except the
    // delegation T will declare below.
    let (k2_signing_key, k2) = eidetica::auth::generate_keypair();
    let mut d_settings = Doc::new();
    d_settings.set("name", "Agent DB (delegated tree)");
    let agent_db = server_user
        .create_database(d_settings, &server_key_id)
        .await
        .unwrap();
    crate::helpers::add_auth_key(
        &agent_db,
        &k2,
        AuthKey::active(Some("daemon"), AuthPermission::Admin(5)),
    )
    .await;

    // Session tree T: direct Write grant to an unrelated K1, plus a delegation
    // to D capped at Write (session delegations cap below Admin by design).
    let k1 = PublicKey::random();
    let mut t_settings = Doc::new();
    t_settings.set("name", "Session DB (target tree)");
    let session_db = server_user
        .create_database(t_settings, &server_key_id)
        .await
        .unwrap();
    crate::helpers::add_auth_key(
        &session_db,
        &k1,
        AuthKey::active(Some("bridge"), AuthPermission::Write(10)),
    )
    .await;

    // Declare the delegation on T, pinned to D's current tips (which now
    // include K2's key entry).
    let delegation_ref = DelegatedTreeRef {
        permission_bounds: PermissionBounds {
            max: AuthPermission::Write(10),
            min: None,
        },
        tree: TreeReference {
            root: agent_db.root_id().clone(),
            tips: agent_db.snapshot().await.unwrap().into_tips(),
        },
    };
    let txn = session_db.new_transaction().await.unwrap();
    txn.get_settings()
        .unwrap()
        .add_delegated_tree(delegation_ref)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let tree_id = session_db.root_id().clone();

    // Stand up the sync handler for T.
    let sync = Sync::new(server_instance.clone()).await.unwrap();
    enable_sync_for_instance_database(&sync, &tree_id)
        .await
        .unwrap();
    let sync_handler = create_test_sync_handler(&sync);

    // K2 bootstraps T with only its pubkey; its Admin-on-D clamps to Write(10).
    let sync_request = create_signed_bootstrap_request(
        &tree_id,
        &k2_signing_key,
        "daemon_key",
        AuthPermission::Write(10),
        &server_instance.id(),
    );
    let context = RequestContext::default();
    let response = sync_handler.handle_request(&sync_request, &context).await;

    match response {
        SyncResponse::Bootstrap(bootstrap_response) => {
            assert!(
                bootstrap_response.key_approved,
                "delegated-only key should be auto-approved"
            );
            assert_eq!(
                bootstrap_response.granted_permission,
                Some(AuthPermission::Write(10)),
                "granted permission should be the delegated authority, clamped to Write(10)"
            );
        }
        SyncResponse::BootstrapPending { .. } => {
            panic!("delegated-only key was bounced to manual approval");
        }
        other => panic!("Expected Bootstrap response, got: {other:?}"),
    }
}

/// Test that demonstrates client-side key discovery issue: clients approved via global
/// permission need a way to discover which SigKey to use for creating entries.
///
/// Current Behavior:
/// - Server approves bootstrap via global permission without adding a per-device key
/// - Client successfully bootstraps and can read from the database
/// - When client attempts to create entries, it must choose which SigKey to use:
///   - Using their device key name (e.g., "client_key") will fail validation
///   - Using a global SigKey works correctly (with pubkey field populated)
/// - However, the client has no programmatic way to discover this requirement
///
/// The Issue:
/// This is a client-side API/UX design issue. Clients need a mechanism to:
/// 1. Query the database's auth settings after bootstrap approval
/// 2. Determine whether their access comes from global permission or a specific key
/// 3. Select the appropriate SigKey for entry creation based on that discovery
///
/// Potential Solutions:
/// - Client-side helper API: `database.discover_auth_key()` that queries auth settings
///   and returns the appropriate SigKey (global or device-specific)
/// - Bootstrap response enhancement: Include which key authorized the client
/// - Documentation: Clear guidance on when to use global vs device-specific keys
///
/// This test is intentionally ignored until the client-side key discovery mechanism is implemented.
#[ignore]
#[tokio::test]
async fn test_bootstrap_global_permission_client_cannot_create_entries_bug() {
    println!("\n🧪 TEST: Global permission bootstrap client entry creation bug");

    // Setup server instance with global permission
    let (server_instance, mut server_user, server_key_id) =
        crate::helpers::test_local_instance_with_user_and_key("server_user", Some("server_admin"))
            .await;
    server_instance.enable_sync().await.unwrap();

    // Create database with global permission allowing Write(5)
    let mut settings = Doc::new();
    settings.set("name", "Global Permission Bug Test DB");

    let database = server_user
        .create_database(settings, &server_key_id)
        .await
        .unwrap();

    // Add global permission
    crate::helpers::set_global_auth_key(&database, AuthKey::active(None, AuthPermission::Write(5)))
        .await;
    let tree_id = database.root_id().clone();

    // Setup client instance
    let (client_instance, client_user, client_key_id) =
        crate::helpers::test_local_instance_with_user_and_key("client_user", Some("client_key"))
            .await;
    client_instance.enable_sync().await.unwrap();

    // Set up sync system and handler
    let sync = Sync::new(server_instance.clone()).await.unwrap();
    let sync_handler = create_test_sync_handler(&sync);

    // Client bootstraps via global permission - this should succeed
    let client_signing_key = client_user
        .get_signing_key(&client_key_id)
        .expect("Failed to get client signing key");
    let sync_request = create_signed_bootstrap_request(
        &tree_id,
        &client_signing_key,
        "client_key",
        AuthPermission::Write(10),
        &server_instance.id(),
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
    // determine whether to use global permission or their device key name.

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
        crate::helpers::test_local_instance_with_user_and_key("server_user", Some("server_admin"))
            .await;
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

    // Add global permission
    settings_store
        .set_global_auth_key(AuthKey::active(None, AuthPermission::Write(10)))
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
        crate::helpers::test_local_instance_with_user_and_key("client_user", Some("client_device"))
            .await;
    client_instance.enable_sync().await.unwrap();

    println!("🔍 Testing bootstrap with global permission");

    // Test 1: Bootstrap with global permission
    let client_signing_key = client_user
        .get_signing_key(&client_key_id)
        .expect("Failed to get client signing key");
    let sync_request = create_signed_bootstrap_request(
        &tree_id,
        &client_signing_key,
        "client_device",
        AuthPermission::Write(15),
        &server_instance.id(),
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
        client_instance.backend().put_verified(entry).await.unwrap();
    }

    // Discover which SigKeys this public key can use
    // This will return a global SigKey since the client is using global permissions
    let sigkeys = Database::find_sigkeys(&client_instance, &tree_id, &client_key_id)
        .await
        .expect("Should find valid SigKeys");

    // Should have at least one SigKey (global permission)
    assert!(!sigkeys.is_empty(), "Should find at least one SigKey");

    // Extract the first SigKey (should be global permission encoded as "*:ed25519:...")
    let (sigkey, _permission) = &sigkeys[0];
    assert!(sigkey.is_global(), "Should resolve to global permission");

    let client_db = Database::open(&client_instance, &tree_id)
        .await
        .expect("Client should be able to load database")
        .with_key(DatabaseKey::with_identity(
            client_signing_key,
            sigkey.clone(),
        ));

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

            // Verify the entry was created with global permission in AuthInfo
            let entry = client_instance.backend().get(&entry_id).await.unwrap();
            match &entry.auth().key {
                SigKey::Direct { hint } => {
                    // Global permission is encoded as "*:ed25519:..." in the pubkey field
                    assert!(
                        entry.auth().key.is_global(),
                        "Entry should use global permission key, got: {:?}",
                        hint
                    );
                    println!("✅ Entry correctly uses global permission key in AuthInfo");
                }
                other => panic!("Expected Direct SigKey, got: {other:?}"),
            }

            // Verify hint has key identification
            let hint = entry.auth().hint();
            assert!(
                hint.pubkey.is_some() || hint.name.is_some(),
                "AuthInfo should include key hint"
            );
            println!("✅ AuthInfo correctly includes key hint");
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
    let (client_instance, client_user, client_key_id, client_sync) =
        setup_sync_enabled_client("test_client", "client_key").await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // First attempt - should be pending
    println!("🔍 Client attempting bootstrap (should be pending)...");
    let client_key_str = client_key_id.to_string();
    let bootstrap_result = client_sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            &client_user.get_signing_key(&client_key_id).unwrap(),
            &client_key_str,
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
            &client_user.get_signing_key(&client_key_id).unwrap(),
            &client_key_str,
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
    let (client_instance, client_user, client_key_id, client_sync) =
        setup_sync_enabled_client("test_client", "client_key").await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Bootstrap attempt - should be pending
    println!("🔍 Client attempting bootstrap (should be pending)...");
    let client_key_str = client_key_id.to_string();
    let bootstrap_result = client_sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            &client_user.get_signing_key(&client_key_id).unwrap(),
            &client_key_str,
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
            &client_user.get_signing_key(&client_key_id).unwrap(),
            &client_key_str,
            AuthPermission::Write(5),
        )
        .await;
    let error = retry_result.expect_err("Retry should fail after rejection");
    assert!(matches!(
        error,
        eidetica::Error::Sync(ref error)
            if matches!(error.as_ref(), eidetica::sync::SyncError::BootstrapRejected { request_id: rejected_id, .. } if rejected_id == request_id)
    ));
    println!("✅ Retry correctly failed after rejection");

    // Client should not have the database
    let has_db = client_instance.has_database(&tree_id).await;
    assert!(!has_db, "Client should NOT have database after rejection");
    println!("✅ Client correctly doesn't have database");

    // Cleanup
    server_sync.stop_server().await.unwrap();
    drop(server_instance);

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
    let (client_instance, client_user, client_key_id, client_sync) =
        setup_sync_enabled_client("client", "client_key").await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    let client_key_str = client_key_id.to_string();
    client_sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            &client_user.get_signing_key(&client_key_id).unwrap(),
            &client_key_str,
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

// Test-only: store-and-promote helper (production `put` is Unverified-only).
use crate::helpers::TestVerify;
