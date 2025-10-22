//! Tree API method tests
//!
//! This module contains tests for Tree API methods including entry retrieval,
//! authentication, validation, and error handling.

use eidetica::{auth::types::SigKey, crdt::Doc, store::DocStore};

use super::helpers::*;
use crate::helpers::*;

/// Test basic entry retrieval functionality
#[test]
fn test_get_entry_basic() {
    let (_instance, tree) = setup_tree_with_key("test_key");

    // Create an entry using helper
    let entry_id = add_data_to_subtree(&tree, "data", &[("test_key", "test_value")]);

    // Test get_entry
    let entry = tree.get_entry(&entry_id).expect("Failed to get entry");
    assert_eq!(entry.id(), entry_id);
    assert_eq!(entry.sig.key, SigKey::Direct("test_key".to_string()));
    assert!(entry.sig.sig.is_some());
}

/// Test get_entries with multiple entries
#[test]
fn test_get_entries_multiple() {
    let (_instance, tree) = setup_db_and_tree_with_key("test_key");

    // Create multiple entries using helper
    let entry_ids = create_linear_chain(&tree, "data", 3);

    // Test get_entries
    let entries = tree.get_entries(&entry_ids).expect("Failed to get entries");
    assert_eq!(entries.len(), 3);

    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(entry.id(), entry_ids[i]);
    }
}

/// Test comprehensive error handling for entry retrieval
#[test]
fn test_entry_retrieval_error_handling() {
    let (_instance, tree) = setup_db_and_tree_with_key("test_key");

    // Create one valid entry using helper
    let entry_id = add_data_to_subtree(&tree, "data", &[("key", "value")]);

    // Test get_entry with existing entry (should succeed)
    assert!(tree.get_entry(&entry_id).is_ok());

    // Test get_entry with non-existent entry (should fail with NotFound)
    let result = tree.get_entry("non_existent_entry");
    assert!(result.is_err());
    assert!(result.unwrap_err().is_not_found());

    // Test get_entries with mixed valid/invalid entries
    let entry_ids = vec![entry_id.as_str(), "non_existent_entry"];
    let result = tree.get_entries(entry_ids);
    assert!(result.is_err());
    assert!(result.unwrap_err().is_not_found());

    // Test auth verification with non-existent entry
    let result = tree.verify_entry_signature("non_existent_entry");
    assert!(result.is_err());
    assert!(result.unwrap_err().is_not_found());
}

/// Test tree validation - entries from different trees should be rejected
#[test]
fn test_tree_validation_rejects_foreign_entries() {
    let db = setup_db_with_key("test_key");

    // Create two separate trees with different initial settings to ensure different root IDs
    let mut settings1 = Doc::new();
    settings1.set_string("name", "tree1".to_string());
    let tree1 = db
        .new_database(settings1, "test_key")
        .expect("Failed to create tree1");

    let mut settings2 = Doc::new();
    settings2.set_string("name", "tree2".to_string());
    let tree2 = db
        .new_database(settings2, "test_key")
        .expect("Failed to create tree2");

    // Create an entry in tree1
    let op1 = tree1
        .new_transaction()
        .expect("Failed to create operation in tree1");
    let store1 = op1
        .get_store::<DocStore>("data")
        .expect("Failed to get subtree in tree1");
    store1
        .set("key", "value1")
        .expect("Failed to set value in tree1");
    let entry1_id = op1.commit().expect("Failed to commit operation in tree1");

    // Create an entry in tree2
    let op2 = tree2
        .new_transaction()
        .expect("Failed to create operation in tree2");
    let store2 = op2
        .get_store::<DocStore>("data")
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
    assert!(error_msg.contains("does not belong to database"));

    // Verify tree2 cannot access tree1's entry
    let result = tree2.get_entry(&entry1_id);
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("does not belong to database"));
}

/// Test tree validation with get_entries
#[test]
fn test_tree_validation_get_entries() {
    let db = setup_db_with_key("test_key");

    // Create two separate trees with different initial settings to ensure different root IDs
    let mut settings1 = Doc::new();
    settings1.set_string("name", "tree1".to_string());
    let tree1 = db
        .new_database(settings1, "test_key")
        .expect("Failed to create tree1");

    let mut settings2 = Doc::new();
    settings2.set_string("name", "tree2".to_string());
    let tree2 = db
        .new_database(settings2, "test_key")
        .expect("Failed to create tree2");

    // Create entries in tree1
    let mut tree1_entries = Vec::new();
    for i in 0..2 {
        let op = tree1
            .new_transaction()
            .expect("Failed to create operation in tree1");
        let store = op
            .get_store::<DocStore>("data")
            .expect("Failed to get subtree in tree1");
        store
            .set("key", format!("value1_{i}"))
            .expect("Failed to set value in tree1");
        let entry_id = op.commit().expect("Failed to commit operation in tree1");
        tree1_entries.push(entry_id);
    }

    // Create an entry in tree2
    let op2 = tree2
        .new_transaction()
        .expect("Failed to create operation in tree2");
    let store2 = op2
        .get_store::<DocStore>("data")
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
    assert!(error_msg.contains("does not belong to database"));
}

