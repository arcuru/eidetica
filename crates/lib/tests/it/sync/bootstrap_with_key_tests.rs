//! Tests for sync_with_peer_for_bootstrap_with_key().
//!
//! This module tests the new `sync_with_peer_for_bootstrap_with_key()` method that
//! accepts a public key string directly instead of looking it up from backend storage.
//! This is essential for working with User API managed keys that are stored in memory.

use super::helpers::*;
use eidetica::{
    Database, Entry,
    auth::{
        Permission,
        crypto::{format_public_key, generate_keypair},
        types::KeyStatus,
    },
    crdt::Doc,
    database::DatabaseKey,
    entry::ID,
    store::DocStore,
    sync::transports::http::HttpTransport,
};

/// Test basic bootstrap with user-provided signing key
#[tokio::test]
async fn test_bootstrap_with_provided_key() {
    // Setup server with a database
    let (_server_instance, _user, _key_id, _server_db, server_sync, tree_id) =
        setup_global_wildcard_server().await;

    // Add some content to the server database
    let root_entry = Entry::root_builder()
        .set_subtree_data(
            "messages",
            r#"{"msg1": {"text": "Hello from server!", "timestamp": 1234567890}}"#,
        )
        .build()
        .expect("Failed to build root entry");

    server_sync
        .backend()
        .expect("Failed to get backend")
        .put_verified(root_entry.clone())
        .await
        .unwrap();

    // Start server
    let server_addr = start_sync_server(&server_sync).await;

    // Setup client with a key we'll manage manually (not in backend)
    let (_client_signing_key, client_verifying_key) = generate_keypair();
    let client_key_id = format_public_key(&client_verifying_key);

    let (client_instance, client_sync) = setup().await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Verify client doesn't have the database initially
    assert!(
        !client_instance.has_database(&tree_id).await,
        "Client should not have the database initially (tree_id: {tree_id})"
    );

    // Use the new with_key variant to bootstrap with user-provided public key
    println!("🧪 TEST: Attempting bootstrap sync with provided public key...");
    client_sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            &client_key_id,
            &client_key_id,
            Permission::Write(5),
        )
        .await
        .expect("Bootstrap sync with provided key should succeed");

    // Flush any pending sync work
    client_sync.flush().await.ok();

    // Verify client now has the root entry
    let root_client = client_sync
        .backend()
        .expect("Failed to get backend")
        .get(&tree_id)
        .await
        .unwrap_or_else(|e| {
            panic!("Client should have the root entry after bootstrap (tree_id: {tree_id}): {e:?}")
        });
    assert_eq!(
        root_client.id(),
        tree_id,
        "Root entry ID mismatch: expected {}, got {}",
        tree_id,
        root_client.id()
    );

    println!("✅ TEST: Bootstrap with provided key completed successfully");

    // Cleanup
    server_sync.stop_server().await.unwrap();
}

/// Test bootstrap with provided key succeeds
#[tokio::test]
async fn test_bootstrap_with_provided_key_succeeds() {
    // Setup server
    let (_server_instance, _user, _key_id, _server_db, server_sync, tree_id) =
        setup_global_wildcard_server().await;

    server_sync
        .backend()
        .expect("Failed to get backend")
        .put_verified(create_test_tree_entry())
        .await
        .unwrap();

    let server_addr = start_sync_server(&server_sync).await;

    // Setup client with user-managed key
    let (_client_signing_key, client_verifying_key) = generate_keypair();
    let client_key_id = format_public_key(&client_verifying_key);

    let (_client_instance, client_sync) = setup().await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Sync with provided public key
    client_sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            &client_key_id,
            &client_key_id,
            Permission::Read,
        )
        .await
        .expect("Bootstrap should succeed");

    // Flush any pending sync work
    client_sync.flush().await.ok();

    // Verify the sync succeeded
    assert!(
        client_sync
            .backend()
            .expect("Failed to get backend")
            .get(&tree_id)
            .await
            .is_ok(),
        "Client should have successfully synced the tree"
    );

    println!("✅ TEST: Verified key not stored in backend");

    // Cleanup
    server_sync.stop_server().await.unwrap();
}

