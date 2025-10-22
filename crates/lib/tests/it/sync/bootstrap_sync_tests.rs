//! Bootstrap sync integration tests.
//!
//! This module tests the new bootstrap-first sync protocol where one peer
//! can join and bootstrap a database from another peer without any prior setup.

use super::helpers::*;
use eidetica::Entry;
use std::time::Duration;

/// Test the new unified sync API for bootstrapping a database from scratch
#[tokio::test]
async fn test_bootstrap_sync_from_zero_state() {
    // Setup server (has the database)
    let (_server_instance, server_sync) = setup();

    // Setup client (starts with no database)
    let (client_instance, client_sync) = setup();

    // Create some entries directly in the server's backend to simulate a tree with content
    // First create a proper root entry
    let root_entry = Entry::root_builder()
        .set_subtree_data(
            "messages",
            r#"{"msg1": {"text": "Hello from server!", "timestamp": 1234567890}}"#,
        )
        .build()
        .expect("Entry should build successfully");
    let test_tree_id = root_entry.id().clone();

    // Now create a child entry referencing the root
    let child_entry = Entry::builder(test_tree_id.clone())
        .set_parents(vec![test_tree_id.clone()])
        .set_subtree_data(
            "messages",
            r#"{"msg2": {"text": "Second message", "timestamp": 1234567891}}"#,
        )
        .build()
        .expect("Entry should build successfully");
    let child_entry_id = child_entry.id().clone();

    // Store entries in server backend
    server_sync
        .backend()
        .expect("Failed to get backend")
        .put_verified(root_entry)
        .unwrap();
    server_sync
        .backend()
        .expect("Failed to get backend")
        .put_verified(child_entry)
        .unwrap();

    // Debug server state
    let server_tips = server_sync
        .backend()
        .expect("Failed to get backend")
        .get_tips(&test_tree_id)
        .unwrap();
    println!("🧪 DEBUG: Server tips: {:?}", server_tips);

    // Start server
    let server_addr = start_sync_server(&server_sync).await;

    // Verify client doesn't have the database initially
    assert!(
        client_instance.load_database(&test_tree_id).is_err(),
        "Client should not have the database initially"
    );

    // Use the new simplified sync API to bootstrap from server
    client_sync.enable_http_transport().unwrap();

    println!("🧪 TEST: Attempting bootstrap sync from server...");
    client_sync
        .sync_with_peer(&server_addr, Some(&test_tree_id))
        .await
        .expect("Bootstrap sync should succeed");

    // Wait a moment for the sync to propagate
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify client now has the entries
    let root_client = client_sync
        .backend()
        .expect("Failed to get backend")
        .get(&test_tree_id)
        .expect("Client should have the root entry");
    assert_eq!(root_client.id(), test_tree_id, "Root entry should match");

    // Check if client also has child entry
    let child_result = client_sync
        .backend()
        .expect("Failed to get backend")
        .get(&child_entry_id);
    println!(
        "🧪 DEBUG: Client has child entry: {:?}",
        child_result.is_ok()
    );

    // Verify client has tips
    let tips = client_sync
        .backend()
        .expect("Failed to get backend")
        .get_tips(&test_tree_id);
    println!("🧪 DEBUG: Client tips result: {:?}", tips);
    match tips {
        Ok(tip_vec) => {
            println!(
                "✅ Client successfully bootstrapped tree with {} tips: {:?}",
                tip_vec.len(),
                tip_vec
            );
            assert!(
                !tip_vec.is_empty(),
                "Client should have tips for the synced tree"
            );
        }
        Err(e) => {
            panic!("Failed to get tips: {:?}", e);
        }
    }

    println!("✅ TEST: Bootstrap sync completed successfully");

    // Cleanup
    server_sync.stop_server_async().await.unwrap();
}

