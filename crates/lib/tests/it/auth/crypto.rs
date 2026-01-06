//! Tests for cryptographic operations in the authentication system.

use eidetica::{
    auth::{
        crypto::format_public_key,
        types::{Permission, SigKey},
    },
    crdt::Doc,
};

use crate::helpers::*;

#[tokio::test]
async fn test_key_management() {
    let (_instance, mut user) = test_instance_with_user("test_user").await;

    // Initially should have default_key only (created during User creation)
    let keys = user.list_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 1);

    // Add a key
    let key_id = user
        .add_private_key(Some("TEST_KEY"))
        .await
        .expect("Failed to add key");
    let public_key =
        eidetica::auth::crypto::parse_public_key(&key_id).expect("Failed to parse key");

    // List keys should now show two keys (default_key + TEST_KEY)
    let keys = user.list_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 2);
    assert!(keys.contains(&key_id));

    // Add another key
    let key_id2 = user
        .add_private_key(Some("TEST_KEY_2"))
        .await
        .expect("Failed to add second key");
    let public_key2 =
        eidetica::auth::crypto::parse_public_key(&key_id2).expect("Failed to parse key");

    // List keys should now show three keys (default_key + TEST_KEY + TEST_KEY_2)
    let keys = user.list_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 3);
    assert!(keys.contains(&key_id));
    assert!(keys.contains(&key_id2));

    // Keys should be different
    assert_ne!(
        format_public_key(&public_key),
        format_public_key(&public_key2)
    );

    // Test signing and verification
    let tree = user
        .create_database(Doc::new(), &key_id)
        .await
        .expect("Failed to create tree");
    let op = tree
        .new_transaction()
        .await
        .expect("Failed to create operation");
    let store = op
        .get_store::<eidetica::store::DocStore>("data")
        .await
        .expect("Failed to get subtree");
    store
        .set("test", "value")
        .await
        .expect("Failed to set value");

    let entry_id = op.commit().await.expect("Failed to commit");

    // Verify the entry was signed correctly
    let entry = tree
        .get_entry(&entry_id)
        .await
        .expect("Failed to get entry");
    assert_eq!(entry.sig.key, SigKey::Direct(key_id.to_string()));
    assert!(entry.sig.sig.is_some());

    // Verify signature with tree's auth configuration
    assert!(
        tree.verify_entry_signature(&entry_id)
            .await
            .expect("Failed to verify")
    );
}

#[tokio::test]
async fn test_generated_key_can_sign() {
    // Validates that keys generated via User API can be used for signing

    let (_instance, mut user) = test_instance_with_user("test_user").await;

    // Add a key (will be generated internally)
    let key_id = user
        .add_private_key(Some("TEST_KEY"))
        .await
        .expect("Failed to add key");

    // The key should be in the list (plus default_key)
    let keys = user.list_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 2);
    assert!(keys.contains(&key_id));

    // Test that we can sign with the key
    let tree = user
        .create_database(Doc::new(), &key_id)
        .await
        .expect("Failed to create tree");
    let op = tree
        .new_transaction()
        .await
        .expect("Failed to create operation");
    let store = op
        .get_store::<eidetica::store::DocStore>("data")
        .await
        .expect("Failed to get subtree");
    store
        .set("test", "value")
        .await
        .expect("Failed to set value");

    let entry_id = op.commit().await.expect("Failed to commit");

    // Verify the entry was signed correctly
    let entry = tree
        .get_entry(&entry_id)
        .await
        .expect("Failed to get entry");
    assert_eq!(entry.sig.key, SigKey::Direct(key_id.to_string()));
    assert!(
        tree.verify_entry_signature(&entry_id)
            .await
            .expect("Failed to verify")
    );
}

