//! Tree API method tests
//!
//! This module contains tests for Tree API methods including entry retrieval,
//! authentication, validation, and error handling.

use eidetica::{auth::types::SigKey, crdt::Doc, store::DocStore};

use super::helpers::*;
use crate::helpers::*;

/// Test basic entry retrieval functionality
#[tokio::test]
async fn test_get_entry_basic() {
    let (_instance, tree, key_id) = setup_tree_with_user_key().await;

    // Create an entry using helper
    let entry_id = add_data_to_subtree(&tree, "data", &[("test_key", "test_value")]).await;

    // Test get_entry
    let entry = tree
        .get_entry(&entry_id)
        .await
        .expect("Failed to get entry");
    assert_eq!(entry.id(), entry_id);
    assert_eq!(entry.sig.key, SigKey::from_pubkey(&key_id));
    assert!(entry.sig.sig.is_some());
}

/// Test get_entries with multiple entries
#[tokio::test]
async fn test_get_entries_multiple() {
    let (_instance, tree, _key_id) = setup_tree_with_user_key().await;

    // Create multiple entries using helper
    let entry_ids = create_linear_chain(&tree, "data", 3).await;

    // Test get_entries
    let entries = tree
        .get_entries(&entry_ids)
        .await
        .expect("Failed to get entries");
    assert_eq!(entries.len(), 3);

    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(entry.id(), entry_ids[i]);
    }
}

/// Test comprehensive error handling for entry retrieval
#[tokio::test]
async fn test_entry_retrieval_error_handling() {
    let (_instance, tree, _key_id) = setup_tree_with_user_key().await;

    // Create one valid entry using helper
    let entry_id = add_data_to_subtree(&tree, "data", &[("key", "value")]).await;

    // Test get_entry with existing entry (should succeed)
    assert!(tree.get_entry(&entry_id).await.is_ok());

    // Test get_entry with non-existent entry (should fail with NotFound)
    let result = tree.get_entry("non_existent_entry").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().is_not_found());

    // Test get_entries with mixed valid/invalid entries
    let entry_ids = vec![entry_id.as_str(), "non_existent_entry"];
    let result = tree.get_entries(entry_ids).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().is_not_found());

    // Test auth verification with non-existent entry
    let result = tree.verify_entry_signature("non_existent_entry").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().is_not_found());
}

/// Test tree validation - entries from different trees should be rejected
#[tokio::test]
async fn test_tree_validation_rejects_foreign_entries() {
    let (_instance, mut user, key_id) =
        test_instance_with_user_and_key("test_user", Some("test_key")).await;

    // Create two separate trees with different initial settings to ensure different root IDs
    let mut settings1 = Doc::new();
    settings1.set("name", "tree1".to_string());
    let tree1 = user
        .create_database(settings1, &key_id)
        .await
        .expect("Failed to create tree1");

    let mut settings2 = Doc::new();
    settings2.set("name", "tree2".to_string());
    let tree2 = user
        .create_database(settings2, &key_id)
        .await
        .expect("Failed to create tree2");

    // Create an entry in tree1
    let op1 = tree1
        .new_transaction()
        .await
        .expect("Failed to create operation in tree1");
    let store1 = op1
        .get_store::<DocStore>("data")
        .await
        .expect("Failed to get subtree in tree1");
    store1
        .set("key", "value1")
        .await
        .expect("Failed to set value in tree1");
    let entry1_id = op1
        .commit()
        .await
        .expect("Failed to commit operation in tree1");

    // Create an entry in tree2
    let op2 = tree2
        .new_transaction()
        .await
        .expect("Failed to create operation in tree2");
    let store2 = op2
        .get_store::<DocStore>("data")
        .await
        .expect("Failed to get subtree in tree2");
    store2
        .set("key", "value2")
        .await
        .expect("Failed to set value in tree2");
    let entry2_id = op2
        .commit()
        .await
        .expect("Failed to commit operation in tree2");

    // Verify tree1 can access its own entry
    assert!(tree1.get_entry(&entry1_id).await.is_ok());

    // Verify tree2 can access its own entry
    assert!(tree2.get_entry(&entry2_id).await.is_ok());

    // Verify tree1 cannot access tree2's entry
    let result = tree1.get_entry(&entry2_id).await;
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("does not belong to database"));

    // Verify tree2 cannot access tree1's entry
    let result = tree2.get_entry(&entry1_id).await;
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("does not belong to database"));
}

