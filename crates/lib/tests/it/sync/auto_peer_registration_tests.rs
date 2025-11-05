//! Tests for automatic peer registration during sync operations.
//!
//! This module tests the automatic peer registration behavior that occurs
//! during handshakes and sync tree requests.

use eidetica::{
    auth::crypto::{format_public_key, generate_challenge, generate_keypair},
    crdt::Doc,
    sync::{
        Address,
        handler::{SyncHandler, SyncHandlerImpl},
        protocol::{
            HandshakeRequest, PROTOCOL_VERSION, RequestContext, SyncRequest, SyncResponse,
            SyncTreeRequest,
        },
        transports::{SyncTransport, http::HttpTransport},
    },
    user::types::{DatabasePreferences, SyncSettings},
};

use super::helpers::*;
use crate::helpers::setup_empty_db;

/// Test that peers are automatically registered when they send a handshake request
#[tokio::test]
async fn test_handshake_automatically_registers_peer() {
    let (_base_db, sync) = setup();
    let instance = sync.instance().expect("Failed to get instance");
    let sync_tree_id = sync.sync_tree_root_id().clone();

    // Create handler
    let handler = SyncHandlerImpl::new(instance, sync_tree_id);

    // Generate a peer key
    let (_, peer_verifying_key) = generate_keypair();
    let peer_pubkey = format_public_key(&peer_verifying_key);

    // Verify peer doesn't exist yet
    assert!(sync.get_peer_info(&peer_pubkey).unwrap().is_none());

    // Create handshake request with listen addresses
    let listen_addresses = vec![
        Address::http("192.168.1.100:8080"),
        Address::iroh("peer_node_id_123"),
    ];

    let handshake_request = HandshakeRequest {
        device_id: peer_pubkey.clone(),
        public_key: peer_pubkey.clone(),
        display_name: Some("Test Peer".to_string()),
        protocol_version: PROTOCOL_VERSION,
        challenge: generate_challenge(),
        listen_addresses: listen_addresses.clone(),
    };

    // Create request context with remote address
    let remote_address = Address::http("203.0.113.42:54321");
    let context = RequestContext {
        remote_address: Some(remote_address.clone()),
        peer_pubkey: None,
    };

    // Send handshake
    let request = SyncRequest::Handshake(handshake_request);
    let response = handler.handle_request(&request, &context).await;

    // Verify we got a successful handshake response
    assert!(matches!(response, SyncResponse::Handshake(_)));

    // Verify peer was automatically registered
    let peer_info = sync.get_peer_info(&peer_pubkey).unwrap();
    assert!(peer_info.is_some());

    let peer_info = peer_info.unwrap();
    assert_eq!(peer_info.pubkey, peer_pubkey);
    assert_eq!(peer_info.display_name, Some("Test Peer".to_string()));

    // Verify addresses were added (both advertised and remote)
    let all_addresses = sync.get_peer_addresses(&peer_pubkey, None).unwrap();
    assert_eq!(all_addresses.len(), 3); // 2 advertised + 1 remote

    // Check that all expected addresses are present
    for addr in &listen_addresses {
        assert!(
            all_addresses.contains(addr),
            "Missing advertised address: {:?}",
            addr
        );
    }
    assert!(
        all_addresses.contains(&remote_address),
        "Missing remote address"
    );
}

