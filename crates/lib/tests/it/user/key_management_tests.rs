//! Key management tests: add, list, get, and manage user keys
//!
//! Tests key operations including:
//! - Adding new keys to users
//! - Listing available keys
//! - Getting specific keys
//! - Key persistence across sessions

use super::helpers::*;

// ===== ADD KEY TESTS =====

#[test]
fn test_add_key_to_passwordless_user() {
    let (instance, username) = setup_instance_with_user("alice", None);
    let mut user = login_user(&instance, &username, None);

    // User starts with 1 key (default key)
    assert_user_key_count(&user, 1);

    // Add a new key
    let key_id = add_user_key(&mut user, Some("My Second Key"));

    // Should now have 2 keys
    assert_user_key_count(&user, 2);

    // Verify the new key exists
    assert_user_has_key(&user, &key_id);
}

#[test]
fn test_add_key_to_password_user() {
    let username = "bob";
    let password = "secure_password";
    let (instance, _) = setup_instance_with_user(username, Some(password));
    let mut user = login_user(&instance, username, Some(password));

    // User starts with 1 key (default key)
    assert_user_key_count(&user, 1);

    // Add a new key
    let key_id = add_user_key(&mut user, Some("Additional Key"));

    // Should now have 2 keys
    assert_user_key_count(&user, 2);

    // Verify the new key exists
    assert_user_has_key(&user, &key_id);
}

#[test]
fn test_add_multiple_keys() {
    let (instance, username) = setup_instance_with_user("charlie", None);
    let mut user = login_user(&instance, &username, None);

    // Add 3 new keys
    let key1 = add_user_key(&mut user, Some("Key 1"));
    let key2 = add_user_key(&mut user, Some("Key 2"));
    let key3 = add_user_key(&mut user, Some("Key 3"));

    // Should have 4 keys total (1 default + 3 new)
    assert_user_key_count(&user, 4);

    // Verify all keys exist
    assert_user_has_key(&user, &key1);
    assert_user_has_key(&user, &key2);
    assert_user_has_key(&user, &key3);
}

#[test]
fn test_add_key_with_custom_display_name() {
    let (instance, username) = setup_instance_with_user("diana", None);
    let mut user = login_user(&instance, &username, None);

    // Add key with custom display name
    let key_id = add_user_key(&mut user, Some("Work Laptop Key"));

    // Verify key was added
    assert_user_has_key(&user, &key_id);
    assert_user_key_count(&user, 2);
}

#[test]
fn test_add_key_without_display_name() {
    let (instance, username) = setup_instance_with_user("eve", None);
    let mut user = login_user(&instance, &username, None);

    // Add key without display name
    let key_id = add_user_key(&mut user, None);

    // Verify key was added
    assert_user_has_key(&user, &key_id);
    assert_user_key_count(&user, 2);
}

// ===== LIST KEYS TESTS =====

#[test]
fn test_list_keys_default() {
    let (instance, username) = setup_instance_with_user("frank", None);
    let user = login_user(&instance, &username, None);

    // List keys should return 1 default key
    let keys = user.list_keys().expect("Should list keys");
    assert_eq!(keys.len(), 1, "Should have 1 default key");
}

#[test]
fn test_list_keys_after_adding() {
    let (instance, username) = setup_instance_with_user("grace", None);
    let mut user = login_user(&instance, &username, None);

    // Add 2 keys
    let key1 = add_user_key(&mut user, Some("Key 1"));
    let key2 = add_user_key(&mut user, Some("Key 2"));

    // List should return 3 keys
    let keys = user.list_keys().expect("Should list keys");
    assert_eq!(keys.len(), 3, "Should have 3 keys");

    // Verify specific keys are in the list
    assert!(keys.contains(&key1), "Should contain key1");
    assert!(keys.contains(&key2), "Should contain key2");
}

#[test]
fn test_list_keys_returns_key_ids() {
    let (instance, username) = setup_instance_with_user("henry", None);
    let mut user = login_user(&instance, &username, None);

    // Add a key
    let added_key_id = add_user_key(&mut user, Some("Test Key"));

    // List keys
    let keys = user.list_keys().expect("Should list keys");

    // Verify added key is in the list
    assert!(
        keys.contains(&added_key_id),
        "Listed keys should contain the added key ID"
    );
}

// ===== GET SIGNING KEY TESTS =====

