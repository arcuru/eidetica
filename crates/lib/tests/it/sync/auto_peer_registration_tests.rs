//! Tests for automatic peer registration during sync operations.
//!
//! This module tests the automatic peer registration behavior that occurs
//! during handshakes and sync tree requests.

#![allow(deprecated)] // Uses LegacyInstanceOps

use eidetica::{
    auth::crypto::{format_public_key, generate_challenge, generate_keypair},
    crdt::Doc,
    sync::{
        Address, PeerId,
        handler::{SyncHandler, SyncHandlerImpl},
        protocol::{
            HandshakeRequest, PROTOCOL_VERSION, RequestContext, SyncRequest, SyncResponse,
            SyncTreeRequest,
        },
        transports::{SyncTransport, http::HttpTransport},
    },
    user::types::{SyncSettings, TrackedDatabase},
};

use super::helpers::*;
use crate::helpers::setup_empty_db;

/// Test that peers are automatically registered when they send a handshake request
#[tokio::test]
async fn test_handshake_automatically_registers_peer() {
    let (_base_db, sync) = setup().await;
    let instance = sync.instance().expect("Failed to get instance");
    let sync_tree_id = sync.sync_tree_root_id().clone();

    // Create handler
    let handler = SyncHandlerImpl::new(instance, sync_tree_id);

    // Generate a peer key
    let (_, peer_verifying_key) = generate_keypair();
    let peer_pubkey = format_public_key(&peer_verifying_key);

    // Verify peer doesn't exist yet
    assert!(sync.get_peer_info(&peer_pubkey).await.unwrap().is_none());

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
    let peer_info = sync.get_peer_info(&peer_pubkey).await.unwrap();
    assert!(peer_info.is_some());

    let peer_info = peer_info.unwrap();
    assert_eq!(peer_info.id.as_str(), peer_pubkey);
    assert_eq!(peer_info.display_name, Some("Test Peer".to_string()));

    // Verify addresses were added (both advertised and remote)
    let all_addresses = sync.get_peer_addresses(&peer_pubkey, None).await.unwrap();
    assert_eq!(all_addresses.len(), 3); // 2 advertised + 1 remote

    // Check that all expected addresses are present
    for addr in &listen_addresses {
        assert!(
            all_addresses.contains(addr),
            "Missing advertised address: {addr:?}"
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
    let (_base_db, sync) = setup().await;
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
    let peers = sync.list_peers().await.unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].id.as_str(), peer_pubkey);
}

/// Test that handshake works without advertised addresses
#[tokio::test]
async fn test_handshake_without_listen_addresses() {
    let (_base_db, sync) = setup().await;
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
    let peer_info = sync.get_peer_info(&peer_pubkey).await.unwrap().unwrap();
    assert_eq!(peer_info.id.as_str(), peer_pubkey);

    let addresses = sync.get_peer_addresses(&peer_pubkey, None).await.unwrap();
    assert_eq!(addresses.len(), 1);
    assert!(addresses.contains(&remote_address));
}

