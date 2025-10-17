use eidetica::{crdt::Doc, store::DocStore};

use super::helpers::*;

#[test]
fn test_authenticated_operations() {
    let (_instance, _user, tree, test_key) = setup_user_and_tree_with_key("test_user", "TEST_KEY");

    // Create an authenticated operation using the key_id
    let op = tree
        .new_authenticated_operation(&test_key)
        .expect("Failed to create authenticated operation");

    // The operation should have the correct auth key ID
    assert_eq!(op.auth_key_name(), Some(test_key.as_str()));

    // Test that we can use the operation
    let store = op
        .get_store::<DocStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    // Commit should work
    let entry_id = op.commit().expect("Failed to commit");

    // Verify the entry is signed
    let entry = tree.get_entry(&entry_id).expect("Failed to get entry");
    assert!(entry.sig.is_signed_by(&test_key));
}

#[test]
fn test_validation_pipeline_with_corrupted_auth_data() {
    let (_instance, mut user) = crate::helpers::test_instance_with_user("test_user");

    let valid_key_id = user
        .add_private_key(Some("VALID_KEY"))
        .expect("Failed to add key");

    // Create tree - auth is automatically bootstrapped with valid_key_id
    // The key is added to auth settings with Permission::Admin(0) and KeyStatus::Active
    let tree = user
        .new_database(Doc::new(), &valid_key_id)
        .expect("Failed to create tree");

    // Valid operation should work
    test_operation_succeeds(&tree, &valid_key_id, "data", "Valid key before corruption");

    // Create operation that corrupts auth settings
    let op = tree
        .new_authenticated_operation(&valid_key_id)
        .expect("Failed to create operation");
    let settings_store = op
        .get_store::<DocStore>("_settings")
        .expect("Failed to get settings subtree");

    // Corrupt the auth settings by setting it to a string instead of a map
    settings_store
        .set("auth", "corrupted_auth_data")
        .expect("Failed to corrupt auth settings");

    let _corruption_entry = op.commit().expect("Failed to commit corruption");

    // After corruption, the system takes a fail-safe approach
    // Both authenticated and unsigned operations should fail when auth is corrupted
    // This prevents operations from proceeding with invalid security configuration

    // Test that authenticated operations fail
    let authenticated_op = tree
        .new_authenticated_operation(&valid_key_id)
        .expect("Should be able to create operation");
    let auth_store = authenticated_op
        .get_store::<DocStore>("data")
        .expect("Failed to get data subtree");
    auth_store
        .set("should_fail", "value")
        .expect("Failed to set value");

    let auth_result = authenticated_op.commit();
    assert!(
        auth_result.is_err(),
        "Authenticated operation should fail with corrupted auth settings"
    );

    // Test that even unsigned operations fail (fail-safe behavior)
    let unsigned_op = tree
        .new_transaction()
        .expect("Should be able to create unsigned operation");
    let unsigned_store = unsigned_op
        .get_store::<DocStore>("data")
        .expect("Failed to get data subtree");
    unsigned_store
        .set("unsigned_after_corruption", "value")
        .expect("Failed to set value");

    let unsigned_result = unsigned_op.commit();
    assert!(
        unsigned_result.is_err(),
        "Unsigned operations should also fail with corrupted auth (fail-safe behavior)"
    );
    let err = unsigned_result.unwrap_err();
    assert!(err.is_authentication_error());
}
