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

#[tokio::test]
async fn test_peer_registration() {
    let (_base_db, sync) = setup().await;

    // Register a peer
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .await
        .unwrap();

    // Verify peer was registered
    let peer_info = sync.get_peer_info(&*TEST_PEER_PUBKEY).await.unwrap();
    assert!(peer_info.is_some());

    let peer_info = peer_info.unwrap();
    assert_eq!(peer_info.pubkey, *TEST_PEER_PUBKEY);
    assert_eq!(peer_info.display_name, Some("Test Peer".to_string()));
    assert_eq!(peer_info.status, PeerStatus::Active);
}

#[tokio::test]
async fn test_peer_registration_without_display_name() {
    let (_base_db, sync) = setup().await;

    // Register a peer without display name
    sync.register_peer(&*TEST_PEER_PUBKEY, None).await.unwrap();

    // Verify peer was registered
    let peer_info = sync
        .get_peer_info(&*TEST_PEER_PUBKEY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(peer_info.pubkey, *TEST_PEER_PUBKEY);
    assert_eq!(peer_info.display_name, None);
    assert_eq!(peer_info.status, PeerStatus::Active);
}

#[tokio::test]
async fn test_update_peer_status() {
    let (_base_db, sync) = setup().await;

    // Register and then update peer status
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .await
        .unwrap();
    sync.update_peer_status(&*TEST_PEER_PUBKEY, PeerStatus::Inactive)
        .await
        .unwrap();

    // Verify status was updated
    let peer_info = sync
        .get_peer_info(&*TEST_PEER_PUBKEY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(peer_info.status, PeerStatus::Inactive);
}

#[tokio::test]
async fn test_update_nonexistent_peer_status() {
    let (_base_db, sync) = setup().await;

    // Try to update status of non-existent peer
    let result = sync
        .update_peer_status(&*TEST_PEER_PUBKEY, PeerStatus::Blocked)
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().is_not_found());
}

#[tokio::test]
async fn test_list_peers() {
    let (_base_db, sync) = setup().await;

    // Initially no peers
    let peers = sync.list_peers().await.unwrap();
    assert!(peers.is_empty());

    // Register multiple peers
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Peer 1"))
        .await
        .unwrap();
    sync.register_peer(&*TEST_PEER_PUBKEY_2, Some("Peer 2"))
        .await
        .unwrap();

    // Verify both peers are listed
    let peers = sync.list_peers().await.unwrap();
    assert_eq!(peers.len(), 2);

    let pubkeys: Vec<String> = peers.iter().map(|d| d.pubkey.clone()).collect();
    assert!(pubkeys.contains(&*TEST_PEER_PUBKEY));
    assert!(pubkeys.contains(&*TEST_PEER_PUBKEY_2));
}

#[tokio::test]
async fn test_remove_peer() {
    let (_base_db, sync) = setup().await;

    // Register a peer
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .await
        .unwrap();

    // Verify peer exists
    assert!(
        sync.get_peer_info(&*TEST_PEER_PUBKEY)
            .await
            .unwrap()
            .is_some()
    );

    // Remove peer
    sync.remove_peer(&*TEST_PEER_PUBKEY).await.unwrap();

    // Verify peer was removed
    assert!(
        sync.get_peer_info(&*TEST_PEER_PUBKEY)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn test_add_tree_sync() {
    let (_base_db, sync) = setup().await;

    // Register a peer first
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .await
        .unwrap();

    // Add synced tree
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .await
        .unwrap();

    // Verify tree was added
    let synced_trees = sync.get_peer_trees(&*TEST_PEER_PUBKEY).await.unwrap();
    assert_eq!(synced_trees.len(), 1);
    assert!(synced_trees.contains(&TEST_TREE_ROOT_ID.to_string()));
}

#[tokio::test]
async fn test_add_multiple_synced_trees() {
    let (_base_db, sync) = setup().await;

    // Register a peer first
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .await
        .unwrap();

    // Add multiple synced trees
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .await
        .unwrap();
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID_2)
        .await
        .unwrap();

    // Verify both trees were added
    let synced_trees = sync.get_peer_trees(&*TEST_PEER_PUBKEY).await.unwrap();
    assert_eq!(synced_trees.len(), 2);
    assert!(synced_trees.contains(&TEST_TREE_ROOT_ID.to_string()));
    assert!(synced_trees.contains(&TEST_TREE_ROOT_ID_2.to_string()));
}

#[tokio::test]
async fn test_add_duplicate_synced_tree() {
    let (_base_db, sync) = setup().await;

    // Register a peer first
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .await
        .unwrap();

    // Add same tree twice
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .await
        .unwrap();
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .await
        .unwrap();

    // Verify tree is only listed once
    let synced_trees = sync.get_peer_trees(&*TEST_PEER_PUBKEY).await.unwrap();
    assert_eq!(synced_trees.len(), 1);
    assert!(synced_trees.contains(&TEST_TREE_ROOT_ID.to_string()));
}

#[tokio::test]
async fn test_remove_tree_sync() {
    let (_base_db, sync) = setup().await;

    // Register a peer and add trees
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .await
        .unwrap();
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .await
        .unwrap();
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID_2)
        .await
        .unwrap();

    // Remove one tree
    sync.remove_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .await
        .unwrap();

    // Verify only one tree remains
    let synced_trees = sync.get_peer_trees(&*TEST_PEER_PUBKEY).await.unwrap();
    assert_eq!(synced_trees.len(), 1);
    assert!(synced_trees.contains(&TEST_TREE_ROOT_ID_2.to_string()));
    assert!(!synced_trees.contains(&TEST_TREE_ROOT_ID.to_string()));
}

#[tokio::test]
async fn test_get_tree_peers() {
    let (_base_db, sync) = setup().await;

    // Register multiple peers
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Peer 1"))
        .await
        .unwrap();
    sync.register_peer(&*TEST_PEER_PUBKEY_2, Some("Peer 2"))
        .await
        .unwrap();

    // Add same tree to both peers
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .await
        .unwrap();
    sync.add_tree_sync(&*TEST_PEER_PUBKEY_2, TEST_TREE_ROOT_ID)
        .await
        .unwrap();

    // Add different tree to one peer
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID_2)
        .await
        .unwrap();

    // Verify peers for first tree
    let peers = sync.get_tree_peers(TEST_TREE_ROOT_ID).await.unwrap();
    assert_eq!(peers.len(), 2);
    assert!(peers.contains(&*TEST_PEER_PUBKEY));
    assert!(peers.contains(&*TEST_PEER_PUBKEY_2));

    // Verify peers for second tree
    let peers = sync.get_tree_peers(TEST_TREE_ROOT_ID_2).await.unwrap();
    assert_eq!(peers.len(), 1);
    assert!(peers.contains(&*TEST_PEER_PUBKEY));
}

