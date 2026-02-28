//! Test that verifies automatic peer tracking when syncing.
//!
//! This test demonstrates that when a peer syncs a tree with the server,
//! the server should automatically track that the peer is interested in that tree
//! WITHOUT requiring manual add_tree_sync() calls.

use std::sync::Arc;

use eidetica::{
    auth::{AuthKey, Permission},
    crdt::Doc,
    sync::{
        Address,
        transports::{SyncTransport, http::HttpTransport},
    },
    user::types::SyncSettings,
};

use super::helpers::*;
use crate::helpers::{add_auth_key, setup_empty_db};

/// Test automatic peer tracking: when client syncs with server, server should
/// automatically track the tree/peer relationship WITHOUT manual setup.
#[tokio::test]
async fn test_server_automatically_tracks_peers_that_sync_trees() {
    // ===== Setup Server =====
    let server_instance = setup_empty_db().await;
    server_instance.enable_sync().await.unwrap();
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

    let server_sync = server_instance.sync().unwrap();

    // Create a database with wildcard "*" permission to allow unauthenticated sync
    let mut db_settings = Doc::new();
    db_settings.set("name", "test_database");

    let server_db = server_user
        .create_database(db_settings, &server_key_id)
        .await
        .unwrap();

    // Add wildcard "*" permission
    add_auth_key(
        &server_db,
        "*",
        AuthKey::active(Some("*"), Permission::Read),
    )
    .await;

    let tree_id = server_db.root_id().clone();

    // Enable sync for this database
    server_user
        .track_database(
            tree_id.clone(),
            server_key_id.clone(),
            SyncSettings::enabled(),
        )
        .await
        .unwrap();

    // Update sync configuration
    server_sync
        .sync_user(
            server_user.user_uuid(),
            server_user.user_database().root_id(),
        )
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
    let client_pubkey = client_sync.get_device_id().unwrap();

    // IMPORTANT: Server needs to know about the client peer for tracking to work
    // (This would normally happen via handshake, but we're testing just the sync request)
    server_sync
        .register_peer(&client_pubkey, Some("Test Client"))
        .await
        .unwrap();

    // Start server
    let mut http_transport = HttpTransport::builder()
        .bind("127.0.0.1:0")
        .build_sync()
        .unwrap();
    let handler = Arc::new(create_test_sync_handler(&server_sync));
    http_transport.start_server(handler).await.unwrap();
    let server_addr = http_transport.get_server_address().unwrap();

    // Register server as a peer and add its address
    let server_pubkey = server_sync.get_device_id().unwrap();
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

    // ===== THE KEY TEST: Client syncs a tree with the server =====
    // Client sets up its side of the relationship (this is normal/expected)
    client_sync
        .add_tree_sync(&server_pubkey, &tree_id)
        .await
        .unwrap();

    // Now client syncs - this will send a SyncTreeRequest to the server
    // Server automatically tracks when it sees the sync request
    client_sync
        .sync_tree_with_peer(&server_pubkey, &tree_id)
        .await
        .unwrap();

    // ===== VERIFICATION: Server should have automatically tracked this relationship =====

    let peer_trees = server_sync.get_peer_trees(&client_pubkey).await.unwrap();

    assert!(
        peer_trees.contains(&tree_id.to_string()),
        "AUTOMATIC TRACKING FAILED: Server did not automatically track that peer {client_pubkey} is syncing tree {tree_id}.\n\
         This means the server received a sync request from the client but did not record the relationship.\n\
         Expected: Server tracks the relationship automatically (no manual add_tree_sync needed)\n\
         Actual: Server has no record of this peer syncing this tree.\n\
         Peer trees found: {peer_trees:?}"
    );

    // Verify bidirectional: client should also know about this relationship
    assert!(
        client_sync
            .is_tree_synced_with_peer(&server_pubkey, &tree_id)
            .await
            .unwrap(),
        "Client should have tracked that it's syncing tree {tree_id} with server {server_pubkey}"
    );

    // Clean up
    http_transport.stop_server().await.unwrap();
}
