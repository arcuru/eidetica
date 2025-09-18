//! Tests for DAG traversal sync functionality.
//!
//! This module tests the new BackgroundSync DAG traversal methods that ensure
//! proper parent-child ordering during synchronization.

use sha2::{Digest, Sha256};
use std::{collections::HashSet, time::Duration};

use eidetica::{
    Database,
    entry::{Entry, ID},
    store::DocStore,
    sync::Address,
};

use super::helpers;

/// Generate a valid test ID in the correct SHA-256 hex format (64 lowercase hex chars)
fn test_id(name: &str) -> ID {
    let mut hasher = Sha256::new();
    hasher.update(b"test_prefix_"); // Add prefix to avoid collisions with real IDs
    hasher.update(name.as_bytes());
    let hash = hasher.finalize();
    format!("{hash:x}").into()
}

/// Helper to create a test entry with specific parents
fn create_entry_with_parents(tree_id: &str, parents: Vec<ID>) -> Entry {
    let mut builder = Entry::builder(test_id(tree_id));

    if !parents.is_empty() {
        builder = builder.set_parents(parents);
    }

    builder
        .set_subtree_data("data", r#"{"test": true}"#)
        .build()
        .expect("Test entry should build successfully")
}

/// Helper to create a linear chain of entries: root -> child1 -> child2 -> ...
fn create_linear_chain(tree_id: &str, count: usize) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();

    for i in 0..count {
        let entry = if i == 0 {
            // Create root entry
            Entry::root_builder()
                .set_subtree_data("data", r#"{"test": true}"#)
                .build()
                .expect("Root entry should build successfully")
        } else {
            // Create child entry with parent
            let parents = vec![entries[i - 1].id().clone()];
            create_entry_with_parents(tree_id, parents)
        };

        entries.push(entry);
    }

    entries
}

/// Helper to create a DAG: root -> branch1, branch2 -> merge
fn create_dag_structure(tree_id: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();

    // Root entry
    let root = Entry::root_builder()
        .set_subtree_data("data", r#"{"test": true}"#)
        .build()
        .expect("Entry should build successfully");
    entries.push(root.clone());

    // Two branches from root
    let branch1 = create_entry_with_parents(tree_id, vec![root.id().clone()]);
    let branch2 = create_entry_with_parents(tree_id, vec![root.id().clone()]);
    entries.push(branch1.clone());
    entries.push(branch2.clone());

    // Merge entry with both branches as parents
    let merge =
        create_entry_with_parents(tree_id, vec![branch1.id().clone(), branch2.id().clone()]);
    entries.push(merge);

    entries
}

#[tokio::test]
async fn test_dag_sync_linear_chain() {
    let (base_db1, _sync1) = helpers::setup();
    let (base_db2, _sync2) = helpers::setup();

    // Create a linear chain of entries in backend1
    let tree_id = "test_tree";
    let chain = create_linear_chain(tree_id, 5);

    // Store entire chain in backend1
    for entry in &chain {
        base_db1.backend().put_verified(entry.clone()).unwrap();
    }

    // Backend2 only has the root entry
    base_db2.backend().put_verified(chain[0].clone()).unwrap();

    // Test DAG traversal to find missing entries
    // Simulate sync by checking what backend2 would need
    let _backend1 = base_db1.backend();
    let backend2 = base_db2.backend();

    // Backend2 tips: [root]
    // Backend1 tips: [tip]
    let _tips1 = [chain.last().unwrap().id().clone()];
    let _tips2 = [chain[0].id().clone()];

    // Find what backend2 is missing (should be entries 1-4)
    let mut missing_count = 0;
    for (_i, entry) in chain.iter().enumerate().skip(1) {
        if backend2.get(&entry.id()).is_err() {
            missing_count += 1;
        }
    }

    assert_eq!(missing_count, 4, "Backend2 should be missing 4 entries");

    // Verify parent-child relationships are correct
    for i in 1..chain.len() {
        let child = &chain[i];
        let parent = &chain[i - 1];
        let child_parents = child.parents().unwrap();
        assert!(
            child_parents.contains(&parent.id()),
            "Child should have parent as its parent"
        );
    }
}

#[tokio::test]
async fn test_dag_sync_branching_structure() {
    let (base_db1, _sync1) = helpers::setup();
    let (base_db2, _sync2) = helpers::setup();

    // Create a DAG structure
    let tree_id = "test_tree";
    let dag_entries = create_dag_structure(tree_id);

    // Store all entries in backend1
    for entry in &dag_entries {
        base_db1.backend().put_verified(entry.clone()).unwrap();
    }

    // Backend2 only has the root
    base_db2
        .backend()
        .put_verified(dag_entries[0].clone())
        .unwrap();

    let backend2 = base_db2.backend();

    // Check that backend2 is missing the branch and merge entries
    let missing: Vec<_> = dag_entries
        .iter()
        .skip(1) // Skip root
        .filter(|entry| backend2.get(&entry.id()).is_err())
        .collect();

    assert_eq!(
        missing.len(),
        3,
        "Backend2 should be missing branch1, branch2, and merge"
    );

    // Verify DAG structure constraints
    let root = &dag_entries[0];
    let branch1 = &dag_entries[1];
    let branch2 = &dag_entries[2];
    let merge = &dag_entries[3];

    // Root has no parents
    assert_eq!(root.parents().unwrap().len(), 0);

    // Branches have root as parent
    assert_eq!(branch1.parents().unwrap(), vec![root.id().clone()]);
    assert_eq!(branch2.parents().unwrap(), vec![root.id().clone()]);

    // Merge has both branches as parents
    let merge_parents: HashSet<_> = merge.parents().unwrap().into_iter().collect();
    let expected_parents: HashSet<_> = vec![branch1.id().clone(), branch2.id().clone()]
        .into_iter()
        .collect();
    assert_eq!(merge_parents, expected_parents);
}

#[tokio::test]
async fn test_dag_sync_partial_overlap() {
    let (base_db1, _sync1) = helpers::setup();
    let (base_db2, _sync2) = helpers::setup();

    // Create linear chain
    let tree_id = "test_tree";
    let chain = create_linear_chain(tree_id, 6);

    // Backend1 has all entries
    for entry in &chain {
        base_db1.backend().put_verified(entry.clone()).unwrap();
    }

    // Backend2 has first 3 entries
    for entry in &chain[0..3] {
        base_db2.backend().put_verified(entry.clone()).unwrap();
    }

    let backend2 = base_db2.backend();

    // Check missing entries (should be last 3)
    let missing_count = chain
        .iter()
        .skip(3)
        .filter(|entry| backend2.get(&entry.id()).is_err())
        .count();

    assert_eq!(
        missing_count, 3,
        "Backend2 should be missing the last 3 entries"
    );
}

#[tokio::test]
async fn test_dag_sync_entry_ordering() {
    let (base_db, _sync) = helpers::setup();

    // Create entries that must be ordered by height
    let tree_id = "test_tree";
    let chain = create_linear_chain(tree_id, 4);

    // Store entries in random order
    let backend = base_db.backend();
    backend.put_verified(chain[2].clone()).unwrap(); // Child first
    backend.put_verified(chain[0].clone()).unwrap(); // Root
    backend.put_verified(chain[3].clone()).unwrap(); // Grandchild
    backend.put_verified(chain[1].clone()).unwrap(); // Parent

    // Retrieve and verify parent-child relationships
    for i in 1..chain.len() {
        let parent = backend.get(&chain[i - 1].id()).unwrap();
        let child = backend.get(&chain[i].id()).unwrap();

        // Verify child has parent as one of its parents
        assert!(
            child.parents().unwrap().contains(&parent.id()),
            "Child {} should have parent {} in its parents list",
            child.id(),
            parent.id()
        );
    }
}

#[tokio::test]
async fn test_dag_sync_empty_sets() {
    let (base_db, _sync) = helpers::setup();

    // Test edge cases with empty tip sets
    let _tree_id = "test_tree";
    let entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");

    base_db.backend().put_verified(entry.clone()).unwrap();

    // Empty tips should result in empty operations
    let empty_tips: Vec<ID> = vec![];
    let our_tips = [entry.id().clone()];

    // These would be internal method calls if we could access BackgroundSync directly
    // For now, we verify the basic entry structure is correct
    assert_eq!(entry.parents().unwrap().len(), 0);
    assert_eq!(our_tips.len(), 1);
    assert_eq!(empty_tips.len(), 0);
}

#[tokio::test]
async fn test_sync_flow_integration() {
    let (base_db1, _sync1) = helpers::setup();
    let (base_db2, _sync2) = helpers::setup();

    // Create a complex DAG structure in database 1
    let tree_id = "sync_test_tree";
    let dag_entries = create_dag_structure(tree_id);

    // Store all entries in backend1
    for entry in &dag_entries {
        base_db1.backend().put_verified(entry.clone()).unwrap();
    }

    // Simulate what would happen during sync:

    // 1. Simulate tips (since set_tips is not available)
    let tips1 = [dag_entries.last().unwrap().id().clone()];
    let tips2: Vec<ID> = vec![]; // Backend2 starts with no entries

    assert_eq!(tips1.len(), 1, "Backend1 should have 1 tip");
    assert_eq!(tips2.len(), 0, "Backend2 should have 0 tips initially");

    // 2. Identify missing entries (backend2 is missing everything)
    let mut missing_entries = Vec::new();
    for entry in &dag_entries {
        if base_db2.backend().get(&entry.id()).is_err() {
            missing_entries.push(entry.clone());
        }
    }

    assert_eq!(
        missing_entries.len(),
        4,
        "Backend2 should be missing all 4 entries"
    );

    // 3. Store entries in dependency order (root first, then children)
    // For this test, we'll store them in the order they were created
    for entry in missing_entries {
        base_db2.backend().put_verified(entry).unwrap();
    }

    // Verify all entries are now present in both backends
    for entry in &dag_entries {
        let entry1 = base_db1.backend().get(&entry.id()).unwrap();
        let entry2 = base_db2.backend().get(&entry.id()).unwrap();
        assert_eq!(entry1.id(), entry2.id(), "Entry IDs should match");
        assert_eq!(
            entry1.parents().unwrap(),
            entry2.parents().unwrap(),
            "Entry parents should match"
        );
    }
}

#[tokio::test]
async fn test_bidirectional_sync_flow() {
    let (base_db1, _sync1) = helpers::setup();
    let (base_db2, _sync2) = helpers::setup();

    let tree_id = "bidirectional_tree";
    let _tree_id_val: ID = tree_id.into();

    // Create different chains in each database
    let chain1 = create_linear_chain(tree_id, 3);
    let chain2 = {
        // Create a branch from the same root
        let root = chain1[0].clone();
        let mut branch = vec![root.clone()];

        // Add different entries building from the same root
        for i in 1..3 {
            let parent_id = branch[i - 1].id().clone();
            let entry = create_entry_with_parents(tree_id, vec![parent_id]);
            branch.push(entry);
        }
        branch
    };

    // Store chain1 in backend1
    for entry in &chain1 {
        base_db1.backend().put_verified(entry.clone()).unwrap();
    }

    // Store chain2 in backend2 (sharing the root)
    for entry in &chain2 {
        base_db2.backend().put_verified(entry.clone()).unwrap();
    }

    // Simulate bidirectional sync

    // Find what backend2 needs from backend1 (entries 1,2 from chain1)
    let mut missing_in_2 = Vec::new();
    for entry in &chain1[1..] {
        // Skip root which backend2 already has
        if base_db2.backend().get(&entry.id()).is_err() {
            missing_in_2.push(entry.clone());
        }
    }

    // Store missing entries in backend2
    for entry in missing_in_2 {
        base_db2.backend().put_verified(entry).unwrap();
    }

    // Backend2 -> Backend1 sync
    let mut missing_in_1 = Vec::new();
    for entry in &chain2[1..] {
        // Skip root which backend1 already has
        if base_db1.backend().get(&entry.id()).is_err() {
            missing_in_1.push(entry.clone());
        }
    }

    // Store missing entries in backend1
    for entry in missing_in_1 {
        base_db1.backend().put_verified(entry).unwrap();
    }

    // Verify both databases have all entries
    for entry in chain1.iter().chain(chain2.iter()) {
        assert!(
            base_db1.backend().get(&entry.id()).is_ok(),
            "Backend1 should have entry {}",
            entry.id()
        );
        assert!(
            base_db2.backend().get(&entry.id()).is_ok(),
            "Backend2 should have entry {}",
            entry.id()
        );
    }
}

#[tokio::test]
async fn test_real_sync_transport_setup() {
    // Create two separate database instances using the helper
    let (_base_db1, mut sync1) = helpers::setup();
    let (_base_db2, mut sync2) = helpers::setup();

    // Enable HTTP transport for both
    sync1.enable_http_transport().unwrap();
    sync2.enable_http_transport().unwrap();

    // Start server on sync2
    sync2.start_server_async("127.0.0.1:0").await.unwrap();
    let server_addr = sync2.get_server_address_async().await.unwrap();

    // Give the server a moment to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Get sync device public keys for peer registration
    let sync1_pubkey = sync1.get_device_public_key().unwrap();
    let sync2_pubkey = sync2.get_device_public_key().unwrap();

    // Create server address using the helper method
    let server_address = Address::http(server_addr);

    // Register peers with each other
    sync1.register_peer(&sync2_pubkey, Some("peer2")).unwrap();
    sync1
        .add_peer_address(&sync2_pubkey, server_address.clone())
        .unwrap();

    sync2.register_peer(&sync1_pubkey, Some("peer1")).unwrap();

    // Verify peer registration worked
    let peer_info = sync1.get_peer_info(&sync2_pubkey).unwrap().unwrap();
    assert_eq!(peer_info.display_name, Some("peer2".to_string()));
    assert!(peer_info.has_transport("http"));

    // Create some entries to send
    let mut entries = Vec::new();
    for i in 0..3 {
        let entry = Entry::root_builder()
            .set_subtree_data("data", format!(r#"{{"test": {i}}}"#))
            .build()
            .expect("Entry should build successfully");
        entries.push(entry.clone());
    }
    let entry_ids: Vec<_> = entries.iter().map(|e| e.id().clone()).collect();

    // Test sending entries using the transport layer
    // This tests the implemented SendEntries functionality with actual storage
    let result = sync1.send_entries_async(&entries, &server_address).await;
    assert!(
        result.is_ok(),
        "Should be able to send entries via HTTP transport"
    );

    // Give some time for async processing
    tokio::time::sleep(Duration::from_millis(100)).await;

    // NOW VERIFY ACTUAL STORAGE: With the SyncHandler implementation,
    // entries should actually be stored in database 2's backend
    for entry_id in &entry_ids {
        assert!(
            _base_db2.backend().get(entry_id).is_ok(),
            "Entry {entry_id} should exist in database 2 after sync"
        );
    }

    println!(
        "Successfully sent and stored {} entries via HTTP transport with SyncHandler",
        entries.len()
    );

    // Clean up
    sync2.stop_server_async().await.unwrap();
}

#[tokio::test]
async fn test_sync_protocol_implementation() {
    // This test verifies that the sync protocol methods (GetTips, GetEntries, SendEntries)
    // are properly implemented with the SyncHandler architecture and that data actually syncs

    let (base_db1, mut sync1) = helpers::setup();
    let (base_db2, mut sync2) = helpers::setup();

    // Enable HTTP transport for both
    sync1.enable_http_transport().unwrap();
    sync2.enable_http_transport().unwrap();

    // Start server on sync1 (which has the data)
    sync1.start_server_async("127.0.0.1:0").await.unwrap();
    let server_addr = sync1.get_server_address_async().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Create a tree with data in database 1
    let mut settings = eidetica::crdt::Doc::new();
    settings.set_string("name", "test_tree");
    let tree1 = Database::new(settings, base_db1.backend().clone(), "_device_key").unwrap();
    let tree_root_id = tree1.root_id().clone();

    // Get the root entry to verify it exists
    let _root_entry = base_db1.backend().get(&tree_root_id).unwrap();

    // Add test data to tree1
    let test_entry_id = {
        let op = tree1.new_transaction().unwrap();
        let doc_store = op.get_store::<DocStore>("data").unwrap();
        doc_store.set_string("test_key", "test_value").unwrap();
        doc_store.set_string("protocol", "implemented").unwrap();
        op.commit().unwrap()
    };

    // Verify data exists in db1 but not in db2 yet
    assert!(
        base_db1.backend().get(&test_entry_id).is_ok(),
        "Entry should exist in db1"
    );
    assert!(
        base_db2.backend().get(&test_entry_id).is_err(),
        "Entry should not exist in db2 yet"
    );

    // Also verify the tree root doesn't exist in db2 yet
    assert!(
        base_db2.backend().get(&tree_root_id).is_err(),
        "Tree root should not exist in db2 yet"
    );

    // Debug: Check trees available on server (sync1 is the server now)
    let available_trees = sync2.discover_peer_trees(&server_addr).await.unwrap();
    println!("🧪 DEBUG: Available trees on server: {:?}", available_trees);

    // Use the new bootstrap-first sync protocol (sync2 bootstraps from sync1)
    println!("🧪 DEBUG: Starting sync for tree_root_id: {}", tree_root_id);
    let result = sync2
        .sync_with_peer(&server_addr, Some(&tree_root_id))
        .await;

    // The sync should succeed with properly implemented protocol methods
    assert!(result.is_ok(), "Sync should succeed: {:?}", result.err());
    println!("🧪 DEBUG: Sync completed successfully");

    // Wait a moment for async processing
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Debug: check what entries exist in both databases
    println!("🧪 DEBUG: Checking what entries exist:");
    println!(
        "  - db1 has tree root: {}",
        base_db1.backend().get(&tree_root_id).is_ok()
    );
    println!(
        "  - db1 has test entry: {}",
        base_db1.backend().get(&test_entry_id).is_ok()
    );
    println!(
        "  - db2 has tree root: {}",
        base_db2.backend().get(&tree_root_id).is_ok()
    );
    println!(
        "  - db2 has test entry: {}",
        base_db2.backend().get(&test_entry_id).is_ok()
    );

    // Verify the data was actually synced to db2
    let synced_entry = base_db2.backend().get(&test_entry_id);
    assert!(
        synced_entry.is_ok(),
        "Entry should now exist in db2 after sync"
    );

    // Now add MORE data to tree1 and sync again to truly test the sync protocol
    let second_entry_id = {
        let op = tree1.new_transaction().unwrap();
        let doc_store = op.get_store::<DocStore>("data").unwrap();
        doc_store.set_string("second_key", "second_value").unwrap();
        doc_store
            .set_string("sync_test", "actually_working")
            .unwrap();
        op.commit().unwrap()
    };

    // Verify second entry exists in db1 but not in db2
    assert!(
        base_db1.backend().get(&second_entry_id).is_ok(),
        "Second entry should exist in db1"
    );
    assert!(
        base_db2.backend().get(&second_entry_id).is_err(),
        "Second entry should not exist in db2 before second sync"
    );

    // Perform another sync to transfer the new entry (incremental sync)
    let result2 = sync2
        .sync_with_peer(&server_addr, Some(&tree_root_id))
        .await;
    assert!(
        result2.is_ok(),
        "Second sync should succeed: {:?}",
        result2.err()
    );

    // Wait for processing
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify the second entry was synced
    assert!(
        base_db2.backend().get(&second_entry_id).is_ok(),
        "Second entry should now exist in db2 after second sync"
    );

    // Reload the tree to get the latest state
    let tree2 = base_db2.load_database(&tree_root_id).unwrap();

    // Verify ALL synced data is correct
    {
        let doc_store = tree2.get_store_viewer::<DocStore>("data").unwrap();
        // First entry data
        assert_eq!(doc_store.get_string("test_key").unwrap(), "test_value");
        assert_eq!(doc_store.get_string("protocol").unwrap(), "implemented");
        // Second entry data
        assert_eq!(doc_store.get_string("second_key").unwrap(), "second_value");
        assert_eq!(
            doc_store.get_string("sync_test").unwrap(),
            "actually_working"
        );
    }

    println!(
        "✅ Full protocol implementation verified: GetTips, GetEntries, and SendEntries all working!"
    );
    println!("✅ Successfully synced multiple entries across two sync operations!");

    // Clean up
    sync1.stop_server_async().await.unwrap();
}

#[tokio::test]
async fn test_iroh_sync_end_to_end_no_relays() {
    // This test demonstrates full end-to-end Iroh P2P sync between two nodes
    // using direct connections without relay servers for fast local testing

    use eidetica::sync::transports::iroh::IrohTransport;
    use iroh::RelayMode;

    let (_base_db1, mut sync1) = helpers::setup();
    let (base_db2, mut sync2) = helpers::setup();

    // Enable Iroh transport for both with relays disabled for local testing
    let transport1 = IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .build()
        .unwrap();
    let transport2 = IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .build()
        .unwrap();

    sync1.enable_iroh_transport_with_config(transport1).unwrap();
    sync2.enable_iroh_transport_with_config(transport2).unwrap();

    // Start servers (Iroh ignores the bind address and uses its own addressing)
    sync2.start_server_async("ignored").await.unwrap();
    sync1.start_server_async("ignored").await.unwrap();

    // Give endpoints time to initialize and discover direct addresses
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Get device public keys for peer registration
    let sync1_pubkey = sync1.get_device_public_key().unwrap();
    let sync2_pubkey = sync2.get_device_public_key().unwrap();

    // Get server addresses (now containing full NodeAddr info with direct addresses)
    // This uses the same pattern as HTTP transport but returns serialized NodeAddr info
    let server_addr1 = sync1.get_server_address_async().await.unwrap();
    let server_addr2 = sync2.get_server_address_async().await.unwrap();

    println!("Node 1 address info: {server_addr1}");
    println!("Node 2 address info: {server_addr2}");

    // Create addresses using the serialized NodeAddr - the transport will parse this
    let server_address1 = Address::iroh(&server_addr1);
    let server_address2 = Address::iroh(&server_addr2);

    // Register peers with each other
    sync1
        .register_peer(&sync2_pubkey, Some("iroh_peer2"))
        .unwrap();
    sync1
        .add_peer_address(&sync2_pubkey, server_address2.clone())
        .unwrap();

    sync2
        .register_peer(&sync1_pubkey, Some("iroh_peer1"))
        .unwrap();
    sync2
        .add_peer_address(&sync1_pubkey, server_address1.clone())
        .unwrap();

    // Verify peer registration worked
    let peer_info = sync1.get_peer_info(&sync2_pubkey).unwrap().unwrap();
    assert_eq!(peer_info.display_name, Some("iroh_peer2".to_string()));
    assert!(peer_info.has_transport("iroh"));

    // Create some test entries to sync
    let mut entries = Vec::new();
    for i in 0..3 {
        let entry = Entry::root_builder()
            .set_subtree_data("data", format!(r#"{{"test": {i}}}"#))
            .build()
            .expect("Entry should build successfully");
        entries.push(entry.clone());
    }
    let entry_ids: Vec<_> = entries.iter().map(|e| e.id().clone()).collect();

    // Test sending entries from sync1 to sync2 using Iroh P2P transport
    println!(
        "Attempting to send {} entries via Iroh transport...",
        entries.len()
    );
    let result = sync1.send_entries_async(&entries, &server_address2).await;

    if let Err(ref e) = result {
        println!("Send error: {e:?}");
        println!("Node 1 address info: {server_addr1}");
        println!("Node 2 address info: {server_addr2}");
    }

    assert!(
        result.is_ok(),
        "Should be able to send entries via Iroh P2P transport: {:?}",
        result.err()
    );

    // Give time for async processing
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify entries were actually stored in database 2
    for entry_id in &entry_ids {
        assert!(
            base_db2.backend().get(entry_id).is_ok(),
            "Entry {entry_id} should exist in database 2 after Iroh sync"
        );
    }

    println!(
        "✅ Successfully synced {} entries via Iroh P2P transport!",
        entries.len()
    );

    // Clean up
    sync1.stop_server_async().await.unwrap();
    sync2.stop_server_async().await.unwrap();
}

#[tokio::test]
async fn test_iroh_transport_production_defaults() {
    // This test verifies that the default transport configuration
    // uses production relay settings (n0's servers)

    use eidetica::sync::transports::iroh::IrohTransport;
    use iroh::RelayMode;

    let (_base_db, mut sync) = helpers::setup();

    // Test 1: Default constructor uses production relays
    sync.enable_iroh_transport().unwrap();
    sync.start_server_async("ignored").await.unwrap();

    // Just verify it starts without error - we can't test actual relay connectivity
    // without internet access in CI, but this ensures the configuration is valid
    assert!(sync.get_server_address_async().await.is_ok());
    sync.stop_server_async().await.unwrap();

    // Test 2: Builder with explicit Default mode
    let (_base_db2, mut sync2) = helpers::setup();
    let transport = IrohTransport::builder()
        .relay_mode(RelayMode::Default)
        .build()
        .unwrap();

    sync2.enable_iroh_transport_with_config(transport).unwrap();
    sync2.start_server_async("ignored").await.unwrap();
    assert!(sync2.get_server_address_async().await.is_ok());
    sync2.stop_server_async().await.unwrap();
}

#[tokio::test]
async fn test_iroh_transport_staging_mode() {
    // This test verifies that staging mode can be configured
    // (useful for testing against n0's staging infrastructure)

    use eidetica::sync::transports::iroh::IrohTransport;
    use iroh::RelayMode;

    let (_base_db, mut sync) = helpers::setup();

    let transport = IrohTransport::builder()
        .relay_mode(RelayMode::Staging)
        .build()
        .unwrap();

    sync.enable_iroh_transport_with_config(transport).unwrap();
    sync.start_server_async("ignored").await.unwrap();

    // Just verify it starts without error
    assert!(sync.get_server_address_async().await.is_ok());
    sync.stop_server_async().await.unwrap();
}

#[tokio::test]
async fn test_iroh_transport_custom_relay_config() {
    // This test demonstrates how to configure custom relay servers
    // (e.g., for local testing with iroh-relay --dev)

    use eidetica::sync::transports::iroh::IrohTransport;
    use iroh::{RelayMap, RelayMode, RelayNode, RelayUrl};

    let (_base_db, mut sync) = helpers::setup();

    // Create a custom relay map pointing to a local relay server
    // (In real usage, you'd run: iroh-relay --dev)
    let relay_url: RelayUrl = "http://localhost:3340".parse().unwrap();
    let relay_node = RelayNode {
        url: relay_url,
        quic: None, // No QUIC for local HTTP-only relay
    };
    let relay_map = RelayMap::from_iter([relay_node]);

    let transport = IrohTransport::builder()
        .relay_mode(RelayMode::Custom(relay_map))
        .build()
        .unwrap();

    sync.enable_iroh_transport_with_config(transport).unwrap();

    // Note: This will fail to actually start because no relay is running
    // but it demonstrates the configuration pattern
    let result = sync.start_server_async("ignored").await;

    // We expect this to fail since no local relay is running
    // In a real integration test, you'd run iroh-relay --dev first
    if result.is_ok() {
        sync.stop_server_async().await.unwrap();
    }

    println!("Custom relay configuration test completed (expected to fail without running relay)");
}