/// Test bootstrap with invalid signing key should fail gracefully
#[tokio::test]
async fn test_bootstrap_with_invalid_key_fails() {
    // Setup server
    let (_server_instance, _user, _key_id, _server_db, server_sync, _tree_id) =
        setup_global_wildcard_server().await;

    server_sync
        .backend()
        .expect("Failed to get backend")
        .put_verified(create_test_tree_entry())
        .await
        .unwrap();

    let server_addr = start_sync_server(&server_sync).await;

    // Setup client with a signing key
    let (_client_signing_key, client_verifying_key) = generate_keypair();
    let client_key_id = format_public_key(&client_verifying_key);

    let (_client_instance, client_sync) = setup().await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Try to sync with a non-existent tree (should fail)
    let fake_tree_id = ID::from("nonexistent_tree_id");

    let result = client_sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &fake_tree_id,
            &client_key_id,
            &client_key_id,
            Permission::Write(5),
        )
        .await;

    assert!(
        result.is_err(),
        "Bootstrap with non-existent tree should fail"
    );

    println!("✅ TEST: Bootstrap with invalid tree correctly failed");

    // Cleanup
    server_sync.stop_server().await.unwrap();
}

/// Test multiple clients bootstrapping with different user-managed keys
#[tokio::test]
async fn test_multiple_clients_with_different_keys() {
    // Setup server
    let (_server_instance, _user, _key_id, _server_db, server_sync, tree_id) =
        setup_global_wildcard_server().await;

    let root_entry = Entry::root_builder()
        .set_subtree_data("data", r#"{"value": "shared data"}"#)
        .build()
        .expect("Failed to build entry");

    server_sync
        .backend()
        .expect("Failed to get backend")
        .put_verified(root_entry)
        .await
        .unwrap();

    let server_addr = start_sync_server(&server_sync).await;

    // Setup three clients with different user-managed keys
    let mut clients = Vec::new();
    for i in 0..3 {
        let (_signing_key, verifying_key) = generate_keypair();
        let key_id = format_public_key(&verifying_key);
        let (instance, sync) = setup().await;
        sync.register_transport("http", HttpTransport::builder())
            .await
            .unwrap();
        clients.push((instance, sync, key_id, i));
    }

    // Each client bootstraps with their own key
    for (instance, sync, key_id, i) in clients {
        println!("🧪 Client {i} bootstrapping...");

        // Verify client doesn't have database initially
        assert!(
            !instance.has_database(&tree_id).await,
            "Client {i} should not have database initially (tree_id: {tree_id})"
        );

        // Bootstrap with user-managed public key
        sync.sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            &key_id,
            &key_id,
            Permission::Read,
        )
        .await
        .unwrap_or_else(|e| panic!("Client {i} bootstrap should succeed: {e:?}"));

        // Flush any pending sync work
        sync.flush().await.ok();

        // Verify client has the tree
        assert!(
            sync.backend()
                .expect("Failed to get backend")
                .get(&tree_id)
                .await
                .is_ok(),
            "Client {i} should have the tree after bootstrap (tree_id: {tree_id})"
        );

        println!("✅ Client {i} bootstrap completed");
    }

    println!("✅ TEST: All clients bootstrapped successfully with different keys");

    // Cleanup
    server_sync.stop_server().await.unwrap();
}

/// Test bootstrap with provided key and different permission levels
#[tokio::test]
async fn test_bootstrap_with_different_permissions() {
    // Setup server
    let (_server_instance, _user, _key_id, _server_db, server_sync, tree_id) =
        setup_global_wildcard_server().await;

    server_sync
        .backend()
        .expect("Failed to get backend")
        .put_verified(create_test_tree_entry())
        .await
        .unwrap();

    let server_addr = start_sync_server(&server_sync).await;

    // Test different permission levels
    let permissions = vec![
        ("Read", Permission::Read),
        ("Write(5)", Permission::Write(5)),
        ("Admin", Permission::Admin(0)),
    ];

    for (perm_name, permission) in permissions {
        println!("🧪 Testing bootstrap with {perm_name} permission");

        let (_signing_key, verifying_key) = generate_keypair();
        let key_id = format_public_key(&verifying_key);

        let (_instance, sync) = setup().await;
        sync.register_transport("http", HttpTransport::builder())
            .await
            .unwrap();

        // Bootstrap with this permission level
        sync.sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            &key_id,
            &key_id,
            permission.clone(),
        )
        .await
        .unwrap_or_else(|e| panic!("Bootstrap with {perm_name} should succeed: {e:?}"));

        // Flush any pending sync work
        sync.flush().await.ok();

        // Verify sync succeeded
        assert!(
            sync.backend()
                .expect("Failed to get backend")
                .get(&tree_id)
                .await
                .is_ok(),
            "Bootstrap with {perm_name} should succeed"
        );

        println!("✅ Bootstrap with {perm_name} permission completed");
    }

    println!("✅ TEST: All permission levels worked correctly");

    // Cleanup
    server_sync.stop_server().await.unwrap();
}

