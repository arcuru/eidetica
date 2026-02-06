//! Integration tests for the authentication system.

use eidetica::{
    Database,
    auth::{types::AuthKey, types::Permission, types::SigKey},
    crdt::Doc,
    store::DocStore,
};

use super::helpers::*;

#[tokio::test]
async fn test_authenticated_operations() {
    let (_instance, _user, tree, test_key) =
        setup_user_and_tree_with_key("test_user", "TEST_KEY").await;

    // Create an operation - database automatically uses its configured key
    let txn = tree
        .new_transaction()
        .await
        .expect("Failed to create transaction");

    // The operation should have the correct auth key ID
    assert_eq!(txn.auth_key_name(), Some(test_key.as_str()));

    // Test that we can use the operation
    let store = txn
        .get_store::<DocStore>("data")
        .await
        .expect("Failed to get subtree");
    store
        .set("test", "value")
        .await
        .expect("Failed to set value");

    // Commit should work
    let entry_id = txn.commit().await.expect("Failed to commit");

    // Verify the entry is signed
    let entry = tree
        .get_entry(&entry_id)
        .await
        .expect("Failed to get entry");
    let hint = entry.sig.hint();
    assert!(
        hint.pubkey.as_deref() == Some(&test_key) || hint.name.as_deref() == Some("TEST_KEY"),
        "Entry should be signed by test key"
    );
}

#[tokio::test]
async fn test_mandatory_authentication() {
    let (_instance, _user, tree, test_key) =
        setup_user_and_tree_with_key("test_user", "TEST_KEY").await;

    // Create an operation - should automatically get the default auth key
    let txn = tree
        .new_transaction()
        .await
        .expect("Failed to create transaction");

    // Should have the default auth key ID set automatically
    assert_eq!(txn.auth_key_name(), Some(test_key.as_str()));

    // Should be able to use it normally
    let store = txn
        .get_store::<DocStore>("data")
        .await
        .expect("Failed to get subtree");
    store
        .set("test", "value")
        .await
        .expect("Failed to set value");

    // Commit should succeed with authentication
    let result = txn.commit().await;
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
    let key2_id = user
        .add_private_key(Some("KEY2"))
        .await
        .expect("Failed to add key2");

    // Create initial tree with KEY1 (signing key becomes Admin(0))
    let tree = user
        .create_database(Doc::new(), &key1_id)
        .await
        .expect("Failed to create tree");

    // Create operation that adds KEY2 to auth settings
    let txn1 = tree
        .new_transaction()
        .await
        .expect("Failed to create transaction");
    let settings_store = txn1.get_settings().expect("Failed to get settings store");

    // Add KEY2 to auth settings using SettingsStore
    let key2_auth = AuthKey::active(Some("KEY2"), Permission::Write(10));
    settings_store
        .set_auth_key(&key2_id, key2_auth)
        .await
        .expect("Failed to add KEY2 to auth settings");

    let entry_id1 = txn1
        .commit()
        .await
        .expect("Failed to commit settings change");

    // Now reload the database with KEY2 to use it (User API pattern)
    let key2_signing_key = user
        .get_signing_key(&key2_id)
        .expect("Failed to get KEY2 signing key")
        .clone();

    let tree_with_key2 = Database::open(
        instance.clone(),
        tree.root_id(),
        key2_signing_key,
        key2_id.clone(),
    )
    .await
    .expect("Failed to reload database with KEY2");

    // Create operation with KEY2 (should work after settings change)
    let txn2 = tree_with_key2
        .new_transaction()
        .await
        .expect("Failed to create transaction with KEY2");
    let data_store = txn2
        .get_store::<DocStore>("data")
        .await
        .expect("Failed to get data subtree");
    data_store
        .set("test", "value")
        .await
        .expect("Failed to set value");

    let entry_id2 = txn2.commit().await.expect("Failed to commit with KEY2");

    // Verify both entries exist and are properly signed
    let entry1 = tree
        .get_entry(&entry_id1)
        .await
        .expect("Failed to get entry1");
    assert_eq!(entry1.sig.key, SigKey::from_pubkey(&key1_id));

    let entry2 = tree_with_key2
        .get_entry(&entry_id2)
        .await
        .expect("Failed to get entry2");
    assert_eq!(entry2.sig.key, SigKey::from_pubkey(&key2_id));
}

#[tokio::test]
async fn test_prevent_auth_corruption() {
    let (_instance, mut user) = crate::helpers::test_instance_with_user("test_user").await;

    let valid_key_id = user
        .add_private_key(Some("VALID_KEY"))
        .await
        .expect("Failed to add key");

    // Create tree with valid key (signing key becomes Admin(0))
    let tree = user
        .create_database(Doc::new(), &valid_key_id)
        .await
        .expect("Failed to create tree");

    // Valid operation should work
    let txn_valid = tree
        .new_transaction()
        .await
        .expect("Failed to create transaction");
    let data_store_valid = txn_valid
        .get_store::<DocStore>("data")
        .await
        .expect("Failed to get subtree");
    data_store_valid
        .set("test", "value")
        .await
        .expect("Failed to set value");
    assert!(
        txn_valid.commit().await.is_ok(),
        "Valid key before corruption should work"
    );

    // Test corruption path 1: Try to corrupt auth settings by setting to wrong type (String instead of Doc)
    let txn = tree
        .new_transaction()
        .await
        .expect("Failed to create transaction");
    let settings_store = txn
        .get_store::<DocStore>("_settings")
        .await
        .expect("Failed to get settings subtree");

    // Corrupt the auth settings by setting it to a string instead of a map
    settings_store
        .set("auth", "corrupted_auth_data")
        .await
        .expect("Failed to corrupt auth settings");

    // The system prevents corruption at commit time
    let result = txn.commit().await;
    assert!(
        result.is_err(),
        "Corruption commit (wrong type) should fail immediately"
    );
    assert!(
        result.unwrap_err().is_authentication_error(),
        "Should be authentication error"
    );

    // Test corruption path 2: Delete auth (creates CRDT tombstone)
    let txn = tree
        .new_transaction()
        .await
        .expect("Failed to create transaction");
    let settings_store = txn
        .get_store::<DocStore>("_settings")
        .await
        .expect("Failed to get settings");
    settings_store
        .delete("auth")
        .await
        .expect("Failed to delete auth settings");

    let result = txn.commit().await;
    assert!(
        result.is_err(),
        "Deletion commit (tombstone) should fail immediately"
    );
    assert!(
        result.unwrap_err().is_authentication_error(),
        "Should be authentication error"
    );

    // Verify database is still functional after preventing corruption
    let txn = tree
        .new_transaction()
        .await
        .expect("Failed to create transaction");
    let store = txn
        .get_store::<DocStore>("data")
        .await
        .expect("Failed to get store");
    store
        .set("after_prevented_corruption", "value")
        .await
        .expect("Failed to set value");
    txn.commit()
        .await
        .expect("Normal operations should still work after preventing corruption");
}
