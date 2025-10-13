//! Key management tests: add, list, get, and manage user keys
//!
//! Tests key operations including:
//! - Adding new keys to users
//! - Listing available keys
//! - Getting specific keys
//! - Key persistence across sessions
//! - Database-key mappings and sigkey retrieval
//! - Multi-key and multi-database scenarios

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

// ===== GET DATABASE SIGKEY TESTS =====

#[test]
fn test_get_database_sigkey() {
    let (instance, username) = setup_instance_with_user("rachel", None);
    let mut user = login_user(&instance, &username, None);

    // Create a database (uses first available key)
    let database = create_named_database(&mut user, "test_db");
    let db_id = database.root_id();

    // Get the first key
    let keys = user.list_keys().expect("Should list keys");
    let key_id = &keys[0];

    // Get database sigkey for this key and database
    let sigkey = user
        .get_database_sigkey(key_id, db_id)
        .expect("Should get database sigkey");

    assert!(sigkey.is_some(), "Should have sigkey mapping for database");
}

#[test]
fn test_get_database_sigkey_for_unmapped_database() {
    let (instance, username) = setup_instance_with_user("sam", None);
    let mut user = login_user(&instance, &username, None);

    // Add a new key (won't be mapped to any database)
    let key_id = add_user_key(&mut user, Some("Unmapped Key"));

    // Create a fake database ID
    use eidetica::entry::ID;
    let fake_db_id = ID::from("fake_database_id");

    // Try to get sigkey for database this key isn't mapped to
    let sigkey = user
        .get_database_sigkey(&key_id, &fake_db_id)
        .expect("Should not error on unmapped database");

    assert!(
        sigkey.is_none(),
        "Should not have sigkey for unmapped database"
    );
}

#[test]
fn test_get_database_sigkey_for_nonexistent_key() {
    let (instance, username) = setup_instance_with_user("tina", None);
    let mut user = login_user(&instance, &username, None);

    // Create a database
    let database = create_named_database(&mut user, "test_db");
    let db_id = database.root_id();

    // Try to get sigkey with nonexistent key
    let result = user.get_database_sigkey("nonexistent_key", db_id);

    assert!(
        result.is_err(),
        "Getting sigkey for nonexistent key should fail"
    );
}

// ===== ADD DATABASE KEY MAPPING TESTS =====

#[test]
fn test_add_database_key_mapping() {
    let (instance, username) = setup_instance_with_user("uma", None);
    let mut user = login_user(&instance, &username, None);

    // Get the default key
    let keys = user.list_keys().expect("Should list keys");
    let default_key = keys[0].clone();

    // Add a new key
    let extra_key = add_user_key(&mut user, Some("Extra Key"));

    // Create a database explicitly with the default key
    let mut settings = eidetica::crdt::Doc::new();
    settings.set_string("name", "test_db");
    let database = user
        .new_database_with_key(settings, &default_key)
        .expect("Should create database");
    let db_id = database.root_id();

    // Initially, the extra key shouldn't have a mapping to the database
    let sigkey_before = user
        .get_database_sigkey(&extra_key, db_id)
        .expect("Should get database sigkey");
    assert!(
        sigkey_before.is_none(),
        "Extra key should not have mapping yet"
    );

    // Add mapping manually for the extra key
    user.add_database_key_mapping(&extra_key, db_id, &extra_key)
        .expect("Should add database key mapping");

    // Now the extra key should have a mapping
    let sigkey_after = user
        .get_database_sigkey(&extra_key, db_id)
        .expect("Should get database sigkey");
    assert!(
        sigkey_after.is_some(),
        "Extra key should have mapping after add_database_key_mapping"
    );

    // Default key should still have its mapping
    let default_sigkey = user
        .get_database_sigkey(&default_key, db_id)
        .expect("Should get default key sigkey");
    assert!(
        default_sigkey.is_some(),
        "Default key should still have mapping"
    );
}

#[test]
fn test_add_database_key_mapping_for_nonexistent_key() {
    let (instance, username) = setup_instance_with_user("victor", None);
    let mut user = login_user(&instance, &username, None);

    // Create a database
    let database = create_named_database(&mut user, "test_db");
    let db_id = database.root_id();

    // Try to add mapping for nonexistent key
    let result = user.add_database_key_mapping("nonexistent_key", db_id, "fake_sigkey");

    assert!(
        result.is_err(),
        "Adding mapping for nonexistent key should fail"
    );
}

// ===== MULTI-KEY MULTI-DATABASE SCENARIOS =====

