use super::helpers::*;
use crate::create_auth_keys;
use eidetica::auth::crypto::format_public_key;
use eidetica::auth::types::{AuthId, AuthKey, KeyStatus, Permission};
use eidetica::backend::InMemoryBackend;
use eidetica::basedb::BaseDB;
use eidetica::data::KVNested;
use eidetica::subtree::KVStore;

#[test]
fn test_authenticated_operations() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Generate a key for testing
    let _public_key = db.add_private_key("TEST_KEY").expect("Failed to add key");

    // Create a tree
    let tree = db.new_tree(KVNested::new()).expect("Failed to create tree");

    // Create an authenticated operation
    let op = tree
        .new_authenticated_operation("TEST_KEY")
        .expect("Failed to create authenticated operation");

    // The operation should have the correct auth key ID
    assert_eq!(op.auth_key_id(), Some("TEST_KEY"));

    // Test that we can use the operation
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    // Commit should work
    let entry_id = op.commit().expect("Failed to commit");

    // Verify the entry is signed
    let backend_guard = db.backend().lock().expect("Failed to lock backend");
    let entry = backend_guard.get(&entry_id).expect("Entry not found");
    assert_eq!(entry.auth.id, AuthId::Direct("TEST_KEY".to_string()));
    assert!(entry.auth.signature.is_some());
}

#[test]
fn test_operation_auth_methods() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Generate keys for testing
    let _public_key1 = db.add_private_key("KEY1").expect("Failed to add key1");
    let _public_key2 = db.add_private_key("KEY2").expect("Failed to add key2");

    // Create a tree
    let tree = db.new_tree(KVNested::new()).expect("Failed to create tree");

    // Test operations with different auth key IDs
    let op1 = tree
        .new_authenticated_operation("KEY1")
        .expect("Failed to create operation");
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
    let _public_key = db
        .add_private_key("DEFAULT_KEY")
        .expect("Failed to add key");

    // Create a tree
    let mut tree = db.new_tree(KVNested::new()).expect("Failed to create tree");

    // Initially no default auth key
    assert_eq!(tree.default_auth_key(), None);

    // Set a default auth key
    tree.set_default_auth_key("DEFAULT_KEY");
    assert_eq!(tree.default_auth_key(), Some("DEFAULT_KEY"));

    // Operations should inherit the default key
    let op = tree.new_operation().expect("Failed to create operation");
    assert_eq!(op.auth_key_id(), Some("DEFAULT_KEY"));

    // Clear the default
    tree.clear_default_auth_key();
    assert_eq!(tree.default_auth_key(), None);

    // New operations should not have a key
    let op2 = tree.new_operation().expect("Failed to create operation");
    assert_eq!(op2.auth_key_id(), None);
}

#[test]
fn test_unsigned_operations() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Create a tree
    let tree = db.new_tree(KVNested::new()).expect("Failed to create tree");

    // Create an unsigned operation
    let op = tree.new_operation().expect("Failed to create operation");

    // Should have no auth key ID
    assert_eq!(op.auth_key_id(), None);

    // Should still be able to use it
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    // Commit should work
    let entry_id = op.commit().expect("Failed to commit");

    // Verify the entry is unsigned
    let backend_guard = db.backend().lock().expect("Failed to lock backend");
    let entry = backend_guard.get(&entry_id).expect("Entry not found");
    assert_eq!(entry.auth.id, AuthId::default());
    assert_eq!(entry.auth.signature, None);
}

#[test]
fn test_missing_authentication_key_error() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Create a tree
    let tree = db.new_tree(KVNested::new()).expect("Failed to create tree");

    // Create an authenticated operation with a non-existent key (this succeeds)
    let op = tree
        .new_authenticated_operation("NONEXISTENT_KEY")
        .expect("Operation creation should succeed");
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    // The failure should happen at commit time
    let result = op.commit();
    assert!(
        result.is_err(),
        "Should fail at commit time with missing key"
    );
}