/// Test that tree/peer relationship is tracked during bootstrap sync
#[tokio::test]
async fn test_bootstrap_sync_tracks_tree_peer_relationship() {
    let instance = setup_empty_db().await;
    instance.enable_sync().await.unwrap();
    instance.create_user("test_user", None).await.unwrap();
    let mut user = instance.login_user("test_user", None).await.unwrap();
    let key_id = user.add_private_key(Some("test_key")).await.unwrap();

    // Create a test database
    let mut settings = Doc::new();
    settings.set("name", "test_database");
    let db = user.create_database(settings, &key_id).await.unwrap();
    let tree_id = db.root_id().clone();

    // Enable sync for this database
    user.track_database(TrackedDatabase {
        database_id: tree_id.clone(),
        key_id,
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .await
    .unwrap();

    let sync = instance.sync().unwrap();
    sync.sync_user(user.user_uuid(), user.user_database().root_id())
        .await
        .unwrap();

    let sync_tree_id = sync.sync_tree_root_id().clone();
    let handler = SyncHandlerImpl::new(instance, sync_tree_id);

    // Generate peer credentials
    let (_, peer_verifying_key) = generate_keypair();
    let peer_pubkey = format_public_key(&peer_verifying_key);

    // Register the peer first (would normally happen during handshake)
    sync.register_peer(&peer_pubkey, Some("Test Peer"))
        .await
        .unwrap();

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
            .await
            .unwrap()
    );

    // Verify peer can be found in tree's peer list
    let tree_peers = sync.get_tree_peers(&tree_id).await.unwrap();
    assert!(tree_peers.contains(&PeerId::new(&peer_pubkey)));

    // Verify tree can be found in peer's tree list
    let peer_trees = sync.get_peer_trees(&peer_pubkey).await.unwrap();
    assert!(peer_trees.contains(&tree_id.to_string()));
}

/// Test that tree/peer relationship is tracked during incremental sync
#[tokio::test]
async fn test_incremental_sync_tracks_tree_peer_relationship() {
    let instance = setup_empty_db().await;
    instance.enable_sync().await.unwrap();
    instance.create_user("test_user", None).await.unwrap();
    let mut user = instance.login_user("test_user", None).await.unwrap();
    let key_id = user.add_private_key(Some("test_key")).await.unwrap();

    // Create a test database with some content
    let mut settings = Doc::new();
    settings.set("name", "test_database");
    let db = user.create_database(settings, &key_id).await.unwrap();
    let tree_id = db.root_id().clone();

    // Enable sync
    user.track_database(TrackedDatabase {
        database_id: tree_id.clone(),
        key_id,
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .await
    .unwrap();

    let sync = instance.sync().unwrap();
    sync.sync_user(user.user_uuid(), user.user_database().root_id())
        .await
        .unwrap();

    // Add an entry to the database
    let tx = db.new_transaction().await.unwrap();
    let store = tx
        .get_store::<eidetica::store::DocStore>("test")
        .await
        .unwrap();
    store
        .set_path(eidetica::crdt::doc::path!("key"), "value")
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let sync_tree_id = sync.sync_tree_root_id().clone();
    let handler = SyncHandlerImpl::new(instance.clone(), sync_tree_id);

    // Generate peer credentials
    let (_, peer_verifying_key) = generate_keypair();
    let peer_pubkey = format_public_key(&peer_verifying_key);

    // Register the peer first (would normally happen during handshake)
    sync.register_peer(&peer_pubkey, Some("Test Peer"))
        .await
        .unwrap();

    // Get current tips for incremental sync
    let tips = instance.backend().get_tips(&tree_id).await.unwrap();

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
            .await
            .unwrap()
    );
}

/// Test that unauthorized keys are rejected when requested_permission is not specified
#[tokio::test]
async fn test_relationship_tracking_skipped_without_peer_pubkey() {
    let instance = setup_empty_db().await;
    instance.enable_sync().await.unwrap();
    instance.create_user("test_user", None).await.unwrap();
    let mut user = instance.login_user("test_user", None).await.unwrap();
    let key_id = user.add_private_key(Some("test_key")).await.unwrap();

    let mut settings = Doc::new();
    settings.set("name", "test_database");
    let db = user.create_database(settings, &key_id).await.unwrap();
    let tree_id = db.root_id().clone();

    user.track_database(TrackedDatabase {
        database_id: tree_id.clone(),
        key_id,
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .await
    .unwrap();

    let sync = instance.sync().unwrap();
    sync.sync_user(user.user_uuid(), user.user_database().root_id())
        .await
        .unwrap();

    let sync_tree_id = sync.sync_tree_root_id().clone();
    let handler = SyncHandlerImpl::new(instance, sync_tree_id);

    let (_, peer_verifying_key) = generate_keypair();
    let peer_pubkey = format_public_key(&peer_verifying_key);

    // Register the peer first (would normally happen during handshake)
    sync.register_peer(&peer_pubkey, Some("Test Peer"))
        .await
        .unwrap();

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

    // Should be rejected because the requesting_key is not authorized for this database
    match response {
        SyncResponse::Error(msg) => {
            assert!(
                msg.contains("not authorized"),
                "Expected authorization error, got: {msg}"
            );
        }
        other => panic!("Expected Error response, got: {other:?}"),
    }

    // Relationship should NOT be tracked (rejected before that point)
    assert!(
        !sync
            .is_tree_synced_with_peer(&peer_pubkey, &tree_id)
            .await
            .unwrap()
    );
}

/// Test that multiple trees can be tracked with the same peer
#[tokio::test]
async fn test_multiple_trees_tracked_with_same_peer() {
    let instance = setup_empty_db().await;
    instance.enable_sync().await.unwrap();
    instance.create_user("test_user", None).await.unwrap();
    let mut user = instance.login_user("test_user", None).await.unwrap();
    let key_id = user.add_private_key(Some("test_key")).await.unwrap();

    // Create two test databases
    let mut settings1 = Doc::new();
    settings1.set("name", "test_database_1");
    let db1 = user.create_database(settings1, &key_id).await.unwrap();
    let tree_id1 = db1.root_id().clone();

    let mut settings2 = Doc::new();
    settings2.set("name", "test_database_2");
    let db2 = user.create_database(settings2, &key_id).await.unwrap();
    let tree_id2 = db2.root_id().clone();

    // Enable sync for both
    user.track_database(TrackedDatabase {
        database_id: tree_id1.clone(),
        key_id: key_id.clone(),
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .await
    .unwrap();

    user.track_database(TrackedDatabase {
        database_id: tree_id2.clone(),
        key_id,
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .await
    .unwrap();

    let sync = instance.sync().unwrap();
    sync.sync_user(user.user_uuid(), user.user_database().root_id())
        .await
        .unwrap();

    let sync_tree_id = sync.sync_tree_root_id().clone();
    let handler = SyncHandlerImpl::new(instance, sync_tree_id);

    let (_, peer_verifying_key) = generate_keypair();
    let peer_pubkey = format_public_key(&peer_verifying_key);

    // Register the peer first (would normally happen during handshake)
    sync.register_peer(&peer_pubkey, Some("Test Peer"))
        .await
        .unwrap();

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
    let peer_trees = sync.get_peer_trees(&peer_pubkey).await.unwrap();
    assert_eq!(peer_trees.len(), 2);
    assert!(peer_trees.contains(&tree_id1.to_string()));
    assert!(peer_trees.contains(&tree_id2.to_string()));
}

/// Test that HTTP transport correctly captures remote address in RequestContext
#[tokio::test]
async fn test_http_transport_request_context() {
    let (_base_db, sync) = setup().await;
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
    let peer_info = sync.get_peer_info(&peer_pubkey).await.unwrap();
    assert!(peer_info.is_some());

    let addresses = sync
        .get_peer_addresses(&peer_pubkey, Some("http"))
        .await
        .unwrap();
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
    let instance = setup_empty_db().await;
    instance.enable_sync().await.unwrap();
    instance.create_user("test_user", None).await.unwrap();
    let mut user = instance.login_user("test_user", None).await.unwrap();
    let key_id = user.add_private_key(Some("test_key")).await.unwrap();

    let mut settings = Doc::new();
    settings.set("name", "test_database");
    let db = user.create_database(settings, &key_id).await.unwrap();
    let tree_id = db.root_id().clone();

    user.track_database(TrackedDatabase {
        database_id: tree_id.clone(),
        key_id,
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .await
    .unwrap();

    let sync = instance.sync().unwrap();
    sync.sync_user(user.user_uuid(), user.user_database().root_id())
        .await
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

/// Test auto-detection of permissions when requested_permission is None - authorized key
#[tokio::test]
async fn test_bootstrap_auto_detects_permission_for_authorized_key() {
    let instance = setup_empty_db().await;
    instance.enable_sync().await.unwrap();
    instance.create_user("test_user", None).await.unwrap();
    let mut user = instance.login_user("test_user", None).await.unwrap();
    let key_id = user.add_private_key(Some("test_key")).await.unwrap();

    // Create database with auth
    let mut settings = Doc::new();
    settings.set("name", "test_database");
    let db = user.create_database(settings, &key_id).await.unwrap();
    let tree_id = db.root_id().clone();

    // Get the user's actual key (the one that's authorized as Admin)
    let user_key_pubkey = user.get_public_key(&key_id).unwrap();

    // Enable sync
    user.track_database(TrackedDatabase {
        database_id: tree_id.clone(),
        key_id: key_id.clone(),
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .await
    .unwrap();

    let sync = instance.sync().unwrap();
    sync.sync_user(user.user_uuid(), user.user_database().root_id())
        .await
        .unwrap();

    let sync_tree_id = sync.sync_tree_root_id().clone();
    let handler = SyncHandlerImpl::new(instance.clone(), sync_tree_id);

    // Bootstrap request with authorized key but no requested_permission
    let sync_request = SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![],
        peer_pubkey: None,
        requesting_key: Some(user_key_pubkey.clone()),
        requesting_key_name: Some(key_id.clone()),
        requested_permission: None, // Should auto-detect from auth settings
    };

    let context = RequestContext {
        remote_address: Some(Address::http("203.0.113.42:54321")),
        peer_pubkey: None,
    };

    let request = SyncRequest::SyncTree(sync_request);
    let response = handler.handle_request(&request, &context).await;

    // Should succeed with auto-detected permission
    match response {
        SyncResponse::Bootstrap(bootstrap_response) => {
            assert_eq!(bootstrap_response.tree_id, tree_id);
            assert!(bootstrap_response.key_approved);
            assert!(bootstrap_response.granted_permission.is_some());
            // Should have detected Admin(0) permission
            assert_eq!(
                bootstrap_response.granted_permission.unwrap(),
                eidetica::auth::Permission::Admin(0)
            );
        }
        other => panic!("Expected Bootstrap response, got: {other:?}"),
    }
}

/// Test that bootstrap rejects unauthorized keys when requested_permission is None
#[tokio::test]
async fn test_bootstrap_rejects_unauthorized_key_when_permission_not_specified() {
    let instance = setup_empty_db().await;
    instance.enable_sync().await.unwrap();
    instance.create_user("test_user", None).await.unwrap();
    let mut user = instance.login_user("test_user", None).await.unwrap();
    let key_id = user.add_private_key(Some("test_key")).await.unwrap();

    // Create database with auth (only user's key is authorized)
    let mut settings = Doc::new();
    settings.set("name", "test_database");
    let db = user.create_database(settings, &key_id).await.unwrap();
    let tree_id = db.root_id().clone();

    // Enable sync
    user.track_database(TrackedDatabase {
        database_id: tree_id.clone(),
        key_id,
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .await
    .unwrap();

    let sync = instance.sync().unwrap();
    sync.sync_user(user.user_uuid(), user.user_database().root_id())
        .await
        .unwrap();

    let sync_tree_id = sync.sync_tree_root_id().clone();
    let handler = SyncHandlerImpl::new(instance, sync_tree_id);

    // Generate an unauthorized key
    let (_, unauthorized_verifying_key) = generate_keypair();
    let unauthorized_pubkey = format_public_key(&unauthorized_verifying_key);

    // Bootstrap request with unauthorized key and no requested_permission
    let sync_request = SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![],
        peer_pubkey: None,
        requesting_key: Some(unauthorized_pubkey),
        requesting_key_name: Some("unauthorized_key".to_string()),
        requested_permission: None,
    };

    let context = RequestContext {
        remote_address: Some(Address::http("203.0.113.42:54321")),
        peer_pubkey: None,
    };

    let request = SyncRequest::SyncTree(sync_request);
    let response = handler.handle_request(&request, &context).await;

    // Should be rejected
    match response {
        SyncResponse::Error(msg) => {
            assert!(
                msg.contains("not authorized"),
                "Expected authorization error, got: {msg}"
            );
        }
        other => panic!("Expected Error response, got: {other:?}"),
    }
}

/// Test auto-detection using global wildcard permission
#[tokio::test]
async fn test_bootstrap_auto_detects_global_wildcard_permission() {
    let instance = setup_empty_db().await;
    instance.enable_sync().await.unwrap();
    instance.create_user("test_user", None).await.unwrap();
    let mut user = instance.login_user("test_user", None).await.unwrap();
    let key_id = user.add_private_key(Some("test_key")).await.unwrap();

    // Create database (user's key will be auto-added as Admin)
    let mut settings = Doc::new();
    settings.set("name", "test_database");
    let db = user.create_database(settings, &key_id).await.unwrap();
    let tree_id = db.root_id().clone();

    // Add global wildcard permission with Read access
    {
        let tx = db.new_transaction().await.unwrap();
        let settings_store = tx.get_settings().unwrap();
        let global_auth_key =
            eidetica::auth::types::AuthKey::active("*", eidetica::auth::Permission::Read).unwrap();
        settings_store
            .set_auth_key("*", global_auth_key)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    // Enable sync
    user.track_database(TrackedDatabase {
        database_id: tree_id.clone(),
        key_id,
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .await
    .unwrap();

    let sync = instance.sync().unwrap();
    sync.sync_user(user.user_uuid(), user.user_database().root_id())
        .await
        .unwrap();

    let sync_tree_id = sync.sync_tree_root_id().clone();
    let handler = SyncHandlerImpl::new(instance, sync_tree_id);

    // Generate a random key (any key should work due to global '*')
    let (_, random_verifying_key) = generate_keypair();
    let random_pubkey = format_public_key(&random_verifying_key);

    // Bootstrap request with random key and no requested_permission
    let sync_request = SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![],
        peer_pubkey: None,
        requesting_key: Some(random_pubkey),
        requesting_key_name: Some("random_key".to_string()),
        requested_permission: None, // Should auto-detect global '*' permission
    };

    let context = RequestContext {
        remote_address: Some(Address::http("203.0.113.42:54321")),
        peer_pubkey: None,
    };

    let request = SyncRequest::SyncTree(sync_request);
    let response = handler.handle_request(&request, &context).await;

    // Should succeed with global wildcard permission
    match response {
        SyncResponse::Bootstrap(bootstrap_response) => {
            assert_eq!(bootstrap_response.tree_id, tree_id);
            assert!(bootstrap_response.key_approved);
            assert_eq!(
                bootstrap_response.granted_permission.unwrap(),
                eidetica::auth::Permission::Read
            );
        }
        other => panic!("Expected Bootstrap response, got: {other:?}"),
    }
}

/// Test that highest permission is used when key has multiple permissions
#[tokio::test]
async fn test_bootstrap_uses_highest_permission_when_key_has_multiple() {
    let instance = setup_empty_db().await;
    instance.enable_sync().await.unwrap();
    instance.create_user("test_user", None).await.unwrap();
    let mut user = instance.login_user("test_user", None).await.unwrap();
    let key_id = user.add_private_key(Some("test_key")).await.unwrap();

    // Generate a key that will have both direct and global permissions
    let (_, special_verifying_key) = generate_keypair();
    let special_pubkey = format_public_key(&special_verifying_key);

    // Create database (user's key will be auto-added as Admin)
    let mut settings = Doc::new();
    settings.set("name", "test_database");
    let db = user.create_database(settings, &key_id).await.unwrap();
    let tree_id = db.root_id().clone();

    // Add both the special key (Write) and global '*' (Read)
    {
        let tx = db.new_transaction().await.unwrap();
        let settings_store = tx.get_settings().unwrap();

        // Add the special key directly with Write(5) permission
        let special_auth_key = eidetica::auth::types::AuthKey::active(
            &special_pubkey,
            eidetica::auth::Permission::Write(5),
        )
        .unwrap();
        settings_store
            .set_auth_key("special_key", special_auth_key)
            .await
            .unwrap();

        // Add global wildcard with Read permission
        let global_auth_key =
            eidetica::auth::types::AuthKey::active("*", eidetica::auth::Permission::Read).unwrap();
        settings_store
            .set_auth_key("*", global_auth_key)
            .await
            .unwrap();

        tx.commit().await.unwrap();
    }

    // Enable sync
    user.track_database(TrackedDatabase {
        database_id: tree_id.clone(),
        key_id,
        sync_settings: SyncSettings {
            sync_enabled: true,
            sync_on_commit: false,
            interval_seconds: None,
            properties: Default::default(),
        },
    })
    .await
    .unwrap();

    let sync = instance.sync().unwrap();
    sync.sync_user(user.user_uuid(), user.user_database().root_id())
        .await
        .unwrap();

    let sync_tree_id = sync.sync_tree_root_id().clone();
    let handler = SyncHandlerImpl::new(instance, sync_tree_id);

    // Bootstrap with the special key (has both Write(5) and Read via global)
    let sync_request = SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![],
        peer_pubkey: None,
        requesting_key: Some(special_pubkey),
        requesting_key_name: Some("special_key".to_string()),
        requested_permission: None, // Should auto-detect highest (Write(5))
    };

    let context = RequestContext {
        remote_address: Some(Address::http("203.0.113.42:54321")),
        peer_pubkey: None,
    };

    let request = SyncRequest::SyncTree(sync_request);
    let response = handler.handle_request(&request, &context).await;

    // Should succeed with highest permission (Write(5), not Read)
    match response {
        SyncResponse::Bootstrap(bootstrap_response) => {
            assert_eq!(bootstrap_response.tree_id, tree_id);
            assert!(bootstrap_response.key_approved);
            assert_eq!(
                bootstrap_response.granted_permission.unwrap(),
                eidetica::auth::Permission::Write(5)
            );
        }
        other => panic!("Expected Bootstrap response, got: {other:?}"),
    }
}
