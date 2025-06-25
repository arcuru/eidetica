use super::helpers::*;
use crate::create_auth_keys;
use crate::helpers::*;
use eidetica::auth::crypto::format_public_key;
use eidetica::auth::types::{AuthId, KeyStatus, Permission};
use eidetica::crdt::Nested;

#[test]
fn test_key_management() {
    let db = setup_empty_db();

    // Initially no keys
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert!(keys.is_empty());

    // Add a key
    let key_id = "TEST_KEY";
    let public_key = db.add_private_key(key_id).expect("Failed to add key");

    // List keys should now show one key
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0], key_id);

    // Add another key
    let key_id2 = "TEST_KEY_2";
    let public_key2 = db.add_private_key(key_id2).expect("Failed to add key");

    // List keys should now show both keys
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 2);
    assert!(keys.contains(&key_id.to_string()));
    assert!(keys.contains(&key_id2.to_string()));

    // Keys should be different
    assert_ne!(
        format_public_key(&public_key),
        format_public_key(&public_key2)
    );

    // Test signing and verification
    let tree = db
        .new_tree(Nested::new(), key_id)
        .expect("Failed to create tree");
    let op = tree
        .new_authenticated_operation(key_id)
        .expect("Failed to create operation");
    let store = op
        .get_subtree::<eidetica::subtree::KVStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    let entry_id = op.commit().expect("Failed to commit");

    // Verify the entry was signed correctly
    let entry = tree.get_entry(&entry_id).expect("Failed to get entry");
    assert_eq!(entry.auth.id, AuthId::Direct(key_id.to_string()));
    assert!(entry.auth.signature.is_some());

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

    // The key should be in the list
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0], key_id);

    // Test that we can sign with the imported key
    let tree = db
        .new_tree(Nested::new(), key_id)
        .expect("Failed to create tree");
    let op = tree
        .new_authenticated_operation(key_id)
        .expect("Failed to create operation");
    let store = op
        .get_subtree::<eidetica::subtree::KVStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    let entry_id = op.commit().expect("Failed to commit");

    // Verify the entry was signed correctly
    let entry = tree.get_entry(&entry_id).expect("Failed to get entry");
    assert_eq!(entry.auth.id, AuthId::Direct(key_id.to_string()));
    assert!(
        tree.verify_entry_signature(&entry_id)
            .expect("Failed to verify")
    );
}

#[test]
fn test_backend_serialization() {
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    let db = setup_db();

    // Add initial key
    let key_id = "TEST_KEY";
    let public_key = db.add_private_key(key_id).expect("Failed to add key");

    // Add another key with import
    let signing_key2 = SigningKey::generate(&mut OsRng);
    let key_id2 = "IMPORTED_KEY";
    db.import_private_key(key_id2, signing_key2.clone())
        .expect("Failed to import key");
    let public_key2 = signing_key2.verifying_key();

    // Set up authentication settings with both keys
    // Note: First key needs admin permission to create tree with auth settings
    let keys = [
        (key_id, Permission::Admin(0), KeyStatus::Active),
        (key_id2, Permission::Write(20), KeyStatus::Active),
    ];
    let public_keys = vec![public_key, public_key2];
    let tree = setup_authenticated_tree(&db, &keys, &public_keys);

    // Create an entry with first key
    let op = tree
        .new_authenticated_operation(key_id)
        .expect("Failed to create operation");
    let store = op
        .get_subtree::<eidetica::subtree::KVStore>("data")
        .expect("Failed to get subtree");
    store.set("test", "value").expect("Failed to set value");

    let entry_id = op.commit().expect("Failed to commit");

    // Verify entry can be retrieved and is properly signed
    let entry = tree.get_entry(&entry_id).expect("Failed to get entry");
    assert_eq!(entry.auth.id, AuthId::Direct(key_id.to_string()));
    assert!(entry.auth.signature.is_some());
    assert!(
        tree.verify_entry_signature(&entry_id)
            .expect("Failed to verify")
    );

    // Create another entry with the imported key
    let op2 = tree
        .new_authenticated_operation(key_id2)
        .expect("Failed to create operation");
    let store2 = op2
        .get_subtree::<eidetica::subtree::KVStore>("data")
        .expect("Failed to get subtree");
    store2.set("test2", "value2").expect("Failed to set value");

    let entry_id2 = op2.commit().expect("Failed to commit");

    // Verify second entry
    let entry2 = tree.get_entry(&entry_id2).expect("Failed to get entry2");
    assert_eq!(entry2.auth.id, AuthId::Direct(key_id2.to_string()));
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

    // Should still only have one key ID
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0], key_id);

    // New key should work for signing
    let tree = db
        .new_tree(Nested::new(), key_id)
        .expect("Failed to create tree");
    let op = tree
        .new_authenticated_operation(key_id)
        .expect("Failed to create operation");
    let store = op
        .get_subtree::<eidetica::subtree::KVStore>("data")
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

    // Remove a key that doesn't exist - should succeed silently
    let result = db.remove_private_key("NONEXISTENT_KEY");
    assert!(result.is_ok());

    // List should still be empty
    let keys = db.list_private_keys().expect("Failed to list keys");
    assert!(keys.is_empty());
}

#[test]
fn test_multiple_authenticated_entries() {
    let keys = create_auth_keys![
        ("USER1", Permission::Admin(0), KeyStatus::Active), // Admin needed to create tree with auth
        ("USER2", Permission::Write(10), KeyStatus::Active)
    ];
    let (db, public_keys) = setup_test_db_with_keys(&keys);
    let tree = setup_authenticated_tree(&db, &keys, &public_keys);

    // Create first authenticated entry
    let op1 = tree
        .new_authenticated_operation("USER1")
        .expect("Failed to create operation");
    let store1 = op1
        .get_subtree::<eidetica::subtree::KVStore>("data")
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
        .get_subtree::<eidetica::subtree::KVStore>("data")
        .expect("Failed to get subtree");
    store2
        .set("user2_data", "world")
        .expect("Failed to set value");
    let entry_id2 = op2.commit().expect("Failed to commit");

    // Verify both entries are properly signed
    let entry1 = tree.get_entry(&entry_id1).expect("Failed to get entry1");
    assert!(entry1.auth.is_signed_by("USER1"));
    let entry2 = tree.get_entry(&entry_id2).expect("Failed to get entry2");
    assert!(entry2.auth.is_signed_by("USER2"));

    // Verify signatures with tree's auth configuration
    assert!(
        tree.verify_entry_signature(&entry_id1)
            .expect("Failed to verify")
    );
    assert!(
        tree.verify_entry_signature(&entry_id2)
            .expect("Failed to verify")
    );

    // Note: Cannot verify cross-validation with specific keys using new API - removed key-specific verification
}