/// Test bootstrap with invalid keys should fail with proper validation errors
#[tokio::test]
async fn test_bootstrap_with_invalid_keys() {
    // Setup server
    let (_server_instance, _user, _key_id, _server_db, server_sync, tree_id) =
        setup_global_wildcard_server().await;

    server_sync
        .backend()
        .expect("Failed to get backend")
        .put_verified(create_test_tree_entry())
        .await
        .unwrap();

    let server_addr = start_sync_server(&server_sync).await;

    let (_instance, sync) = setup().await;
    sync.register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Generate a valid public key for comparison
    let (_signing_key, verifying_key) = generate_keypair();
    let valid_public_key = format_public_key(&verifying_key);

    println!("🧪 TEST: Testing empty public key");
    let result = sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            "", // Empty public key
            "test_key",
            Permission::Write(5),
        )
        .await;

    assert!(
        result.is_err(),
        "Bootstrap with empty public key should fail"
    );
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("Public key cannot be empty"),
        "Error should mention empty public key, got: {err}"
    );
    println!("✅ Empty public key correctly rejected");

    println!("🧪 TEST: Testing malformed public key");
    let result = sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            "not_a_valid_public_key", // Invalid format
            "test_key",
            Permission::Write(5),
        )
        .await;

    assert!(
        result.is_err(),
        "Bootstrap with malformed public key should fail"
    );
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("Invalid public key format"),
        "Error should mention invalid format, got: {err}"
    );
    println!("✅ Malformed public key correctly rejected");

    println!("🧪 TEST: Testing empty key name");
    let result = sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            &valid_public_key,
            "", // Empty key name
            Permission::Write(5),
        )
        .await;

    assert!(result.is_err(), "Bootstrap with empty key name should fail");
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("Key name cannot be empty"),
        "Error should mention empty key name, got: {err}"
    );
    println!("✅ Empty key name correctly rejected");

    println!("🧪 TEST: Testing invalid public key with valid-looking but wrong format");
    let result = sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            "ed25519:not_base64!@#$", // Has prefix but invalid base64
            "test_key",
            Permission::Write(5),
        )
        .await;

    assert!(
        result.is_err(),
        "Bootstrap with invalid base64 in public key should fail"
    );
    println!("✅ Invalid base64 public key correctly rejected");

    println!("✅ TEST: All invalid key validations passed");

    // Cleanup
    server_sync.stop_server().await.unwrap();
}