#[tokio::test]
async fn test_multi_key_authentication() {
    use eidetica::auth::crypto::format_public_key;
    use eidetica::auth::types::AuthKey;

    let (instance, mut user) = test_instance_with_user("test_user").await;

    // Add two keys using User API
    let key_id1 = user
        .add_private_key(Some("TEST_KEY"))
        .await
        .expect("Failed to add key");
    let public_key1 =
        eidetica::auth::crypto::parse_public_key(&key_id1).expect("Failed to parse key");

    let key_id2 = user
        .add_private_key(Some("SECOND_KEY"))
        .await
        .expect("Failed to add second key");
    let public_key2 =
        eidetica::auth::crypto::parse_public_key(&key_id2).expect("Failed to parse key");

    // Set up authentication settings with both keys
    // Note: First key needs admin permission to create tree with auth settings
    let mut settings = Doc::new();
    let mut auth_settings = Doc::new();
    auth_settings
        .set_json(
            &key_id1,
            AuthKey::active(format_public_key(&public_key1), Permission::Admin(0)).unwrap(),
        )
        .unwrap();
    auth_settings
        .set_json(
            &key_id2,
            AuthKey::active(format_public_key(&public_key2), Permission::Write(20)).unwrap(),
        )
        .unwrap();
    settings.set("auth", auth_settings);

    // Create database with first key (admin key)
    let tree = user
        .create_database(settings, &key_id1)
        .await
        .expect("Failed to create tree");

    // Create an entry with first key (tree is already loaded with key_id1)
    let op = tree
        .new_transaction()
        .await
        .expect("Failed to create operation");
    let store = op
        .get_store::<eidetica::store::DocStore>("data")
        .await
        .expect("Failed to get subtree");
    store
        .set("test", "value")
        .await
        .expect("Failed to set value");

    let entry_id = op.commit().await.expect("Failed to commit");

    // Verify entry can be retrieved and is properly signed
    let entry = tree
        .get_entry(&entry_id)
        .await
        .expect("Failed to get entry");
    assert_eq!(entry.sig.key, SigKey::Direct(key_id1.to_string()));
    assert!(entry.sig.sig.is_some());
    assert!(
        tree.verify_entry_signature(&entry_id)
            .await
            .expect("Failed to verify")
    );

    // Create another entry with the second key - need to reload database with that key
    let signing_key2_for_load = user
        .get_signing_key(&key_id2)
        .expect("Failed to get signing key")
        .clone();
    let tree_with_key2 = eidetica::Database::open(
        instance.clone(),
        tree.root_id(),
        signing_key2_for_load,
        key_id2.clone(),
    )
    .await
    .expect("Failed to load database with key2");

    let op2 = tree_with_key2
        .new_transaction()
        .await
        .expect("Failed to create operation");
    let store2 = op2
        .get_store::<eidetica::store::DocStore>("data")
        .await
        .expect("Failed to get subtree");
    store2
        .set("test2", "value2")
        .await
        .expect("Failed to set value");

    let entry_id2 = op2.commit().await.expect("Failed to commit");

    // Verify second entry (can verify from either tree instance)
    let entry2 = tree
        .get_entry(&entry_id2)
        .await
        .expect("Failed to get entry2");
    assert_eq!(entry2.sig.key, SigKey::Direct(key_id2.to_string()));
    assert!(
        tree.verify_entry_signature(&entry_id2)
            .await
            .expect("Failed to verify")
    );
}

#[tokio::test]
async fn test_keys_have_unique_identity() {
    // Validates that each key added via User API has a unique identity

    let (_instance, mut user) = test_instance_with_user("test_user").await;

    // Add initial key
    let key_id1 = user
        .add_private_key(Some("TEST_KEY"))
        .await
        .expect("Failed to add key");
    let public_key1 =
        eidetica::auth::crypto::parse_public_key(&key_id1).expect("Failed to parse key");

    // Add another key with different name
    let key_id2 = user
        .add_private_key(Some("TEST_KEY_2"))
        .await
        .expect("Failed to add another key");
    let public_key2 =
        eidetica::auth::crypto::parse_public_key(&key_id2).expect("Failed to parse key");

    // Should be different keys
    assert_ne!(public_key1, public_key2);
    assert_ne!(key_id1, key_id2);

    // Should now have three keys (default_key + TEST_KEY + TEST_KEY_2)
    let keys = user.list_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 3);
    assert!(keys.contains(&key_id1));
    assert!(keys.contains(&key_id2));

    // Both keys should work for signing
    let tree = user
        .create_database(Doc::new(), &key_id1)
        .await
        .expect("Failed to create tree");
    let op = tree
        .new_transaction()
        .await
        .expect("Failed to create operation");
    let store = op
        .get_store::<eidetica::store::DocStore>("data")
        .await
        .expect("Failed to get subtree");
    store
        .set("test", "value")
        .await
        .expect("Failed to set value");

    let entry_id = op.commit().await.expect("Failed to commit");

    // Verify with tree's auth configuration
    assert!(
        tree.verify_entry_signature(&entry_id)
            .await
            .expect("Failed to verify")
    );
}
