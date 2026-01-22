//! Tests for sync-enabled security checks.
//!
//! These tests verify that the sync handler properly enforces the sync-enabled
//! requirement for all sync operations, preventing unauthorized access to databases
//! that don't have sync enabled in user preferences.

use eidetica::{
    Database,
    auth::{AuthSettings, Permission, types::AuthKey},
    crdt::Doc,
    store::DocStore,
    sync::{handler::SyncHandler, transports::http::HttpTransport},
    user::types::{SyncSettings, TrackedDatabase},
};

use super::helpers;

/// Test that bootstrap requests are rejected for databases without sync enabled.
#[tokio::test]
async fn test_bootstrap_rejected_when_sync_disabled() {
    // Create server with database but NO sync enabled
    let server_instance = helpers::setup_instance_with_initialized().await;
    server_instance
        .create_user("server_user", None)
        .await
        .unwrap();
    let mut server_user = server_instance
        .login_user("server_user", None)
        .await
        .unwrap();
    let server_key_id = server_user
        .add_private_key(Some("server_key"))
        .await
        .unwrap();

    // Create database (user.create_database adds the user's key automatically)
    let mut settings = Doc::new();
    settings.set("name", "test_database");

    let server_database = server_user
        .create_database(settings, &server_key_id)
        .await
        .unwrap();
    let tree_id = server_database.root_id().clone();

    // Add database to user preferences but with sync DISABLED
    server_user
        .track_database(TrackedDatabase {
            database_id: tree_id.clone(),
            key_id: server_key_id.clone(),
            sync_settings: SyncSettings {
                sync_enabled: false, // Sync is DISABLED
                sync_on_commit: false,
                interval_seconds: None,
                properties: Default::default(),
            },
        })
        .await
        .unwrap();

    // Update sync configuration to reflect disabled state
    let server_sync = server_instance.sync().unwrap();
    server_sync
        .sync_user(
            server_user.user_uuid(),
            server_user.user_database().root_id(),
        )
        .await
        .unwrap();

    // Enable HTTP transport and start server
    server_sync
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    server_sync.accept_connections().await.unwrap();
    let server_addr = server_sync.get_server_address().await.unwrap();

    // Create client that will attempt to bootstrap
    let (client_instance, client_sync) = helpers::setup().await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Attempt to sync - should be rejected as "Tree not found"
    let result = client_sync
        .sync_with_peer(&server_addr, Some(&tree_id))
        .await;

    // Verify the request was rejected
    assert!(
        result.is_err(),
        "Bootstrap should be rejected when sync is disabled"
    );

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("Tree not found") || error_msg.contains("not found"),
        "Error should indicate tree not found (hiding sync-disabled status): {error_msg}"
    );

    // Verify database was NOT synced to client
    assert!(
        !client_instance.has_database(&tree_id).await,
        "Database should not exist on client after rejected bootstrap"
    );

    // Clean up
    server_sync.stop_server().await.unwrap();
}

/// Test that incremental sync requests are rejected for databases without sync enabled.
#[tokio::test]
async fn test_incremental_sync_rejected_when_sync_disabled() {
    // Create server with database and sync initially ENABLED
    let server_instance = helpers::setup_instance_with_initialized().await;
    server_instance
        .create_user("server_user", None)
        .await
        .unwrap();
    let mut server_user = server_instance
        .login_user("server_user", None)
        .await
        .unwrap();
    let server_key_id = server_user
        .add_private_key(Some("server_key"))
        .await
        .unwrap();

    // Create database with wildcard "*" permission to allow unauthenticated sync
    // (We're testing sync-enabled checks, not authentication)
    let mut settings = Doc::new();
    settings.set("name", "test_database");

    let server_pubkey = server_user
        .get_public_key(&server_key_id)
        .expect("Failed to get server public key");

    let mut auth_settings = AuthSettings::new();
    auth_settings
        .add_key(
            &server_key_id,
            AuthKey::active(&server_pubkey, Permission::Admin(0)).unwrap(),
        )
        .unwrap();
    auth_settings
        .add_key("*", AuthKey::active("*", Permission::Read).unwrap())
        .unwrap();
    settings.set("auth", auth_settings.as_doc().clone());

    let server_database = server_user
        .create_database(settings, &server_key_id)
        .await
        .unwrap();
    let tree_id = server_database.root_id().clone();

    // Add database with sync ENABLED initially
    server_user
        .track_database(TrackedDatabase {
            database_id: tree_id.clone(),
            key_id: server_key_id.clone(),
            sync_settings: SyncSettings {
                sync_enabled: true, // Sync is ENABLED initially
                sync_on_commit: false,
                interval_seconds: None,
                properties: Default::default(),
            },
        })
        .await
        .unwrap();

    let server_sync = server_instance.sync().unwrap();
    server_sync
        .sync_user(
            server_user.user_uuid(),
            server_user.user_database().root_id(),
        )
        .await
        .unwrap();

    // Enable HTTP transport and start server
    server_sync
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    server_sync.accept_connections().await.unwrap();
    let server_addr = server_sync.get_server_address().await.unwrap();

    // Create client and perform initial bootstrap (should succeed)
    let (client_instance, client_sync) = helpers::setup().await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    let result = client_sync
        .sync_with_peer(&server_addr, Some(&tree_id))
        .await;
    assert!(
        result.is_ok(),
        "Initial bootstrap should succeed when sync enabled"
    );

    // Verify database was bootstrapped to client
    assert!(
        client_instance.has_database(&tree_id).await,
        "Database should exist on client after bootstrap"
    );

    // Load the database on client to get tips for incremental sync
    // Use global "*" permission (configured above with Permission::Read)
    let (reader_key, _) = eidetica::auth::generate_keypair();
    let client_db = Database::open(
        client_instance.clone(),
        &tree_id,
        reader_key,
        "*".to_string(),
    )
    .await
    .unwrap();
    let client_tips = client_instance
        .backend()
        .get_tips(client_db.root_id())
        .await
        .unwrap();

    // NOW disable sync on the server
    server_user
        .track_database(TrackedDatabase {
            database_id: tree_id.clone(),
            key_id: server_key_id.clone(),
            sync_settings: SyncSettings {
                sync_enabled: false, // Sync is now DISABLED
                sync_on_commit: false,
                interval_seconds: None,
                properties: Default::default(),
            },
        })
        .await
        .unwrap();

    server_sync
        .sync_user(
            server_user.user_uuid(),
            server_user.user_database().root_id(),
        )
        .await
        .unwrap();

    // Make a change on the server
    {
        let tx = server_database.new_transaction().await.unwrap();
        let doc_store = tx
            .get_store::<eidetica::store::DocStore>("data")
            .await
            .unwrap();
        doc_store.set("key", "value").await.unwrap();
        tx.commit().await.unwrap();
    }

    // Attempt incremental sync - should be rejected
    use eidetica::sync::protocol::{SyncRequest, SyncTreeRequest};
    let sync_handler = helpers::create_test_sync_handler(&server_sync);
    let sync_request = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: client_tips, // Non-empty tips = incremental sync
        peer_pubkey: None,
        requesting_key: None,
        requesting_key_name: None,
        requested_permission: None,
    });

    let context = eidetica::sync::protocol::RequestContext::default();
    let response = sync_handler.handle_request(&sync_request, &context).await;

    // Verify the request was rejected
    match response {
        eidetica::sync::protocol::SyncResponse::Error(msg) => {
            assert!(
                msg.contains("Tree not found") || msg.contains("not found"),
                "Error should indicate tree not found: {msg}"
            );
        }
        other => {
            panic!("Expected Error response for incremental sync when disabled, got: {other:?}")
        }
    }

    // Clean up
    server_sync.stop_server().await.unwrap();
}

