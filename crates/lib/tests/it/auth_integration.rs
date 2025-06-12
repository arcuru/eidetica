use eidetica::auth::crypto::{format_public_key, verify_entry_signature};
use eidetica::auth::types::{AuthId, AuthKey, KeyStatus, Permission};
use eidetica::backend::{Backend, InMemoryBackend};
use eidetica::basedb::BaseDB;
use eidetica::data::KVNested;
use eidetica::subtree::KVStore;

#[test]
fn test_key_management() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Initially no keys
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert!(keys.is_empty());

    // Generate a new key
    let public_key1 = db.add_private_key("KEY_LAPTOP").expect("Failed to add key");

    // Verify key was stored
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 1);
    assert!(keys.contains(&"KEY_LAPTOP".to_string()));

    // Get the public key back
    let retrieved_public_key = db
        .get_public_key("KEY_LAPTOP")
        .expect("Failed to get public key")
        .expect("Key not found");
    assert_eq!(public_key1, retrieved_public_key);

    // Get formatted public key
    let formatted = db
        .get_formatted_public_key("KEY_LAPTOP")
        .expect("Failed to get formatted key")
        .expect("Key not found");
    assert_eq!(formatted, format_public_key(&public_key1));

    // Add another key
    let _public_key2 = db
        .add_private_key("KEY_DESKTOP")
        .expect("Failed to add second key");

    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 2);
    assert!(keys.contains(&"KEY_LAPTOP".to_string()));
    assert!(keys.contains(&"KEY_DESKTOP".to_string()));

    // Test non-existent key
    let missing_key = db
        .get_public_key("KEY_NONEXISTENT")
        .expect("Failed to query missing key");
    assert!(missing_key.is_none());

    // Remove a key
    db.remove_private_key("KEY_LAPTOP")
        .expect("Failed to remove key");

    let keys = db
        .list_private_keys()
        .expect("Failed to list keys after removal");
    assert_eq!(keys.len(), 1);
    assert!(!keys.contains(&"KEY_LAPTOP".to_string()));
    assert!(keys.contains(&"KEY_DESKTOP".to_string()));

    // Verify removed key is gone
    let removed_key = db
        .get_public_key("KEY_LAPTOP")
        .expect("Failed to query removed key");
    assert!(removed_key.is_none());
}

#[test]
fn test_import_private_key() {
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Generate a key externally
    let external_key = SigningKey::generate(&mut OsRng);
    let external_public = external_key.verifying_key();

    // Import the key
    db.import_private_key("IMPORTED_KEY", external_key)
        .expect("Failed to import key");

    // Verify it was imported correctly
    let retrieved_public = db
        .get_public_key("IMPORTED_KEY")
        .expect("Failed to get imported key")
        .expect("Imported key not found");

    assert_eq!(external_public, retrieved_public);

    // Verify it shows up in the list
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 1);
    assert!(keys.contains(&"IMPORTED_KEY".to_string()));
}

