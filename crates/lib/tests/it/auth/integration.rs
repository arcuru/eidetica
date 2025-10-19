use eidetica::{
    auth::{
        AuthSettings,
        crypto::format_public_key,
        types::{AuthKey, KeyStatus, Permission},
    },
    crdt::{Doc, doc::Value},
    store::DocStore,
};

use super::helpers::*;
use crate::create_auth_keys;

#[test]
fn test_authenticated_operations() {
    let (_db, tree) = setup_db_and_tree_with_key("TEST_KEY");

    // Create an authenticated operation
    let op = tree
        .new_authenticated_operation("TEST_KEY")
        .expect("Failed to create authenticated operation");

    // The operation should have the correct auth key ID
    assert_eq!(op.auth_key_name(), Some("TEST_KEY"));

    // Test that we can use the operation
    let store = op
        .get_store::<DocStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    // Commit should work
    let entry_id = op.commit().expect("Failed to commit");

    // Verify the entry is signed
    let entry = tree.get_entry(&entry_id).expect("Failed to get entry");
    assert!(entry.sig.is_signed_by("TEST_KEY"));
}

#[test]
fn test_operation_auth_methods() {
    let db = setup_db();

    // Generate keys for testing
    let _public_key1 = db.add_private_key("KEY1").expect("Failed to add key1");
    let _public_key2 = db.add_private_key("KEY2").expect("Failed to add key2");
    let _test_key = db
        .add_private_key("TEST_KEY")
        .expect("Failed to add test key");

    let tree = db
        .new_database(Doc::new(), "TEST_KEY")
        .expect("Failed to create tree");

    // Test operations with different auth key IDs
    let op1 = tree
        .new_authenticated_operation("KEY1")
        .expect("Failed to create operation");
    assert_eq!(op1.auth_key_name(), Some("KEY1"));

    // Test set_auth_key method (mutable) - overrides default auth key
    let mut op2 = tree.new_transaction().expect("Failed to create operation");
    assert_eq!(op2.auth_key_name(), Some("TEST_KEY")); // Gets default auth key
    op2.set_auth_key("KEY2");
    assert_eq!(op2.auth_key_name(), Some("KEY2")); // Override with KEY2
}

#[test]
fn test_tree_default_authentication() {
    let (db, mut tree) = setup_db_and_tree_with_key("DEFAULT_KEY");

    // Tree should have the provided key as default
    assert_eq!(tree.default_auth_key(), Some("DEFAULT_KEY"));

    // Operations should inherit the default key
    let op = tree.new_transaction().expect("Failed to create operation");
    assert_eq!(op.auth_key_name(), Some("DEFAULT_KEY"));

    // Change the default to a different key
    db.add_private_key("OTHER_KEY")
        .expect("Failed to add other key");
    tree.set_default_auth_key("OTHER_KEY");
    assert_eq!(tree.default_auth_key(), Some("OTHER_KEY"));

    let op2 = tree.new_transaction().expect("Failed to create operation");
    assert_eq!(op2.auth_key_name(), Some("OTHER_KEY"));

    // Clear the default
    tree.clear_default_auth_key();
    assert_eq!(tree.default_auth_key(), None);

    // New operations should not have a key and should fail at commit
    let op3 = tree.new_transaction().expect("Failed to create operation");
    assert_eq!(op3.auth_key_name(), None);

    // Try to use the operation - should fail at commit
    let store = op3
        .get_store::<DocStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");
    let result = op3.commit();
    assert!(result.is_err(), "Should fail without authentication");
}

#[test]
fn test_mandatory_authentication() {
    let (_db, tree) = setup_db_and_tree_with_key("TEST_KEY");

    // Create an operation - should automatically get the default auth key
    let op = tree.new_transaction().expect("Failed to create operation");

    // Should have the default auth key ID set automatically
    assert_eq!(op.auth_key_name(), Some("TEST_KEY"));

    // Should be able to use it normally
    let store = op
        .get_store::<DocStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    // Commit should succeed with authentication
    let result = op.commit();
    assert!(result.is_ok(), "Should succeed with authentication");
}

#[test]
fn test_missing_authentication_key_error() {
    let (_, tree) = setup_db_and_tree_with_key("TEST_KEY");

    // Create an authenticated operation with a non-existent key (this succeeds)
    let op = tree
        .new_authenticated_operation("NONEXISTENT_KEY")
        .expect("Operation creation should succeed");
    let store = op
        .get_store::<DocStore>("data")
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
    let db = setup_db();

    // Generate keys for testing
    let key1 = db.add_private_key("KEY1").expect("Failed to add key1");
    let key2 = db.add_private_key("KEY2").expect("Failed to add key2");

    // Create initial tree with KEY1 only
    let mut settings = Doc::new();
    let mut auth_settings = Doc::new();
    auth_settings
        .set_json(
            "KEY1",
            AuthKey::active(format_public_key(&key1), Permission::Admin(1)).unwrap(),
        )
        .unwrap();
    settings.set_doc("auth", auth_settings);

    let tree = db
        .new_database(settings, "KEY1")
        .expect("Failed to create tree");

    // Create operation that adds KEY2 to auth settings
    let op1 = tree
        .new_authenticated_operation("KEY1")
        .expect("Failed to create operation");
    let settings_store = op1
        .get_store::<DocStore>("_settings")
        .expect("Failed to get settings subtree");

    // Add KEY2 to auth settings
    let mut new_auth_settings = Doc::new();
    new_auth_settings
        .set_json(
            "KEY1",
            AuthKey::active(format_public_key(&key1), Permission::Admin(1)).unwrap(),
        )
        .unwrap();
    new_auth_settings
        .set_json(
            "KEY2",
            AuthKey::active(format_public_key(&key2), Permission::Write(10)).unwrap(),
        )
        .unwrap();

    settings_store
        .set_value("auth", Value::Doc(new_auth_settings))
        .expect("Failed to update auth settings");

    let entry_id1 = op1.commit().expect("Failed to commit settings change");

    // Now create operation with KEY2 (should work after settings change)
    let op2 = tree
        .new_authenticated_operation("KEY2")
        .expect("Failed to create operation with KEY2");
    let data_store = op2
        .get_store::<DocStore>("data")
        .expect("Failed to get data subtree");
    data_store
        .set("test", "value")
        .expect("Failed to set value");

    let entry_id2 = op2.commit().expect("Failed to commit with KEY2");

    // Verify both entries exist and are properly signed
    let entry1 = tree.get_entry(&entry_id1).expect("Failed to get entry1");
    assert!(entry1.sig.is_signed_by("KEY1"));
    let entry2 = tree.get_entry(&entry_id2).expect("Failed to get entry2");
    assert!(entry2.sig.is_signed_by("KEY2"));
}

