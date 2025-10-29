//! Tests for User-level bootstrap request approval/rejection workflow.
//!
//! This test suite verifies that users can approve and reject bootstrap requests
//! using their keys, with proper permission validation and error handling.

use eidetica::{
    Instance,
    auth::{
        AuthKey, Permission,
        crypto::{format_public_key, generate_keypair},
        settings::AuthSettings,
    },
    backend::database::InMemory,
    constants::SETTINGS,
    crdt::Doc,
    instance::LegacyInstanceOps,
    store::DocStore,
    sync::{
        RequestStatus, Sync,
        handler::{SyncHandler, SyncHandlerImpl},
        protocol::{SyncRequest, SyncTreeRequest},
    },
};

// === TEST INFRASTRUCTURE ===

/// Create a server instance with a user who owns a database
///
/// Returns: (Instance, User, Database, Sync, tree_id, user_key_id)
fn setup_user_with_database() -> eidetica::Result<(
    Instance,
    eidetica::user::User,
    eidetica::Database,
    Sync,
    eidetica::entry::ID,
    String,
)> {
    let backend = Box::new(InMemory::new());
    let instance = Instance::open(backend)?;

    // Create and login user
    instance
        .create_user("alice", None)
        .expect("Failed to create user");
    let mut user = instance
        .login_user("alice", None)
        .expect("Failed to login user");

    // Get the default key (earliest created key)
    let user_key_id = user.get_default_key().expect("Failed to get default key");

    // Create a database owned by this user with explicit key
    let mut settings = Doc::new();
    settings.set_string("name", "Alice's Database");

    let database = user
        .create_database(settings, &user_key_id)
        .expect("Failed to create database");
    let tree_id = database.root_id().clone();

    // Add _device_key to the database's auth configuration so sync handler can modify the database
    let device_key_name = "_device_key";
    let device_pubkey = instance
        .get_formatted_public_key(device_key_name)
        .expect("Failed to get device public key");

    // Add _device_key as Admin to the database
    let tx = database
        .new_transaction()
        .expect("Failed to create transaction");
    let settings_store = tx.get_settings().expect("Failed to get settings store");
    let device_auth_key = AuthKey::active(device_pubkey, Permission::Admin(0))
        .expect("Failed to create device auth key");
    settings_store
        .set_auth_key(device_key_name, device_auth_key)
        .expect("Failed to set device key");
    tx.commit().expect("Failed to commit device key");

    // Create sync instance
    let sync = Sync::new(instance.clone()).expect("Failed to create sync");

    // Enable sync for this database
    use eidetica::user::types::{DatabasePreferences, SyncSettings};
    user.add_database(DatabasePreferences {
        database_id: tree_id.clone(),
        key_id: user_key_id.clone(),
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .expect("Failed to add database to user preferences");

    // Sync the user database to update combined settings
    sync.sync_user(user.user_uuid(), user.user_database().root_id())
        .expect("Failed to sync user database");

    Ok((instance, user, database, sync, tree_id, user_key_id))
}

/// Create a client keypair and formatted public key
fn create_client_key() -> (ed25519_dalek::SigningKey, String) {
    let (signing_key, verifying_key) = generate_keypair();
    let pubkey = format_public_key(&verifying_key);
    (signing_key, pubkey)
}

/// Create and submit a bootstrap request, returning the request ID
async fn create_pending_request(
    sync: &Sync,
    tree_id: &eidetica::entry::ID,
    client_pubkey: &str,
    permission: Permission,
) -> String {
    let handler = SyncHandlerImpl::new(
        sync.instance().expect("Failed to get instance").clone(),
        sync.sync_tree_root_id().clone(),
    );

    let request = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![], // Empty tips = bootstrap needed
        requesting_key: Some(client_pubkey.to_string()),
        requesting_key_name: Some("laptop_key".to_string()),
        requested_permission: Some(permission),
    });

    let context = eidetica::sync::protocol::RequestContext::default();
    let response = handler.handle_request(&request, &context).await;

    match response {
        eidetica::sync::protocol::SyncResponse::BootstrapPending { request_id, .. } => request_id,
        other => panic!("Expected BootstrapPending, got: {:?}", other),
    }
}

