//! HTTP transport checks for the boundary between pulls and persistent routes.

use std::sync::Arc;

use eidetica::{
    Database,
    account::types::SyncSettings,
    auth::{AuthKey, Permission},
    crdt::Doc,
    sync::{
        Address,
        transports::{SyncTransport, http::HttpTransport},
    },
};

use super::helpers::*;
use crate::helpers::{set_global_auth_key, test_local_instance as setup_empty_db};

/// A client can pull a public tree over HTTP without creating a reverse route.
#[tokio::test]
async fn test_http_pull_does_not_register_a_reverse_route() {
    // ===== Setup Server =====
    let server_instance = setup_empty_db().await;
    server_instance.enable_sync().await.unwrap();
    crate::helpers::create_account(&server_instance, "server_user", "")
        .await
        .unwrap();
    let mut server_user = server_instance
        .login_account("server_user", "")
        .await
        .unwrap();
    let server_key_id = server_user
        .add_private_key(Some("server_key"))
        .await
        .unwrap();

    let server_sync = server_instance.sync().unwrap();

    // Create a database with global permission to allow unauthenticated sync
    let mut db_settings = Doc::new();
    db_settings.set("name", "test_database");

    let server_db = server_user
        .create_database(db_settings, &server_key_id)
        .await
        .unwrap();

    // Add global permission
    set_global_auth_key(&server_db, AuthKey::active(None, Permission::Read)).await;

    let tree_id = server_db.root_id().clone();

    // Enable sync for this database
    server_user
        .track_database(tree_id.clone(), &server_key_id, SyncSettings::enabled())
        .await
        .unwrap();

    // Update sync configuration
    server_sync
        .sync_account(server_user.login_id(), server_user.account().id())
        .await
        .unwrap();

    // ===== Setup Client =====
    let client_instance = setup_empty_db().await;
    client_instance.enable_sync().await.unwrap();

    let client_sync = client_instance.sync().unwrap();

    // Enable HTTP transport on client
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Get client's device public key
    let client_pubkey = client_sync.get_device_pubkey().unwrap();

    // A known device is not necessarily a push recipient for this tree.
    server_sync
        .register_peer(&client_pubkey, Some("Test Client"))
        .await
        .unwrap();

    // Start server
    let http_transport = HttpTransport::builder()
        .bind("127.0.0.1:0")
        .build_sync()
        .unwrap();
    let handler = Arc::new(create_test_sync_handler(&server_sync));
    http_transport.start_server(handler).await.unwrap();
    let server_addr = http_transport.get_server_address().unwrap();

    // Register server as a peer and add its address
    let server_pubkey = server_sync.get_device_pubkey().unwrap();
    client_sync
        .register_peer(&server_pubkey, Some("Test Server"))
        .await
        .unwrap();
    client_sync
        .add_peer_address(
            &server_pubkey,
            Address {
                transport_type: "http".to_string(),
                address: server_addr,
            },
        )
        .await
        .unwrap();

    // The client configures its side of the relationship and pulls the tree.
    client_sync
        .add_tree_sync(&server_pubkey, &tree_id)
        .await
        .unwrap();

    client_sync
        .sync_tree_with_peer(&server_pubkey, &tree_id)
        .await
        .unwrap();

    let pulled = Database::open(&client_instance, &tree_id).await.unwrap();
    assert_eq!(
        pulled.snapshot().await.unwrap(),
        server_db.snapshot().await.unwrap()
    );
    assert!(
        !server_sync
            .is_tree_synced_with_peer(&client_pubkey, &tree_id)
            .await
            .unwrap(),
        "a public HTTP pull must not configure future pushes"
    );

    // Clean up
    http_transport.stop_server().await.unwrap();
}