/// Test authentication helpers with signed entries
#[test]
fn test_auth_helpers_signed_entries() {
    let (_instance, tree) = setup_tree_with_auth_config("TEST_KEY");

    // Create signed entry using helper
    let entry_id = add_authenticated_data(&tree, "data", &[("key", "value")]);

    // Test entry auth access using helper
    assert_entry_authentication(&tree, &entry_id, "TEST_KEY");

    // Test entry belongs to tree
    assert_entry_belongs_to_tree(&tree, &entry_id);

    // Test manual auth checks
    let entry = tree.get_entry(&entry_id).expect("Failed to get entry");
    let sig_info = &entry.sig;
    assert!(sig_info.is_signed_by("TEST_KEY"));
    assert!(!sig_info.is_signed_by("OTHER_KEY"));
}

/// Test authentication helpers with default authenticated entries
#[test]
fn test_auth_helpers_default_authenticated_entries() {
    let (_instance, tree) = setup_db_and_tree_with_key("test_key");

    // Create entry using default authentication helper
    let entry_id = add_data_to_subtree(&tree, "data", &[("key", "value")]);

    // Test entry auth access using helper
    assert_entry_authentication(&tree, &entry_id, "test_key");

    // Test manual auth checks
    let entry = tree.get_entry(&entry_id).expect("Failed to get entry");
    let sig_info = &entry.sig;
    assert!(sig_info.is_signed_by("test_key"));
    assert!(!sig_info.is_signed_by("OTHER_KEY"));
}

/// Test verify_entry_signature with different authentication scenarios
#[test]
fn test_verify_entry_signature_auth_scenarios() {
    let (_instance, tree) = setup_tree_with_auth_config("TEST_KEY");

    // Test 1: Create entry signed with valid key using helper
    let signed_entry_id = add_authenticated_data(&tree, "data", &[("key", "value1")]);

    // Should verify successfully using helper
    assert_entry_authentication(&tree, &signed_entry_id, "TEST_KEY");

    // Test 2: Create unsigned entry using helper
    let unsigned_entry_id = add_data_to_subtree(&tree, "data", &[("key", "value2")]);

    // Should be valid (backward compatibility for unsigned entries)
    let is_valid_unsigned = tree
        .verify_entry_signature(&unsigned_entry_id)
        .expect("Failed to verify unsigned entry");
    assert!(is_valid_unsigned);
}

/// Test verify_entry_signature with unauthorized key
#[test]
fn test_verify_entry_signature_unauthorized_key() {
    let (instance, tree) = setup_tree_with_auth_config("AUTHORIZED_KEY");

    // Add unauthorized key to backend (but not to tree's auth settings)
    let _unauthorized_public_key = instance
        .add_private_key("UNAUTHORIZED_KEY")
        .expect("Failed to add unauthorized key");

    // Test with authorized key (should succeed) using helper
    let authorized_entry_id = add_authenticated_data(&tree, "data", &[("key", "value1")]);

    assert_entry_authentication(&tree, &authorized_entry_id, "AUTHORIZED_KEY");

    // Test with unauthorized key (should fail during commit because key is not in tree's auth settings)
    let unauthorized_signing_key = instance
        .backend()
        .get_private_key("UNAUTHORIZED_KEY")
        .expect("Failed to get unauthorized signing key")
        .expect("Unauthorized key should exist in backend");

    let tree_with_unauthorized_key = eidetica::Database::open(
        instance.clone(),
        tree.root_id(),
        unauthorized_signing_key,
        "UNAUTHORIZED_KEY".to_string(),
    )
    .expect("Failed to load tree with unauthorized key");

    let op2 = tree_with_unauthorized_key
        .new_transaction()
        .expect("Failed to create operation");
    let store2 = op2
        .get_store::<DocStore>("data")
        .expect("Failed to get subtree");
    store2.set("key", "value2").expect("Failed to set value");
    let commit_result = op2.commit();

    // The commit should fail because the unauthorized key is not in the tree's auth settings
    assert!(commit_result.is_err());
    let error_msg = commit_result.unwrap_err().to_string();
    assert!(
        error_msg.contains("authentication validation failed")
            || error_msg.contains("not found")
            || error_msg.contains("No active key found")
    );
}

/// Test that verify_entry_signature validates against tree auth configuration
#[test]
fn test_verify_entry_signature_validates_tree_auth() {
    let (_instance, tree) = setup_tree_with_auth_config("VALID_KEY");

    // Create a signed entry using helper
    let entry_id = add_authenticated_data(&tree, "data", &[("key", "value")]);

    // Verify the entry using helper - should validate against tree's auth settings
    assert_entry_authentication(&tree, &entry_id, "VALID_KEY");

    // Note: In the future, this test should also verify that:
    // 1. Entries remain valid even if the key is later revoked (historical validation)
    // 2. Entry metadata contains the settings tips that were active when it was created
    // 3. Validation uses those historical settings rather than current settings
}

/// Test tree queries functionality
#[test]
fn test_tree_queries() {
    let (_instance, tree) = setup_db_and_tree_with_key("test_key");

    // Get initial entries
    let initial_entries = tree
        .get_all_entries()
        .expect("Failed to get initial entries");
    let initial_count = initial_entries.len();
    assert!(initial_count >= 1); // At least the root entry

    // Create a few entries
    let mut entry_ids = Vec::new();
    for i in 0..3 {
        let op = tree.new_transaction().expect("Failed to create operation");
        let store = op
            .get_store::<DocStore>("data")
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

/// Test performance: batch get_entries vs individual get_entry calls
#[test]
fn test_batch_vs_individual_retrieval() {
    let (_instance, tree) = setup_db_and_tree_with_key("test_key");

    // Create multiple entries
    let mut entry_ids = Vec::new();
    for i in 0..5 {
        let op = tree.new_transaction().expect("Failed to create operation");
        let store = op
            .get_store::<DocStore>("data")
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
