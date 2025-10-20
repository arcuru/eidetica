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
fn test_prevent_auth_corruption() {
    let (_instance, mut user) = crate::helpers::test_instance_with_user("test_user");

    let key_id = user
        .get_default_key()
        .expect("User should have default key");

    // Create database (auth automatically bootstrapped with user's key)
    let tree = user
        .new_database(Doc::new(), &key_id)
        .expect("Failed to create tree");

    // Verify initial operation works
    let op = tree.new_transaction().expect("Failed to create operation");
    let store = op
        .get_store::<DocStore>("data")
        .expect("Failed to get store");
    store
        .set("before_corruption", "value")
        .expect("Failed to set");
    op.commit().expect("Initial operation should succeed");

    // Test corruption path 1: Set auth to wrong type (String instead of Doc)
    let op = tree.new_transaction().expect("Failed to create operation");
    let settings_store = op
        .get_store::<DocStore>("_settings")
        .expect("Failed to get settings");
    settings_store
        .set("auth", "corrupted_auth_data")
        .expect("Failed to corrupt auth settings");

    let result = op.commit();
    assert!(
        result.is_err(),
        "Corruption commit (wrong type) should fail immediately"
    );
    assert!(
        result.unwrap_err().is_authentication_error(),
        "Should be authentication error"
    );

    // Test corruption path 2: Delete auth (creates CRDT tombstone)
    let op = tree.new_transaction().expect("Failed to create operation");
    let settings_store = op
        .get_store::<DocStore>("_settings")
        .expect("Failed to get settings");
    settings_store
        .delete("auth")
        .expect("Failed to delete auth settings");

    let result = op.commit();
    assert!(
        result.is_err(),
        "Deletion commit (tombstone) should fail immediately"
    );
    assert!(
        result.unwrap_err().is_authentication_error(),
        "Should be authentication error"
    );

    // Verify database is still functional after preventing corruption
    let op = tree.new_transaction().expect("Failed to create operation");
    let store = op
        .get_store::<DocStore>("data")
        .expect("Failed to get store");
    store
        .set("after_prevented_corruption", "value")
        .expect("Failed to set value");
    op.commit()
        .expect("Normal operations should still work after preventing corruption");
}
