//! Tests for peer management functionality in the sync module.

use eidetica::{
    auth::crypto::{format_public_key, generate_keypair},
    sync::{Address, PeerStatus},
};

use super::helpers::*;

// Generate valid test keys using lazy_static to ensure they're generated once
use std::sync::LazyLock;

static TEST_PEER_PUBKEY: LazyLock<String> = LazyLock::new(|| {
    let (_, verifying_key) = generate_keypair();
    format_public_key(&verifying_key)
});

static TEST_PEER_PUBKEY_2: LazyLock<String> = LazyLock::new(|| {
    let (_, verifying_key) = generate_keypair();
    format_public_key(&verifying_key)
});
const TEST_TREE_ROOT_ID: &str = "tree_root_id_123";
const TEST_TREE_ROOT_ID_2: &str = "tree_root_id_456";

#[test]
fn test_peer_registration() {
    let (_base_db, mut sync) = setup();

    // Register a peer
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .unwrap();

    // Verify peer was registered
    let peer_info = sync.get_peer_info(&*TEST_PEER_PUBKEY).unwrap();
    assert!(peer_info.is_some());

    let peer_info = peer_info.unwrap();
    assert_eq!(peer_info.pubkey, *TEST_PEER_PUBKEY);
    assert_eq!(peer_info.display_name, Some("Test Peer".to_string()));
    assert_eq!(peer_info.status, PeerStatus::Active);
}

#[test]
fn test_peer_registration_without_display_name() {
    let (_base_db, mut sync) = setup();

    // Register a peer without display name
    sync.register_peer(&*TEST_PEER_PUBKEY, None).unwrap();

    // Verify peer was registered
    let peer_info = sync.get_peer_info(&*TEST_PEER_PUBKEY).unwrap().unwrap();
    assert_eq!(peer_info.pubkey, *TEST_PEER_PUBKEY);
    assert_eq!(peer_info.display_name, None);
    assert_eq!(peer_info.status, PeerStatus::Active);
}

#[test]
fn test_update_peer_status() {
    let (_base_db, mut sync) = setup();

    // Register and then update peer status
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .unwrap();
    sync.update_peer_status(&*TEST_PEER_PUBKEY, PeerStatus::Inactive)
        .unwrap();

    // Verify status was updated
    let peer_info = sync.get_peer_info(&*TEST_PEER_PUBKEY).unwrap().unwrap();
    assert_eq!(peer_info.status, PeerStatus::Inactive);
}

#[test]
fn test_update_nonexistent_peer_status() {
    let (_base_db, mut sync) = setup();

    // Try to update status of non-existent peer
    let result = sync.update_peer_status(&*TEST_PEER_PUBKEY, PeerStatus::Blocked);
    assert!(result.is_err());
    assert!(result.unwrap_err().is_not_found());
}

#[test]
fn test_list_peers() {
    let (_base_db, mut sync) = setup();

    // Initially no peers
    let peers = sync.list_peers().unwrap();
    assert!(peers.is_empty());

    // Register multiple peers
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Peer 1"))
        .unwrap();
    sync.register_peer(&*TEST_PEER_PUBKEY_2, Some("Peer 2"))
        .unwrap();

    // Verify both peers are listed
    let peers = sync.list_peers().unwrap();
    assert_eq!(peers.len(), 2);

    let pubkeys: Vec<String> = peers.iter().map(|d| d.pubkey.clone()).collect();
    assert!(pubkeys.contains(&*TEST_PEER_PUBKEY));
    assert!(pubkeys.contains(&*TEST_PEER_PUBKEY_2));
}

#[test]
fn test_remove_peer() {
    let (_base_db, mut sync) = setup();

    // Register a peer
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .unwrap();

    // Verify peer exists
    assert!(sync.get_peer_info(&*TEST_PEER_PUBKEY).unwrap().is_some());

    // Remove peer
    sync.remove_peer(&*TEST_PEER_PUBKEY).unwrap();

    // Verify peer was removed
    assert!(sync.get_peer_info(&*TEST_PEER_PUBKEY).unwrap().is_none());
}

#[test]
fn test_add_tree_sync() {
    let (_base_db, mut sync) = setup();

    // Register a peer first
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .unwrap();

    // Add synced tree
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .unwrap();

    // Verify tree was added
    let synced_trees = sync.get_peer_trees(&*TEST_PEER_PUBKEY).unwrap();
    assert_eq!(synced_trees.len(), 1);
    assert!(synced_trees.contains(&TEST_TREE_ROOT_ID.to_string()));
}