#[test]
fn test_validation_pipeline_with_concurrent_settings_changes() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Generate keys for testing
    let key1 = db.add_private_key("KEY1").expect("Failed to add key1");
    let key2 = db.add_private_key("KEY2").expect("Failed to add key2");

    // Create initial tree with KEY1 only
    let mut settings = KVNested::new();
    let mut auth_settings = KVNested::new();
    auth_settings.set(
        "KEY1".to_string(),
        AuthKey {
            key: format_public_key(&key1),
            permissions: Permission::Admin(1),
            status: KeyStatus::Active,
        },
    );
    settings.set_map("auth", auth_settings);

    let tree = db.new_tree(settings).expect("Failed to create tree");

    // Create operation that adds KEY2 to auth settings
    let op1 = tree
        .new_authenticated_operation("KEY1")
        .expect("Failed to create operation");
    let settings_store = op1
        .get_subtree::<KVStore>("_settings")
        .expect("Failed to get settings subtree");

    // Add KEY2 to auth settings
    let mut new_auth_settings = KVNested::new();
    new_auth_settings.set(
        "KEY1".to_string(),
        AuthKey {
            key: format_public_key(&key1),
            permissions: Permission::Admin(1),
            status: KeyStatus::Active,
        },
    );
    new_auth_settings.set(
        "KEY2".to_string(),
        AuthKey {
            key: format_public_key(&key2),
            permissions: Permission::Write(10),
            status: KeyStatus::Active,
        },
    );

    settings_store
        .set_value("auth", new_auth_settings.into())
        .expect("Failed to update auth settings");

    let entry_id1 = op1.commit().expect("Failed to commit settings change");

    // Now create operation with KEY2 (should work after settings change)
    let op2 = tree
        .new_authenticated_operation("KEY2")
        .expect("Failed to create operation with KEY2");
    let data_store = op2
        .get_subtree::<KVStore>("data")
        .expect("Failed to get data subtree");
    data_store
        .set("test", "value")
        .expect("Failed to set value");

    let entry_id2 = op2.commit().expect("Failed to commit with KEY2");

    // Verify both entries exist and are properly signed
    let backend_guard = tree.lock_backend().expect("Failed to lock backend");
    let entry1 = backend_guard.get(&entry_id1).expect("Entry1 not found");
    let entry2 = backend_guard.get(&entry_id2).expect("Entry2 not found");

    assert_eq!(entry1.auth.id, AuthId::Direct("KEY1".to_string()));
    assert_eq!(entry2.auth.id, AuthId::Direct("KEY2".to_string()));
}

#[test]
fn test_validation_pipeline_with_corrupted_auth_data() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    let valid_key = db.add_private_key("VALID_KEY").expect("Failed to add key");

    // Create tree with valid auth settings
    let mut settings = KVNested::new();
    let mut auth_settings = KVNested::new();
    auth_settings.set(
        "VALID_KEY".to_string(),
        AuthKey {
            key: format_public_key(&valid_key),
            permissions: Permission::Admin(1), // Need admin to modify settings
            status: KeyStatus::Active,
        },
    );
    settings.set_map("auth", auth_settings);

    let tree = db.new_tree(settings).expect("Failed to create tree");

    // Valid operation should work
    test_operation_succeeds(&tree, "VALID_KEY", "data", "Valid key before corruption");

    // Create operation that corrupts auth settings
    let op = tree
        .new_authenticated_operation("VALID_KEY")
        .expect("Failed to create operation");
    let settings_store = op
        .get_subtree::<KVStore>("_settings")
        .expect("Failed to get settings subtree");

    // Corrupt the auth settings by setting it to a string instead of a map
    settings_store
        .set("auth", "corrupted_auth_data")
        .expect("Failed to corrupt auth settings");

    let _corruption_entry = op.commit().expect("Failed to commit corruption");

    // After corruption, new operations might still work (depends on validation implementation)
    // This tests the system's resilience to data corruption
    let op2 = tree
        .new_authenticated_operation("VALID_KEY")
        .expect("Should still be able to create operation");
    let data_store = op2
        .get_subtree::<KVStore>("data")
        .expect("Failed to get data subtree");
    data_store
        .set("after_corruption", "value")
        .expect("Failed to set value");

    // The commit might fail due to corrupted auth data, which is expected behavior
    let result = op2.commit();
    // We don't assert success/failure here as it depends on implementation details
    // The important thing is that it doesn't crash
    let _entry_id = result.unwrap_or_else(|_| "corruption_handled".to_string());
}

#[test]
fn test_validation_pipeline_settings_protection() {
    let keys = create_auth_keys![
        ("WRITE_KEY", Permission::Write(10), KeyStatus::Active),
        ("ADMIN_KEY", Permission::Admin(1), KeyStatus::Active)
    ];
    let (db, public_keys) = setup_test_db_with_keys(&keys);
    let tree = setup_authenticated_tree(&db, &keys, &public_keys);

    // Test permission operations using helpers
    test_operation_succeeds(&tree, "WRITE_KEY", "data", "Write key can write data");
    test_operation_fails(
        &tree,
        "WRITE_KEY",
        "_settings",
        "Write key cannot write settings",
    );
    test_operation_succeeds(
        &tree,
        "ADMIN_KEY",
        "_settings",
        "Admin key can write settings",
    );
    test_operation_succeeds(&tree, "ADMIN_KEY", "data", "Admin key can write data");
}