#[test]
fn test_backend_serialization() {
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    use tempfile::NamedTempFile;

    // Create a temporary file for testing serialization
    let temp_file = NamedTempFile::new().expect("Failed to create temp file");
    let temp_path = temp_file.path();

    // Original keys to store
    let key1 = SigningKey::generate(&mut OsRng);
    let key2 = SigningKey::generate(&mut OsRng);
    let public_key1 = key1.verifying_key();
    let public_key2 = key2.verifying_key();

    // Test backend-level key storage and serialization
    {
        // Create backend and add private keys directly through the Backend trait
        let mut backend = InMemoryBackend::new();

        // Store keys using the Backend trait methods
        backend
            .store_private_key("KEY_ONE", key1)
            .expect("Failed to store key1");
        backend
            .store_private_key("KEY_TWO", key2)
            .expect("Failed to store key2");

        // Verify keys are stored
        let stored_keys = backend.list_private_keys().expect("Failed to list keys");
        assert_eq!(stored_keys.len(), 2);
        assert!(stored_keys.contains(&"KEY_ONE".to_string()));
        assert!(stored_keys.contains(&"KEY_TWO".to_string()));

        // Verify we can retrieve the keys and they produce the correct public keys
        let retrieved_key1 = backend
            .get_private_key("KEY_ONE")
            .expect("Failed to get key1")
            .expect("Key1 not found");
        let retrieved_key2 = backend
            .get_private_key("KEY_TWO")
            .expect("Failed to get key2")
            .expect("Key2 not found");
        assert_eq!(retrieved_key1.verifying_key(), public_key1);
        assert_eq!(retrieved_key2.verifying_key(), public_key2);

        // Save the backend with keys to file
        backend
            .save_to_file(temp_path)
            .expect("Failed to save backend");
    }

    // Load from file and verify keys are preserved
    {
        let loaded_backend =
            InMemoryBackend::load_from_file(temp_path).expect("Failed to load backend");

        // Verify the loaded backend has the same keys
        let loaded_keys = loaded_backend
            .list_private_keys()
            .expect("Failed to list loaded keys");
        assert_eq!(loaded_keys.len(), 2);
        assert!(loaded_keys.contains(&"KEY_ONE".to_string()));
        assert!(loaded_keys.contains(&"KEY_TWO".to_string()));

        // Verify the loaded keys produce the correct public keys
        let loaded_key1 = loaded_backend
            .get_private_key("KEY_ONE")
            .expect("Failed to get loaded key1")
            .expect("Loaded key1 not found");
        let loaded_key2 = loaded_backend
            .get_private_key("KEY_TWO")
            .expect("Failed to get loaded key2")
            .expect("Loaded key2 not found");

        assert_eq!(loaded_key1.verifying_key(), public_key1);
        assert_eq!(loaded_key2.verifying_key(), public_key2);

        // Test that we can use the loaded backend through BaseDB
        let db = BaseDB::new(Box::new(loaded_backend));

        // Verify BaseDB can see the loaded keys
        let db_keys = db.list_private_keys().expect("Failed to list DB keys");
        assert_eq!(db_keys.len(), 2);

        // Verify we can get the correct public keys through BaseDB
        let db_public1 = db
            .get_public_key("KEY_ONE")
            .expect("Failed to get DB public key1")
            .expect("DB key1 not found");
        let db_public2 = db
            .get_public_key("KEY_TWO")
            .expect("Failed to get DB public key2")
            .expect("DB key2 not found");

        assert_eq!(db_public1, public_key1);
        assert_eq!(db_public2, public_key2);
    }
}

#[test]
fn test_overwrite_existing_key() {
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Add initial key
    let public_key1 = db
        .add_private_key("TEST_KEY")
        .expect("Failed to add initial key");

    // Overwrite with new key
    let new_private_key = SigningKey::generate(&mut OsRng);
    let new_public_key = new_private_key.verifying_key();

    db.import_private_key("TEST_KEY", new_private_key)
        .expect("Failed to overwrite key");

    // Verify the key was overwritten
    let retrieved_public = db
        .get_public_key("TEST_KEY")
        .expect("Failed to get public key")
        .expect("Key not found");

    assert_eq!(new_public_key, retrieved_public);
    assert_ne!(public_key1, retrieved_public); // Should be different from original

    // Should still only have one key
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 1);
}

#[test]
fn test_remove_nonexistent_key() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Remove a key that doesn't exist - should succeed silently
    db.remove_private_key("NONEXISTENT_KEY")
        .expect("Failed to remove nonexistent key");

    // Should still have no keys
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert!(keys.is_empty());
}

#[test]
fn test_authenticated_operations() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Generate a key for testing
    let public_key = db.add_private_key("TEST_KEY").expect("Failed to add key");

    // Create a tree
    let settings = KVNested::new();
    let tree = db.new_tree(settings).expect("Failed to create tree");

    // Create an authenticated operation
    let op = tree
        .new_authenticated_operation("TEST_KEY")
        .expect("Failed to create authenticated operation");

    // Verify the operation has the correct auth key
    assert_eq!(op.auth_key_id(), Some("TEST_KEY"));

    // Use the operation to add some data
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("key1", "value1").expect("Failed to set value");

    // Commit the operation
    let entry_id = op.commit().expect("Failed to commit");

    // Retrieve the entry and verify it's signed
    let backend_guard = db.backend().lock().expect("Failed to lock backend");
    let entry = backend_guard.get(&entry_id).expect("Entry not found");

    // Check authentication info
    assert_eq!(entry.auth.id, AuthId::Direct("TEST_KEY".to_string()));
    assert!(entry.auth.signature.is_some());

    // Verify the signature
    let is_valid = verify_entry_signature(entry, &public_key).expect("Failed to verify signature");
    assert!(is_valid, "Entry signature should be valid");
}