#[test]
fn test_add_multiple_synced_trees() {
    let (_base_db, mut sync) = setup();

    // Register a peer first
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .unwrap();

    // Add multiple synced trees
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .unwrap();
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID_2)
        .unwrap();

    // Verify both trees were added
    let synced_trees = sync.get_peer_trees(&*TEST_PEER_PUBKEY).unwrap();
    assert_eq!(synced_trees.len(), 2);
    assert!(synced_trees.contains(&TEST_TREE_ROOT_ID.to_string()));
    assert!(synced_trees.contains(&TEST_TREE_ROOT_ID_2.to_string()));
}

#[test]
fn test_add_duplicate_synced_tree() {
    let (_base_db, mut sync) = setup();

    // Register a peer first
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .unwrap();

    // Add same tree twice
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .unwrap();
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .unwrap();

    // Verify tree is only listed once
    let synced_trees = sync.get_peer_trees(&*TEST_PEER_PUBKEY).unwrap();
    assert_eq!(synced_trees.len(), 1);
    assert!(synced_trees.contains(&TEST_TREE_ROOT_ID.to_string()));
}

#[test]
fn test_remove_tree_sync() {
    let (_base_db, mut sync) = setup();

    // Register a peer and add trees
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .unwrap();
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .unwrap();
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID_2)
        .unwrap();

    // Remove one tree
    sync.remove_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .unwrap();

    // Verify only one tree remains
    let synced_trees = sync.get_peer_trees(&*TEST_PEER_PUBKEY).unwrap();
    assert_eq!(synced_trees.len(), 1);
    assert!(synced_trees.contains(&TEST_TREE_ROOT_ID_2.to_string()));
    assert!(!synced_trees.contains(&TEST_TREE_ROOT_ID.to_string()));
}

#[test]
fn test_get_tree_peers() {
    let (_base_db, mut sync) = setup();

    // Register multiple peers
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Peer 1"))
        .unwrap();
    sync.register_peer(&*TEST_PEER_PUBKEY_2, Some("Peer 2"))
        .unwrap();

    // Add same tree to both peers
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .unwrap();
    sync.add_tree_sync(&*TEST_PEER_PUBKEY_2, TEST_TREE_ROOT_ID)
        .unwrap();

    // Add different tree to one peer
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID_2)
        .unwrap();

    // Verify peers for first tree
    let peers = sync.get_tree_peers(TEST_TREE_ROOT_ID).unwrap();
    assert_eq!(peers.len(), 2);
    assert!(peers.contains(&*TEST_PEER_PUBKEY));
    assert!(peers.contains(&*TEST_PEER_PUBKEY_2));

    // Verify peers for second tree
    let peers = sync.get_tree_peers(TEST_TREE_ROOT_ID_2).unwrap();
    assert_eq!(peers.len(), 1);
    assert!(peers.contains(&*TEST_PEER_PUBKEY));
}

#[test]
fn test_is_tree_synced_with_peer() {
    let (_base_db, mut sync) = setup();

    // Register a peer
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .unwrap();

    // Initially no trees are synced
    assert!(
        !sync
            .is_tree_synced_with_peer(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
            .unwrap()
    );

    // Add a synced tree
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .unwrap();

    // Verify tree is now synced
    assert!(
        sync.is_tree_synced_with_peer(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
            .unwrap()
    );
    assert!(
        !sync
            .is_tree_synced_with_peer(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID_2)
            .unwrap()
    );
}

#[test]
fn test_http_address_management() {
    let (_base_db, mut sync) = setup();

    // Register a peer first
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .unwrap();

    // Add HTTP addresses
    let http_addr1 = Address::http("192.168.1.100:8080");
    let http_addr2 = Address::http("example.com:8443");

    sync.add_peer_address(&*TEST_PEER_PUBKEY, http_addr1.clone())
        .unwrap();
    sync.add_peer_address(&*TEST_PEER_PUBKEY, http_addr2.clone())
        .unwrap();

    // Verify HTTP addresses were set
    let addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("http"))
        .unwrap();
    assert_eq!(addresses.len(), 2);
    assert!(addresses.contains(&http_addr1));
    assert!(addresses.contains(&http_addr2));

    // Test removing a specific address
    let removed = sync
        .remove_peer_address(&*TEST_PEER_PUBKEY, &http_addr1)
        .unwrap();
    assert!(removed);

    // Verify only one address remains
    let addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("http"))
        .unwrap();
    assert_eq!(addresses.len(), 1);
    assert!(addresses.contains(&http_addr2));
}