/// Test tree validation with get_entries
#[tokio::test]
async fn test_tree_validation_get_entries() {
    let (_instance, mut user, key_id) =
        test_instance_with_user_and_key("test_user", Some("test_key")).await;

    // Create two separate trees with different initial settings to ensure different root IDs
    let mut settings1 = Doc::new();
    settings1.set("name", "tree1".to_string());
    let tree1 = user
        .create_database(settings1, &key_id)
        .await
        .expect("Failed to create tree1");

    let mut settings2 = Doc::new();
    settings2.set("name", "tree2".to_string());
    let tree2 = user
        .create_database(settings2, &key_id)
        .await
        .expect("Failed to create tree2");

    // Create entries in tree1
    let mut tree1_entries = Vec::new();
    for i in 0..2 {
        let op = tree1
            .new_transaction()
            .await
            .expect("Failed to create operation in tree1");
        let store = op
            .get_store::<DocStore>("data")
            .await
            .expect("Failed to get subtree in tree1");
        store
            .set("key", format!("value1_{i}"))
            .await
            .expect("Failed to set value in tree1");
        let entry_id = op
            .commit()
            .await
            .expect("Failed to commit operation in tree1");
        tree1_entries.push(entry_id);
    }

    // Create an entry in tree2
    let op2 = tree2
        .new_transaction()
        .await
        .expect("Failed to create operation in tree2");
    let store2 = op2
        .get_store::<DocStore>("data")
        .await
        .expect("Failed to get subtree in tree2");
    store2
        .set("key", "value2")
        .await
        .expect("Failed to set value in tree2");
    let entry2_id = op2
        .commit()
        .await
        .expect("Failed to commit operation in tree2");

    // Verify tree1 can get all its own entries
    let entries = tree1
        .get_entries(&tree1_entries)
        .await
        .expect("Failed to get tree1 entries");
    assert_eq!(entries.len(), 2);

    // Verify get_entries fails when trying to get entries from different trees
    let mixed_entries = vec![tree1_entries[0].clone(), entry2_id];
    let result = tree1.get_entries(&mixed_entries).await;
    assert!(result.is_err());
    let error_msg = result.unwrap_err().to_string();
    assert!(error_msg.contains("does not belong to database"));
}

/// Test authentication helpers with signed entries
#[tokio::test]
async fn test_auth_helpers_signed_entries() {
    let (_instance, tree, key_id) = setup_tree_with_user_auth().await;

    // Create signed entry using helper
    let entry_id = add_authenticated_data(&tree, "data", &[("key", "value")]).await;

    // Test entry auth access using helper
    assert_entry_authentication(&tree, &entry_id, &key_id).await;

    // Test entry belongs to tree
    assert_entry_belongs_to_tree(&tree, &entry_id).await;

    // Test manual auth checks
    let entry = tree
        .get_entry(&entry_id)
        .await
        .expect("Failed to get entry");
    let sig_info = &entry.sig;
    let hint = sig_info.hint();
    assert!(hint.pubkey.as_deref() == Some(&key_id) || hint.name.as_deref() == Some(&key_id));
    assert!(
        hint.pubkey.as_deref() != Some("OTHER_KEY") && hint.name.as_deref() != Some("OTHER_KEY")
    );
}

/// Test authentication helpers with default authenticated entries
#[tokio::test]
async fn test_auth_helpers_default_authenticated_entries() {
    let (_instance, tree, key_id) = setup_tree_with_user_key().await;

    // Create entry using default authentication helper
    let entry_id = add_data_to_subtree(&tree, "data", &[("key", "value")]).await;

    // Test entry auth access using helper
    assert_entry_authentication(&tree, &entry_id, &key_id).await;

    // Test manual auth checks
    let entry = tree
        .get_entry(&entry_id)
        .await
        .expect("Failed to get entry");
    let sig_info = &entry.sig;
    let hint = sig_info.hint();
    assert!(hint.pubkey.as_deref() == Some(&key_id) || hint.name.as_deref() == Some(&key_id));
    assert!(
        hint.pubkey.as_deref() != Some("OTHER_KEY") && hint.name.as_deref() != Some("OTHER_KEY")
    );
}

/// Test verify_entry_signature with different authentication scenarios
#[tokio::test]
async fn test_verify_entry_signature_auth_scenarios() {
    let (_instance, tree, key_id) = setup_tree_with_user_auth().await;

    // Test 1: Create entry signed with valid key using helper
    let signed_entry_id = add_authenticated_data(&tree, "data", &[("key", "value1")]).await;

    // Should verify successfully using helper
    assert_entry_authentication(&tree, &signed_entry_id, &key_id).await;

    // Test 2: Create unsigned entry using helper
    let unsigned_entry_id = add_data_to_subtree(&tree, "data", &[("key", "value2")]).await;

    // Should be valid (backward compatibility for unsigned entries)
    let is_valid_unsigned = tree
        .verify_entry_signature(&unsigned_entry_id)
        .await
        .expect("Failed to verify unsigned entry");
    assert!(is_valid_unsigned);
}