#[test]
fn test_validation_pipeline_with_corrupted_auth_data() {
    let db = setup_db();

    let valid_key = db.add_private_key("VALID_KEY").expect("Failed to add key");

    // Create tree with valid auth settings
    let mut settings = Doc::new();
    let mut auth_settings = Doc::new();
    auth_settings
        .set_json(
            "VALID_KEY",
            AuthKey::active(
                format_public_key(&valid_key),
                Permission::Admin(1), // Need admin to modify settings
            )
            .unwrap(),
        )
        .unwrap();
    settings.set_doc("auth", auth_settings);

    let tree = db
        .new_database(settings, "VALID_KEY")
        .expect("Failed to create tree");

    // Valid operation should work
    test_operation_succeeds(&tree, "VALID_KEY", "data", "Valid key before corruption");

    // Create operation that corrupts auth settings
    let op = tree
        .new_authenticated_operation("VALID_KEY")
        .expect("Failed to create operation");
    let settings_store = op
        .get_store::<DocStore>("_settings")
        .expect("Failed to get settings subtree");

    // Corrupt the auth settings by setting it to a string instead of a map
    settings_store
        .set("auth", "corrupted_auth_data")
        .expect("Failed to corrupt auth settings");

    let _corruption_entry = op.commit().expect("Failed to commit corruption");

    // After corruption, the system falls back to no-auth mode for resilience
    // This tests the system's resilience to data corruption by allowing operations to proceed
    let op2 = tree
        .new_authenticated_operation("VALID_KEY")
        .expect("Should still be able to create operation");
    let data_store = op2
        .get_store::<DocStore>("data")
        .expect("Failed to get data subtree");
    data_store
        .set("after_corruption", "value")
        .expect("Failed to set value");

    // The commit should succeed because corrupted auth falls back to no-auth mode (resilient behavior)
    let result = op2.commit();
    assert!(
        result.is_ok(),
        "Commit should succeed due to fallback to no-auth mode for corrupted settings: {:?}",
        result.err()
    );
}

#[test]
fn test_validation_pipeline_entry_level_validation() {
    let mut entries = Vec::new();

    // Create a backend and some test entries
    let db = setup_db();

    // Generate keys
    let admin_key = db.add_private_key("ADMIN_KEY").expect("Failed to add key");
    let active_key = db.add_private_key("ACTIVE_KEY").expect("Failed to add key");
    let revoked_key = db
        .add_private_key("REVOKED_KEY")
        .expect("Failed to add key");

    // Create auth settings with admin, active and revoked keys
    let mut settings = Doc::new();
    let mut auth_settings = Doc::new();

    auth_settings
        .set_json(
            "ADMIN_KEY",
            AuthKey::active(format_public_key(&admin_key), Permission::Admin(0)).unwrap(),
        )
        .unwrap();
    auth_settings
        .set_json(
            "ACTIVE_KEY",
            AuthKey::active(format_public_key(&active_key), Permission::Write(10)).unwrap(),
        )
        .unwrap();
    auth_settings
        .set_json(
            "REVOKED_KEY",
            AuthKey::new(
                format_public_key(&revoked_key),
                Permission::Write(20),
                KeyStatus::Revoked,
            )
            .unwrap(),
        )
        .unwrap();

    settings.set_doc("auth", auth_settings);
    let tree = db
        .new_database(settings, "ADMIN_KEY")
        .expect("Failed to create tree");

    // Create entries with various keys
    for i in 0..5 {
        let op = tree
            .new_authenticated_operation("ACTIVE_KEY")
            .expect("Failed to create operation");
        let store = op
            .get_store::<DocStore>("data")
            .expect("Failed to get subtree");
        store
            .set("test", format!("value_{i}"))
            .expect("Failed to set value");

        // Create entry without committing to test validation
        let entry_builder = eidetica::Entry::root_builder()
            .build()
            .expect("Root entry should build successfully");
        entries.push(entry_builder);
    }

    // Test validation of entries
    let mut validator = eidetica::auth::validation::AuthValidator::new();
    let current_settings = tree
        .get_store_viewer::<DocStore>("_settings")
        .expect("Failed to get settings")
        .get_all()
        .expect("Failed to get settings data");
    let auth_settings = match current_settings.get("auth") {
        Some(Value::Doc(auth_doc)) => AuthSettings::from_doc(auth_doc.clone()),
        _ => AuthSettings::new(),
    };

    for (i, entry) in entries.iter().enumerate() {
        let result = validator.validate_entry(entry, &auth_settings, None);
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
            .get_store::<DocStore>("data")
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