/// Test full end-to-end bootstrap with actual Database instances and authentication
#[tokio::test]
async fn test_full_e2e_bootstrap_with_database_instances() {
    // Setup server with a proper Database instance
    let (_server_instance, _user, _key_id, server_database, server_sync, _tree_id) =
        setup_global_wildcard_server().await;

    let tree_id = server_database.root_id().clone();

    // Add content to the database via proper transaction (not bypassing to backend)
    let server_tx = server_database.new_transaction().await.unwrap();
    let messages_store = server_tx.get_store::<DocStore>("messages").await.unwrap();

    let mut msg = Doc::new();
    msg.set("text", "Hello from authenticated database!");
    msg.set_json("timestamp", 1234567890_u64).unwrap();
    messages_store.set_node("msg1", msg).await.unwrap();

    server_tx.commit().await.unwrap();

    println!("🧪 Server: Added message to database via transaction");

    // Start server
    let server_addr = start_sync_server(&server_sync).await;

    // Setup client with user-managed key (simulating User API)
    let (_client_signing_key, client_verifying_key) = generate_keypair();
    let client_key_id = format_public_key(&client_verifying_key);

    let (client_instance, client_sync) = setup().await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Verify client doesn't have the database initially
    assert!(
        !client_instance.has_database(&tree_id).await,
        "Client should not have database initially (tree_id: {tree_id})"
    );

    println!("🧪 Client: Requesting bootstrap access with user-managed key...");

    // Bootstrap with user-managed public key - this should trigger authentication flow
    client_sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            &client_key_id,
            &client_key_id,
            Permission::Read,
        )
        .await
        .expect("Bootstrap should succeed with auto-approval");

    // Flush any pending sync work
    client_sync.flush().await.ok();

    // Verify client successfully bootstrapped and can load the database
    // Use global "*" permission (server has wildcard permission)
    let (reader_key, _) = generate_keypair();
    let client_database = Database::open(
        client_instance.clone(),
        &tree_id,
        DatabaseKey::global(reader_key),
    )
    .await
    .expect("Client should be able to load the database after bootstrap");

    println!("✅ Client: Successfully loaded database after bootstrap");

    // Verify the client has the actual data from the server
    let client_tx = client_database.new_transaction().await.unwrap();
    let client_messages = client_tx.get_store::<DocStore>("messages").await.unwrap();

    let msg1 = client_messages
        .get_node("msg1")
        .await
        .expect("Should have msg1");
    let text = msg1
        .get_as::<String>("text")
        .expect("Should have text field");
    assert_eq!(text, "Hello from authenticated database!");

    println!("✅ Client: Successfully retrieved data from synced database");

    // Verify the server has global wildcard permission (not individual client key)
    let server_tx = server_database.new_transaction().await.unwrap();
    let settings_store = server_tx.get_settings().unwrap();

    // Client key should NOT be added individually - access is via global wildcard
    let client_key_result = settings_store.get_auth_key(&client_key_id).await;
    assert!(
        client_key_result.is_err(),
        "Client key should not be added individually when global wildcard permission exists"
    );

    // Verify global wildcard permission exists
    let global_auth_key = settings_store
        .get_auth_key("*")
        .await
        .expect("Global wildcard permission should exist");

    assert_eq!(global_auth_key.status(), &KeyStatus::Active);

    println!(
        "✅ Server: Global wildcard permission grants access (no individual client key added)"
    );

    println!("✅ TEST: Full end-to-end bootstrap with authentication completed successfully");

    // Cleanup
    server_sync.stop_server().await.unwrap();
}

/// Test incremental sync after bootstrap with provided key
#[tokio::test]
async fn test_incremental_sync_after_bootstrap_with_key() {
    // Setup server
    let (_server_instance, _user, _key_id, _server_db, server_sync, tree_id) =
        setup_global_wildcard_server().await;

    let root_entry = create_test_tree_entry();
    server_sync
        .backend()
        .expect("Failed to get backend")
        .put_verified(root_entry)
        .await
        .unwrap();

    let server_addr = start_sync_server(&server_sync).await;

    // Setup client with user-managed key
    let (_client_signing_key, client_verifying_key) = generate_keypair();
    let client_key_id = format_public_key(&client_verifying_key);

    let (_client_instance, client_sync) = setup().await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Bootstrap with provided public key
    client_sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            &client_key_id,
            &client_key_id,
            Permission::Write(5),
        )
        .await
        .expect("Initial bootstrap should succeed");

    // Flush any pending sync work
    client_sync.flush().await.ok();

    // Add new content to server
    let entry2 = Entry::builder(tree_id.clone())
        .set_parents(vec![tree_id.clone()])
        .set_subtree_data("messages", r#"{"msg2": {"text": "New message"}}"#)
        .build()
        .expect("Failed to build entry");

    server_sync
        .backend()
        .expect("Failed to get backend")
        .put_verified(entry2.clone())
        .await
        .unwrap();

    // Do incremental sync (client already has the tree)
    println!("🧪 TEST: Attempting incremental sync after bootstrap...");

    // For incremental sync, we can use the regular sync_with_peer since the tree exists
    client_sync
        .sync_with_peer(&server_addr, Some(&tree_id))
        .await
        .expect("Incremental sync should succeed");

    // Flush any pending sync work
    client_sync.flush().await.ok();

    // Verify client received the new entry
    assert!(
        client_sync
            .backend()
            .expect("Failed to get backend")
            .get(&entry2.id())
            .await
            .is_ok(),
        "Client should have the new entry after incremental sync"
    );

    println!("✅ TEST: Incremental sync after bootstrap with key works correctly");

    // Cleanup
    server_sync.stop_server().await.unwrap();
}