#[test]
fn test_one_key_multiple_databases() {
    let (instance, username) = setup_instance_with_user("wendy", None);
    let mut user = login_user(&instance, &username, None);

    // Create 3 databases
    let db1 = create_named_database(&mut user, "database_1");
    let db2 = create_named_database(&mut user, "database_2");
    let db3 = create_named_database(&mut user, "database_3");

    // Get the first key (used for all databases)
    let keys = user.list_keys().expect("Should list keys");
    let key_id = &keys[0];

    // Verify this key has mappings to all 3 databases
    let sigkey1 = user
        .get_database_sigkey(key_id, db1.root_id())
        .expect("Should get sigkey for db1");
    let sigkey2 = user
        .get_database_sigkey(key_id, db2.root_id())
        .expect("Should get sigkey for db2");
    let sigkey3 = user
        .get_database_sigkey(key_id, db3.root_id())
        .expect("Should get sigkey for db3");

    assert!(sigkey1.is_some(), "Should have mapping to db1");
    assert!(sigkey2.is_some(), "Should have mapping to db2");
    assert!(sigkey3.is_some(), "Should have mapping to db3");
}

#[test]
fn test_multiple_keys_one_database() {
    let (instance, username) = setup_instance_with_user("xander", None);
    let mut user = login_user(&instance, &username, None);

    // Create a database (uses first key)
    let database = create_named_database(&mut user, "shared_db");
    let db_id = database.root_id();

    // Add 2 more keys
    let key2 = add_user_key(&mut user, Some("Key 2"));
    let key3 = add_user_key(&mut user, Some("Key 3"));

    // Add mappings for the new keys to the same database
    user.add_database_key_mapping(&key2, db_id, &key2)
        .expect("Should add mapping for key2");
    user.add_database_key_mapping(&key3, db_id, &key3)
        .expect("Should add mapping for key3");

    // Verify all keys have mappings to the database
    let keys = user.list_keys().expect("Should list keys");
    let key1 = &keys[0]; // First key

    let sigkey1 = user
        .get_database_sigkey(key1, db_id)
        .expect("Should get sigkey for key1");
    let sigkey2 = user
        .get_database_sigkey(&key2, db_id)
        .expect("Should get sigkey for key2");
    let sigkey3 = user
        .get_database_sigkey(&key3, db_id)
        .expect("Should get sigkey for key3");

    assert!(sigkey1.is_some(), "Key1 should have mapping");
    assert!(sigkey2.is_some(), "Key2 should have mapping");
    assert!(sigkey3.is_some(), "Key3 should have mapping");
}

#[test]
fn test_complex_key_database_mappings() {
    let (instance, username) = setup_instance_with_user("yara", None);
    let mut user = login_user(&instance, &username, None);

    // Get the default key
    let keys = user.list_keys().expect("Should list keys");
    let key1 = keys[0].clone();

    // Create 2 additional keys (3 total with default)
    let key2 = add_user_key(&mut user, Some("Work Key"));
    let key3 = add_user_key(&mut user, Some("Home Key"));

    // Create 3 databases explicitly with the default key
    let mut settings1 = eidetica::crdt::Doc::new();
    settings1.set_string("name", "work_db");
    let db1 = user
        .new_database_with_key(settings1, &key1)
        .expect("Should create work_db");

    let mut settings2 = eidetica::crdt::Doc::new();
    settings2.set_string("name", "home_db");
    let db2 = user
        .new_database_with_key(settings2, &key1)
        .expect("Should create home_db");

    let mut settings3 = eidetica::crdt::Doc::new();
    settings3.set_string("name", "shared_db");
    let db3 = user
        .new_database_with_key(settings3, &key1)
        .expect("Should create shared_db");

    // Add specific manual mappings:
    // - key2 -> work_db and shared_db
    // - key3 -> home_db and shared_db
    user.add_database_key_mapping(&key2, db1.root_id(), &key2)
        .expect("Map key2 to work_db");
    user.add_database_key_mapping(&key2, db3.root_id(), &key2)
        .expect("Map key2 to shared_db");
    user.add_database_key_mapping(&key3, db2.root_id(), &key3)
        .expect("Map key3 to home_db");
    user.add_database_key_mapping(&key3, db3.root_id(), &key3)
        .expect("Map key3 to shared_db");

    // Verify key1 has all databases (created them)
    assert!(
        user.get_database_sigkey(&key1, db1.root_id())
            .expect("Should get sigkey")
            .is_some(),
        "key1 should have work_db"
    );
    assert!(
        user.get_database_sigkey(&key1, db2.root_id())
            .expect("Should get sigkey")
            .is_some(),
        "key1 should have home_db"
    );
    assert!(
        user.get_database_sigkey(&key1, db3.root_id())
            .expect("Should get sigkey")
            .is_some(),
        "key1 should have shared_db"
    );

    // Verify key2 has work_db and shared_db
    assert!(
        user.get_database_sigkey(&key2, db1.root_id())
            .expect("Should get sigkey")
            .is_some(),
        "key2 should have work_db"
    );
    assert!(
        user.get_database_sigkey(&key2, db2.root_id())
            .expect("Should get sigkey")
            .is_none(),
        "key2 should NOT have home_db"
    );
    assert!(
        user.get_database_sigkey(&key2, db3.root_id())
            .expect("Should get sigkey")
            .is_some(),
        "key2 should have shared_db"
    );

    // Verify key3 has home_db and shared_db
    assert!(
        user.get_database_sigkey(&key3, db1.root_id())
            .expect("Should get sigkey")
            .is_none(),
        "key3 should NOT have work_db"
    );
    assert!(
        user.get_database_sigkey(&key3, db2.root_id())
            .expect("Should get sigkey")
            .is_some(),
        "key3 should have home_db"
    );
    assert!(
        user.get_database_sigkey(&key3, db3.root_id())
            .expect("Should get sigkey")
            .is_some(),
        "key3 should have shared_db"
    );
}

