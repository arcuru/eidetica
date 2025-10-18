use eidetica::{
    auth::{
        crypto::format_public_key,
        types::{Permission, SigKey},
    },
    crdt::Doc,
};

use crate::helpers::*;

#[test]
fn test_key_management() {
    let db = setup_empty_db();

    // Initially should have _device_key only (created during Instance init)
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0], "_device_key");

    // Add a key
    let key_id = "TEST_KEY";
    let public_key = db.add_private_key(key_id).expect("Failed to add key");

    // List keys should now show two keys (_device_key + TEST_KEY)
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 2);
    assert!(keys.contains(&"_device_key".to_string()));
    assert!(keys.contains(&key_id.to_string()));

    // Add another key
    let key_id2 = "TEST_KEY_2";
    let public_key2 = db.add_private_key(key_id2).expect("Failed to add key");

    // List keys should now show three keys (_device_key + TEST_KEY + TEST_KEY_2)
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 3);
    assert!(keys.contains(&"_device_key".to_string()));
    assert!(keys.contains(&key_id.to_string()));
    assert!(keys.contains(&key_id2.to_string()));

    // Keys should be different
    assert_ne!(
        format_public_key(&public_key),
        format_public_key(&public_key2)
    );

    // Test signing and verification
    let tree = db
        .new_database(Doc::new(), key_id)
        .expect("Failed to create tree");
    let op = tree.new_transaction().expect("Failed to create operation");
    let store = op
        .get_store::<eidetica::store::DocStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    let entry_id = op.commit().expect("Failed to commit");

    // Verify the entry was signed correctly
    let entry = tree.get_entry(&entry_id).expect("Failed to get entry");
    assert_eq!(entry.sig.key, SigKey::Direct(key_id.to_string()));
    assert!(entry.sig.sig.is_some());

    // Verify signature with tree's auth configuration
    assert!(
        tree.verify_entry_signature(&entry_id)
            .expect("Failed to verify")
    );
    // Note: Cannot verify with wrong key using new API - removed separate key verification
}

#[test]
fn test_import_private_key() {
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    let db = setup_empty_db();

    // Generate a key externally
    let signing_key = SigningKey::generate(&mut OsRng);

    // Import the key
    let key_id = "IMPORTED_KEY";
    db.import_private_key(key_id, signing_key.clone())
        .expect("Failed to import key");

    // The key should be in the list (plus _device_key)
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 2);
    assert!(keys.contains(&"_device_key".to_string()));
    assert!(keys.contains(&key_id.to_string()));

    // Test that we can sign with the imported key
    let tree = db
        .new_database(Doc::new(), key_id)
        .expect("Failed to create tree");
    let op = tree.new_transaction().expect("Failed to create operation");
    let store = op
        .get_store::<eidetica::store::DocStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    let entry_id = op.commit().expect("Failed to commit");

    // Verify the entry was signed correctly
    let entry = tree.get_entry(&entry_id).expect("Failed to get entry");
    assert_eq!(entry.sig.key, SigKey::Direct(key_id.to_string()));
    assert!(
        tree.verify_entry_signature(&entry_id)
            .expect("Failed to verify")
    );
}

#[test]
fn test_backend_serialization() {
    use eidetica::auth::crypto::format_public_key;
    use eidetica::auth::types::AuthKey;

    let (instance, mut user) = test_instance_with_user("test_user");

    // Add two keys using User API
    let key_id1 = user
        .add_private_key(Some("TEST_KEY"))
        .expect("Failed to add key");
    let public_key1 =
        eidetica::auth::crypto::parse_public_key(&key_id1).expect("Failed to parse key");

    let key_id2 = user
        .add_private_key(Some("SECOND_KEY"))
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
    settings.set_doc("auth", auth_settings);

    // Create database with first key (admin key)
    let tree = user
        .new_database(settings, &key_id1)
        .expect("Failed to create tree");

    // Create an entry with first key (tree is already loaded with key_id1)
    let op = tree.new_transaction().expect("Failed to create operation");
    let store = op
        .get_store::<eidetica::store::DocStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    let entry_id = op.commit().expect("Failed to commit");

    // Verify entry can be retrieved and is properly signed
    let entry = tree.get_entry(&entry_id).expect("Failed to get entry");
    assert_eq!(entry.sig.key, SigKey::Direct(key_id1.to_string()));
    assert!(entry.sig.sig.is_some());
    assert!(
        tree.verify_entry_signature(&entry_id)
            .expect("Failed to verify")
    );

    // Create another entry with the imported key - need to reload database with that key
    let signing_key2_for_load = user
        .get_signing_key(&key_id2)
        .expect("Failed to get signing key")
        .clone();
    let tree_with_key2 = eidetica::Database::open(
        instance.backend().clone(),
        tree.root_id(),
        signing_key2_for_load,
        key_id2.clone(),
    )
    .expect("Failed to load database with key2");

    let op2 = tree_with_key2
        .new_transaction()
        .expect("Failed to create operation");
    let store2 = op2
        .get_store::<eidetica::store::DocStore>("data")
        .expect("Failed to get subtree");
    store2.set("test2", "value2").expect("Failed to set value");

    let entry_id2 = op2.commit().expect("Failed to commit");

    // Verify second entry (can verify from either tree instance)
    let entry2 = tree.get_entry(&entry_id2).expect("Failed to get entry2");
    assert_eq!(entry2.sig.key, SigKey::Direct(key_id2.to_string()));
    assert!(
        tree.verify_entry_signature(&entry_id2)
            .expect("Failed to verify")
    );
}

#[test]
fn test_overwrite_existing_key() {
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    let db = setup_empty_db();

    // Add initial key
    let key_id = "TEST_KEY";
    let public_key1 = db.add_private_key(key_id).expect("Failed to add key");

    // Overwrite with a new key
    // TODO: This behavior should be changed so that keys are unique to the tree, not the db.
    let signing_key2 = SigningKey::generate(&mut OsRng);
    db.import_private_key(key_id, signing_key2.clone())
        .expect("Failed to import key");
    let public_key2 = signing_key2.verifying_key();

    // Should be different keys
    assert_ne!(public_key1, public_key2);

    // Should still only have two key IDs (_device_key + TEST_KEY)
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 2);
    assert!(keys.contains(&"_device_key".to_string()));
    assert!(keys.contains(&key_id.to_string()));

    // New key should work for signing
    let tree = db
        .new_database(Doc::new(), key_id)
        .expect("Failed to create tree");
    let op = tree.new_transaction().expect("Failed to create operation");
    let store = op
        .get_store::<eidetica::store::DocStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    let entry_id = op.commit().expect("Failed to commit");

    // Verify with tree's auth configuration
    assert!(
        tree.verify_entry_signature(&entry_id)
            .expect("Failed to verify")
    );

    // Note: Cannot verify with specific keys using new API - removed key-specific verification
}

#[test]
fn test_remove_nonexistent_key() {
    let db = setup_empty_db();

    // Remove a key that doesn't exist - should fail (not yet implemented)
    let result = db.remove_private_key("NONEXISTENT_KEY");
    assert!(result.is_err());

    // List should still have _device_key only
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0], "_device_key");
}