#[test]
fn test_operation_auth_methods() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Generate keys for testing
    db.add_private_key("KEY1").expect("Failed to add key1");
    db.add_private_key("KEY2").expect("Failed to add key2");

    // Create a tree
    let settings = KVNested::new();
    let tree = db.new_tree(settings).expect("Failed to create tree");

    // Test with_auth method (chaining)
    let op1 = tree
        .new_operation()
        .expect("Failed to create operation")
        .with_auth("KEY1");
    assert_eq!(op1.auth_key_id(), Some("KEY1"));

    // Test set_auth_key method (mutable)
    let mut op2 = tree.new_operation().expect("Failed to create operation");
    assert_eq!(op2.auth_key_id(), None);
    op2.set_auth_key("KEY2");
    assert_eq!(op2.auth_key_id(), Some("KEY2"));
}

#[test]
fn test_tree_default_authentication() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Generate a key for testing
    db.add_private_key("DEFAULT_KEY")
        .expect("Failed to add key");

    // Create a tree
    let settings = KVNested::new();
    let mut tree = db.new_tree(settings).expect("Failed to create tree");

    // Initially no default auth
    assert_eq!(tree.default_auth_key(), None);

    // Set default auth key
    tree.set_default_auth_key("DEFAULT_KEY");
    assert_eq!(tree.default_auth_key(), Some("DEFAULT_KEY"));

    // New operations should use the default key
    let op = tree.new_operation().expect("Failed to create operation");
    assert_eq!(op.auth_key_id(), Some("DEFAULT_KEY"));

    // Clear default auth
    tree.clear_default_auth_key();
    assert_eq!(tree.default_auth_key(), None);

    // New operations should not have auth
    let op2 = tree.new_operation().expect("Failed to create operation");
    assert_eq!(op2.auth_key_id(), None);
}

#[test]
fn test_unsigned_operations() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Create a tree
    let settings = KVNested::new();
    let tree = db.new_tree(settings).expect("Failed to create tree");

    // Create an operation without authentication
    let op = tree.new_operation().expect("Failed to create operation");
    assert_eq!(op.auth_key_id(), None);

    // Use the operation to add some data
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("key1", "value1").expect("Failed to set value");

    // Commit the operation
    let entry_id = op.commit().expect("Failed to commit");

    // Retrieve the entry and verify it's unsigned
    let backend_guard = db.backend().lock().expect("Failed to lock backend");
    let entry = backend_guard.get(&entry_id).expect("Entry not found");

    // Check that auth info is default (empty direct key)
    assert_eq!(entry.auth.id, AuthId::Direct(String::new()));
    assert!(entry.auth.signature.is_none());
}

#[test]
fn test_missing_authentication_key_error() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Create a tree
    let settings = KVNested::new();
    let tree = db.new_tree(settings).expect("Failed to create tree");

    // Create an operation with a non-existent key
    let op = tree
        .new_authenticated_operation("NONEXISTENT_KEY")
        .expect("Failed to create operation");

    // Use the operation to add some data
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("key1", "value1").expect("Failed to set value");

    // Commit should fail because the key doesn't exist
    let result = op.commit();
    assert!(result.is_err());

    // Check that the error mentions the missing key
    let error_msg = format!("{:?}", result.unwrap_err());
    assert!(error_msg.contains("NONEXISTENT_KEY"));
    assert!(error_msg.contains("not found"));
}

#[test]
fn test_multiple_authenticated_entries() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Generate keys for testing
    let public_key1 = db.add_private_key("USER1").expect("Failed to add key1");
    let public_key2 = db.add_private_key("USER2").expect("Failed to add key2");

    // Create a tree
    let mut settings = KVNested::new();
    // Add the two keys as Write keys in the auth settings
    let mut auth_settings = KVNested::new();
    auth_settings.set(
        "USER1".to_string(),
        AuthKey {
            key: format_public_key(&public_key1),
            permissions: Permission::Write(10),
            status: KeyStatus::Active,
        },
    );
    auth_settings.set(
        "USER2".to_string(),
        AuthKey {
            key: format_public_key(&public_key2),
            permissions: Permission::Write(10),
            status: KeyStatus::Active,
        },
    );
    settings.set_map("auth", auth_settings);

    // This creates a tree with a separate, new root signing key but with the two keys above already
    // configured as write keys.
    let tree = db.new_tree(settings).expect("Failed to create tree");

    // Create first authenticated entry
    let op1 = tree
        .new_authenticated_operation("USER1")
        .expect("Failed to create operation");
    let store1 = op1
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store1
        .set("user1_data", "hello")
        .expect("Failed to set value");
    let entry_id1 = op1.commit().expect("Failed to commit");

    // Create second authenticated entry
    let op2 = tree
        .new_authenticated_operation("USER2")
        .expect("Failed to create operation");
    let store2 = op2
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store2
        .set("user2_data", "world")
        .expect("Failed to set value");
    let entry_id2 = op2.commit().expect("Failed to commit");

    // Verify both entries are properly signed
    let backend_guard = db.backend().lock().expect("Failed to lock backend");

    let entry1 = backend_guard.get(&entry_id1).expect("Entry1 not found");
    assert_eq!(entry1.auth.id, AuthId::Direct("USER1".to_string()));
    assert!(verify_entry_signature(entry1, &public_key1).expect("Failed to verify"));

    let entry2 = backend_guard.get(&entry_id2).expect("Entry2 not found");
    assert_eq!(entry2.auth.id, AuthId::Direct("USER2".to_string()));
    assert!(verify_entry_signature(entry2, &public_key2).expect("Failed to verify"));

    // Verify cross-validation fails (entry1 with key2 should fail)
    assert!(!verify_entry_signature(entry1, &public_key2).expect("Failed to verify"));
    assert!(!verify_entry_signature(entry2, &public_key1).expect("Failed to verify"));
}

