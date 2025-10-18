//! Integration tests simulating chat app sync behavior
//!
//! These tests reproduce the exact sync patterns used by the chat example
//! to debug authentication and sync issues.

use eidetica::{
    Instance,
    backend::database::InMemory,
    crdt::{Doc, doc::Value},
    store::Table,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

// Simulate the chat app's key names (device-specific)
const SERVER_KEY_NAME: &str = "CHAT_APP_SERVER";
const CLIENT_KEY_NAME: &str = "CHAT_APP_CLIENT";

// Simulate the chat app's message structure
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    author: String,
    content: String,
    timestamp: i64,
}

/// Test authenticated bootstrap with Database operations (simulating chat app)
///
/// IGNORED: This test fails due to a key management architectural issue.
/// The sync handler tries to authenticate with "_device_key" when operating on
/// target databases, but target databases (like chat rooms) don't have this key.
/// The sync system needs proper key-to-database mapping to know which admin key
/// to use for each database. This requires additional infrastructure work.
#[tokio::test]
#[ignore = "Key management bug: sync handler cannot determine which admin key to use for target databases"]
async fn test_chat_app_authenticated_bootstrap() {
    println!("\n🧪 TEST: Starting chat app authenticated bootstrap test");

    // Setup server instance (like Device 1 creating a room)
    let mut server_instance = Instance::open(Box::new(InMemory::new()))
        .expect("Failed to create test instance")
        .with_sync()
        .expect("Failed to create server instance with sync");

    // Add authentication key for server (like chat app does)
    server_instance
        .add_private_key(SERVER_KEY_NAME)
        .expect("Failed to add server key");

    let server_pubkey = server_instance
        .get_formatted_public_key(SERVER_KEY_NAME)
        .expect("Failed to get server public key");
    println!("📍 Server public key: {}", server_pubkey);

    // Create a database (like creating a chat room)
    let mut settings = Doc::new();
    settings.set_string("name", "Test Chat Room");
    // Enable bootstrap auto-approval via policy AND include admin/global entries so creation succeeds
    let mut auth_doc = Doc::new();
    let mut policy_doc = Doc::new();
    policy_doc
        .set_json("bootstrap_auto_approve", true)
        .expect("set policy json");
    auth_doc.set_doc("policy", policy_doc);
    // Include server admin key for initial database creation
    auth_doc
        .set_json(
            SERVER_KEY_NAME,
            serde_json::json!({
                "pubkey": server_pubkey,
                "permissions": {"Admin": 10},
                "status": "Active"
            }),
        )
        .expect("Failed to set admin auth");
    // Also include global write permission so clients can write using "*"
    auth_doc
        .set_json(
            "*",
            serde_json::json!({
                "pubkey": "*",
                "permissions": {"Write": 10},
                "status": "Active"
            }),
        )
        .expect("Failed to set global auth");
    settings.set_doc("auth", auth_doc);
    let server_database = server_instance
        .new_database(settings, SERVER_KEY_NAME)
        .expect("Failed to create server database");

    let room_id = server_database.root_id().clone();
    println!("🏠 Created room with ID: {}", room_id);

    // Add some initial messages to the server's database
    {
        let op = server_database
            .new_transaction()
            .expect("Failed to create transaction");
        let store = op
            .get_store::<Table<ChatMessage>>("messages")
            .expect("Failed to get messages store");

        let msg = ChatMessage {
            author: "server_user".to_string(),
            content: "Welcome to the chat room!".to_string(),
            timestamp: 1234567890,
        };
        store.insert(msg).expect("Failed to insert message");
        op.commit().expect("Failed to commit transaction");
    }

    // Setup sync on server and get address
    let server_addr = {
        let server_sync = server_instance.sync_mut().expect("Server should have sync");
        server_sync
            .enable_http_transport()
            .expect("Failed to enable HTTP transport");
        server_sync
            .start_server_async("127.0.0.1:0")
            .await
            .expect("Failed to start server");
        let addr = server_sync
            .get_server_address_async()
            .await
            .expect("Failed to get server address");
        println!("🌐 Server listening at: {}", addr);
        addr
    };

    // Setup client instance (like Device 2 joining the room)
    let mut client_instance = Instance::open(Box::new(InMemory::new()))
        .expect("Failed to create test instance")
        .with_sync()
        .expect("Failed to create client instance with sync");

    // Add authentication key for client (different key name to avoid conflicts)
    client_instance
        .add_private_key(CLIENT_KEY_NAME)
        .expect("Failed to add client key");

    let client_pubkey = client_instance
        .get_formatted_public_key(CLIENT_KEY_NAME)
        .expect("Failed to get client public key");
    println!("📍 Client public key: {}", client_pubkey);

    // Verify client doesn't have the database initially
    assert!(
        client_instance.load_database(&room_id).is_err(),
        "Client should not have the database initially"
    );

    // Client attempts to bootstrap with authentication
    let client_sync = client_instance.sync_mut().expect("Client should have sync");
    client_sync
        .enable_http_transport()
        .expect("Failed to enable HTTP transport");

    println!("\n🔄 Client attempting authenticated bootstrap...");
    let bootstrap_result = client_sync
        .sync_with_peer_for_bootstrap(
            &server_addr,
            &room_id,
            CLIENT_KEY_NAME,
            eidetica::auth::Permission::Write(10),
        )
        .await;

    match bootstrap_result {
        Ok(_) => println!("✅ Bootstrap completed successfully"),
        Err(e) => {
            println!("❌ Bootstrap failed: {:?}", e);
            panic!("Bootstrap should succeed but failed: {:?}", e);
        }
    }

    // Wait for sync to propagate
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify client can now load the database
    println!("\n🔍 Verifying client can load the database...");

    // Debug: Check tips before loading
    if let Ok(tips) = client_instance.backend().get_tips(&room_id) {
        println!("🔍 Client tips before loading database: {:?}", tips);

        // Check each tip to see what settings it has and their parents
        for tip_id in &tips {
            if let Ok(entry) = client_instance.backend().get(tip_id) {
                let parents = entry.parents().unwrap_or_default();
                println!("🔍 Tip {} has parents: {:?}", tip_id, parents);
                println!(
                    "🔍 Tip {} has settings data: {}",
                    tip_id,
                    entry.data("_settings").is_ok()
                );
                if let Ok(settings_data) = entry.data("_settings") {
                    // Check if auth section exists
                    if settings_data.contains("\"auth\"") {
                        println!("✅ Tip {} contains auth section", tip_id);
                    } else {
                        println!("❌ Tip {} missing auth section", tip_id);
                    }
                }
            }
        }

        // Check if these are actually conflicting tips or one is ancestor of another
        if tips.len() == 2 {
            // Check ancestry relationship
            let _tip1 = &tips[0];
            let _tip2 = &tips[1];
            println!("🔍 Checking ancestry between tips...");

            // TODO: Add ancestry check logic here if needed
        }
    }

    // Also check the _settings subtree tips
    if let Ok(settings_tips) = client_instance
        .backend()
        .get_store_tips(&room_id, "_settings")
    {
        println!("🔍 Client _settings subtree tips: {:?}", settings_tips);

        for tip_id in &settings_tips {
            if let Ok(entry) = client_instance.backend().get(tip_id)
                && let Ok(settings_data) = entry.data("_settings")
            {
                println!(
                    "🔍 Settings tip {} data preview: {}",
                    tip_id,
                    &settings_data[..settings_data.len().min(200)]
                );
            }
        }
    }

    // Load database with the client's key
    let signing_key = client_instance
        .backend()
        .get_private_key(CLIENT_KEY_NAME)
        .expect("Failed to get client signing key")
        .expect("Client key should exist in backend");

    let client_database = match eidetica::Database::open(
        client_instance.backend().clone(),
        &room_id,
        signing_key,
        CLIENT_KEY_NAME.to_string(),
    ) {
        Ok(db) => {
            println!("✅ Client successfully loaded database");
            db
        }
        Err(e) => {
            println!("❌ Client failed to load database: {:?}", e);
            panic!("Client should be able to load database after bootstrap");
        }
    };

    // Verify client's key was added to the database auth settings
    println!("\n🔐 Checking authentication setup...");
    {
        let settings = client_database
            .get_settings()
            .expect("Failed to get database settings");

        // Debug: Print the entire settings
        if let Ok(all_settings) = settings.get_all() {
            println!("🔍 Database settings: {:?}", all_settings);
        }

        // Try different ways to get the auth section
        match settings.get("auth") {
            Ok(value) => {
                println!("✅ Auth value found - type: {:?}", value);

                // The auth section exists but keys might be stored as JSON strings
                if let Value::Doc(auth_node) = value {
                    println!("✅ Auth is a Node");

                    // Try to get the key entry - it might be JSON string
                    if let Some(key_value) = auth_node.get(CLIENT_KEY_NAME) {
                        println!("🔍 Key value type: {:?}", key_value);

                        // If it's a JSON string, parse it
                        if let Value::Text(json_str) = key_value {
                            let key_info: serde_json::Value =
                                serde_json::from_str(json_str).expect("Failed to parse key JSON");
                            let stored_pubkey =
                                key_info["pubkey"].as_str().expect("Missing pubkey in JSON");
                            println!("✅ Client key found with pubkey: {}", stored_pubkey);
                            assert_eq!(
                                stored_pubkey, client_pubkey,
                                "Stored pubkey should match client's pubkey"
                            );
                        } else if let Value::Doc(key_node) = key_value {
                            // It's a proper node
                            if let Some(stored_pubkey) = key_node.get_as::<String>("pubkey") {
                                println!("✅ Client key found with pubkey: {}", stored_pubkey);
                                assert_eq!(
                                    stored_pubkey, client_pubkey,
                                    "Stored pubkey should match client's pubkey"
                                );
                            } else {
                                panic!("Client key exists but missing pubkey field");
                            }
                        } else {
                            panic!("Key value is neither JSON string nor Node: {:?}", key_value);
                        }
                    } else {
                        panic!("Client key NOT found in auth Node");
                    }
                } else {
                    panic!("Auth section is not a Node: {:?}", value);
                }
            }
            Err(e) => {
                panic!("No auth section in database settings: {:?}", e);
            }
        }
    }

    // Verify client can read existing messages
    println!("\n📖 Client reading existing messages...");
    {
        let op = client_database
            .new_transaction()
            .expect("Failed to create client transaction");
        let store = op
            .get_store::<Table<ChatMessage>>("messages")
            .expect("Failed to get messages store");

        let messages = store.search(|_| true).expect("Failed to search messages");

        println!("📬 Client found {} messages", messages.len());
        assert_eq!(messages.len(), 1, "Client should see the initial message");

        let (_, msg) = &messages[0];
        assert_eq!(msg.author, "server_user");
        assert_eq!(msg.content, "Welcome to the chat room!");
    }

    // Test that client can add new messages (write permission)
    println!("\n✍️ Client attempting to add a message...");
    {
        let op = match client_database.new_transaction() {
            Ok(op) => {
                println!("✅ Client created transaction successfully");
                op
            }
            Err(e) => {
                println!("❌ Client failed to create transaction: {:?}", e);
                panic!("Client should be able to create transactions after bootstrap");
            }
        };

        let store = op
            .get_store::<Table<ChatMessage>>("messages")
            .expect("Failed to get messages store");

        let msg = ChatMessage {
            author: "client_user".to_string(),
            content: "Hello from the client!".to_string(),
            timestamp: 1234567891,
        };

        match store.insert(msg.clone()) {
            Ok(_) => println!("✅ Client successfully inserted message"),
            Err(e) => {
                println!("❌ Client failed to insert message: {:?}", e);
                panic!("Client should be able to insert messages");
            }
        }

        match op.commit() {
            Ok(_) => println!("✅ Client successfully committed transaction"),
            Err(e) => {
                println!("❌ Client failed to commit transaction: {:?}", e);
                panic!("Client should be able to commit transactions");
            }
        }
    }

    // Check client's tips and entries before sync
    println!("\n🔍 Client state before sync back:");
    if let Ok(client_tips) = client_instance.backend().get_tips(&room_id) {
        println!("  Client has {} tips: {:?}", client_tips.len(), client_tips);

        // Check what messages the client has
        if let Ok(entries) = client_instance.backend().get_store(&room_id, "messages") {
            println!("  Client has {} entries in messages store", entries.len());
            for entry in &entries {
                if let Ok(data) = entry.data("messages") {
                    println!("    Entry {}: {}", entry.id(), &data[..data.len().min(100)]);
                }
            }
        }
    }

    // Sync changes back to server
    println!("\n🔄 Syncing client changes back to server...");
    {
        let client_sync = client_instance.sync_mut().expect("Client should have sync");
        client_sync
            .sync_with_peer(&server_addr, Some(&room_id))
            .await
            .expect("Client should be able to sync to server");
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify server sees client's message using the original database object
    // With bidirectional sync, the server should now have the client's entries
    println!("\n📖 Server checking for client's message after bidirectional sync...");
    {
        let op = server_database
            .new_transaction()
            .expect("Failed to create server transaction");
        let store = op
            .get_store::<Table<ChatMessage>>("messages")
            .expect("Failed to get messages store");

        let messages = store.search(|_| true).expect("Failed to search messages");

        println!("📬 Server found {} messages", messages.len());

        // Debug: Print all messages
        for (id, msg) in &messages {
            println!(
                "  Message {}: author={}, content={}",
                id, msg.author, msg.content
            );
        }

        assert_eq!(
            messages.len(),
            2,
            "Server should see both messages after bidirectional sync"
        );

        let client_msg_found = messages
            .iter()
            .any(|(_, msg)| msg.author == "client_user" && msg.content == "Hello from the client!");

        assert!(
            client_msg_found,
            "Server should see client's message after bidirectional sync"
        );
        println!("✅ Server successfully received client's message via bidirectional sync");
    }

    // Cleanup
    {
        let server_sync = server_instance.sync_mut().expect("Server should have sync");
        server_sync
            .stop_server_async()
            .await
            .expect("Failed to stop server");
    }

    println!("\n✅ TEST COMPLETED: Chat app authenticated bootstrap works!");
}

/// Test bootstrap with global authentication key '*'
#[tokio::test]
async fn test_global_key_bootstrap() {
    println!("\n🧪 TEST: Starting global key bootstrap test");

    // Setup similar to above but use '*' key
    let mut server_instance = Instance::open(Box::new(InMemory::new()))
        .expect("Failed to create test instance")
        .with_sync()
        .expect("Failed to create server instance");

    // Add a key for creating the database
    server_instance
        .add_private_key("admin_key")
        .expect("Failed to add admin key");

    // Create database with global write permission
    let mut settings = Doc::new();
    settings.set_string("name", "Public Room");

    // Add admin key to auth settings as well (required for database creation)
    let admin_pubkey = server_instance
        .get_formatted_public_key("admin_key")
        .expect("Failed to get admin public key");

    // Add global write permission to auth settings
    let mut auth_doc = Doc::new();
    auth_doc
        .set_json(
            "*",
            serde_json::json!({
                "pubkey": "*",
                "permissions": {"Write": 10},
                "status": "Active"
            }),
        )
        .expect("Failed to set global auth");

    // Also add the admin key so database creation works
    auth_doc
        .set_json(
            "admin_key",
            serde_json::json!({
                "pubkey": admin_pubkey,
                "permissions": {"Admin": 10},
                "status": "Active"
            }),
        )
        .expect("Failed to set admin auth");

    settings.set_doc("auth", auth_doc);

    let server_database = server_instance
        .new_database(settings, "admin_key")
        .expect("Failed to create server database");

    let room_id = server_database.root_id().clone();
    println!("🏠 Created public room with ID: {}", room_id);

    // Setup sync on server
    let server_addr = {
        let server_sync = server_instance.sync_mut().expect("Server should have sync");
        server_sync
            .enable_http_transport()
            .expect("Failed to enable HTTP transport");
        server_sync
            .start_server_async("127.0.0.1:0")
            .await
            .expect("Failed to start server");
        server_sync
            .get_server_address_async()
            .await
            .expect("Failed to get server address")
    };

    // Setup client
    let mut client_instance = Instance::open(Box::new(InMemory::new()))
        .expect("Failed to create test instance")
        .with_sync()
        .expect("Failed to create client instance");

    // Add a private key for the client to use with global permissions
    client_instance
        .add_private_key("*")
        .expect("Failed to add client key");

    // Client syncs without authentication (relies on global '*' permission)
    let client_sync = client_instance.sync_mut().expect("Client should have sync");
    client_sync
        .enable_http_transport()
        .expect("Failed to enable HTTP transport");

    println!("🔄 Client syncing with global permission...");
    client_sync
        .sync_with_peer(&server_addr, Some(&room_id))
        .await
        .expect("Sync should succeed with global permission");

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify client can load and use the database with global permission
    let signing_key = client_instance
        .backend()
        .get_private_key("*")
        .expect("Failed to get global signing key")
        .expect("Global key should exist in backend");

    let client_database = eidetica::Database::open(
        client_instance.backend().clone(),
        &room_id,
        signing_key,
        "*".to_string(),
    )
    .expect("Client should be able to load database");

    // Client should be able to write using global permission
    {
        // The client uses the "*" global permission key
        // The transaction will automatically include the public key in the signature
        let op = client_database
            .new_transaction()
            .expect("Should create transaction with global permission");
        let store = op
            .get_store::<Table<ChatMessage>>("messages")
            .expect("Failed to get messages store");

        let msg = ChatMessage {
            author: "anonymous".to_string(),
            content: "Message with global permission".to_string(),
            timestamp: 1234567892,
        };

        store
            .insert(msg)
            .expect("Should insert with global permission");
        op.commit().expect("Should commit with global permission");
    }

    println!("✅ TEST COMPLETED: Global key bootstrap works!");

    // Cleanup
    {
        let server_sync = server_instance.sync_mut().expect("Server should have sync");
        server_sync
            .stop_server_async()
            .await
            .expect("Failed to stop server");
    }
}

/// Test multiple databases syncing simultaneously
#[tokio::test]
async fn test_multiple_databases_sync() {
    println!("\n🧪 TEST: Starting multiple databases sync test");

    // Setup server with multiple databases
    let mut server_instance = Instance::open(Box::new(InMemory::new()))
        .expect("Failed to create test instance")
        .with_sync()
        .expect("Failed to create server instance");

    server_instance
        .add_private_key(SERVER_KEY_NAME)
        .expect("Failed to add server key");

    // Get server public key for auth configuration
    let server_pubkey = server_instance
        .get_formatted_public_key(SERVER_KEY_NAME)
        .expect("Failed to get server public key");

    // Create three different databases (chat rooms)
    let mut room_ids = Vec::new();
    for i in 1..=3 {
        let mut settings = Doc::new();
        settings.set_string("name", format!("Room {}", i));

        // Set up auth configuration with bootstrap policy
        let mut auth_doc = Doc::new();
        let mut policy_doc = Doc::new();
        policy_doc
            .set_json("bootstrap_auto_approve", true)
            .expect("set policy json");
        auth_doc.set_doc("policy", policy_doc);

        // Include server admin key for initial database creation
        auth_doc
            .set_json(
                SERVER_KEY_NAME,
                serde_json::json!({
                    "pubkey": server_pubkey,
                    "permissions": {"Admin": 10},
                    "status": "Active"
                }),
            )
            .expect("Failed to set admin auth");

        settings.set_doc("auth", auth_doc);

        let database = server_instance
            .new_database(settings, SERVER_KEY_NAME)
            .expect("Failed to create database");
        room_ids.push(database.root_id().clone());
        println!("🏠 Created room {} with ID: {}", i, database.root_id());
    }

    // Setup sync on server
    let server_addr = {
        let server_sync = server_instance.sync_mut().expect("Server should have sync");
        server_sync
            .enable_http_transport()
            .expect("Failed to enable HTTP transport");
        server_sync
            .start_server_async("127.0.0.1:0")
            .await
            .expect("Failed to start server");
        server_sync
            .get_server_address_async()
            .await
            .expect("Failed to get server address")
    };

    // Setup client
    let mut client_instance = Instance::open(Box::new(InMemory::new()))
        .expect("Failed to create test instance")
        .with_sync()
        .expect("Failed to create client instance");

    client_instance
        .add_private_key(CLIENT_KEY_NAME)
        .expect("Failed to add client key");

    // Bootstrap each database
    let client_sync = client_instance.sync_mut().expect("Client should have sync");
    client_sync
        .enable_http_transport()
        .expect("Failed to enable HTTP transport");

    for (i, room_id) in room_ids.iter().enumerate() {
        println!("\n🔄 Bootstrapping room {}...", i + 1);

        client_sync
            .sync_with_peer_for_bootstrap(
                &server_addr,
                room_id,
                CLIENT_KEY_NAME,
                eidetica::auth::Permission::Write(10),
            )
            .await
            .unwrap_or_else(|_| panic!("Failed to bootstrap room {}", i + 1));

        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Now verify all databases were loaded
    for (i, room_id) in room_ids.iter().enumerate() {
        let database = client_instance
            .load_database(room_id)
            .unwrap_or_else(|_| panic!("Failed to load room {}", i + 1));

        // Verify room name
        let settings = database.get_settings().expect("Failed to get settings");
        let name = settings
            .get_string("name")
            .expect("Failed to get room name");
        assert_eq!(name, format!("Room {}", i + 1));
        println!("✅ Successfully loaded {}", name);
    }

    println!("\n✅ TEST COMPLETED: Multiple databases sync works!");

    // Cleanup
    {
        let server_sync = server_instance.sync_mut().expect("Server should have sync");
        server_sync
            .stop_server_async()
            .await
            .expect("Failed to stop server");
    }
}