/// Grant a user's key permission on a database
///
/// # Arguments
/// * `database` - The database to grant permission on
/// * `user_key_id` - The user's key ID (public key string)
/// * `user` - The user who owns the key (for getting the signing key)
/// * `permission` - The permission level to grant
fn grant_user_permission_on_database(
    database: &eidetica::Database,
    user_key_id: &str,
    user: &eidetica::user::User,
    permission: Permission,
) -> eidetica::Result<()> {
    use eidetica::auth::types::AuthKey;

    // Get user's public key
    let signing_key = user.get_signing_key(user_key_id)?;
    let pubkey = format_public_key(&signing_key.verifying_key());

    // Update database auth settings using SettingsStore API
    let tx = database.new_transaction()?;
    let settings_store = tx.get_settings()?;
    settings_store.set_auth_key(user_key_id, AuthKey::active(pubkey, permission)?)?;
    tx.commit()?;

    Ok(())
}

// === USER-LEVEL APPROVAL TESTS ===

#[tokio::test]
async fn test_user_approve_bootstrap_request() {
    let (_instance, user, database, sync, tree_id, user_key_id) =
        setup_user_with_database().expect("Failed to setup test");

    // Create a client requesting access
    let (_client_key, client_pubkey) = create_client_key();

    // Create a bootstrap request
    let request_id =
        create_pending_request(&sync, &tree_id, &client_pubkey, Permission::Write(5)).await;

    // Verify request is pending
    let pending = user
        .pending_bootstrap_requests(&sync)
        .expect("Failed to list pending requests");
    assert_eq!(pending.len(), 1);
    println!("✅ Bootstrap request created and pending");

    // User approves the request using their key
    user.approve_bootstrap_request(&sync, &request_id, &user_key_id)
        .expect("Failed to approve bootstrap request");
    println!("✅ User successfully approved bootstrap request");

    // Verify request is now approved
    let (_, approved_request) = sync
        .get_bootstrap_request(&request_id)
        .expect("Failed to get bootstrap request")
        .expect("Bootstrap request not found");

    assert!(matches!(
        approved_request.status,
        RequestStatus::Approved { .. }
    ));

    // Verify the key was added to the target database
    let transaction = database
        .new_transaction()
        .expect("Failed to create transaction");
    let settings_store = transaction
        .get_store::<DocStore>(SETTINGS)
        .expect("Failed to get settings store");
    let auth_doc = settings_store
        .get_node("auth")
        .expect("Failed to get auth settings");
    let auth_settings = AuthSettings::from_doc(auth_doc);
    let added_key = auth_settings
        .get_key("laptop_key")
        .expect("Failed to get auth key");

    assert_eq!(added_key.pubkey(), &client_pubkey);
    assert_eq!(added_key.permissions(), &Permission::Write(5));
    println!("✅ Requesting key successfully added to database with correct permissions");

    // No more pending requests
    let pending = user
        .pending_bootstrap_requests(&sync)
        .expect("Failed to list pending requests");
    assert_eq!(pending.len(), 0);
}

#[tokio::test]
async fn test_user_reject_bootstrap_request() {
    let (_instance, user, database, sync, tree_id, user_key_id) =
        setup_user_with_database().expect("Failed to setup test");

    // Create a client requesting access
    let (_client_key, client_pubkey) = create_client_key();

    // Create a bootstrap request
    let request_id =
        create_pending_request(&sync, &tree_id, &client_pubkey, Permission::Write(5)).await;

    // Verify request is pending
    let pending = user
        .pending_bootstrap_requests(&sync)
        .expect("Failed to list pending requests");
    assert_eq!(pending.len(), 1);

    // User rejects the request
    user.reject_bootstrap_request(&sync, &request_id, &user_key_id)
        .expect("Failed to reject bootstrap request");
    println!("✅ User successfully rejected bootstrap request");

    // Verify request is now rejected
    let (_, rejected_request) = sync
        .get_bootstrap_request(&request_id)
        .expect("Failed to get bootstrap request")
        .expect("Bootstrap request not found");

    assert!(matches!(
        rejected_request.status,
        RequestStatus::Rejected { .. }
    ));

    // Verify the key was NOT added to the target database
    let transaction = database
        .new_transaction()
        .expect("Failed to create transaction");
    let settings_store = transaction
        .get_store::<DocStore>(SETTINGS)
        .expect("Failed to get settings store");

    // Check that the key doesn't exist in auth settings
    let auth_result = settings_store.get_node("auth");
    if let Ok(auth_doc) = auth_result {
        let auth_settings = AuthSettings::from_doc(auth_doc);
        assert!(
            auth_settings.get_key("laptop_key").is_err(),
            "Key should not have been added to database"
        );
    }
    println!("✅ Requesting key correctly NOT added to database after rejection");

    // No more pending requests
    let pending = user
        .pending_bootstrap_requests(&sync)
        .expect("Failed to list pending requests");
    assert_eq!(pending.len(), 0);
}

