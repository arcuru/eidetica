use crate::helpers::*;
use eidetica::Error;
use eidetica::auth::types::{AuthKey, KeyStatus, Permission, SigKey};
use eidetica::crdt::Nested;
use eidetica::subtree::KVStore;

/// Test basic entry retrieval functionality
#[test]
fn test_get_entry_basic() {
    let tree = setup_tree_with_key("test_key");

    // Create an operation and commit it
    let op = tree.new_operation().expect("Failed to create operation");
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store
        .set("test_key", "test_value")
        .expect("Failed to set value");
    let entry_id = op.commit().expect("Failed to commit operation");

    // Test get_entry
    let entry = tree.get_entry(&entry_id).expect("Failed to get entry");
    assert_eq!(entry.id(), entry_id);
    assert_eq!(entry.sig.key, SigKey::Direct("test_key".to_string()));
    assert!(entry.sig.sig.is_some());
}

/// Test get_entry with non-existent entry
#[test]
fn test_get_entry_not_found() {
    let (_db, tree) = setup_db_and_tree_with_key("test_key");

    // Try to get non-existent entry
    let result = tree.get_entry("non_existent_entry");
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), Error::NotFound));
}

/// Test get_entries with multiple entries
#[test]
fn test_get_entries_multiple() {
    let (_db, tree) = setup_db_and_tree_with_key("test_key");

    // Create multiple entries
    let mut entry_ids = Vec::new();
    for i in 0..3 {
        let op = tree.new_operation().expect("Failed to create operation");
        let store = op
            .get_subtree::<KVStore>("data")
            .expect("Failed to get subtree");
        store
            .set("key", format!("value_{i}"))
            .expect("Failed to set value");
        let entry_id = op.commit().expect("Failed to commit operation");
        entry_ids.push(entry_id);
    }

    // Test get_entries
    let entries = tree.get_entries(&entry_ids).expect("Failed to get entries");
    assert_eq!(entries.len(), 3);

    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(entry.id(), entry_ids[i]);
    }
}

/// Test get_entries with non-existent entry
#[test]
fn test_get_entries_not_found() {
    let (_db, tree) = setup_db_and_tree_with_key("test_key");

    // Create one valid entry
    let op = tree.new_operation().expect("Failed to create operation");
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("key", "value").expect("Failed to set value");
    let valid_entry_id = op.commit().expect("Failed to commit operation");

    // Try to get entries including non-existent one
    let entry_ids = vec![valid_entry_id.as_str(), "non_existent_entry"];
    let result = tree.get_entries(entry_ids);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), Error::NotFound));
}

/// Test entry existence checking via get_entry
#[test]
fn test_entry_existence_checking() {
    let (_db, tree) = setup_db_and_tree_with_key("test_key");

    // Create an entry
    let op = tree.new_operation().expect("Failed to create operation");
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("key", "value").expect("Failed to set value");
    let entry_id = op.commit().expect("Failed to commit operation");

    // Test get_entry with existing entry (should succeed)
    assert!(tree.get_entry(&entry_id).is_ok());

    // Test get_entry with non-existent entry (should fail with NotFound)
    let result = tree.get_entry("non_existent_entry");
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), Error::NotFound));
}

/// Test tree validation - entries from different trees should be rejected
#[test]
fn test_tree_validation_rejects_foreign_entries() {
    let db = setup_db_with_key("test_key");

    // Create two separate trees with different initial settings to ensure different root IDs
    let mut settings1 = Nested::new();
    settings1.set_string("name".to_string(), "tree1".to_string());
    let tree1 = db
        .new_tree(settings1, "test_key")
        .expect("Failed to create tree1");

    let mut settings2 = Nested::new();
    settings2.set_string("name".to_string(), "tree2".to_string());
    let tree2 = db
        .new_tree(settings2, "test_key")
        .expect("Failed to create tree2");

    // Create an entry in tree1
    let op1 = tree1
        .new_operation()
        .expect("Failed to create operation in tree1");
    let store1 = op1
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree in tree1");
    store1
        .set("key", "value1")
        .expect("Failed to set value in tree1");
    let entry1_id = op1.commit().expect("Failed to commit operation in tree1");

    // Create an entry in tree2
    let op2 = tree2
        .new_operation()
        .expect("Failed to create operation in tree2");
    let store2 = op2
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree in tree2");
    store2
        .set("key", "value2")
        .expect("Failed to set value in tree2");
    let entry2_id = op2.commit().expect("Failed to commit operation in tree2");

    // Verify tree1 can access its own entry
    assert!(tree1.get_entry(&entry1_id).is_ok());

    // Verify tree2 can access its own entry
    assert!(tree2.get_entry(&entry2_id).is_ok());

    // Verify tree1 cannot access tree2's entry
    let result = tree1.get_entry(&entry2_id);
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("does not belong to tree"));

    // Verify tree2 cannot access tree1's entry
    let result = tree2.get_entry(&entry1_id);
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("does not belong to tree"));
}

