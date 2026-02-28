//! Integration tests simulating chat app sync behavior.
//!
//! These tests verify bootstrap and sync patterns using the User API:
//!
//! - `test_chat_app_authenticated_bootstrap`: Manual approval flow where the server
//!   requires explicit admin approval before granting access. Client uses their
//!   registered key name as the SigKey.
//!
//! - `test_global_key_bootstrap`: Auto-approval flow where the server has a global
//!   wildcard permission that grants access to any client. Client uses "*" as the
//!   SigKey (with pubkey embedded in the signature).
//!
//! - `test_multiple_databases_sync`: Verifies syncing multiple databases from a
//!   single server.

use super::helpers::{
    enable_sync_for_instance_database, set_global_wildcard_permission, setup_sync_enabled_client,
    setup_sync_enabled_server, setup_sync_enabled_server_with_auto_approve, start_sync_server,
};
use crate::helpers::test_instance_with_user_and_key;
use eidetica::{
    Database,
    auth::{Permission, generate_keypair, types::SigKey},
    crdt::Doc,
    database::DatabaseKey,
    store::Table,
    sync::transports::http::HttpTransport,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ChatMessage {
    author: String,
    content: String,
    timestamp: i64,
}

/// Test authenticated bootstrap with manual approval.
///
/// Verifies that a client can bootstrap an authenticated database from a server
/// when the server requires manual approval (no global wildcard permission).
/// This tests the complete approval workflow:
/// 1. Client requests bootstrap → pending
/// 2. Admin approves the request
/// 3. Client retries → succeeds with their registered key
/// 4. Client can read/write data using their own key
#[tokio::test]
async fn test_chat_app_authenticated_bootstrap() {
    // Setup server WITHOUT auto-approval (no global wildcard permission)
    let (server_instance, server_user, server_key_id, server_database, tree_id, server_sync) =
        setup_sync_enabled_server("server_user", "server_key", "Chat Room").await;

    // Add initial message to the server database
    {
        let tx = server_database.new_transaction().await.unwrap();
        let store = tx
            .get_store::<Table<ChatMessage>>("messages")
            .await
            .unwrap();
        store
            .insert(ChatMessage {
                author: "server_user".to_string(),
                content: "Welcome to the chat room!".to_string(),
                timestamp: 1234567890,
            })
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    // Start server
    server_sync
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    let server_addr = start_sync_server(&server_sync).await;

    // Setup client
    let (client_instance, client_user, client_key_id, client_sync) =
        setup_sync_enabled_client("client_user", "client_key").await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // First bootstrap attempt should fail (pending approval)
    let client_key_str = client_key_id.to_string();
    let first_attempt = client_sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            &client_key_str,
            "client_key",
            Permission::Write(10),
        )
        .await;
    assert!(
        first_attempt.is_err(),
        "First bootstrap attempt should fail (pending approval)"
    );

    // Verify request is pending on server
    let pending_requests = server_sync
        .pending_bootstrap_requests()
        .await
        .expect("Should list pending requests");
    assert_eq!(
        pending_requests.len(),
        1,
        "Should have exactly one pending request"
    );
    let (request_id, pending_request) = &pending_requests[0];
    assert_eq!(pending_request.tree_id, tree_id);
    assert_eq!(pending_request.requesting_pubkey, client_key_str);

    // Admin approves the request
    server_user
        .approve_bootstrap_request(&server_sync, request_id, &server_key_id)
        .await
        .expect("Admin should approve bootstrap request");
    server_sync.flush().await.ok();

    // Client retries bootstrap - should now succeed
    // We still need to provide credentials since the database requires authentication
    client_sync
        .sync_with_peer_for_bootstrap_with_key(
            &server_addr,
            &tree_id,
            &client_key_str,
            "client_key",
            Permission::Write(10),
        )
        .await
        .expect("Bootstrap retry should succeed after approval");
    client_sync.flush().await.ok();

    // Verify client has the database
    assert!(
        client_instance.has_database(&tree_id).await,
        "Client should have database after bootstrap"
    );

    // Client opens database using their registered key (discovered via find_sigkeys)
    let sigkeys = Database::find_sigkeys(&client_instance, &tree_id, &client_key_str)
        .await
        .expect("Should find valid SigKeys");
    assert!(!sigkeys.is_empty(), "Should find at least one SigKey");

    let (sigkey, _) = &sigkeys[0];
    let sigkey_str = match sigkey {
        SigKey::Direct(hint) => hint
            .pubkey
            .clone()
            .or(hint.name.clone())
            .expect("Should have pubkey or name"),
        _ => panic!("Expected Direct SigKey"),
    };

    // The sigkey should be the client_key_id (pubkey) since keys are indexed by pubkey
    assert_eq!(
        sigkey_str, client_key_str,
        "Should use registered key's pubkey"
    );

    let client_signing_key = client_user
        .get_signing_key(&client_key_id)
        .expect("Should have signing key")
        .clone();

    let client_database = Database::open(
        client_instance.clone(),
        &tree_id,
        DatabaseKey::from_legacy_sigkey(client_signing_key, &sigkey_str),
    )
    .await
    .expect("Client should load database");

    // Verify client can read the initial message
    {
        let tx = client_database.new_transaction().await.unwrap();
        let store = tx
            .get_store::<Table<ChatMessage>>("messages")
            .await
            .unwrap();
        let messages = store.search(|_| true).await.unwrap();

        assert_eq!(messages.len(), 1, "Client should see initial message");
        assert_eq!(messages[0].1.content, "Welcome to the chat room!");
    }

    // Client adds a new message using their registered key
    {
        let tx = client_database.new_transaction().await.unwrap();
        let store = tx
            .get_store::<Table<ChatMessage>>("messages")
            .await
            .unwrap();
        store
            .insert(ChatMessage {
                author: "client_user".to_string(),
                content: "Hello from the client!".to_string(),
                timestamp: 1234567891,
            })
            .await
            .unwrap();
        tx.commit()
            .await
            .expect("Client should commit with registered key");
    }

    // Sync back to server
    client_sync
        .sync_with_peer(&server_addr, Some(&tree_id))
        .await
        .unwrap();
    client_sync.flush().await.ok();

    // Verify server received client's message
    {
        let tx = server_database.new_transaction().await.unwrap();
        let store = tx
            .get_store::<Table<ChatMessage>>("messages")
            .await
            .unwrap();
        let messages = store.search(|_| true).await.unwrap();

        assert_eq!(messages.len(), 2, "Server should see both messages");
        let client_msg = messages.iter().any(|(_, m)| m.author == "client_user");
        assert!(client_msg, "Server should have client's message");
    }

    // Cleanup
    server_sync.stop_server().await.unwrap();
    drop(server_instance);
}

/// Test bootstrap with global "*" permission (auto-approval).
///
/// Verifies that databases with global wildcard permission allow any client
/// to sync and write without explicit key registration. The client still signs
/// entries, but uses the global "*" as the SigKey instead of a registered key name.
#[tokio::test]
async fn test_global_key_bootstrap() {
    // Setup server with global wildcard permission (auto-approval)
    let (server_instance, _server_user, _server_key_id, server_database, tree_id, server_sync) =
        setup_sync_enabled_server_with_auto_approve("server_user", "server_key", "Public Room")
            .await;

    // Start server
    server_sync
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    let server_addr = start_sync_server(&server_sync).await;

    // Setup client
    let (client_instance, client_user, client_key_id, client_sync) =
        setup_sync_enabled_client("client_user", "client_key").await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Client syncs without bootstrap credentials (relies on global wildcard permission)
    client_sync
        .sync_with_peer(&server_addr, Some(&tree_id))
        .await
        .expect("Sync should succeed with global permission");
    client_sync.flush().await.ok();

    // Client opens database with global permission
    let client_key_str = client_key_id.to_string();
    let sigkeys = Database::find_sigkeys(&client_instance, &tree_id, &client_key_str)
        .await
        .expect("Should find valid SigKeys");

    let (sigkey, _) = &sigkeys[0];
    // Global permission is encoded as "*:ed25519:..." in the pubkey field
    assert!(sigkey.is_global(), "Should resolve to global permission");
    let sigkey_str = match sigkey {
        SigKey::Direct(hint) => hint
            .pubkey
            .clone()
            .or(hint.name.clone())
            .expect("Should have pubkey or name"),
        _ => panic!("Expected Direct SigKey"),
    };

    let client_signing_key = client_user
        .get_signing_key(&client_key_id)
        .expect("Should have signing key")
        .clone();

    let client_database = Database::open(
        client_instance.clone(),
        &tree_id,
        DatabaseKey::from_legacy_sigkey(client_signing_key, &sigkey_str),
    )
    .await
    .expect("Client should load database");

    // Client writes using global permission
    {
        let tx = client_database.new_transaction().await.unwrap();
        let store = tx
            .get_store::<Table<ChatMessage>>("messages")
            .await
            .unwrap();
        store
            .insert(ChatMessage {
                author: "anonymous".to_string(),
                content: "Message with global permission".to_string(),
                timestamp: 1234567892,
            })
            .await
            .unwrap();
        tx.commit()
            .await
            .expect("Should commit with global permission");
    }

    // Verify entry uses global permission key (encoded as "*:ed25519:...")
    let tips = client_instance.backend().get_tips(&tree_id).await.unwrap();
    let latest_entry = client_instance.backend().get(&tips[0]).await.unwrap();
    assert!(
        latest_entry.sig.key.is_global(),
        "Entry should use global permission key"
    );
    // For global permission, the actual pubkey should be recorded in the hint
    let hint = latest_entry.sig.hint();
    assert!(
        hint.pubkey.is_some() || hint.name.is_some(),
        "SigInfo should have key hint"
    );

    // Cleanup
    server_sync.stop_server().await.unwrap();
    drop(server_instance);
    drop(server_database);
}

/// Test multiple databases syncing simultaneously.
///
/// Verifies that a client can bootstrap and sync multiple databases from a server.
#[tokio::test]
async fn test_multiple_databases_sync() {
    // Setup server instance with sync
    let (server_instance, mut server_user, server_key_id) =
        test_instance_with_user_and_key("server_user", Some("server_key")).await;
    server_instance.enable_sync().await.unwrap();

    let server_sync = server_instance.sync().unwrap();

    // Create three databases
    let mut room_ids = Vec::new();
    for i in 1..=3 {
        let mut settings = Doc::new();
        settings.set("name", format!("Room {i}"));

        let database = server_user
            .create_database(settings, &server_key_id)
            .await
            .unwrap();

        // Add global wildcard permission for auto-approval
        set_global_wildcard_permission(&database).await.unwrap();

        let tree_id = database.root_id().clone();
        enable_sync_for_instance_database(&server_sync, &tree_id)
            .await
            .unwrap();

        room_ids.push(tree_id);
    }

    // Start server
    server_sync
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    let server_addr = start_sync_server(&server_sync).await;

    // Setup client
    let (client_instance, _client_user, client_key_id, client_sync) =
        setup_sync_enabled_client("client_user", "client_key").await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Bootstrap each database
    let client_key_str = client_key_id.to_string();
    for (i, room_id) in room_ids.iter().enumerate() {
        client_sync
            .sync_with_peer_for_bootstrap_with_key(
                &server_addr,
                room_id,
                &client_key_str,
                "client_key",
                Permission::Write(10),
            )
            .await
            .unwrap_or_else(|e| panic!("Failed to bootstrap room {}: {e}", i + 1));
        client_sync.flush().await.ok();
    }

    // Verify all databases were synced
    for (i, room_id) in room_ids.iter().enumerate() {
        assert!(
            client_instance.has_database(room_id).await,
            "Client should have room {}",
            i + 1
        );

        // Open and verify room name
        let (reader_key, _) = generate_keypair();
        let database = Database::open(
            client_instance.clone(),
            room_id,
            DatabaseKey::global(reader_key),
        )
        .await
        .unwrap_or_else(|e| panic!("Failed to load room {}: {e}", i + 1));

        let settings = database.get_settings().await.unwrap();
        let name = settings.get_string("name").await.unwrap();
        assert_eq!(name, format!("Room {}", i + 1));
    }

    // Cleanup
    server_sync.stop_server().await.unwrap();
    drop(server_instance);
}
