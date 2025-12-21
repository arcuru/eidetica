use eidetica::{
    auth::{AuthKey, Permission, format_public_key},
    crdt::Doc,
    store::DocStore,
};

use super::helpers::*;

#[tokio::test]
async fn test_authenticated_operations() {
    let (_instance, _user, tree, test_key) = setup_user_and_tree_with_key("test_user", "TEST_KEY").await;

    // Create an operation - database automatically uses its configured key
    let op = tree
        .new_transaction().await
        .expect("Failed to create transaction");

    // The operation should have the correct auth key ID
    assert_eq!(op.auth_key_name(), Some(test_key.as_str()));

    // Test that we can use the operation
    let store = op
        .get_store::<DocStore>("data").await
        .expect("Failed to get subtree");
    store.set("test", "value").await.expect("Failed to set value");

    // Commit should work
    let entry_id = op.commit().await.expect("Failed to commit");

    // Verify the entry is signed
    let entry = tree.get_entry(&entry_id).await.expect("Failed to get entry");
    assert!(entry.sig.is_signed_by(&test_key));
}

#[tokio::test]
async fn test_tree_default_authentication() {
    let (instance, mut user, tree, default_key) =
        setup_user_and_tree_with_key("test_user", "DEFAULT_KEY").await;

    // Tree should have the provided key as default
    assert_eq!(tree.default_auth_key(), Some(default_key.as_str()));

    // Operations should inherit the default key
    let op = tree.new_transaction().await.expect("Failed to create operation");
    assert_eq!(op.auth_key_name(), Some(default_key.as_str()));

    // Add another key and reload database with it
    let other_key = user
        .add_private_key(Some("OTHER_KEY"))
        .await
        .expect("Failed to add other key");

    // Get the signing key from UserKeyManager to reload the database
    let other_signing_key = user
        .get_signing_key(&other_key)
        .expect("Failed to get signing key")
        .clone();

    // Reload the database with the other key - User API requires new Database instance per key
    let tree_with_other_key = eidetica::Database::open(
        instance.clone(),
        tree.root_id(),
        other_signing_key,
        other_key.clone(),
    )
    .expect("Failed to reload database with other key");

    assert_eq!(
        tree_with_other_key.default_auth_key(),
        Some(other_key.as_str())
    );

    let op2 = tree_with_other_key
        .new_transaction().await
        .expect("Failed to create operation");
    assert_eq!(op2.auth_key_name(), Some(other_key.as_str()));

    // Test database without key (using open_readonly) - operations should have no key
    let tree_no_key = eidetica::Database::open_readonly(tree.root_id().clone(), &instance)
        .expect("Failed to create database without key");

    assert_eq!(tree_no_key.default_auth_key(), None);

    // New transactions should not have a key and should fail at commit
    let op3 = tree_no_key
        .new_transaction().await
        .expect("Failed to create operation");
    assert_eq!(op3.auth_key_name(), None);

    // Try to use the transaction - should fail at commit
    let store = op3
        .get_store::<DocStore>("data").await
        .expect("Failed to get subtree");
    store.set("test", "value").await.expect("Failed to set value");
    let result = op3.commit().await;
    assert!(result.is_err(), "Should fail without authentication");
}

#[tokio::test]
async fn test_mandatory_authentication() {
    let (_instance, _user, tree, test_key) = setup_user_and_tree_with_key("test_user", "TEST_KEY").await;

    // Create an operation - should automatically get the default auth key
    let op = tree.new_transaction().await.expect("Failed to create operation");

    // Should have the default auth key ID set automatically
    assert_eq!(op.auth_key_name(), Some(test_key.as_str()));

    // Should be able to use it normally
    let store = op
        .get_store::<DocStore>("data").await
        .expect("Failed to get subtree");
    store.set("test", "value").await.expect("Failed to set value");

    // Commit should succeed with authentication
    let result = op.commit().await;
    assert!(result.is_ok(), "Should succeed with authentication");
}

