//! Security tests: password handling, encryption, and permission isolation
//!
//! Tests security-related functionality including:
//! - Password edge cases (empty, unicode, special chars)
//! - User key isolation and uniqueness
//! - Database access permission boundaries
//! - Session security

use super::helpers::*;
use eidetica::auth::crypto::generate_keypair;

// ===== PASSWORD EDGE CASES =====

#[tokio::test]
async fn test_empty_password_different_from_no_password() {
    let instance = setup_instance().await;
    let username = "alice";

    // Create user with empty password (different from passwordless)
    instance
        .create_user(username, Some(""))
        .await
        .expect("Create with empty password");

    // Should require empty password to login
    let result = instance.login_user(username, None).await;
    assert!(result.is_err(), "Empty password is not same as no password");

    // Should succeed with empty password
    let _user = instance
        .login_user(username, Some(""))
        .await
        .expect("Login with empty password");
}

#[tokio::test]
async fn test_case_sensitive_passwords() {
    let username = "bob";
    let password = "MyPassword";
    let (instance, _) = setup_instance_with_user(username, Some(password)).await;

    // Wrong case should fail
    let result = instance.login_user(username, Some("mypassword")).await;
    assert!(result.is_err(), "Passwords should be case-sensitive");

    let result = instance.login_user(username, Some("MYPASSWORD")).await;
    assert!(result.is_err(), "Passwords should be case-sensitive");
}

#[tokio::test]
async fn test_special_characters_in_password() {
    let username = "alice";
    let password = "p@ssw0rd!#$%^&*()";
    let (instance, _) = setup_instance_with_user(username, Some(password)).await;

    let _user = login_user(&instance, username, Some(password)).await;
}

#[tokio::test]
async fn test_unicode_in_password() {
    let username = "alice";
    let password = "密码🔒パスワード";
    let (instance, _) = setup_instance_with_user(username, Some(password)).await;

    let _user = login_user(&instance, username, Some(password)).await;
}

#[tokio::test]
async fn test_long_password() {
    let username = "alice";
    let password = "a".repeat(1000); // Very long password
    let (instance, _) = setup_instance_with_user(username, Some(&password)).await;

    let _user = login_user(&instance, username, Some(&password)).await;
}

// ===== KEY ISOLATION AND UNIQUENESS =====

#[tokio::test]
async fn test_key_ids_are_unique_per_user() {
    let (instance, _) =
        setup_instance_with_users(&[("alice", None), ("bob", None), ("charlie", None)]).await;

    let alice = login_user(&instance, "alice", None).await;
    let bob = login_user(&instance, "bob", None).await;
    let charlie = login_user(&instance, "charlie", None).await;

    let alice_keys = alice.list_keys().expect("Alice keys");
    let bob_keys = bob.list_keys().expect("Bob keys");
    let charlie_keys = charlie.list_keys().expect("Charlie keys");

    // No key ID overlap
    for alice_key in &alice_keys {
        assert!(
            !bob_keys.contains(alice_key),
            "Alice and Bob should not share keys"
        );
        assert!(
            !charlie_keys.contains(alice_key),
            "Alice and Charlie should not share keys"
        );
    }
}

#[tokio::test]
async fn test_user_list_keys_only_shows_own_keys() {
    let (_instance, mut user1, mut user2, _, _) =
        setup_two_passwordless_users("alice", "bob").await;

    // Alice adds keys
    let alice_key1 = add_user_key(&mut user1, Some("Alice Key 1")).await;
    let alice_key2 = add_user_key(&mut user1, Some("Alice Key 2")).await;

    // Bob adds keys
    let bob_key1 = add_user_key(&mut user2, Some("Bob Key 1")).await;

    // Alice should only see her keys
    let alice_keys = user1.list_keys().expect("Alice keys");
    assert!(
        alice_keys.contains(&alice_key1),
        "Alice should see her keys"
    );
    assert!(
        alice_keys.contains(&alice_key2),
        "Alice should see her keys"
    );
    assert!(
        !alice_keys.contains(&bob_key1),
        "Alice should not see Bob's keys"
    );

    // Bob should only see his keys
    let bob_keys = user2.list_keys().expect("Bob keys");
    assert!(bob_keys.contains(&bob_key1), "Bob should see his keys");
    assert!(
        !bob_keys.contains(&alice_key1),
        "Bob should not see Alice's keys"
    );
    assert!(
        !bob_keys.contains(&alice_key2),
        "Bob should not see Alice's keys"
    );
}

#[tokio::test]
async fn test_generated_keys_are_unique() {
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let mut user = login_user(&instance, &username, None).await;

    // Generate multiple keys
    let key1 = add_user_key(&mut user, None).await;
    let key2 = add_user_key(&mut user, None).await;
    let key3 = add_user_key(&mut user, None).await;

    // All keys should be different
    assert_ne!(key1, key2, "Keys should be unique");
    assert_ne!(key2, key3, "Keys should be unique");
    assert_ne!(key1, key3, "Keys should be unique");
}

#[tokio::test]
async fn test_keys_from_different_users_are_unique() {
    let (_instance, mut user1, mut user2, _, _) =
        setup_two_passwordless_users("alice", "bob").await;

    // Generate keys for both users
    let alice_key = add_user_key(&mut user1, Some("Alice Key")).await;
    let bob_key = add_user_key(&mut user2, Some("Bob Key")).await;

    // Keys should be different even with same display name
    assert_ne!(
        alice_key, bob_key,
        "Keys from different users should be unique"
    );
}

// ===== PERMISSION BOUNDARY TESTS =====

#[tokio::test]
async fn test_invalid_key_id_fails() {
    let (instance, username) = setup_instance_with_user("alice", None).await;
    let user = login_user(&instance, &username, None).await;

    let (_, fake_key) = generate_keypair();
    let result = user.get_signing_key(&fake_key);
    assert!(result.is_err(), "Getting invalid key should fail");
}

#[tokio::test]
async fn test_cannot_use_another_users_database_key() {
    let (_instance, mut user1, user2, _, _) = setup_two_passwordless_users("alice", "bob").await;

    // Alice creates a database
    let alice_db = create_named_database(&mut user1, "Alice DB").await;
    let db_id = alice_db.root_id();

    // Bob tries to find a key for Alice's database
    let result = user2.find_key(db_id).expect("Should not error");

    // Bob should not find a key (None returned)
    assert!(
        result.is_none(),
        "Bob should not have access to Alice's database key"
    );
}

#[tokio::test]
async fn test_database_access_requires_key() {
    let (_instance, mut user1, user2, _, _) = setup_two_passwordless_users("alice", "bob").await;

    // Alice creates a database
    let alice_db = create_named_database(&mut user1, "Alice's Private DB").await;
    let db_id = alice_db.root_id();

    // Bob tries to load the database
    let result = user2.open_database(db_id).await;

    // Bob should not be able to load without proper bootstrap
    assert!(result.is_err(), "User without key should not load database");
}

// ===== SESSION SECURITY =====

#[tokio::test]
async fn test_multiple_sessions_see_persisted_changes() {
    let (instance, username) = setup_instance_with_user("alice", None).await;

    // Session 1: Add a key
    let mut user1 = login_user(&instance, &username, None).await;
    let key1 = add_user_key(&mut user1, Some("Key from session 1")).await;
    user1.logout().expect("Logout session 1");

    // Session 2: Should see the key that was persisted
    let user2 = login_user(&instance, &username, None).await;
    let user2_keys = user2.list_keys().expect("User2 keys");

    assert!(
        user2_keys.contains(&key1),
        "Session 2 should see persisted key"
    );
}