// ===== MANUAL MAPPING PERSISTENCE TESTS =====

#[test]
fn test_manual_mappings_persist_across_sessions() {
    let username = "zara";
    let instance = setup_instance();

    // First session: create user, add key, create database, add mapping
    instance
        .create_user(username, None)
        .expect("Failed to create user");
    let mut user1 = login_user(&instance, username, None);

    let extra_key = add_user_key(&mut user1, Some("Extra Key"));
    let database = create_named_database(&mut user1, "persistent_db");
    let db_id = database.root_id().clone();

    // Add manual mapping
    user1
        .add_database_key_mapping(&extra_key, &db_id, &extra_key)
        .expect("Should add mapping");

    // Verify mapping exists
    let sigkey_before = user1
        .get_database_sigkey(&extra_key, &db_id)
        .expect("Should get sigkey");
    assert!(sigkey_before.is_some(), "Mapping should exist");

    user1.logout().expect("Logout should succeed");

    // Second session: verify mapping persisted
    let user2 = login_user(&instance, username, None);

    let sigkey_after = user2
        .get_database_sigkey(&extra_key, &db_id)
        .expect("Should get sigkey");
    assert!(
        sigkey_after.is_some(),
        "Manual mapping should persist across sessions"
    );
    assert_eq!(
        sigkey_before, sigkey_after,
        "Sigkey should be the same after re-login"
    );
}

#[test]
fn test_multiple_manual_mappings_persist() {
    let username = "aaron";
    let password = "password123";
    let instance = setup_instance();

    // First session: create complex mapping scenario
    instance
        .create_user(username, Some(password))
        .expect("Create user");
    let mut user1 = login_user(&instance, username, Some(password));

    // Get the default key
    let keys = user1.list_keys().expect("Should list keys");
    let key1 = keys[0].clone();

    // Create 2 extra keys
    let key2 = add_user_key(&mut user1, Some("Key 2"));
    let key3 = add_user_key(&mut user1, Some("Key 3"));

    // Create 3 databases explicitly with the default key
    let mut settings1 = eidetica::crdt::Doc::new();
    settings1.set_string("name", "db1");
    let db1 = user1
        .new_database_with_key(settings1, &key1)
        .expect("Should create db1");

    let mut settings2 = eidetica::crdt::Doc::new();
    settings2.set_string("name", "db2");
    let db2 = user1
        .new_database_with_key(settings2, &key1)
        .expect("Should create db2");

    let mut settings3 = eidetica::crdt::Doc::new();
    settings3.set_string("name", "db3");
    let db3 = user1
        .new_database_with_key(settings3, &key1)
        .expect("Should create db3");

    // Add multiple manual mappings
    user1
        .add_database_key_mapping(&key2, db1.root_id(), &key2)
        .expect("Map key2 to db1");
    user1
        .add_database_key_mapping(&key2, db2.root_id(), &key2)
        .expect("Map key2 to db2");
    user1
        .add_database_key_mapping(&key3, db2.root_id(), &key3)
        .expect("Map key3 to db2");
    user1
        .add_database_key_mapping(&key3, db3.root_id(), &key3)
        .expect("Map key3 to db3");

    user1.logout().expect("Logout should succeed");

    // Second session: verify all mappings persisted
    let user2 = login_user(&instance, username, Some(password));

    // Verify key2 mappings
    assert!(
        user2
            .get_database_sigkey(&key2, db1.root_id())
            .expect("Should get sigkey")
            .is_some(),
        "key2->db1 mapping should persist"
    );
    assert!(
        user2
            .get_database_sigkey(&key2, db2.root_id())
            .expect("Should get sigkey")
            .is_some(),
        "key2->db2 mapping should persist"
    );
    assert!(
        user2
            .get_database_sigkey(&key2, db3.root_id())
            .expect("Should get sigkey")
            .is_none(),
        "key2 should NOT have db3 mapping"
    );

    // Verify key3 mappings
    assert!(
        user2
            .get_database_sigkey(&key3, db1.root_id())
            .expect("Should get sigkey")
            .is_none(),
        "key3 should NOT have db1 mapping"
    );
    assert!(
        user2
            .get_database_sigkey(&key3, db2.root_id())
            .expect("Should get sigkey")
            .is_some(),
        "key3->db2 mapping should persist"
    );
    assert!(
        user2
            .get_database_sigkey(&key3, db3.root_id())
            .expect("Should get sigkey")
            .is_some(),
        "key3->db3 mapping should persist"
    );
}