/// Test tree validation with get_entries
#[test]
fn test_tree_validation_get_entries() {
    let db = setup_db_with_key("test_key");

    // Create two separate trees with different initial settings to ensure different root IDs
    let mut settings1 = Nested::new();
    settings1.set_string("name".to_string(), "tree1".to_string());
    let tree1 = db
        .new_tree(settings1, "test_key")
        .expect("Failed to create tree1");

    let mut settings2 = Nested::new();
    settings2.set_string("name".to_string(), "tree2".to_string());
    let tree2 = db
        .new_tree(settings2, "test_key")
        .expect("Failed to create tree2");

    // Create entries in tree1
    let mut tree1_entries = Vec::new();
    for i in 0..2 {
        let op = tree1
            .new_operation()
            .expect("Failed to create operation in tree1");
        let store = op
            .get_subtree::<KVStore>("data")
            .expect("Failed to get subtree in tree1");
        store
            .set("key", format!("value1_{i}"))
            .expect("Failed to set value in tree1");
        let entry_id = op.commit().expect("Failed to commit operation in tree1");
        tree1_entries.push(entry_id);
    }

    // Create an entry in tree2
    let op2 = tree2
        .new_operation()
        .expect("Failed to create operation in tree2");
    let store2 = op2
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree in tree2");
    store2
        .set("key", "value2")
        .expect("Failed to set value in tree2");
    let entry2_id = op2.commit().expect("Failed to commit operation in tree2");

    // Verify tree1 can get all its own entries
    let entries = tree1
        .get_entries(&tree1_entries)
        .expect("Failed to get tree1 entries");
    assert_eq!(entries.len(), 2);

    // Verify get_entries fails when trying to get entries from different trees
    let mixed_entries = vec![tree1_entries[0].clone(), entry2_id];
    let result = tree1.get_entries(&mixed_entries);
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("does not belong to tree"));
}