#[tokio::test]
async fn test_is_tree_synced_with_peer() {
    let (_base_db, sync) = setup().await;

    // Register a peer
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .await
        .unwrap();

    // Initially no trees are synced
    assert!(
        !sync
            .is_tree_synced_with_peer(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
            .await
            .unwrap()
    );

    // Add a synced tree
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .await
        .unwrap();

    // Verify tree is now synced
    assert!(
        sync.is_tree_synced_with_peer(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
            .await
            .unwrap()
    );
    assert!(
        !sync
            .is_tree_synced_with_peer(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID_2)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn test_http_address_management() {
    let (_base_db, sync) = setup().await;

    // Register a peer first
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .await
        .unwrap();

    // Add HTTP addresses
    let http_addr1 = Address::http("192.168.1.100:8080");
    let http_addr2 = Address::http("example.com:8443");

    sync.add_peer_address(&*TEST_PEER_PUBKEY, http_addr1.clone())
        .await
        .unwrap();
    sync.add_peer_address(&*TEST_PEER_PUBKEY, http_addr2.clone())
        .await
        .unwrap();

    // Verify HTTP addresses were set
    let addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("http"))
        .await
        .unwrap();
    assert_eq!(addresses.len(), 2);
    assert!(addresses.contains(&http_addr1));
    assert!(addresses.contains(&http_addr2));

    // Test removing a specific address
    let removed = sync
        .remove_peer_address(&*TEST_PEER_PUBKEY, &http_addr1)
        .await
        .unwrap();
    assert!(removed);

    // Verify only one address remains
    let addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("http"))
        .await
        .unwrap();
    assert_eq!(addresses.len(), 1);
    assert!(addresses.contains(&http_addr2));
}

#[tokio::test]
async fn test_iroh_info_management() {
    let (_base_db, sync) = setup().await;

    // Register a peer first
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .await
        .unwrap();

    // Add Iroh addresses
    let iroh_addr1 = Address::iroh("iroh_node_id_123");
    let iroh_addr2 = Address::iroh("iroh_node_id_456");

    sync.add_peer_address(&*TEST_PEER_PUBKEY, iroh_addr1.clone())
        .await
        .unwrap();
    sync.add_peer_address(&*TEST_PEER_PUBKEY, iroh_addr2.clone())
        .await
        .unwrap();

    // Verify Iroh addresses were set
    let addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("iroh"))
        .await
        .unwrap();
    assert_eq!(addresses.len(), 2);
    assert!(addresses.contains(&iroh_addr1));
    assert!(addresses.contains(&iroh_addr2));

    // Test removing a specific address
    let removed = sync
        .remove_peer_address(&*TEST_PEER_PUBKEY, &iroh_addr1)
        .await
        .unwrap();
    assert!(removed);

    // Verify only one address remains
    let addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("iroh"))
        .await
        .unwrap();
    assert_eq!(addresses.len(), 1);
    assert!(addresses.contains(&iroh_addr2));
}