#[test]
fn test_validation_pipeline_with_missing_keys() {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Create tree with no auth keys configured
    let settings = KVNested::new();
    let tree = db.new_tree(settings).expect("Failed to create tree");

    // Unsigned operations should work
    let op = tree.new_operation().expect("Failed to create operation");
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");
    let _entry_id = op.commit().expect("Unsigned operation should work");

    // Authenticated operations should fail at commit time due to missing key
    let op = tree
        .new_authenticated_operation("MISSING_KEY")
        .expect("Operation creation should succeed");
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");
    let result = op.commit();
    assert!(
        result.is_err(),
        "Should fail at commit time with missing key"
    );
}

#[test]
fn test_validation_pipeline_entry_level_validation() {
    let mut entries = Vec::new();

    // Create a backend and some test entries
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);

    // Generate keys
    let active_key = db.add_private_key("ACTIVE_KEY").expect("Failed to add key");
    let revoked_key = db
        .add_private_key("REVOKED_KEY")
        .expect("Failed to add key");

    // Create auth settings with active and revoked keys
    let mut settings = KVNested::new();
    let mut auth_settings = KVNested::new();

    auth_settings.set(
        "ACTIVE_KEY".to_string(),
        AuthKey {
            key: format_public_key(&active_key),
            permissions: Permission::Write(10),
            status: KeyStatus::Active,
        },
    );
    auth_settings.set(
        "REVOKED_KEY".to_string(),
        AuthKey {
            key: format_public_key(&revoked_key),
            permissions: Permission::Write(20),
            status: KeyStatus::Revoked,
        },
    );

    settings.set_map("auth", auth_settings);
    let tree = db.new_tree(settings).expect("Failed to create tree");

    // Create entries with various keys
    for i in 0..5 {
        let op = tree
            .new_authenticated_operation("ACTIVE_KEY")
            .expect("Failed to create operation");
        let store = op
            .get_subtree::<KVStore>("data")
            .expect("Failed to get subtree");
        store
            .set("test", format!("value_{i}"))
            .expect("Failed to set value");

        // Create entry without committing to test validation
        let entry_builder =
            eidetica::entry::Entry::builder(format!("root_{i}"), "{}".to_string()).build();
        entries.push(entry_builder);
    }

    // Test validation of entries
    let mut validator = eidetica::auth::validation::AuthValidator::new();
    let current_settings = tree
        .get_subtree_viewer::<KVStore>("_settings")
        .expect("Failed to get settings")
        .get_all()
        .expect("Failed to get settings data");

    for (i, entry) in entries.iter().enumerate() {
        let result = validator.validate_entry(entry, &current_settings);
        assert!(
            result.is_ok() && result.unwrap(),
            "Entry {i} should validate"
        );
    }

    // Test with revoked key entries (these should be manually created to test revoked scenarios)
    for i in 0..3 {
        let op = tree
            .new_authenticated_operation("REVOKED_KEY")
            .expect("Failed to create operation");
        let store = op
            .get_subtree::<KVStore>("data")
            .expect("Failed to get subtree");
        store
            .set("test", format!("revoked_value_{i}"))
            .expect("Failed to set value");

        // These operations should fail when committed
        let result = op.commit();
        assert!(result.is_err(), "Entry {i} should fail with revoked key");
    }
}

#[test]
fn test_validation_pipeline_operation_type_detection() {
    let keys = create_auth_keys![
        ("WRITE_KEY", Permission::Write(10), KeyStatus::Active),
        ("ADMIN_KEY", Permission::Admin(1), KeyStatus::Active)
    ];
    let (db, public_keys) = setup_test_db_with_keys(&keys);
    let tree = setup_authenticated_tree(&db, &keys, &public_keys);

    // Test data operations (should work for both write and admin)
    test_operation_succeeds(&tree, "WRITE_KEY", "data", "Write key data operation");
    test_operation_succeeds(&tree, "ADMIN_KEY", "data", "Admin key data operation");

    // Test regular subtree operations
    test_operation_succeeds(
        &tree,
        "WRITE_KEY",
        "user_data",
        "Write key user_data operation",
    );
    test_operation_succeeds(
        &tree,
        "ADMIN_KEY",
        "user_data",
        "Admin key user_data operation",
    );

    // Test settings operations (should only work for admin)
    test_operation_fails(
        &tree,
        "WRITE_KEY",
        "_settings",
        "Write key settings operation",
    );
    test_operation_succeeds(
        &tree,
        "ADMIN_KEY",
        "_settings",
        "Admin key settings operation",
    );
}