#[test]
fn test_iroh_info_management() {
    let (_base_db, mut sync) = setup();

    // Register a peer first
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .unwrap();

    // Add Iroh addresses
    let iroh_addr1 = Address::iroh("iroh_node_id_123");
    let iroh_addr2 = Address::iroh("iroh_node_id_456");

    sync.add_peer_address(&*TEST_PEER_PUBKEY, iroh_addr1.clone())
        .unwrap();
    sync.add_peer_address(&*TEST_PEER_PUBKEY, iroh_addr2.clone())
        .unwrap();

    // Verify Iroh addresses were set
    let addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("iroh"))
        .unwrap();
    assert_eq!(addresses.len(), 2);
    assert!(addresses.contains(&iroh_addr1));
    assert!(addresses.contains(&iroh_addr2));

    // Test removing a specific address
    let removed = sync
        .remove_peer_address(&*TEST_PEER_PUBKEY, &iroh_addr1)
        .unwrap();
    assert!(removed);

    // Verify only one address remains
    let addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("iroh"))
        .unwrap();
    assert_eq!(addresses.len(), 1);
    assert!(addresses.contains(&iroh_addr2));
}

#[test]
fn test_remove_transport_addresses() {
    let (_base_db, mut sync) = setup();

    // Register a peer and set transport addresses
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .unwrap();

    let http_addr = Address::http("example.com:8080");
    let iroh_addr = Address::iroh("iroh_node_123");

    sync.add_peer_address(&*TEST_PEER_PUBKEY, http_addr.clone())
        .unwrap();
    sync.add_peer_address(&*TEST_PEER_PUBKEY, iroh_addr.clone())
        .unwrap();

    // Verify both are set
    let http_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("http"))
        .unwrap();
    assert_eq!(http_addresses.len(), 1);
    let iroh_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("iroh"))
        .unwrap();
    assert_eq!(iroh_addresses.len(), 1);

    // Remove HTTP address
    let removed = sync
        .remove_peer_address(&*TEST_PEER_PUBKEY, &http_addr)
        .unwrap();
    assert!(removed);

    let http_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("http"))
        .unwrap();
    assert_eq!(http_addresses.len(), 0);
    let iroh_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("iroh"))
        .unwrap();
    assert_eq!(iroh_addresses.len(), 1); // Iroh should still be there

    // Remove Iroh address
    let removed = sync
        .remove_peer_address(&*TEST_PEER_PUBKEY, &iroh_addr)
        .unwrap();
    assert!(removed);

    let iroh_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("iroh"))
        .unwrap();
    assert_eq!(iroh_addresses.len(), 0);
}

#[test]
fn test_peer_removal_cleans_all_data() {
    let (_base_db, mut sync) = setup();

    // Register a peer and add all types of data
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .unwrap();
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .unwrap();

    let http_addr = Address::http("example.com:8080");
    let iroh_addr = Address::iroh("iroh_node_123");

    sync.add_peer_address(&*TEST_PEER_PUBKEY, http_addr.clone())
        .unwrap();
    sync.add_peer_address(&*TEST_PEER_PUBKEY, iroh_addr.clone())
        .unwrap();

    // Verify all data is present
    assert!(sync.get_peer_info(&*TEST_PEER_PUBKEY).unwrap().is_some());
    assert!(!sync.get_peer_trees(&*TEST_PEER_PUBKEY).unwrap().is_empty());
    let http_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("http"))
        .unwrap();
    assert!(!http_addresses.is_empty());
    let iroh_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("iroh"))
        .unwrap();
    assert!(!iroh_addresses.is_empty());

    // Remove peer
    sync.remove_peer(&*TEST_PEER_PUBKEY).unwrap();

    // Verify all data was cleaned up
    assert!(sync.get_peer_info(&*TEST_PEER_PUBKEY).unwrap().is_none());
    assert!(sync.get_peer_trees(&*TEST_PEER_PUBKEY).unwrap().is_empty());
    let http_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("http"))
        .unwrap();
    assert!(http_addresses.is_empty());
    let iroh_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("iroh"))
        .unwrap();
    assert!(iroh_addresses.is_empty());
}