/// Test that duplicate handshakes don't cause errors
#[tokio::test]
async fn test_duplicate_handshakes_handled_gracefully() {
    let (_base_db, sync) = setup();
    let instance = sync.instance().expect("Failed to get instance");
    let sync_tree_id = sync.sync_tree_root_id().clone();

    let handler = SyncHandlerImpl::new(instance, sync_tree_id);

    let (_, peer_verifying_key) = generate_keypair();
    let peer_pubkey = format_public_key(&peer_verifying_key);

    let handshake_request = HandshakeRequest {
        device_id: peer_pubkey.clone(),
        public_key: peer_pubkey.clone(),
        display_name: Some("Test Peer".to_string()),
        protocol_version: PROTOCOL_VERSION,
        challenge: generate_challenge(),
        listen_addresses: vec![Address::http("192.168.1.100:8080")],
    };

    let context = RequestContext {
        remote_address: Some(Address::http("203.0.113.42:54321")),
        peer_pubkey: None,
    };

    // Send first handshake
    let request = SyncRequest::Handshake(handshake_request.clone());
    let response1 = handler.handle_request(&request, &context).await;
    assert!(matches!(response1, SyncResponse::Handshake(_)));

    // Send second identical handshake - should not fail
    let response2 = handler.handle_request(&request, &context).await;
    assert!(matches!(response2, SyncResponse::Handshake(_)));

    // Peer should still exist and be registered only once
    let peers = sync.list_peers().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].pubkey, peer_pubkey);
}

/// Test that handshake works without advertised addresses
#[tokio::test]
async fn test_handshake_without_listen_addresses() {
    let (_base_db, sync) = setup();
    let instance = sync.instance().expect("Failed to get instance");
    let sync_tree_id = sync.sync_tree_root_id().clone();

    let handler = SyncHandlerImpl::new(instance, sync_tree_id);

    let (_, peer_verifying_key) = generate_keypair();
    let peer_pubkey = format_public_key(&peer_verifying_key);

    // Handshake with empty listen_addresses
    let handshake_request = HandshakeRequest {
        device_id: peer_pubkey.clone(),
        public_key: peer_pubkey.clone(),
        display_name: Some("Test Peer".to_string()),
        protocol_version: PROTOCOL_VERSION,
        challenge: generate_challenge(),
        listen_addresses: vec![],
    };

    let remote_address = Address::http("203.0.113.42:54321");
    let context = RequestContext {
        remote_address: Some(remote_address.clone()),
        peer_pubkey: None,
    };

    let request = SyncRequest::Handshake(handshake_request);
    let response = handler.handle_request(&request, &context).await;

    assert!(matches!(response, SyncResponse::Handshake(_)));

    // Peer should still be registered with just the remote address
    let peer_info = sync.get_peer_info(&peer_pubkey).unwrap().unwrap();
    assert_eq!(peer_info.pubkey, peer_pubkey);

    let addresses = sync.get_peer_addresses(&peer_pubkey, None).unwrap();
    assert_eq!(addresses.len(), 1);
    assert!(addresses.contains(&remote_address));
}