#[tokio::test]
async fn test_user_approve_with_nonexistent_key() {
    let (_instance, user, _database, sync, tree_id, _user_key_id) =
        setup_user_with_database().expect("Failed to setup test");

    // Create a client requesting access
    let (_client_key, client_pubkey) = create_client_key();

    // Create a bootstrap request
    let request_id =
        create_pending_request(&sync, &tree_id, &client_pubkey, Permission::Write(5)).await;

    // Try to approve with a key the user doesn't own
    let result = user.approve_bootstrap_request(&sync, &request_id, "nonexistent_key");

    assert!(result.is_err(), "Approval should fail with nonexistent key");
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Key not found") || error_msg.contains("not found"),
        "Error should indicate key not found: {}",
        error_msg
    );
    println!("✅ Approval correctly fails with nonexistent key");
}

#[tokio::test]
async fn test_user_reject_with_nonexistent_key() {
    let (_instance, user, _database, sync, tree_id, _user_key_id) =
        setup_user_with_database().expect("Failed to setup test");

    // Create a client requesting access
    let (_client_key, client_pubkey) = create_client_key();

    // Create a bootstrap request
    let request_id =
        create_pending_request(&sync, &tree_id, &client_pubkey, Permission::Write(5)).await;

    // Try to reject with a key the user doesn't own
    let result = user.reject_bootstrap_request(&sync, &request_id, "nonexistent_key");

    assert!(
        result.is_err(),
        "Rejection should fail with nonexistent key"
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Key not found") || error_msg.contains("not found"),
        "Error should indicate key not found: {}",
        error_msg
    );
    println!("✅ Rejection correctly fails with nonexistent key");
}

#[tokio::test]
async fn test_user_approve_nonexistent_request() {
    let (_instance, user, _database, sync, _tree_id, user_key_id) =
        setup_user_with_database().expect("Failed to setup test");

    // Try to approve a request that doesn't exist
    let result = user.approve_bootstrap_request(&sync, "nonexistent_request_id", &user_key_id);

    assert!(
        result.is_err(),
        "Approval should fail for non-existent request"
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Request not found") || error_msg.contains("not found"),
        "Error should indicate request not found: {}",
        error_msg
    );
    println!("✅ Approval correctly fails for non-existent request");
}

#[tokio::test]
async fn test_user_reject_nonexistent_request() {
    let (_instance, user, _database, sync, _tree_id, user_key_id) =
        setup_user_with_database().expect("Failed to setup test");

    // Try to reject a request that doesn't exist
    let result = user.reject_bootstrap_request(&sync, "nonexistent_request_id", &user_key_id);

    assert!(
        result.is_err(),
        "Rejection should fail for non-existent request"
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Request not found") || error_msg.contains("not found"),
        "Error should indicate request not found: {}",
        error_msg
    );
    println!("✅ Rejection correctly fails for non-existent request");
}

#[tokio::test]
async fn test_user_cannot_approve_twice() {
    let (_instance, user, _database, sync, tree_id, user_key_id) =
        setup_user_with_database().expect("Failed to setup test");

    // Create a client requesting access
    let (_client_key, client_pubkey) = create_client_key();

    // Create a bootstrap request
    let request_id =
        create_pending_request(&sync, &tree_id, &client_pubkey, Permission::Write(5)).await;

    // Approve once
    user.approve_bootstrap_request(&sync, &request_id, &user_key_id)
        .expect("First approval should succeed");

    // Try to approve again
    let result = user.approve_bootstrap_request(&sync, &request_id, &user_key_id);

    assert!(result.is_err(), "Second approval should fail");
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Invalid request state") || error_msg.contains("state"),
        "Error should indicate invalid state: {}",
        error_msg
    );
    println!("✅ Double approval correctly prevented");
}