// ===== Phase 3: Authentication Validation Tests =====

#[test]
fn test_backend_authentication_validation() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Generate a key for testing
    let public_key = db.add_private_key("TEST_KEY").expect("Failed to add key");

    // Create a tree with authentication enabled
    let mut settings = KVNested::new();
    let mut auth_settings = KVNested::new();

    // Add the test key to authentication settings
    let auth_key = AuthKey {
        key: format_public_key(&public_key),
        permissions: Permission::Write(10),
        status: KeyStatus::Active,
    };
    auth_settings.set("TEST_KEY".to_string(), auth_key);
    settings.set_map("auth", auth_settings);

    let tree = db.new_tree(settings).expect("Failed to create tree");

    // Create an authenticated operation
    let op = tree
        .new_authenticated_operation("TEST_KEY")
        .expect("Failed to create authenticated operation");
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    // This should succeed because the key is configured in auth settings
    let entry_id = op.commit().expect("Failed to commit");

    // Verify the entry was stored
    let backend_guard = tree.lock_backend().expect("Failed to lock backend");
    let entry = backend_guard.get(&entry_id).expect("Entry not found");
    assert_eq!(entry.auth.id, AuthId::Direct("TEST_KEY".to_string()));
}

#[test]
fn test_authentication_validation_revoked_key() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Generate a key for testing
    let public_key = db
        .add_private_key("REVOKED_KEY")
        .expect("Failed to add key");

    // Create a tree with authentication enabled, but mark the key as revoked
    let mut settings = KVNested::new();
    let mut auth_settings = KVNested::new();

    let auth_key = AuthKey {
        key: format_public_key(&public_key),
        permissions: Permission::Write(10),
        status: KeyStatus::Revoked, // Key is revoked
    };
    auth_settings.set("REVOKED_KEY".to_string(), auth_key);
    settings.set_map("auth", auth_settings);

    let tree = db.new_tree(settings).expect("Failed to create tree");

    // Create an authenticated operation
    let op = tree
        .new_authenticated_operation("REVOKED_KEY")
        .expect("Failed to create authenticated operation");
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    // This should fail because the key is revoked
    let result = op.commit();
    assert!(result.is_err());

    // Check that the error mentions authentication validation failure
    let error_msg = format!("{:?}", result.unwrap_err());
    assert!(error_msg.contains("authentication validation failed"));
}

