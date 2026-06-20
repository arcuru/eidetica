//! Test for bidirectional sync scenarios.
//!
//! Verifies that two peers can sync changes back and forth:
//! 1. Peer 0 creates a room and adds message A
//! 2. Peer 1 bootstraps the room from peer 0 (carries message A)
//! 3. Peer 1 adds message B
//! 4. Peer 1 syncs back to peer 0
//! 5. Peer 0 adds message C — CRDT merge handles the concurrent change, no
//!    "no common ancestor" error
//!
//! Built on [`Cluster`], the multi-peer harness: it owns the instance / user /
//! key / transport / bootstrap wiring so this test is just the scenario.

use eidetica::{
    Result,
    auth::{Permission, types::AuthKey},
    crdt::Doc,
    store::Table,
    testing::{Cluster, add_auth_keys, set_global_auth_key},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    author: String,
    content: String,
    timestamp: String, // Simplified to avoid chrono serde issues
}

impl ChatMessage {
    fn new(author: &str, content: &str) -> Self {
        Self {
            author: author.to_string(),
            content: content.to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                .to_string(),
        }
    }
}

/// Insert one chat message into the database's `messages` table.
async fn add_message(db: &eidetica::Database, msg: ChatMessage) -> Result<()> {
    let txn = db.new_transaction().await?;
    txn.get_store::<Table<ChatMessage>>("messages")
        .await?
        .insert(msg)
        .await?;
    txn.commit().await?;
    Ok(())
}

/// All messages currently in the database's `messages` table.
async fn all_messages(db: &eidetica::Database) -> Result<Vec<ChatMessage>> {
    let txn = db.new_transaction().await?;
    let store = txn.get_store::<Table<ChatMessage>>("messages").await?;
    let messages: Vec<(String, ChatMessage)> = store.search(|_| true).await?;
    Ok(messages.into_iter().map(|(_, m)| m).collect())
}

/// Test bidirectional sync between two peers.
///
/// Verifies the scenario:
/// 1. Peer 0 creates room and adds message A
/// 2. Peer 1 bootstraps from peer 0
/// 3. Peer 1 adds message B
/// 4. Peer 1 syncs back to peer 0
/// 5. Peer 0 adds message C (should succeed with proper CRDT merge)
#[tokio::test]
async fn test_bidirectional_sync_no_common_ancestor_issue() -> Result<()> {
    let mut net = Cluster::builder().peers(2).build().await?;

    // === STEP 1: Peer 0 creates the room and adds message A ===
    let key0 = net.peer(0).key_id().clone();
    let device0 = net.peer(0).instance().id();

    let mut settings = Doc::new();
    settings.set("name", "Bidirectional Test Room");
    let db0 = net
        .peer_mut(0)
        .user_mut()
        .create_database(settings, &key0)
        .await?;
    let room_id = db0.root_id().clone();

    // Auth: the user's key and the device key as admins, plus a global wildcard
    // so the bootstrapping peer can join and write.
    add_auth_keys(
        &db0,
        &[
            (&key0, AuthKey::active(Some("admin"), Permission::Admin(10))),
            (
                &device0,
                AuthKey::active(Some("device"), Permission::Admin(10)),
            ),
        ],
    )
    .await?;
    set_global_auth_key(&db0, AuthKey::active(None, Permission::Admin(10))).await?;

    net.peer_mut(0).serve(&room_id).await?;
    add_message(
        &db0,
        ChatMessage::new("alice", "Hello from Device 1 (Message A)"),
    )
    .await?;

    // === STEP 2: Peer 1 bootstraps from peer 0 and sees message A ===
    // bootstrap(from, to, ..): peer 1 is the joiner, peer 0 the source.
    net.bootstrap(0, 1, &room_id, Permission::Write(10)).await?;

    let db1 = net.peer_mut(1).user_mut().open_database(&room_id).await?;

    let messages = all_messages(&db1).await?;
    assert_eq!(
        messages.len(),
        1,
        "Peer 1 should have 1 message after bootstrap"
    );
    assert_eq!(messages[0].content, "Hello from Device 1 (Message A)");

    // === STEP 3: Peer 1 adds message B ===
    add_message(
        &db1,
        ChatMessage::new("bob", "Hello from Device 2 (Message B)"),
    )
    .await?;

    // === STEP 4: Peer 1 syncs back to peer 0 ===
    net.exchange(1, 0, &room_id).await?;

    let messages = all_messages(&db0).await?;
    assert_eq!(
        messages.len(),
        2,
        "Peer 0 should have 2 messages after sync back"
    );

    // === STEP 5: Peer 0 adds message C — the concurrent change CRDT must merge ===
    // This is where a "no common ancestor" regression would surface.
    add_message(
        &db0,
        ChatMessage::new("alice", "Hello again from Device 1 (Message C)"),
    )
    .await?;

    let messages = all_messages(&db0).await?;
    assert_eq!(
        messages.len(),
        3,
        "Peer 0 should have 3 messages after adding C"
    );
    let contents: Vec<&str> = messages.iter().map(|m| m.content.as_str()).collect();
    assert!(contents.contains(&"Hello from Device 1 (Message A)"));
    assert!(contents.contains(&"Hello from Device 2 (Message B)"));
    assert!(contents.contains(&"Hello again from Device 1 (Message C)"));

    Ok(())
}