#[tokio::test]
async fn test_user_cannot_reject_after_approval() {
    let (_instance, user, _database, sync, tree_id, user_key_id) =
        setup_user_with_database().expect("Failed to setup test");

    // Create a client requesting access
    let (_client_key, client_pubkey) = create_client_key();

    // Create a bootstrap request
    let request_id =
        create_pending_request(&sync, &tree_id, &client_pubkey, Permission::Write(5)).await;

    // Approve first
    user.approve_bootstrap_request(&sync, &request_id, &user_key_id)
        .expect("Approval should succeed");

    // Try to reject after approval
    let result = user.reject_bootstrap_request(&sync, &request_id, &user_key_id);

    assert!(result.is_err(), "Rejection should fail after approval");
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Invalid request state") || error_msg.contains("state"),
        "Error should indicate invalid state: {}",
        error_msg
    );
    println!("✅ Rejection after approval correctly prevented");
}

#[tokio::test]
async fn test_multiple_users() {
    // Create instances with 2 users
    let backend = Box::new(InMemory::new());
    let instance = Instance::open(backend).expect("Failed to create instance1");
    instance
        .create_user("alice", None)
        .expect("Failed to create alice");
    let mut alice = instance
        .login_user("alice", None)
        .expect("Failed to login alice");
    let alice_key = alice
        .get_default_key()
        .expect("Failed to get alice's default key");

    instance
        .create_user("bob", None)
        .expect("Failed to create bob");
    let mut bob = instance
        .login_user("bob", None)
        .expect("Failed to login bob");
    let bob_key = bob
        .get_default_key()
        .expect("Failed to get bob's default key");

    // Alice creates a database with her key
    let mut alice_db_settings = Doc::new();
    alice_db_settings.set_string("name", "Alice's Database");
    let alice_db = alice
        .create_database(alice_db_settings, &alice_key)
        .expect("Failed to create Alice's database");
    let alice_tree_id = alice_db.root_id().clone();

    // Bob creates a database with his key
    let mut bob_db_settings = Doc::new();
    bob_db_settings.set_string("name", "Bob's Database");
    let bob_db = bob
        .create_database(bob_db_settings, &bob_key)
        .expect("Failed to create Bob's database");
    let bob_tree_id = bob_db.root_id().clone();

    // Add _device_key to Alice's database for sync
    let device_key_name = "_device_key";
    let device_pubkey = instance
        .get_formatted_public_key(device_key_name)
        .expect("Failed to get device public key");
    let alice_tx = alice_db
        .new_transaction()
        .expect("Failed to create Alice transaction");
    let alice_settings = alice_tx
        .get_settings()
        .expect("Failed to get Alice's settings");
    let device_auth_key = eidetica::auth::types::AuthKey::active(
        device_pubkey.clone(),
        eidetica::auth::Permission::Admin(0),
    )
    .expect("Failed to create device auth key");
    alice_settings
        .set_auth_key(device_key_name, device_auth_key)
        .expect("Failed to set Alice device key");
    alice_tx.commit().expect("Failed to commit Alice auth");

    // Add _device_key to Bob's database for sync
    let bob_tx = bob_db
        .new_transaction()
        .expect("Failed to create Bob transaction");
    let bob_settings = bob_tx.get_settings().expect("Failed to get Bob's settings");
    let device_auth_key =
        eidetica::auth::types::AuthKey::active(device_pubkey, eidetica::auth::Permission::Admin(0))
            .expect("Failed to create device auth key");
    bob_settings
        .set_auth_key(device_key_name, device_auth_key)
        .expect("Failed to set Bob device key");
    bob_tx.commit().expect("Failed to commit Bob auth");

    // Enable sync for Alice's database
    use eidetica::user::types::{DatabasePreferences, SyncSettings};
    alice
        .add_database(DatabasePreferences {
            database_id: alice_tree_id.clone(),
            key_id: alice_key.clone(),
            sync_settings: SyncSettings {
                sync_enabled: true,
                sync_on_commit: false,
                interval_seconds: None,
                properties: Default::default(),
            },
        })
        .expect("Failed to add Alice's database preferences");

    // Enable sync for Bob's database
    bob.add_database(DatabasePreferences {
        database_id: bob_tree_id.clone(),
        key_id: bob_key.clone(),
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .expect("Failed to add Bob's database preferences");

    // Create sync instance
    let sync = Sync::new(instance.clone()).expect("Failed to create sync object");

    // Sync both users to propagate combined settings
    sync.sync_user(alice.user_uuid(), alice.user_database().root_id())
        .expect("Failed to sync Alice's user data");
    sync.sync_user(bob.user_uuid(), bob.user_database().root_id())
        .expect("Failed to sync Bob's user data");

    // Client requests access to Alice's database
    let (_client_key, client_pubkey) = create_client_key();
    let alice_request_id =
        create_pending_request(&sync, &alice_tree_id, &client_pubkey, Permission::Write(5)).await;

    // Client requests access to Bob's database (different request)
    let bob_request_id =
        create_pending_request(&sync, &bob_tree_id, &client_pubkey, Permission::Read).await;

    // Alice approves her database request
    alice
        .approve_bootstrap_request(&sync, &alice_request_id, &alice_key)
        .expect("Alice should approve her request");

    // Bob rejects his database request
    bob.reject_bootstrap_request(&sync, &bob_request_id, &bob_key)
        .expect("Bob should reject his request");

    // Verify Alice's request is approved
    let (_, alice_request) = sync
        .get_bootstrap_request(&alice_request_id)
        .expect("Failed to get Alice's request")
        .expect("Alice's request not found");
    assert!(matches!(
        alice_request.status,
        RequestStatus::Approved { .. }
    ));

    // Verify Bob's request is rejected
    let (_, bob_request) = sync
        .get_bootstrap_request(&bob_request_id)
        .expect("Failed to get Bob's request")
        .expect("Bob's request not found");
    assert!(matches!(bob_request.status, RequestStatus::Rejected { .. }));

    // Verify key added to Alice's database
    let alice_tx = alice_db
        .new_transaction()
        .expect("Failed to create Alice transaction");
    let alice_settings = alice_tx
        .get_store::<DocStore>(SETTINGS)
        .expect("Failed to get Alice's settings");
    let alice_auth = alice_settings
        .get_node("auth")
        .expect("Failed to get Alice's auth");
    let alice_auth_settings = AuthSettings::from_doc(alice_auth);
    assert!(alice_auth_settings.get_key("laptop_key").is_ok());

    // Verify key NOT added to Bob's database
    let bob_tx = bob_db
        .new_transaction()
        .expect("Failed to create Bob transaction");
    let bob_settings = bob_tx
        .get_store::<DocStore>(SETTINGS)
        .expect("Failed to get Bob's settings");
    let bob_auth = bob_settings
        .get_node("auth")
        .expect("Failed to get Bob's auth");
    let bob_auth_settings = AuthSettings::from_doc(bob_auth);
    assert!(bob_auth_settings.get_key("laptop_key").is_err());

    println!("✅ Multiple users can independently manage bootstrap requests for their databases");
}

#[tokio::test]
async fn test_user_list_pending_bootstrap_requests() {
    let (_instance, user, _database, sync, tree_id, _user_key_id) =
        setup_user_with_database().expect("Failed to setup test");

    // Initially no pending requests
    let pending = user
        .pending_bootstrap_requests(&sync)
        .expect("Failed to list pending requests");
    assert_eq!(pending.len(), 0);

    // Create multiple bootstrap requests
    let (_client1_key, client1_pubkey) = create_client_key();
    let (_client2_key, client2_pubkey) = create_client_key();

    let _request_id1 =
        create_pending_request(&sync, &tree_id, &client1_pubkey, Permission::Write(5)).await;
    let _request_id2 =
        create_pending_request(&sync, &tree_id, &client2_pubkey, Permission::Admin(1)).await;

    // List pending requests
    let pending = user
        .pending_bootstrap_requests(&sync)
        .expect("Failed to list pending requests");
    assert_eq!(pending.len(), 2);

    // Verify both requests are for the correct database
    for (_, request) in &pending {
        assert_eq!(request.tree_id, tree_id);
        assert!(matches!(request.status, RequestStatus::Pending));
    }

    println!("✅ User can list all pending bootstrap requests");
}

#[tokio::test]
async fn test_user_without_admin_cannot_modify() {
    // Create Alice with a database
    let backend = Box::new(InMemory::new());
    let instance = Instance::open(backend).expect("Failed to create instance");
    instance
        .create_user("alice", None)
        .expect("Failed to create alice");
    let mut alice = instance
        .login_user("alice", None)
        .expect("Failed to login alice");
    let alice_key = alice
        .get_default_key()
        .expect("Failed to get Alice's default key");

    let mut db_settings = Doc::new();
    db_settings.set_string("name", "Alice's Database");
    let alice_db = alice
        .create_database(db_settings, &alice_key)
        .expect("Failed to create Alice's database");
    let tree_id = alice_db.root_id().clone();

    // Add _device_key to Alice's database for sync
    let device_key_name = "_device_key";
    let device_pubkey = instance
        .get_formatted_public_key(device_key_name)
        .expect("Failed to get device public key");
    let alice_tx = alice_db
        .new_transaction()
        .expect("Failed to create Alice transaction");
    let alice_settings = alice_tx
        .get_settings()
        .expect("Failed to get Alice's settings");
    let device_auth_key = AuthKey::active(device_pubkey, Permission::Admin(0))
        .expect("Failed to create device auth key");
    alice_settings
        .set_auth_key(device_key_name, device_auth_key)
        .expect("Failed to set Alice device key");
    alice_tx.commit().expect("Failed to commit Alice auth");

    // Enable sync for Alice's database
    use eidetica::user::types::{DatabasePreferences, SyncSettings};
    alice
        .add_database(DatabasePreferences {
            database_id: tree_id.clone(),
            key_id: alice_key.clone(),
            sync_settings: SyncSettings {
                sync_enabled: true,
                sync_on_commit: false,
                interval_seconds: None,
                properties: Default::default(),
            },
        })
        .expect("Failed to add Alice's database preferences");

    // Create Bob and add a key for him
    instance
        .create_user("bob", None)
        .expect("Failed to create bob");
    let mut bob = instance
        .login_user("bob", None)
        .expect("Failed to login bob");
    let bob_key = bob
        .add_private_key(Some("Bob's Key"))
        .expect("Failed to add Bob's key");

    // Grant Bob Write permission (NOT Admin) on Alice's database using helper
    grant_user_permission_on_database(&alice_db, &bob_key, &bob, Permission::Write(10))
        .expect("Failed to grant Bob write permission");

    // Update Bob's key mapping to include Alice's database
    bob.map_key(&bob_key, &tree_id, &bob_key)
        .expect("Failed to update Bob's key mapping");

    // Create a sync instance and bootstrap request
    let sync = Sync::new(instance.clone()).expect("Failed to create sync");

    // Sync Alice's user data to propagate combined settings
    sync.sync_user(alice.user_uuid(), alice.user_database().root_id())
        .expect("Failed to sync Alice's user data");

    let (_client_key, client_pubkey) = create_client_key();
    let request_id =
        create_pending_request(&sync, &tree_id, &client_pubkey, Permission::Write(5)).await;

    // Bob (who only has Write permission, not Admin) tries to reject the request
    let result = bob.reject_bootstrap_request(&sync, &request_id, &bob_key);

    // Should fail because Bob doesn't have Admin permission
    assert!(
        result.is_err(),
        "Bob should not be able to reject without Admin permission"
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Insufficient permission") || error_msg.contains("permission"),
        "Error should indicate insufficient permission: {}",
        error_msg
    );

    println!("✅ User without Admin permission correctly cannot reject bootstrap requests");

    // Now confirm that Bob cannot approve the request either
    let result = bob.approve_bootstrap_request(&sync, &request_id, &bob_key);

    // Should fail because Bob doesn't have Admin permission
    assert!(
        result.is_err(),
        "Bob should not be able to approve without Admin permission"
    );
    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Insufficient permission") || error_msg.contains("permission"),
        "Error should indicate insufficient permission: {}",
        error_msg
    );

    println!("✅ User without Admin permission correctly cannot approve bootstrap requests");
}