/// Test that sync works normally when enabled (positive test case).
#[tokio::test]
async fn test_sync_succeeds_when_enabled() {
    // Create server with database and sync ENABLED
    let server_instance = helpers::setup_instance_with_initialized().await;
    server_instance
        .create_user("server_user", None)
        .await
        .unwrap();
    let mut server_user = server_instance
        .login_user("server_user", None)
        .await
        .unwrap();
    let server_key_id = server_user
        .add_private_key(Some("server_key"))
        .await
        .unwrap();

    // Create database with wildcard "*" permission to allow unauthenticated sync
    // (We're testing sync-enabled checks, not authentication)
    let mut settings = Doc::new();
    settings.set("name", "test_database");

    let server_pubkey = server_user
        .get_public_key(&server_key_id)
        .expect("Failed to get server public key");

    let mut auth_settings = AuthSettings::new();
    auth_settings
        .add_key(
            &server_key_id,
            AuthKey::active(&server_pubkey, Permission::Admin(0)).unwrap(),
        )
        .unwrap();
    auth_settings
        .add_key("*", AuthKey::active("*", Permission::Read).unwrap())
        .unwrap();
    settings.set("auth", auth_settings.as_doc().clone());

    let server_database = server_user
        .create_database(settings, &server_key_id)
        .await
        .unwrap();
    let tree_id = server_database.root_id().clone();

    // Add test data
    {
        let tx = server_database.new_transaction().await.unwrap();
        let doc_store = tx.get_store::<DocStore>("data").await.unwrap();
        doc_store.set("test_key", "test_value").await.unwrap();
        tx.commit().await.unwrap();
    }

    // Add database with sync ENABLED
    server_user
        .track_database(TrackedDatabase {
            database_id: tree_id.clone(),
            key_id: server_key_id.clone(),
            sync_settings: SyncSettings {
                sync_enabled: true, // Sync is ENABLED
                sync_on_commit: false,
                interval_seconds: None,
                properties: Default::default(),
            },
        })
        .await
        .unwrap();

    let server_sync = server_instance.sync().unwrap();
    server_sync
        .sync_user(
            server_user.user_uuid(),
            server_user.user_database().root_id(),
        )
        .await
        .unwrap();

    // Enable HTTP transport and start server
    server_sync
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    server_sync.accept_connections().await.unwrap();
    let server_addr = server_sync.get_server_address().await.unwrap();

    // Create client and sync
    let (client_instance, client_sync) = helpers::setup().await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    let result = client_sync
        .sync_with_peer(&server_addr, Some(&tree_id))
        .await;

    // Verify sync succeeded
    assert!(result.is_ok(), "Sync should succeed when enabled");

    // Verify data was synced
    // Use global "*" permission (configured with Permission::Read)
    let (reader_key, _) = eidetica::auth::generate_keypair();
    let client_db = Database::open(
        client_instance.clone(),
        &tree_id,
        reader_key,
        "*".to_string(),
    )
    .await
    .unwrap();
    let doc_store = client_db
        .get_store_viewer::<DocStore>("data")
        .await
        .unwrap();
    assert_eq!(
        doc_store.get_string("test_key").await.unwrap(),
        "test_value",
        "Data should be synced to client"
    );

    // Clean up
    server_sync.stop_server().await.unwrap();
}