/// Test authentication helpers with signed entries
#[test]
fn test_auth_helpers_signed_entries() {
    let db = setup_db();
    // Add a private key
    let key_id = "TEST_KEY";
    let public_key = db.add_private_key(key_id).expect("Failed to add key");

    // Create auth settings
    let mut settings = Nested::new();
    let mut auth_settings = Nested::new();
    auth_settings.set(
        key_id.to_string(),
        AuthKey {
            pubkey: eidetica::auth::crypto::format_public_key(&public_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    );
    settings.set_map("auth", auth_settings);

    let tree = db
        .new_tree(settings, key_id)
        .expect("Failed to create tree");

    // Create signed entry
    let op = tree
        .new_authenticated_operation(key_id)
        .expect("Failed to create operation");
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("key", "value").expect("Failed to set value");
    let entry_id = op.commit().expect("Failed to commit operation");

    // Test entry auth access
    let entry = tree.get_entry(&entry_id).expect("Failed to get entry");
    let sig_info = &entry.sig;
    assert_eq!(sig_info.key, SigKey::Direct(key_id.to_string()));
    assert!(sig_info.sig.is_some());

    // Test verify_entry_signature
    let is_valid = tree
        .verify_entry_signature(&entry_id)
        .expect("Failed to verify signature");
    assert!(is_valid);

    // Test is_signed_by helper
    assert!(sig_info.is_signed_by(key_id));
    assert!(!sig_info.is_signed_by("OTHER_KEY"));
}

/// Test authentication helpers with default authenticated entries
#[test]
fn test_auth_helpers_default_authenticated_entries() {
    let (_db, tree) = setup_db_and_tree_with_key("test_key");

    // Create entry using default authentication
    let op = tree.new_operation().expect("Failed to create operation");
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("key", "value").expect("Failed to set value");
    let entry_id = op.commit().expect("Failed to commit operation");

    // Test entry auth access - should be signed with default key
    let entry = tree.get_entry(&entry_id).expect("Failed to get entry");
    let sig_info = &entry.sig;
    assert_eq!(sig_info.key, SigKey::Direct("test_key".to_string()));
    assert!(sig_info.sig.is_some());

    // Test is_signed_by helper
    assert!(sig_info.is_signed_by("test_key"));
    assert!(!sig_info.is_signed_by("OTHER_KEY"));
}

/// Test verify_entry_signature with different authentication scenarios
#[test]
fn test_verify_entry_signature_auth_scenarios() {
    let db = setup_db();
    // Add a key
    let key_id = "TEST_KEY";
    let public_key = db.add_private_key(key_id).expect("Failed to add key");

    // Create auth settings
    let mut settings = Nested::new();
    let mut auth_settings = Nested::new();
    auth_settings.set(
        key_id.to_string(),
        AuthKey {
            pubkey: eidetica::auth::crypto::format_public_key(&public_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    );
    settings.set_map("auth", auth_settings);

    let tree = db
        .new_tree(settings, key_id)
        .expect("Failed to create tree");

    // Test 1: Create entry signed with valid key
    let op1 = tree
        .new_authenticated_operation(key_id)
        .expect("Failed to create operation");
    let store1 = op1
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store1.set("key", "value1").expect("Failed to set value");
    let signed_entry_id = op1.commit().expect("Failed to commit operation");

    // Should verify successfully
    let is_valid = tree
        .verify_entry_signature(&signed_entry_id)
        .expect("Failed to verify signature");
    assert!(is_valid);

    // Test 2: Create unsigned entry
    let op2 = tree.new_operation().expect("Failed to create operation");
    let store2 = op2
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store2.set("key", "value2").expect("Failed to set value");
    let unsigned_entry_id = op2.commit().expect("Failed to commit operation");

    // Should be valid (backward compatibility for unsigned entries)
    let is_valid_unsigned = tree
        .verify_entry_signature(&unsigned_entry_id)
        .expect("Failed to verify unsigned entry");
    assert!(is_valid_unsigned);
}

/// Test verify_entry_signature with unauthorized key
#[test]
fn test_verify_entry_signature_unauthorized_key() {
    let db = setup_db();
    // Add two keys to the backend
    let authorized_key_id = "AUTHORIZED_KEY";
    let unauthorized_key_id = "UNAUTHORIZED_KEY";
    let authorized_public_key = db
        .add_private_key(authorized_key_id)
        .expect("Failed to add authorized key");
    let _unauthorized_public_key = db
        .add_private_key(unauthorized_key_id)
        .expect("Failed to add unauthorized key");

    // Create auth settings with only the authorized key
    let mut settings = Nested::new();
    let mut auth_settings = Nested::new();
    auth_settings.set(
        authorized_key_id.to_string(),
        AuthKey {
            pubkey: eidetica::auth::crypto::format_public_key(&authorized_public_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    );
    settings.set_map("auth", auth_settings);

    let tree = db
        .new_tree(settings, authorized_key_id)
        .expect("Failed to create tree");

    // Test with authorized key (should succeed)
    let op1 = tree
        .new_authenticated_operation(authorized_key_id)
        .expect("Failed to create operation");
    let store1 = op1
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store1.set("key", "value1").expect("Failed to set value");
    let authorized_entry_id = op1.commit().expect("Failed to commit operation");

    let is_valid = tree
        .verify_entry_signature(&authorized_entry_id)
        .expect("Failed to verify signature");
    assert!(is_valid);

    // Test with unauthorized key (should fail during commit because key is not in tree's auth settings)
    let op2 = tree
        .new_authenticated_operation(unauthorized_key_id)
        .expect("Failed to create operation");
    let store2 = op2
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store2.set("key", "value2").expect("Failed to set value");
    let commit_result = op2.commit();

    // The commit should fail because the unauthorized key is not in the tree's auth settings
    assert!(commit_result.is_err());
    let error_msg = commit_result.unwrap_err().to_string();
    assert!(
        error_msg.contains("authentication validation failed") || error_msg.contains("not found")
    );
}

/// Test that verify_entry_signature validates against tree auth configuration
#[test]
fn test_verify_entry_signature_validates_tree_auth() {
    let db = setup_db();
    // Add a key
    let key_id = "VALID_KEY";
    let public_key = db.add_private_key(key_id).expect("Failed to add key");

    // Create auth settings
    let mut settings = Nested::new();
    let mut auth_settings = Nested::new();
    auth_settings.set(
        key_id.to_string(),
        AuthKey {
            pubkey: eidetica::auth::crypto::format_public_key(&public_key),
            permissions: Permission::Admin(0),
            status: KeyStatus::Active,
        },
    );
    settings.set_map("auth", auth_settings);

    let tree = db
        .new_tree(settings, key_id)
        .expect("Failed to create tree");

    // Create a signed entry
    let op = tree
        .new_authenticated_operation(key_id)
        .expect("Failed to create operation");
    let store = op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get subtree");
    store.set("key", "value").expect("Failed to set value");
    let entry_id = op.commit().expect("Failed to commit operation");

    // Verify the entry - should validate against tree's auth settings
    let is_valid = tree
        .verify_entry_signature(&entry_id)
        .expect("Failed to verify signature");
    assert!(
        is_valid,
        "Entry should be valid when signed with authorized key"
    );

    // Note: In the future, this test should also verify that:
    // 1. Entries remain valid even if the key is later revoked (historical validation)
    // 2. Entry metadata contains the settings tips that were active when it was created
    // 3. Validation uses those historical settings rather than current settings
}

/// Test tree queries functionality
#[test]
fn test_tree_queries() {
    let (_db, tree) = setup_db_and_tree_with_key("test_key");

    // Get initial entries
    let initial_entries = tree
        .get_all_entries()
        .expect("Failed to get initial entries");
    let initial_count = initial_entries.len();
    assert!(initial_count >= 1); // At least the root entry

    // Create a few entries
    let mut entry_ids = Vec::new();
    for i in 0..3 {
        let op = tree.new_operation().expect("Failed to create operation");
        let store = op
            .get_subtree::<KVStore>("data")
            .expect("Failed to get subtree");
        store
            .set("key", format!("value_{i}"))
            .expect("Failed to set value");
        let entry_id = op.commit().expect("Failed to commit operation");
        entry_ids.push(entry_id);
    }

    // Test get_all_entries
    let all_entries = tree.get_all_entries().expect("Failed to get all entries");
    assert_eq!(all_entries.len(), initial_count + 3);

    // Verify all our created entries are in the result
    for entry_id in &entry_ids {
        let found = all_entries.iter().any(|entry| entry.id() == *entry_id);
        assert!(found, "Entry {entry_id} not found in all_entries");
    }
}

/// Test error handling for auth helpers
#[test]
fn test_auth_helpers_error_handling() {
    let (_db, tree) = setup_db_and_tree_with_key("test_key");

    // Test with non-existent entry
    let result = tree.get_entry("non_existent_entry");
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), Error::NotFound));

    let result = tree.verify_entry_signature("non_existent_entry");
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), Error::NotFound));
}

/// Test performance: batch get_entries vs individual get_entry calls
#[test]
fn test_batch_vs_individual_retrieval() {
    let (_db, tree) = setup_db_and_tree_with_key("test_key");

    // Create multiple entries
    let mut entry_ids = Vec::new();
    for i in 0..5 {
        let op = tree.new_operation().expect("Failed to create operation");
        let store = op
            .get_subtree::<KVStore>("data")
            .expect("Failed to get subtree");
        store
            .set("key", format!("value_{i}"))
            .expect("Failed to set value");
        let entry_id = op.commit().expect("Failed to commit operation");
        entry_ids.push(entry_id);
    }

    // Test individual retrieval
    let mut individual_entries = Vec::new();
    for entry_id in &entry_ids {
        let entry = tree.get_entry(entry_id).expect("Failed to get entry");
        individual_entries.push(entry);
    }

    // Test batch retrieval
    let batch_entries = tree.get_entries(&entry_ids).expect("Failed to get entries");

    // Results should be identical
    assert_eq!(individual_entries.len(), batch_entries.len());
    for (individual, batch) in individual_entries.iter().zip(batch_entries.iter()) {
        assert_eq!(individual.id(), batch.id());
        assert_eq!(individual.sig, batch.sig);
    }
}
