//! Test for bidirectional sync scenario that triggers "no common ancestor" error.
//!
//! This test reproduces the specific scenario:
//! 1. Device 1 creates room and adds message A
//! 2. Device 1 syncs to Device 2 (bootstrap)
//! 3. Device 2 adds message B
//! 4. Device 2 syncs back to Device 1
//! 5. Device 1 tries to add message C -> "no common ancestor found" error

use eidetica::{
    Instance, Result, auth::Permission, backend::database::InMemory, crdt::Doc, store::Table,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    author: String,
    content: String,
    timestamp: String, // Simplified to avoid chrono serde issues
}

impl ChatMessage {
    fn new(author: String, content: String) -> Self {
        Self {
            author,
            content,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                .to_string(),
        }
    }
}

const CHAT_APP_KEY: &str = "CHAT_APP_USER";

/// Test the exact scenario that causes "no common ancestor" error.
#[tokio::test]
#[ignore = "BUG: Signature verification fails during bidirectional sync"]
async fn test_bidirectional_sync_no_common_ancestor_issue() -> Result<()> {
    println!(
        "\n🧪 TEST: Bidirectional sync test (original sync bug fixed, now has signature verification issue)"
    );

    // === STEP 1: Device 1 creates room and adds message A ===
    println!("📱 STEP 1: Device 1 creates room and adds message A");

    let device1_instance =
        Instance::open(Box::new(InMemory::new())).expect("Failed to create test instance");
    device1_instance
        .enable_sync()
        .expect("Failed to initialize sync on device1");

    device1_instance
        .add_private_key(CHAT_APP_KEY)
        .expect("Failed to add device1 key");

    let _device1_pubkey = device1_instance
        .get_formatted_public_key(CHAT_APP_KEY)
        .expect("Failed to get device1 public key");

    // Create database with simple settings like the chat app
    let mut settings = Doc::new();
    settings.set_string("name", "Bidirectional Test Room");
    // Enable automatic bootstrap approval via global wildcard permission
    let mut auth_doc = Doc::new();
    // Include device1 admin key for initial database creation
    let device1_admin_pubkey = device1_instance
        .get_formatted_public_key(CHAT_APP_KEY)
        .expect("Failed to get device1 public key");
    auth_doc
        .set_json(
            CHAT_APP_KEY,
            serde_json::json!({
                "pubkey": device1_admin_pubkey,
                "permissions": {"Admin": 10},
                "status": "Active"
            }),
        )
        .expect("Failed to set admin auth");
    // Add global wildcard permission for automatic bootstrap approval
    auth_doc
        .set_json(
            "*",
            serde_json::json!({
                "pubkey": "*",
                "permissions": {"Admin": 10},
                "status": "Active"
            }),
        )
        .expect("Failed to set global wildcard permission");
    settings.set_doc("auth", auth_doc);

    let device1_database = device1_instance
        .new_database(settings, CHAT_APP_KEY)
        .expect("Failed to create database on device1");

    let room_id = device1_database.root_id().clone();

    // Add message A on device 1
    let message_a = ChatMessage::new(
        "alice".to_string(),
        "Hello from Device 1 (Message A)".to_string(),
    );
    println!("💬 Device 1 adding: {}", message_a.content);

    {
        let op = device1_database.new_transaction()?;
        let messages_store = op.get_store::<Table<ChatMessage>>("messages")?;
        messages_store.insert(message_a.clone())?;
        op.commit()?;
    }

    // Start server on device 1
    let device1_server_addr = {
        let sync = device1_instance.sync().expect("Device1 should have sync");
        sync.enable_http_transport()
            .expect("Failed to enable HTTP transport");
        sync.start_server_async("127.0.0.1:0")
            .await
            .expect("Failed to start server");
        sync.get_server_address_async()
            .await
            .expect("Failed to get server address")
    };

    println!("🌐 Device 1 server started at: {}", device1_server_addr);

    // === STEP 2: Device 2 bootstraps and syncs from Device 1 ===
    println!("\n📱 STEP 2: Device 2 bootstraps and syncs from Device 1");

    let device2_instance =
        Instance::open(Box::new(InMemory::new())).expect("Failed to create test instance");
    device2_instance
        .enable_sync()
        .expect("Failed to initialize sync on device2");

    device2_instance
        .add_private_key(CHAT_APP_KEY)
        .expect("Failed to add device2 key");

    // Bootstrap sync from device 1 to device 2
    let bootstrap_result = {
        let device2_sync = device2_instance.sync().expect("Device2 should have sync");
        device2_sync
            .enable_http_transport()
            .expect("Failed to enable HTTP transport");

        device2_sync
            .sync_with_peer_for_bootstrap(
                &device1_server_addr,
                &room_id,
                CHAT_APP_KEY,
                Permission::Write(10),
            )
            .await
    };

    println!("🔄 Bootstrap result: {:?}", bootstrap_result);
    assert!(bootstrap_result.is_ok(), "Bootstrap should succeed");

    // Wait for sync to complete
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify device 2 has the database and message A
    // Load database with the key for device2
    let signing_key = device2_instance
        .backend()
        .get_private_key(CHAT_APP_KEY)
        .expect("Failed to get device2 signing key")
        .expect("Device2 key should exist in backend");

    let device2_database = eidetica::Database::open(
        device2_instance.clone(),
        &room_id,
        signing_key,
        CHAT_APP_KEY.to_string(),
    )
    .expect("Failed to load database with key on device2");

    // Check device 2 has message A
    {
        let op = device2_database.new_transaction()?;
        let messages_store = op.get_store::<Table<ChatMessage>>("messages")?;
        let messages: Vec<(String, ChatMessage)> = messages_store.search(|_| true)?;
        let messages: Vec<ChatMessage> = messages.into_iter().map(|(_, msg)| msg).collect();
        println!(
            "📋 Device 2 messages after bootstrap: {} messages",
            messages.len()
        );
        for msg in &messages {
            println!("   - {}: {}", msg.author, msg.content);
        }
        assert_eq!(
            messages.len(),
            1,
            "Device 2 should have 1 message after bootstrap"
        );
        assert_eq!(messages[0].content, "Hello from Device 1 (Message A)");
    }

    // === STEP 3: Device 2 adds message B ===
    println!("\n📱 STEP 3: Device 2 adds message B");

    let message_b = ChatMessage::new(
        "bob".to_string(),
        "Hello from Device 2 (Message B)".to_string(),
    );
    println!("💬 Device 2 adding: {}", message_b.content);

    {
        let op = device2_database.new_transaction()?;
        let messages_store = op.get_store::<Table<ChatMessage>>("messages")?;
        messages_store.insert(message_b.clone())?;
        op.commit()?;
    }

    // === STEP 4: Device 2 syncs back to Device 1 ===
    println!("\n🔄 STEP 4: Device 2 syncs back to Device 1");

    let sync_back_result = {
        let device2_sync = device2_instance.sync().expect("Device2 should have sync");
        device2_sync
            .sync_with_peer(&device1_server_addr, Some(&room_id))
            .await
    };
    println!("🔄 Sync back result: {:?}", sync_back_result);
    assert!(sync_back_result.is_ok(), "Sync back should succeed");

    // Wait for sync to complete
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify device 1 now has both messages
    {
        let op = device1_database.new_transaction()?;
        let messages_store = op.get_store::<Table<ChatMessage>>("messages")?;
        let messages: Vec<(String, ChatMessage)> = messages_store.search(|_| true)?;
        let messages: Vec<ChatMessage> = messages.into_iter().map(|(_, msg)| msg).collect();
        println!(
            "📋 Device 1 messages after sync back: {} messages",
            messages.len()
        );
        for msg in &messages {
            println!("   - {}: {}", msg.author, msg.content);
        }
        assert_eq!(
            messages.len(),
            2,
            "Device 1 should have 2 messages after sync back"
        );
    }

    // === STEP 5: Device 1 tries to add message C (trigger "no common ancestor" error) ===
    println!("\n📱 STEP 5: Device 1 tries to add message C (this should trigger the error)");

    // Debug: Check current tips before adding message C
    let current_tips = device1_database
        .backend()
        .expect("Failed to get backend")
        .get_tips(&room_id)
        .expect("Failed to get tips");
    println!(
        "🔍 Device 1 current tree tips before adding C: {:?}",
        current_tips
    );
    let current_subtree_tips = device1_database
        .backend()
        .expect("Failed to get backend")
        .get_store_tips(&room_id, "messages")
        .expect("Failed to get store tips");
    println!(
        "🔍 Device 1 current messages store tips before adding C: {:?}",
        current_subtree_tips
    );

    // Debug: Show all entries in the tree to understand the DAG structure
    println!("🔍 All entries in Device 1's tree:");
    let all_entries = device1_database
        .backend()
        .expect("Failed to get backend")
        .get_tree(&room_id)
        .expect("Failed to get tree entries");
    for (i, entry) in all_entries.iter().enumerate() {
        let parents = entry.parents().unwrap_or_default();
        let subtrees = entry.subtrees();
        println!(
            "   {}. Entry {}: parents={:?}, subtrees={:?}",
            i + 1,
            entry.id(),
            parents,
            subtrees
        );

        // Show subtree parents for the messages store
        if subtrees.contains(&"messages".to_string())
            && let Ok(subtree_parents) = entry.subtree_parents("messages")
        {
            println!("      └─ messages subtree parents: {:?}", subtree_parents);
        }
    }

    let message_c = ChatMessage::new(
        "alice".to_string(),
        "Hello again from Device 1 (Message C)".to_string(),
    );
    println!("💬 Device 1 attempting to add: {}", message_c.content);

    // This is where the "no common ancestor" error should occur
    let add_result = {
        let op = device1_database.new_transaction()?;
        let messages_store = op.get_store::<Table<ChatMessage>>("messages")?;
        let insert_result = messages_store.insert(message_c.clone());
        match insert_result {
            Ok(_primary_key) => match op.commit() {
                Ok(_commit_id) => {
                    println!("✅ Message C added successfully (no error occurred)");
                    Ok(())
                }
                Err(e) => {
                    println!("❌ Error during commit: {:?}", e);
                    Err(e)
                }
            },
            Err(e) => {
                println!("❌ Error during insert: {:?}", e);
                Err(e)
            }
        }
    };

    match add_result {
        Ok(()) => {
            println!("🎉 SUCCESS: No common ancestor error did not occur - BUG IS FIXED!");

            // Check final message count
            let op = device1_database.new_transaction()?;
            let messages_store = op.get_store::<Table<ChatMessage>>("messages")?;
            let messages: Vec<(String, ChatMessage)> = messages_store.search(|_| true)?;
            let messages: Vec<ChatMessage> = messages.into_iter().map(|(_, msg)| msg).collect();
            println!("📋 Device 1 final messages: {} messages", messages.len());
            for msg in &messages {
                println!("   - {}: {}", msg.author, msg.content);
            }

            // Verify we have all 3 messages
            assert_eq!(
                messages.len(),
                3,
                "Device 1 should have 3 messages after adding C"
            );
            let contents: Vec<&str> = messages.iter().map(|m| m.content.as_str()).collect();
            assert!(contents.contains(&"Hello from Device 1 (Message A)"));
            assert!(contents.contains(&"Hello from Device 2 (Message B)"));
            assert!(contents.contains(&"Hello again from Device 1 (Message C)"));
        }
        Err(e) => {
            println!("🎯 ERROR STILL REPRODUCED: {:?}", e);
            let error_str = e.to_string();

            if error_str.to_lowercase().contains("ancestor") {
                panic!(
                    "SYNC BUG: 'no common ancestor' error still occurs during bidirectional sync - this needs to be fixed: {}",
                    e
                );
            } else {
                panic!(
                    "SYNC BUG: Unexpected error during bidirectional sync: {}",
                    e
                );
            }
        }
    }

    // Cleanup
    let server_sync = device1_instance.sync().expect("Device1 should have sync");
    server_sync.stop_server_async().await.unwrap();

    println!("🧹 Test completed successfully");

    Ok(())
}
