//! Tests for the declarative sync API
//!
//! These tests verify the new register_sync_peer() API and that
//! peer relationships are tracked correctly for sync_on_commit.

use eidetica::{Result, sync::SyncPeerInfo};

use super::helpers::setup_instance_with_initialized;

/// Test that register_sync_peer immediately creates the peer/tree relationship.
/// This is critical for sync_on_commit to work - it needs to find peers to push to.
#[test]
fn test_register_sync_peer_tracks_relationship_immediately() -> Result<()> {
    let instance = setup_instance_with_initialized();
    let sync = instance.sync().expect("sync available");

    // Create a test database
    let settings = eidetica::crdt::Doc::new();
    let (signing_key, _) = eidetica::auth::crypto::generate_keypair();
    let db = eidetica::Database::create(settings, &instance, signing_key, "test_key".to_string())?;
    let tree_id = db.root_id().clone();

    // Generate a fake peer pubkey
    let (_, peer_verifying_key) = eidetica::auth::crypto::generate_keypair();
    let peer_pubkey = eidetica::auth::crypto::format_public_key(&peer_verifying_key);

    // Before registering, peer should not exist
    let peers_before = sync.list_peers()?;
    assert_eq!(peers_before.len(), 0, "No peers should exist initially");

    // Register the peer using declarative API
    let _handle = sync.register_sync_peer(SyncPeerInfo {
        peer_pubkey: peer_pubkey.clone(),
        tree_id: tree_id.clone(),
        addresses: vec![eidetica::sync::Address {
            transport_type: "http".to_string(),
            address: "http://test:8080".to_string(),
        }],
        auth: None,
        display_name: Some("Test Peer".to_string()),
    })?;

    // Verify peer exists immediately
    let peers_after = sync.list_peers()?;
    assert_eq!(peers_after.len(), 1, "Peer should be registered");
    assert_eq!(peers_after[0].pubkey, peer_pubkey);
    assert_eq!(peers_after[0].display_name, Some("Test Peer".to_string()));

    // Verify the peer has the address
    assert_eq!(peers_after[0].addresses.len(), 1);
    assert_eq!(peers_after[0].addresses[0].address, "http://test:8080");

    Ok(())
}

/// Test that SyncHandle methods work correctly
#[test]
fn test_sync_handle_methods() -> Result<()> {
    let instance = setup_instance_with_initialized();
    let sync = instance.sync().expect("sync available");

    let settings = eidetica::crdt::Doc::new();
    let (signing_key, _) = eidetica::auth::crypto::generate_keypair();
    let db = eidetica::Database::create(settings, &instance, signing_key, "test_key".to_string())?;
    let tree_id = db.root_id().clone();

    let (_, peer_verifying_key) = eidetica::auth::crypto::generate_keypair();
    let peer_pubkey = eidetica::auth::crypto::format_public_key(&peer_verifying_key);

    let handle = sync.register_sync_peer(SyncPeerInfo {
        peer_pubkey: peer_pubkey.clone(),
        tree_id: tree_id.clone(),
        addresses: vec![],
        auth: None,
        display_name: None,
    })?;

    // Test getter methods
    assert_eq!(handle.tree_id(), &tree_id);
    assert_eq!(handle.peer_pubkey(), &peer_pubkey);

    // Test status - Database::create() creates root entry, so has_local_data should be true
    let status = handle.status()?;
    assert!(
        status.has_local_data,
        "Created database should have root entry"
    );

    Ok(())
}

/// Test that add_address works
#[test]
fn test_sync_handle_add_address() -> Result<()> {
    let instance = setup_instance_with_initialized();
    let sync = instance.sync().expect("sync available");

    let settings = eidetica::crdt::Doc::new();
    let (signing_key, _) = eidetica::auth::crypto::generate_keypair();
    let db = eidetica::Database::create(settings, &instance, signing_key, "test_key".to_string())?;

    let (_, peer_verifying_key) = eidetica::auth::crypto::generate_keypair();
    let peer_pubkey = eidetica::auth::crypto::format_public_key(&peer_verifying_key);

    let handle = sync.register_sync_peer(SyncPeerInfo {
        peer_pubkey: peer_pubkey.clone(),
        tree_id: db.root_id().clone(),
        addresses: vec![eidetica::sync::Address {
            transport_type: "http".to_string(),
            address: "http://primary:8080".to_string(),
        }],
        auth: None,
        display_name: None,
    })?;

    // Verify initial address
    let peer = sync.get_peer_info(&peer_pubkey)?.expect("peer exists");
    assert_eq!(peer.addresses.len(), 1);

    // Add another address
    handle.add_address(eidetica::sync::Address {
        transport_type: "http".to_string(),
        address: "http://backup:8080".to_string(),
    })?;

    // Verify both addresses exist
    let peer = sync.get_peer_info(&peer_pubkey)?.expect("peer exists");
    assert_eq!(peer.addresses.len(), 2);
    assert!(peer.addresses.iter().any(|a| a.address.contains("primary")));
    assert!(peer.addresses.iter().any(|a| a.address.contains("backup")));

    Ok(())
}

/// Test that get_sync_status reports correct state
#[test]
fn test_get_sync_status() -> Result<()> {
    let instance = setup_instance_with_initialized();
    let sync = instance.sync().expect("sync available");

    let settings = eidetica::crdt::Doc::new();
    let (signing_key, _) = eidetica::auth::crypto::generate_keypair();
    let db = eidetica::Database::create(settings, &instance, signing_key, "test_key".to_string())?;

    // Database::create() creates root entry, so should have local data
    let status = sync.get_sync_status(db.root_id(), "fake_peer")?;
    assert!(
        status.has_local_data,
        "Created database should report has_local_data"
    );

    Ok(())
}