#[test]
fn test_get_signing_key() {
    let (instance, username) = setup_instance_with_user("iris", None);
    let mut user = login_user(&instance, &username, None);

    // Add a key
    let key_id = add_user_key(&mut user, Some("My Key"));

    // Get the signing key
    let signing_key = user
        .get_signing_key(&key_id)
        .expect("Should get signing key");

    // Verify it's a valid signing key
    let verifying_key = signing_key.verifying_key();
    assert!(
        verifying_key.as_bytes().len() == 32,
        "Should be valid Ed25519 key"
    );
}

#[test]
fn test_get_default_signing_key() {
    let (instance, username) = setup_instance_with_user("jack", None);
    let user = login_user(&instance, &username, None);

    // Get the first (default) key
    let keys = user.list_keys().expect("Should list keys");
    let default_key_id = &keys[0];

    let signing_key = user
        .get_signing_key(default_key_id)
        .expect("Should get default signing key");

    // Verify it's a valid signing key
    let verifying_key = signing_key.verifying_key();
    assert!(
        verifying_key.as_bytes().len() == 32,
        "Should be valid Ed25519 key"
    );
}

#[test]
fn test_get_nonexistent_signing_key() {
    let (instance, username) = setup_instance_with_user("kate", None);
    let user = login_user(&instance, &username, None);

    // Try to get a key that doesn't exist
    let fake_key_id = "nonexistent_key_id";
    let result = user.get_signing_key(fake_key_id);

    assert!(
        result.is_err(),
        "Getting nonexistent signing key should fail"
    );
}

// ===== KEY PERSISTENCE TESTS =====

#[test]
fn test_keys_persist_across_sessions() {
    let username = "leo";
    let instance = setup_instance();

    // First session: create user and add keys
    instance
        .create_user(username, None)
        .expect("Failed to create user");
    let mut user1 = login_user(&instance, username, None);
    let key1 = add_user_key(&mut user1, Some("Session 1 Key"));
    user1.logout().expect("Logout should succeed");

    // Second session: verify keys persisted
    let user2 = login_user(&instance, username, None);
    assert_user_key_count(&user2, 2); // Default + 1 added
    assert_user_has_key(&user2, &key1);

    // Verify we can get the signing key
    let _signing_key = user2
        .get_signing_key(&key1)
        .expect("Should get persisted signing key");
}

#[test]
fn test_multiple_keys_persist() {
    let username = "mia";
    let password = "test_password";
    let instance = setup_instance();

    // First session: add multiple keys
    instance
        .create_user(username, Some(password))
        .expect("Create user");
    let mut user1 = login_user(&instance, username, Some(password));

    let key1 = add_user_key(&mut user1, Some("Work Key"));
    let key2 = add_user_key(&mut user1, Some("Home Key"));
    let key3 = add_user_key(&mut user1, Some("Mobile Key"));

    user1.logout().expect("Logout should succeed");

    // Second session: verify all keys persisted
    let user2 = login_user(&instance, username, Some(password));
    assert_user_key_count(&user2, 4); // Default + 3 added

    assert_user_has_key(&user2, &key1);
    assert_user_has_key(&user2, &key2);
    assert_user_has_key(&user2, &key3);
}

// ===== KEY ID UNIQUENESS TESTS =====

#[test]
fn test_key_ids_are_unique() {
    let (instance, username) = setup_instance_with_user("noah", None);
    let mut user = login_user(&instance, &username, None);

    // Add keys with same display name
    let key1 = add_user_key(&mut user, Some("Same Name"));
    let key2 = add_user_key(&mut user, Some("Same Name"));

    // Both keys should exist with different IDs
    assert_ne!(key1, key2, "Keys should have different IDs");
    assert_user_has_key(&user, &key1);
    assert_user_has_key(&user, &key2);
}

// ===== DATABASE ACCESS TESTS =====

#[test]
fn test_find_key_for_database() {
    let (instance, username) = setup_instance_with_user("paul", None);
    let mut user = login_user(&instance, &username, None);

    // Create a database (uses first available key)
    let database = create_named_database(&mut user, "test_db");
    let db_id = database.root_id();

    // Find key for database
    let key = user
        .find_key_for_database(db_id)
        .expect("Should find key for database");

    assert!(key.is_some(), "Should find a key for the database");
}

#[test]
fn test_find_key_for_nonexistent_database() {
    let (instance, username) = setup_instance_with_user("quinn", None);
    let user = login_user(&instance, &username, None);

    // Create a fake database ID
    use eidetica::entry::ID;
    let fake_db_id = ID::from("fake_database_id");

    // Try to find key for nonexistent database
    let key = user
        .find_key_for_database(&fake_db_id)
        .expect("Should not error on nonexistent DB");

    assert!(
        key.is_none(),
        "Should not find key for nonexistent database"
    );
}