#[tokio::test]
async fn test_validation_pipeline_with_concurrent_settings_changes() {
    let (instance, mut user) = crate::helpers::test_instance_with_user("test_user").await;

    // Generate keys for testing
    let key1_id = user
        .add_private_key(Some("KEY1"))
        .await
        .expect("Failed to add key1");
    let key1_pubkey =
        eidetica::auth::crypto::parse_public_key(&key1_id).expect("Failed to parse key1");
    let key2_id = user
        .add_private_key(Some("KEY2"))
        .await
        .expect("Failed to add key2");
    let key2_pubkey =
        eidetica::auth::crypto::parse_public_key(&key2_id).expect("Failed to parse key2");

    // Create initial tree with KEY1 only
    let mut settings = Doc::new();
    let mut auth_settings = Doc::new();
    auth_settings
        .set_json(
            &key1_id,
            AuthKey::active(format_public_key(&key1_pubkey), Permission::Admin(1)).unwrap(),
        )
        .unwrap();
    settings.set("auth", auth_settings);

    let tree = user
        .create_database(settings, &key1_id)
        .await
        .expect("Failed to create tree");

    // Create operation that adds KEY2 to auth settings
    let op1 = tree.new_transaction().await.expect("Failed to create operation");
    let settings_store = op1.get_settings().expect("Failed to get settings store");

    // Add KEY2 to auth settings using SettingsStore
    let key2_auth =
        AuthKey::active(format_public_key(&key2_pubkey), Permission::Write(10)).unwrap();
    settings_store
        .set_auth_key(&key2_id, key2_auth)
        .await
        .expect("Failed to add KEY2 to auth settings");

    let entry_id1 = op1.commit().await.expect("Failed to commit settings change");

    // Now reload the database with KEY2 to use it (User API pattern)
    let key2_signing_key = user
        .get_signing_key(&key2_id)
        .expect("Failed to get KEY2 signing key")
        .clone();

    let tree_with_key2 = eidetica::Database::open(
        instance.clone(),
        tree.root_id(),
        key2_signing_key,
        key2_id.clone(),
    )
    .expect("Failed to reload database with KEY2");

    // Create operation with KEY2 (should work after settings change)
    let op2 = tree_with_key2
        .new_transaction().await
        .expect("Failed to create operation with KEY2");
    let data_store = op2
        .get_store::<DocStore>("data").await
        .expect("Failed to get data subtree");
    data_store
        .set("test", "value")
        .await
        .expect("Failed to set value");

    let entry_id2 = op2.commit().await.expect("Failed to commit with KEY2");

    // Verify both entries exist and are properly signed
    let entry1 = tree.get_entry(&entry_id1).await.expect("Failed to get entry1");
    assert!(entry1.sig.is_signed_by(&key1_id));
    let entry2 = tree_with_key2
        .get_entry(&entry_id2)
        .await
        .expect("Failed to get entry2");
    assert!(entry2.sig.is_signed_by(&key2_id));
}

#[tokio::test]
async fn test_prevent_auth_corruption() {
    let (_instance, mut user) = crate::helpers::test_instance_with_user("test_user").await;

    let valid_key_id = user
        .add_private_key(Some("VALID_KEY"))
        .await
        .expect("Failed to add key");
    let valid_key_pubkey =
        eidetica::auth::crypto::parse_public_key(&valid_key_id).expect("Failed to parse key");

    // Create tree with valid auth settings
    let mut settings = Doc::new();
    let mut auth_settings = Doc::new();
    auth_settings
        .set_json(
            &valid_key_id,
            AuthKey::active(
                format_public_key(&valid_key_pubkey),
                Permission::Admin(1), // Need admin to modify settings
            )
            .unwrap(),
        )
        .unwrap();
    settings.set("auth", auth_settings);

    let tree = user
        .create_database(settings, &valid_key_id)
        .await
        .expect("Failed to create tree");

    // Valid operation should work
    let op_valid = tree.new_transaction().await.expect("Failed to create operation");
    let data_store_valid = op_valid
        .get_store::<DocStore>("data").await
        .expect("Failed to get subtree");
    data_store_valid
        .set("test", "value")
        .await
        .expect("Failed to set value");
    assert!(
        op_valid.commit().await.is_ok(),
        "Valid key before corruption should work"
    );

    // Test corruption path 1: Try to corrupt auth settings by setting to wrong type (String instead of Doc)
    let op = tree.new_transaction().await.expect("Failed to create operation");
    let settings_store = op
        .get_store::<DocStore>("_settings").await
        .expect("Failed to get settings subtree");

    // Corrupt the auth settings by setting it to a string instead of a map
    settings_store
        .set("auth", "corrupted_auth_data")
        .await
        .expect("Failed to corrupt auth settings");

    // The system prevents corruption at commit time
    let result = op.commit().await;
    assert!(
        result.is_err(),
        "Corruption commit (wrong type) should fail immediately"
    );
    assert!(
        result.unwrap_err().is_authentication_error(),
        "Should be authentication error"
    );

    // Test corruption path 2: Delete auth (creates CRDT tombstone)
    let op = tree.new_transaction().await.expect("Failed to create operation");
    let settings_store = op
        .get_store::<DocStore>("_settings").await
        .expect("Failed to get settings");
    settings_store
        .delete("auth")
        .await
        .expect("Failed to delete auth settings");

    let result = op.commit().await;
    assert!(
        result.is_err(),
        "Deletion commit (tombstone) should fail immediately"
    );
    assert!(
        result.unwrap_err().is_authentication_error(),
        "Should be authentication error"
    );

    // Verify database is still functional after preventing corruption
    let op = tree.new_transaction().await.expect("Failed to create operation");
    let store = op
        .get_store::<DocStore>("data").await
        .expect("Failed to get store");
    store
        .set("after_prevented_corruption", "value")
        .await
        .expect("Failed to set value");
    op.commit().await
        .expect("Normal operations should still work after preventing corruption");
}
