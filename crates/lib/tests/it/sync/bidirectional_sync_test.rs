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
#[ignore = "BUG: Bidirectional sync 'no common ancestor' error - CRDT merge algorithm fails in specific sync scenarios"]
async fn test_bidirectional_sync_no_common_ancestor_issue() -> Result<()> {
    println!("\n🧪 TEST: Bidirectional sync causing no common ancestor error");

    // === STEP 1: Device 1 creates room and adds message A ===
    println!("📱 STEP 1: Device 1 creates room and adds message A");

    let mut device1_instance = Instance::new(Box::new(InMemory::new()))
        .with_sync()
        .expect("Failed to create device1 instance");

    device1_instance
        .add_private_key(CHAT_APP_KEY)
        .expect("Failed to add device1 key");

    let _device1_pubkey = device1_instance
        .get_formatted_public_key(CHAT_APP_KEY)
        .expect("Failed to get device1 public key")
        .expect("Device1 key should exist");

    // Create database with simple settings like the chat app
    let mut settings = Doc::new();
    settings.set_string("name", "Bidirectional Test Room");

    let mut device1_database = device1_instance
        .new_database(settings, CHAT_APP_KEY)
        .expect("Failed to create database on device1");

    let room_id = device1_database.root_id().clone();
    device1_database.set_default_auth_key(CHAT_APP_KEY);

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
        let sync = device1_instance
            .sync_mut()
            .expect("Device1 should have sync");
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

    let mut device2_instance = Instance::new(Box::new(InMemory::new()))
        .with_sync()
        .expect("Failed to create device2 instance");

    device2_instance
        .add_private_key(CHAT_APP_KEY)
        .expect("Failed to add device2 key");

    // Bootstrap sync from device 1 to device 2
    let bootstrap_result = {
        let device2_sync = device2_instance
            .sync_mut()
            .expect("Device2 should have sync");
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
    let mut device2_database = device2_instance
        .load_database(&room_id)
        .expect("Device2 should have the database after bootstrap");

    device2_database.set_default_auth_key(CHAT_APP_KEY);

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
        let device2_sync = device2_instance
            .sync_mut()
            .expect("Device2 should have sync");
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
            println!("✅ Bidirectional sync works correctly - message C added successfully");

            // Check final message count
            let op = device1_database.new_transaction()?;
            let messages_store = op.get_store::<Table<ChatMessage>>("messages")?;
            let messages: Vec<(String, ChatMessage)> = messages_store.search(|_| true)?;
            let messages: Vec<ChatMessage> = messages.into_iter().map(|(_, msg)| msg).collect();
            println!("📋 Device 1 final messages: {} messages", messages.len());
            for msg in &messages {
                println!("   - {}: {}", msg.author, msg.content);
            }

            // Verify we have all 3 messages (A, B, C)
            assert_eq!(
                messages.len(),
                3,
                "Should have all 3 messages when sync works correctly"
            );
        }
        Err(e) => {
            println!("❌ SYNC BUG STILL EXISTS: {:?}", e);
            let error_str = e.to_string();

            if error_str.to_lowercase().contains("ancestor") {
                panic!(
                    "SYNC BUG: 'no common ancestor' error still occurs in bidirectional sync - this needs to be fixed"
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
    let server_sync = device1_instance
        .sync_mut()
        .expect("Device1 should have sync");
    server_sync.stop_server_async().await.unwrap();

    println!("🧹 Test completed successfully");

    Ok(())
}