#[tokio::test]
async fn test_remove_transport_addresses() {
    let (_base_db, sync) = setup().await;

    // Register a peer and set transport addresses
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .await
        .unwrap();

    let http_addr = Address::http("example.com:8080");
    let iroh_addr = Address::iroh("iroh_node_123");

    sync.add_peer_address(&*TEST_PEER_PUBKEY, http_addr.clone())
        .await
        .unwrap();
    sync.add_peer_address(&*TEST_PEER_PUBKEY, iroh_addr.clone())
        .await
        .unwrap();

    // Verify both are set
    let http_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("http"))
        .await
        .unwrap();
    assert_eq!(http_addresses.len(), 1);
    let iroh_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("iroh"))
        .await
        .unwrap();
    assert_eq!(iroh_addresses.len(), 1);

    // Remove HTTP address
    let removed = sync
        .remove_peer_address(&*TEST_PEER_PUBKEY, &http_addr)
        .await
        .unwrap();
    assert!(removed);

    let http_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("http"))
        .await
        .unwrap();
    assert_eq!(http_addresses.len(), 0);
    let iroh_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("iroh"))
        .await
        .unwrap();
    assert_eq!(iroh_addresses.len(), 1); // Iroh should still be there

    // Remove Iroh address
    let removed = sync
        .remove_peer_address(&*TEST_PEER_PUBKEY, &iroh_addr)
        .await
        .unwrap();
    assert!(removed);

    let iroh_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("iroh"))
        .await
        .unwrap();
    assert_eq!(iroh_addresses.len(), 0);
}

#[tokio::test]
async fn test_peer_removal_cleans_all_data() {
    let (_base_db, sync) = setup().await;

    // Register a peer and add all types of data
    sync.register_peer(&*TEST_PEER_PUBKEY, Some("Test Peer"))
        .await
        .unwrap();
    sync.add_tree_sync(&*TEST_PEER_PUBKEY, TEST_TREE_ROOT_ID)
        .await
        .unwrap();

    let http_addr = Address::http("example.com:8080");
    let iroh_addr = Address::iroh("iroh_node_123");

    sync.add_peer_address(&*TEST_PEER_PUBKEY, http_addr.clone())
        .await
        .unwrap();
    sync.add_peer_address(&*TEST_PEER_PUBKEY, iroh_addr.clone())
        .await
        .unwrap();

    // Verify all data is present
    assert!(
        sync.get_peer_info(&*TEST_PEER_PUBKEY)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        !sync
            .get_peer_trees(&*TEST_PEER_PUBKEY)
            .await
            .unwrap()
            .is_empty()
    );
    let http_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("http"))
        .await
        .unwrap();
    assert!(!http_addresses.is_empty());
    let iroh_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("iroh"))
        .await
        .unwrap();
    assert!(!iroh_addresses.is_empty());

    // Remove peer
    sync.remove_peer(&*TEST_PEER_PUBKEY).await.unwrap();

    // Verify all data was cleaned up
    assert!(
        sync.get_peer_info(&*TEST_PEER_PUBKEY)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        sync.get_peer_trees(&*TEST_PEER_PUBKEY)
            .await
            .unwrap()
            .is_empty()
    );
    let http_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("http"))
        .await
        .unwrap();
    assert!(http_addresses.is_empty());
    let iroh_addresses = sync
        .get_peer_addresses(&*TEST_PEER_PUBKEY, Some("iroh"))
        .await
        .unwrap();
    assert!(iroh_addresses.is_empty());
}