/// Test verify_entry_signature with unauthorized key
#[tokio::test]
async fn test_verify_entry_signature_unauthorized_key() {
    // Create user with first key (will be authorized)
    let (instance, mut user, authorized_key_id) =
        test_instance_with_user_and_key("test_user", Some("AUTHORIZED_KEY")).await;

    // Add second key (will NOT be authorized in the database)
    let unauthorized_key_id = user
        .add_private_key(Some("UNAUTHORIZED_KEY"))
        .await
        .expect("Failed to add unauthorized key");

    // Create database with ONLY the authorized key
    let mut settings = Doc::new();
    settings.set("name", "AuthenticatedTree");
    let tree = user
        .create_database(settings, &authorized_key_id)
        .await
        .expect("Failed to create tree");

    // Test with authorized key (should succeed) using helper
    let authorized_entry_id = add_authenticated_data(&tree, "data", &[("key", "value1")]).await;

    assert_entry_authentication(&tree, &authorized_entry_id, &authorized_key_id).await;

    // Test with unauthorized key (should fail at open because key is not in tree's auth settings)
    let unauthorized_signing_key = user
        .get_signing_key(&unauthorized_key_id)
        .expect("Failed to get unauthorized signing key");

    // Database::open should fail because the unauthorized key is not in the tree's auth settings
    // and no global permission exists
    let open_result = eidetica::Database::open(
        instance.clone(),
        tree.root_id(),
        unauthorized_signing_key,
        unauthorized_key_id.clone(),
    )
    .await;

    assert!(open_result.is_err());
    let error_msg = open_result.unwrap_err().to_string();
    assert!(
        error_msg.contains("not found in auth settings")
            || error_msg.contains("no global permission"),
        "Expected error about key not found, got: {error_msg}"
    );
}

/// Test that verify_entry_signature validates against tree auth configuration
#[tokio::test]
async fn test_verify_entry_signature_validates_tree_auth() {
    let (_instance, tree, key_id) = setup_tree_with_user_auth().await;

    // Create a signed entry using helper
    let entry_id = add_authenticated_data(&tree, "data", &[("key", "value")]).await;

    // Verify the entry using helper - should validate against tree's auth settings
    assert_entry_authentication(&tree, &entry_id, &key_id).await;

    // Note: In the future, this test should also verify that:
    // 1. Entries remain valid even if the key is later revoked (historical validation)
    // 2. Entry metadata contains the settings tips that were active when it was created
    // 3. Validation uses those historical settings rather than current settings
}

/// Test tree queries functionality
#[tokio::test]
async fn test_tree_queries() {
    let (_instance, tree, _key_id) = setup_tree_with_user_key().await;

    // Get initial entries
    let initial_entries = tree
        .get_all_entries()
        .await
        .expect("Failed to get initial entries");
    let initial_count = initial_entries.len();
    assert!(initial_count >= 1); // At least the root entry

    // Create a few entries
    let mut entry_ids = Vec::new();
    for i in 0..3 {
        let op = tree
            .new_transaction()
            .await
            .expect("Failed to create operation");
        let store = op
            .get_store::<DocStore>("data")
            .await
            .expect("Failed to get subtree");
        store
            .set("key", format!("value_{i}"))
            .await
            .expect("Failed to set value");
        let entry_id = op.commit().await.expect("Failed to commit operation");
        entry_ids.push(entry_id);
    }

    // Test get_all_entries
    let all_entries = tree
        .get_all_entries()
        .await
        .expect("Failed to get all entries");
    assert_eq!(all_entries.len(), initial_count + 3);

    // Verify all our created entries are in the result
    for entry_id in &entry_ids {
        let found = all_entries.iter().any(|entry| entry.id() == *entry_id);
        assert!(found, "Entry {entry_id} not found in all_entries");
    }
}

/// Test performance: batch get_entries vs individual get_entry calls
#[tokio::test]
async fn test_batch_vs_individual_retrieval() {
    let (_instance, tree, _key_id) = setup_tree_with_user_key().await;

    // Create multiple entries
    let mut entry_ids = Vec::new();
    for i in 0..5 {
        let op = tree
            .new_transaction()
            .await
            .expect("Failed to create operation");
        let store = op
            .get_store::<DocStore>("data")
            .await
            .expect("Failed to get subtree");
        store
            .set("key", format!("value_{i}"))
            .await
            .expect("Failed to set value");
        let entry_id = op.commit().await.expect("Failed to commit operation");
        entry_ids.push(entry_id);
    }

    // Test individual retrieval
    let mut individual_entries = Vec::new();
    for entry_id in &entry_ids {
        let entry = tree.get_entry(entry_id).await.expect("Failed to get entry");
        individual_entries.push(entry);
    }

    // Test batch retrieval
    let batch_entries = tree
        .get_entries(&entry_ids)
        .await
        .expect("Failed to get entries");

    // Results should be identical
    assert_eq!(individual_entries.len(), batch_entries.len());
    for (individual, batch) in individual_entries.iter().zip(batch_entries.iter()) {
        assert_eq!(individual.id(), batch.id());
        assert_eq!(individual.sig, batch.sig);
    }
}