#[test]
fn test_permission_checking_admin_operations() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Generate keys with different permission levels
    let write_key = db
        .add_private_key("WRITE_KEY")
        .expect("Failed to add write key");
    let admin_key = db
        .add_private_key("ADMIN_KEY")
        .expect("Failed to add admin key");

    // Create a tree with authentication enabled
    let mut settings = KVNested::new();
    let mut auth_settings = KVNested::new();

    // Add write-only key
    let write_auth_key = AuthKey {
        key: format_public_key(&write_key),
        permissions: Permission::Write(10),
        status: KeyStatus::Active,
    };
    auth_settings.set("WRITE_KEY".to_string(), write_auth_key);

    // Add admin key
    let admin_auth_key = AuthKey {
        key: format_public_key(&admin_key),
        permissions: Permission::Admin(1),
        status: KeyStatus::Active,
    };
    auth_settings.set("ADMIN_KEY".to_string(), admin_auth_key);

    settings.set_map("auth", auth_settings);

    let tree = db.new_tree(settings).expect("Failed to create tree");

    // Test: Write key should be able to write data
    let op1 = tree
        .new_authenticated_operation("WRITE_KEY")
        .expect("Failed to create operation");
    let store1 = op1
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store1.set("test", "value").expect("Failed to set value");

    let result1 = op1.commit();
    assert!(result1.is_ok(), "Write key should be able to write data");

    // Test: Admin key should be able to write data
    let op2 = tree
        .new_authenticated_operation("ADMIN_KEY")
        .expect("Failed to create operation");
    let store2 = op2
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store2.set("test2", "value2").expect("Failed to set value");

    let result2 = op2.commit();
    assert!(result2.is_ok(), "Admin key should be able to write data");

    // Test: Admin key should be able to modify settings
    let op3 = tree
        .new_authenticated_operation("ADMIN_KEY")
        .expect("Failed to create operation");
    let store3 = op3
        .get_subtree::<KVStore>("_settings")
        .expect("Failed to get settings subtree");
    store3
        .set("new_setting", "value")
        .expect("Failed to set setting");

    let result3 = op3.commit();
    assert!(
        result3.is_ok(),
        "Admin key should be able to modify settings"
    );

    // Test: Write key should NOT be able to modify settings
    let op4 = tree
        .new_authenticated_operation("WRITE_KEY")
        .expect("Failed to create operation");
    let store4 = op4
        .get_subtree::<KVStore>("_settings")
        .expect("Failed to get settings subtree");
    store4
        .set("forbidden_setting", "value")
        .expect("Failed to set setting");

    let result4 = op4.commit();
    assert!(
        result4.is_err(),
        "Write key should NOT be able to modify settings"
    );

    // Check that the error mentions authentication validation failure
    let error_msg = format!("{:?}", result4.unwrap_err());
    assert!(error_msg.contains("authentication validation failed"));
}

#[test]
fn test_unsigned_entries_still_work() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Create a tree with authentication enabled
    let mut settings = KVNested::new();
    let auth_settings = KVNested::new();
    settings.set_map("auth", auth_settings);

    let tree = db.new_tree(settings).expect("Failed to create tree");

    // Create an operation without authentication (should still work for backward compatibility)
    let op = tree.new_operation().expect("Failed to create operation");
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    // This should succeed - unsigned entries are allowed for backward compatibility
    let entry_id = op.commit().expect("Unsigned entries should still work");

    // Verify the entry was stored and is unsigned
    let backend_guard = tree.lock_backend().expect("Failed to lock backend");
    let entry = backend_guard.get(&entry_id).expect("Entry not found");
    assert_eq!(entry.auth.id, AuthId::default());
}

#[test]
fn test_mixed_authenticated_and_unsigned_entries() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Generate a key for testing
    let public_key = db.add_private_key("MIXED_KEY").expect("Failed to add key");

    // Create a tree with authentication enabled
    let mut settings = KVNested::new();
    let mut auth_settings = KVNested::new();

    let auth_key = AuthKey {
        key: format_public_key(&public_key),
        permissions: Permission::Write(10),
        status: KeyStatus::Active,
    };
    auth_settings.set("MIXED_KEY".to_string(), auth_key);
    settings.set_map("auth", auth_settings);

    let tree = db.new_tree(settings).expect("Failed to create tree");

    // Create an unsigned entry
    let op1 = tree
        .new_operation()
        .expect("Failed to create unsigned operation");
    let store1 = op1
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store1
        .set("unsigned", "value")
        .expect("Failed to set value");
    let unsigned_id = op1.commit().expect("Failed to commit unsigned entry");

    // Create a signed entry
    let op2 = tree
        .new_authenticated_operation("MIXED_KEY")
        .expect("Failed to create signed operation");
    let store2 = op2
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store2.set("signed", "value").expect("Failed to set value");
    let signed_id = op2.commit().expect("Failed to commit signed entry");

    // Both entries should exist and be retrievable
    let backend_guard = tree.lock_backend().expect("Failed to lock backend");

    let unsigned_entry = backend_guard
        .get(&unsigned_id)
        .expect("Unsigned entry not found");
    assert_eq!(unsigned_entry.auth.id, AuthId::default());

    let signed_entry = backend_guard
        .get(&signed_id)
        .expect("Signed entry not found");
    assert_eq!(
        signed_entry.auth.id,
        AuthId::Direct("MIXED_KEY".to_string())
    );
}