/// Test incremental sync after bootstrap (both peers now have the database)
#[tokio::test]
async fn test_incremental_sync_after_bootstrap() {
    // Setup server and client
    let (_server_instance, server_sync) = setup();
    let (_client_instance, client_sync) = setup();

    // Create some entries on server
    let root_entry = create_test_tree_entry();
    let test_tree_id = root_entry.id().clone();

    server_sync
        .backend()
        .expect("Failed to get backend")
        .put_verified(root_entry)
        .unwrap();

    // Start server
    let server_addr = start_sync_server(&server_sync).await;

    // Bootstrap client
    client_sync.enable_http_transport().unwrap();
    client_sync
        .sync_with_peer(&server_addr, Some(&test_tree_id))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify client has bootstrapped tree
    assert!(
        client_sync
            .backend()
            .expect("Failed to get backend")
            .get(&test_tree_id)
            .is_ok(),
        "Client should have the tree"
    );

    // Add new content to server AFTER bootstrap
    let entry2 = Entry::builder(test_tree_id.clone())
        .set_parents(vec![test_tree_id.clone()])
        .set_subtree_data(
            "messages",
            r#"{"post_bootstrap": {"text": "After bootstrap message"}}"#,
        )
        .build()
        .expect("Entry should build successfully");

    server_sync
        .backend()
        .expect("Failed to get backend")
        .put_verified(entry2.clone())
        .unwrap();

    // Now do incremental sync (client already has the tree)
    println!("🧪 TEST: Attempting incremental sync...");
    client_sync
        .sync_with_peer(&server_addr, Some(&test_tree_id))
        .await
        .expect("Incremental sync should succeed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify client received the new entry
    let entry2_client_result = client_sync
        .backend()
        .expect("Failed to get backend")
        .get(&entry2.id());
    assert!(
        entry2_client_result.is_ok(),
        "Client should have received the new entry"
    );

    // Verify tips have been updated
    let tips = client_sync
        .backend()
        .expect("Failed to get backend")
        .get_tips(&test_tree_id)
        .unwrap();
    assert!(
        tips.contains(&entry2.id()),
        "Client tips should include the new entry"
    );

    println!("✅ TEST: Incremental sync completed successfully");

    // Cleanup
    server_sync.stop_server_async().await.unwrap();
}

/// Test error handling when trying to bootstrap a non-existent tree
#[tokio::test]
async fn test_bootstrap_nonexistent_tree() {
    let (_server_instance, server_sync) = setup();
    let (_client_instance, client_sync) = setup();

    // Start server (with no databases)
    server_sync.enable_http_transport().unwrap();
    server_sync.start_server_async("127.0.0.1:0").await.unwrap();
    let server_addr = server_sync.get_server_address_async().await.unwrap();

    // Try to bootstrap a tree that doesn't exist
    client_sync.enable_http_transport().unwrap();
    let fake_tree_id = eidetica::entry::ID::from("fake_tree_id_that_doesnt_exist");

    let result = client_sync
        .sync_with_peer(&server_addr, Some(&fake_tree_id))
        .await;

    // Should fail gracefully
    assert!(result.is_err(), "Syncing non-existent tree should fail");

    println!(
        "✅ TEST: Non-existent tree sync failed as expected: {:?}",
        result.err()
    );

    // Cleanup
    server_sync.stop_server_async().await.unwrap();
}

/// Test the discover_peer_trees API (placeholder test since it's not fully implemented)
#[tokio::test]
async fn test_discover_peer_trees_placeholder() {
    let (_server_instance, server_sync) = setup();
    let (_client_instance, client_sync) = setup();

    // Start server
    let server_addr = start_sync_server(&server_sync).await;

    // Try to discover trees (currently returns empty list)
    client_sync.enable_http_transport().unwrap();
    let trees = client_sync.discover_peer_trees(&server_addr).await.unwrap();

    // Currently returns empty, but should not error
    assert!(
        trees.is_empty(),
        "discover_peer_trees currently returns empty list"
    );

    println!("✅ TEST: Tree discovery placeholder works (returns empty list)");

    // Cleanup
    server_sync.stop_server_async().await.unwrap();
}

/// Test bootstrap behavior with malformed request data
#[tokio::test]
async fn test_bootstrap_malformed_request_data() {
    let (_server_instance, server_sync) = setup();
    let (_client_instance, client_sync) = setup();

    // Create a valid tree on server
    let root_entry = create_test_tree_entry();
    let test_tree_id = root_entry.id().clone();

    server_sync
        .backend()
        .expect("Failed to get backend")
        .put_verified(root_entry)
        .unwrap();

    // Start server
    let server_addr = start_sync_server(&server_sync).await;

    client_sync.enable_http_transport().unwrap();

    // Test 1: Invalid tree ID format
    let malformed_tree_id = eidetica::entry::ID::from("invalid_tree_format");
    let result = client_sync
        .sync_with_peer_for_bootstrap(
            &server_addr,
            &malformed_tree_id,
            "client_key",
            eidetica::auth::Permission::Write(5),
        )
        .await;

    assert!(
        result.is_err(),
        "Bootstrap should fail with invalid tree ID"
    );
    println!("✅ Bootstrap correctly rejected malformed tree ID");

    // Test 2: Empty key name (should be handled gracefully)
    let result = client_sync
        .sync_with_peer_for_bootstrap(
            &server_addr,
            &test_tree_id,
            "", // Empty key name
            eidetica::auth::Permission::Write(5),
        )
        .await;

    assert!(result.is_err(), "Bootstrap should fail with empty key name");
    println!("✅ Bootstrap correctly rejected empty key name");

    // Cleanup
    server_sync.stop_server_async().await.unwrap();
}

/// Test bootstrap with conflicting tree IDs
#[tokio::test]
async fn test_bootstrap_conflicting_tree_ids() {
    let (_server_instance, server_sync) = setup();
    let (_client_instance, client_sync) = setup();

    // Create a tree on server
    let root_entry = create_test_tree_entry();
    let _actual_tree_id = root_entry.id().clone();

    server_sync
        .backend()
        .expect("Failed to get backend")
        .put_verified(root_entry)
        .unwrap();

    // Start server
    let server_addr = start_sync_server(&server_sync).await;

    client_sync.enable_http_transport().unwrap();

    // Try to bootstrap with a different tree ID than what exists
    let different_tree_id = eidetica::entry::ID::from("different_tree_that_doesnt_exist");
    let result = client_sync
        .sync_with_peer_for_bootstrap(
            &server_addr,
            &different_tree_id,
            "client_key",
            eidetica::auth::Permission::Write(5),
        )
        .await;

    assert!(
        result.is_err(),
        "Bootstrap should fail for non-existent tree"
    );

    let error_msg = result.unwrap_err().to_string();
    assert!(
        error_msg.contains("not found") || error_msg.contains("exist"),
        "Error should indicate tree not found: {}",
        error_msg
    );

    println!("✅ Bootstrap correctly rejected request for non-existent tree");

    // Cleanup
    server_sync.stop_server_async().await.unwrap();
}