/// Test that tree/peer relationship is tracked during bootstrap sync
#[tokio::test]
async fn test_bootstrap_sync_tracks_tree_peer_relationship() {
    let instance = setup_empty_db();
    instance.enable_sync().unwrap();
    instance.create_user("test_user", None).unwrap();
    let mut user = instance.login_user("test_user", None).unwrap();
    let key_id = user.add_private_key(Some("test_key")).unwrap();

    // Create a test database
    let mut settings = Doc::new();
    settings.set_string("name", "test_database");
    let db = user.create_database(settings, &key_id).unwrap();
    let tree_id = db.root_id().clone();

    // Enable sync for this database
    user.add_database(DatabasePreferences {
        database_id: tree_id.clone(),
        key_id,
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .unwrap();

    let sync = instance.sync().unwrap();
    sync.sync_user(user.user_uuid(), user.user_database().root_id())
        .unwrap();

    let sync_tree_id = sync.sync_tree_root_id().clone();
    let handler = SyncHandlerImpl::new(instance, sync_tree_id);

    // Generate peer credentials
    let (_, peer_verifying_key) = generate_keypair();
    let peer_pubkey = format_public_key(&peer_verifying_key);

    // Register the peer first (would normally happen during handshake)
    sync.register_peer(&peer_pubkey, Some("Test Peer")).unwrap();

    // Create bootstrap request (empty tips)
    let sync_request = SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![], // Empty tips = bootstrap
        peer_pubkey: None,
        requesting_key: Some(peer_pubkey.clone()),
        requesting_key_name: Some("peer_key".to_string()),
        requested_permission: None,
    };

    let context = RequestContext {
        remote_address: Some(Address::http("203.0.113.42:54321")),
        peer_pubkey: Some(peer_pubkey.clone()),
    };

    let request = SyncRequest::SyncTree(sync_request);
    let _response = handler.handle_request(&request, &context).await;

    // Verify tree/peer relationship was tracked
    assert!(
        sync.is_tree_synced_with_peer(&peer_pubkey, &tree_id)
            .unwrap()
    );

    // Verify peer can be found in tree's peer list
    let tree_peers = sync.get_tree_peers(&tree_id).unwrap();
    assert!(tree_peers.contains(&peer_pubkey));

    // Verify tree can be found in peer's tree list
    let peer_trees = sync.get_peer_trees(&peer_pubkey).unwrap();
    assert!(peer_trees.contains(&tree_id.to_string()));
}

/// Test that tree/peer relationship is tracked during incremental sync
#[tokio::test]
async fn test_incremental_sync_tracks_tree_peer_relationship() {
    let instance = setup_empty_db();
    instance.enable_sync().unwrap();
    instance.create_user("test_user", None).unwrap();
    let mut user = instance.login_user("test_user", None).unwrap();
    let key_id = user.add_private_key(Some("test_key")).unwrap();

    // Create a test database with some content
    let mut settings = Doc::new();
    settings.set_string("name", "test_database");
    let db = user.create_database(settings, &key_id).unwrap();
    let tree_id = db.root_id().clone();

    // Enable sync
    user.add_database(DatabasePreferences {
        database_id: tree_id.clone(),
        key_id,
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .unwrap();

    let sync = instance.sync().unwrap();
    sync.sync_user(user.user_uuid(), user.user_database().root_id())
        .unwrap();

    // Add an entry to the database
    let tx = db.new_transaction().unwrap();
    let store = tx.get_store::<eidetica::store::DocStore>("test").unwrap();
    store
        .set_path(eidetica::crdt::doc::path!("key"), "value")
        .unwrap();
    tx.commit().unwrap();

    let sync_tree_id = sync.sync_tree_root_id().clone();
    let handler = SyncHandlerImpl::new(instance.clone(), sync_tree_id);

    // Generate peer credentials
    let (_, peer_verifying_key) = generate_keypair();
    let peer_pubkey = format_public_key(&peer_verifying_key);

    // Register the peer first (would normally happen during handshake)
    sync.register_peer(&peer_pubkey, Some("Test Peer")).unwrap();

    // Get current tips for incremental sync
    let tips = instance.backend().get_tips(&tree_id).unwrap();

    // Create incremental sync request (non-empty tips)
    let sync_request = SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: tips, // Non-empty tips = incremental
        peer_pubkey: None,
        requesting_key: Some(peer_pubkey.clone()),
        requesting_key_name: Some("peer_key".to_string()),
        requested_permission: None,
    };

    let context = RequestContext {
        remote_address: Some(Address::http("203.0.113.42:54321")),
        peer_pubkey: Some(peer_pubkey.clone()),
    };

    let request = SyncRequest::SyncTree(sync_request);
    let _response = handler.handle_request(&request, &context).await;

    // Verify tree/peer relationship was tracked
    assert!(
        sync.is_tree_synced_with_peer(&peer_pubkey, &tree_id)
            .unwrap()
    );
}

/// Test that relationship tracking is gracefully skipped when peer_pubkey is not available
#[tokio::test]
async fn test_relationship_tracking_skipped_without_peer_pubkey() {
    let instance = setup_empty_db();
    instance.enable_sync().unwrap();
    instance.create_user("test_user", None).unwrap();
    let mut user = instance.login_user("test_user", None).unwrap();
    let key_id = user.add_private_key(Some("test_key")).unwrap();

    let mut settings = Doc::new();
    settings.set_string("name", "test_database");
    let db = user.create_database(settings, &key_id).unwrap();
    let tree_id = db.root_id().clone();

    user.add_database(DatabasePreferences {
        database_id: tree_id.clone(),
        key_id,
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .unwrap();

    let sync = instance.sync().unwrap();
    sync.sync_user(user.user_uuid(), user.user_database().root_id())
        .unwrap();

    let sync_tree_id = sync.sync_tree_root_id().clone();
    let handler = SyncHandlerImpl::new(instance, sync_tree_id);

    let (_, peer_verifying_key) = generate_keypair();
    let peer_pubkey = format_public_key(&peer_verifying_key);

    // Register the peer first (would normally happen during handshake)
    sync.register_peer(&peer_pubkey, Some("Test Peer")).unwrap();

    let sync_request = SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![],
        peer_pubkey: None,
        requesting_key: Some(peer_pubkey.clone()),
        requesting_key_name: Some("peer_key".to_string()),
        requested_permission: None,
    };

    // Context without peer_pubkey - tracking should be skipped
    let context = RequestContext {
        remote_address: Some(Address::http("203.0.113.42:54321")),
        peer_pubkey: None,
    };

    let request = SyncRequest::SyncTree(sync_request);
    let response = handler.handle_request(&request, &context).await;

    // Should still get a valid response (sync succeeds)
    assert!(matches!(response, SyncResponse::Bootstrap(_)));

    // But relationship should NOT be tracked (because we don't use requesting_key)
    assert!(
        !sync
            .is_tree_synced_with_peer(&peer_pubkey, &tree_id)
            .unwrap()
    );
}

/// Test that multiple trees can be tracked with the same peer
#[tokio::test]
async fn test_multiple_trees_tracked_with_same_peer() {
    let instance = setup_empty_db();
    instance.enable_sync().unwrap();
    instance.create_user("test_user", None).unwrap();
    let mut user = instance.login_user("test_user", None).unwrap();
    let key_id = user.add_private_key(Some("test_key")).unwrap();

    // Create two test databases
    let mut settings1 = Doc::new();
    settings1.set_string("name", "test_database_1");
    let db1 = user.create_database(settings1, &key_id).unwrap();
    let tree_id1 = db1.root_id().clone();

    let mut settings2 = Doc::new();
    settings2.set_string("name", "test_database_2");
    let db2 = user.create_database(settings2, &key_id).unwrap();
    let tree_id2 = db2.root_id().clone();

    // Enable sync for both
    user.add_database(DatabasePreferences {
        database_id: tree_id1.clone(),
        key_id: key_id.clone(),
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .unwrap();

    user.add_database(DatabasePreferences {
        database_id: tree_id2.clone(),
        key_id,
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .unwrap();

    let sync = instance.sync().unwrap();
    sync.sync_user(user.user_uuid(), user.user_database().root_id())
        .unwrap();

    let sync_tree_id = sync.sync_tree_root_id().clone();
    let handler = SyncHandlerImpl::new(instance, sync_tree_id);

    let (_, peer_verifying_key) = generate_keypair();
    let peer_pubkey = format_public_key(&peer_verifying_key);

    // Register the peer first (would normally happen during handshake)
    sync.register_peer(&peer_pubkey, Some("Test Peer")).unwrap();

    let context = RequestContext {
        remote_address: Some(Address::http("203.0.113.42:54321")),
        peer_pubkey: Some(peer_pubkey.clone()),
    };

    // Request first tree
    let request1 = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id1.clone(),
        our_tips: vec![],
        peer_pubkey: None,
        requesting_key: Some(peer_pubkey.clone()),
        requesting_key_name: Some("peer_key".to_string()),
        requested_permission: None,
    });
    let _response1 = handler.handle_request(&request1, &context).await;

    // Request second tree
    let request2 = SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id2.clone(),
        our_tips: vec![],
        peer_pubkey: None,
        requesting_key: Some(peer_pubkey.clone()),
        requesting_key_name: Some("peer_key".to_string()),
        requested_permission: None,
    });
    let _response2 = handler.handle_request(&request2, &context).await;

    // Verify both trees are tracked
    let peer_trees = sync.get_peer_trees(&peer_pubkey).unwrap();
    assert_eq!(peer_trees.len(), 2);
    assert!(peer_trees.contains(&tree_id1.to_string()));
    assert!(peer_trees.contains(&tree_id2.to_string()));
}

/// Test that HTTP transport correctly captures remote address in RequestContext
#[tokio::test]
async fn test_http_transport_request_context() {
    let (_base_db, sync) = setup();
    let instance = sync.instance().expect("Failed to get instance");
    let sync_tree_id = sync.sync_tree_root_id().clone();

    // Create handler
    let handler = std::sync::Arc::new(SyncHandlerImpl::new(instance, sync_tree_id));

    // Start HTTP server
    let mut transport = HttpTransport::new().unwrap();
    transport
        .start_server("127.0.0.1:0", handler.clone())
        .await
        .unwrap();

    let server_addr = transport.get_server_address().unwrap();

    // Generate peer credentials
    let (_peer_signing_key, peer_verifying_key) = generate_keypair();
    let peer_pubkey = format_public_key(&peer_verifying_key);

    // Create handshake request
    let challenge = generate_challenge();
    let handshake_request = HandshakeRequest {
        device_id: peer_pubkey.clone(),
        public_key: peer_pubkey.clone(),
        display_name: Some("HTTP Test Peer".to_string()),
        protocol_version: PROTOCOL_VERSION,
        challenge: challenge.clone(),
        listen_addresses: vec![],
    };

    // Send handshake via HTTP transport
    let server_address = Address::http(&server_addr);
    let request = SyncRequest::Handshake(handshake_request);
    let response = transport
        .send_request(&server_address, &request)
        .await
        .unwrap();

    // Verify handshake succeeded
    assert!(matches!(response, SyncResponse::Handshake(_)));

    // Verify peer was registered with an HTTP address
    let peer_info = sync.get_peer_info(&peer_pubkey).unwrap();
    assert!(peer_info.is_some());

    let addresses = sync.get_peer_addresses(&peer_pubkey, Some("http")).unwrap();
    assert!(
        !addresses.is_empty(),
        "Peer should have at least one HTTP address"
    );

    // Cleanup
    transport.stop_server().await.unwrap();
}

/// Test that sync requests fail without peer identifiers
#[tokio::test]
async fn test_sync_without_peer_identifier_works() {
    let instance = setup_empty_db();
    instance.enable_sync().unwrap();
    instance.create_user("test_user", None).unwrap();
    let mut user = instance.login_user("test_user", None).unwrap();
    let key_id = user.add_private_key(Some("test_key")).unwrap();

    let mut settings = Doc::new();
    settings.set_string("name", "test_database");
    let db = user.create_database(settings, &key_id).unwrap();
    let tree_id = db.root_id().clone();

    user.add_database(DatabasePreferences {
        database_id: tree_id.clone(),
        key_id,
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .unwrap();

    let sync = instance.sync().unwrap();
    sync.sync_user(user.user_uuid(), user.user_database().root_id())
        .unwrap();

    let sync_tree_id = sync.sync_tree_root_id().clone();
    let handler = SyncHandlerImpl::new(instance, sync_tree_id);

    // Sync request without any peer identifier
    let sync_request = SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![],
        peer_pubkey: None,
        requesting_key: None,
        requesting_key_name: None,
        requested_permission: None,
    };

    // Context also without peer_pubkey
    let context = RequestContext {
        remote_address: Some(Address::http("203.0.113.42:54321")),
        peer_pubkey: None,
    };

    let request = SyncRequest::SyncTree(sync_request);
    let response = handler.handle_request(&request, &context).await;

    assert!(matches!(response, SyncResponse::Error(_)));
}
