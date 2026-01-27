//! Multi-user tests: concurrent access, sharing, and isolation
//!
//! Tests multi-user scenarios including:
//! - User isolation
//! - Concurrent operations
//! - User enumeration

use eidetica::store::DocStore;

use super::helpers::*;

// ===== CONCURRENT DATABASE CREATION =====

#[tokio::test]
async fn test_many_users_create_databases() {
    let user_count = 5;
    let (instance, _) = setup_instance_with_users(&[
        ("user1", None),
        ("user2", None),
        ("user3", None),
        ("user4", None),
        ("user5", None),
    ])
    .await;

    let mut databases = Vec::new();

    for i in 1..=user_count {
        let username = format!("user{i}");
        let mut user = login_user(&instance, &username, None).await;
        let db = create_named_database(&mut user, &format!("Database {i}")).await;
        databases.push(db);
    }

    // All databases should have unique IDs
    for i in 0..databases.len() {
        for j in (i + 1)..databases.len() {
            assert_ne!(
                databases[i].root_id(),
                databases[j].root_id(),
                "Databases {i} and {j} should have different IDs"
            );
        }
    }
}

// ===== USER ISOLATION TESTS =====

#[tokio::test]
async fn test_user_cannot_access_another_users_key() {
    let (_instance, user1, user2, _, _) = setup_two_passwordless_users("alice", "bob").await;

    // Get Alice's key ID
    let alice_keys = user1.list_keys().expect("Alice should have keys");
    let alice_key_id = &alice_keys[0];

    // Bob tries to get Alice's key
    let result = user2.get_signing_key(alice_key_id);

    // Should fail
    assert!(
        result.is_err(),
        "User should not be able to access another user's key"
    );
}

#[tokio::test]
async fn test_users_have_independent_key_lists() {
    let (_instance, mut user1, mut user2, _, _) =
        setup_two_passwordless_users("alice", "bob").await;

    // Alice adds a key
    let _alice_new_key = add_user_key(&mut user1, Some("Alice New Key")).await;

    // Bob adds a key
    let _bob_new_key = add_user_key(&mut user2, Some("Bob New Key")).await;

    // Alice should have 2 keys, Bob should have 2 keys
    assert_user_key_count(&user1, 2);
    assert_user_key_count(&user2, 2);

    // But they should be different keys
    let alice_keys = user1.list_keys().expect("Alice keys");
    let bob_keys = user2.list_keys().expect("Bob keys");

    // No overlap
    for alice_key in &alice_keys {
        assert!(
            !bob_keys.contains(alice_key),
            "Bob should not have Alice's keys"
        );
    }
}

// ===== DATABASE SHARING TESTS (via Bootstrap) =====

#[tokio::test]
async fn test_database_created_by_one_user() {
    let (_instance, mut user1, _user2, _, _) = setup_two_passwordless_users("alice", "bob").await;

    // Alice creates a database
    let alice_db = create_named_database(&mut user1, "Alice's Database").await;

    // Verify Alice can find the key for it
    let key = user1
        .find_key(alice_db.root_id())
        .expect("Should not error");

    assert!(key.is_some(), "Alice should have key for her database");
}

// ===== CONCURRENT LOGIN TESTS =====

#[tokio::test]
async fn test_multiple_simultaneous_logins() {
    let (instance, _) = setup_instance_with_users(&[("alice", None), ("bob", None)]).await;

    // Simulate simultaneous logins
    let alice1 = login_user(&instance, "alice", None).await;
    let bob1 = login_user(&instance, "bob", None).await;
    let alice2 = login_user(&instance, "alice", None).await;
    let bob2 = login_user(&instance, "bob", None).await;

    // All should have valid keys
    assert_user_key_count(&alice1, 1);
    assert_user_key_count(&bob1, 1);
    assert_user_key_count(&alice2, 1);
    assert_user_key_count(&bob2, 1);
}

// ===== USER ENUMERATION TESTS =====

#[tokio::test]
async fn test_list_users() {
    let (instance, _) =
        setup_instance_with_users(&[("alice", None), ("bob", None), ("charlie", None)]).await;

    // List all users
    let users = instance.list_users().await.expect("Should list users");

    // Should have all 3 users
    assert_eq!(users.len(), 3, "Should have 3 users");
    assert!(users.contains(&"alice".to_string()));
    assert!(users.contains(&"bob".to_string()));
    assert!(users.contains(&"charlie".to_string()));
}

#[tokio::test]
async fn test_list_users_empty_instance() {
    let instance = setup_instance().await;

    let users = instance.list_users().await.expect("Should list users");

    // Should be empty in unified mode (no implicit user)
    assert!(users.is_empty(), "New unified instance should have 0 users");
}

// ===== CONCURRENT DATABASE OPERATIONS =====

#[tokio::test]
async fn test_users_modify_own_databases_concurrently() {
    let (_instance, mut user1, mut user2, _, _) =
        setup_two_passwordless_users("alice", "bob").await;

    // Each creates a database
    let alice_db = create_named_database(&mut user1, "Alice DB").await;
    let bob_db = create_named_database(&mut user2, "Bob DB").await;

    // Each writes to their database
    let alice_tx = alice_db.new_transaction().await.expect("Alice transaction");
    {
        let alice_store = alice_tx
            .get_store::<DocStore>("data")
            .await
            .expect("Alice store");
        alice_store
            .set("key", "alice_value")
            .await
            .expect("Alice write");
    }
    alice_tx.commit().await.expect("Alice commit");

    let bob_tx = bob_db.new_transaction().await.expect("Bob transaction");
    {
        let bob_store = bob_tx
            .get_store::<DocStore>("data")
            .await
            .expect("Bob store");
        bob_store.set("key", "bob_value").await.expect("Bob write");
    }
    bob_tx.commit().await.expect("Bob commit");

    // Verify data is independent
    let alice_tx2 = alice_db
        .new_transaction()
        .await
        .expect("Alice read transaction");
    let alice_store2 = alice_tx2
        .get_store::<DocStore>("data")
        .await
        .expect("Alice read store");
    let alice_value = alice_store2.get("key").await.expect("Alice read");
    assert_eq!(
        alice_value.as_text(),
        Some("alice_value"),
        "Alice's data should be preserved"
    );

    let bob_tx2 = bob_db
        .new_transaction()
        .await
        .expect("Bob read transaction");
    let bob_store2 = bob_tx2
        .get_store::<DocStore>("data")
        .await
        .expect("Bob read store");
    let bob_value = bob_store2.get("key").await.expect("Bob read");
    assert_eq!(
        bob_value.as_text(),
        Some("bob_value"),
        "Bob's data should be preserved"
    );
}
