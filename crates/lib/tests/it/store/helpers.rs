//! Comprehensive helper functions for subtree testing
//!
//! This module provides utilities for testing Doc, YDoc, and Table subtree functionality
//! including basic operations, CRUD operations, search functionality, and integration scenarios.

#[cfg(feature = "y-crdt")]
use eidetica::store::{YDoc, YrsBinary};
use eidetica::{
    Database, Registered, Transaction,
    crdt::{
        Doc,
        doc::{List, Value},
    },
    entry::ID,
    store::{DocStore, PasswordStore, Table},
};
use serde::{Deserialize, Serialize};
#[cfg(feature = "y-crdt")]
use yrs::{GetString, Map as YrsMapTrait, ReadTxn, Text, Transact};

use crate::helpers::*;

// ===== TEST DATA STRUCTURES =====

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestRecord {
    pub name: String,
    pub age: u32,
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SimpleRecord {
    pub value: i32,
}

// ===== DICT OPERATION HELPERS =====

/// Create and commit a basic Doc operation with key-value data
pub async fn create_dict_operation(
    tree: &Database,
    subtree_name: &str,
    data: &[(&str, &str)],
) -> ID {
    let txn = tree.new_transaction().await.unwrap();
    let dict = txn.get_store::<DocStore>(subtree_name).await.unwrap();

    for (key, value) in data {
        dict.set(*key, *value).await.unwrap();
    }

    txn.commit().await.unwrap()
}

/// Create Doc operation with nested Map values
pub async fn create_dict_with_nested_map(tree: &Database, subtree_name: &str) -> ID {
    let txn = tree.new_transaction().await.unwrap();
    let dict = txn.get_store::<DocStore>(subtree_name).await.unwrap();

    // Set regular string
    dict.set("key1", "value1").await.unwrap();

    // Set nested map
    let mut nested = Doc::new();
    nested.set("inner", "nested_value");
    dict.set_value("key2", Value::Doc(nested)).await.unwrap();

    txn.commit().await.unwrap()
}

/// Create Doc operation with List values
pub async fn create_dict_with_list(tree: &Database, subtree_name: &str, list_items: &[&str]) -> ID {
    let txn = tree.new_transaction().await.unwrap();
    let dict = txn.get_store::<DocStore>(subtree_name).await.unwrap();

    let mut fruits = List::new();
    for item in list_items {
        fruits.push(Value::Text(item.to_string()));
    }

    dict.set_list("fruits", fruits).await.unwrap();
    txn.commit().await.unwrap()
}

/// Test multiple Doc operations across commits
pub async fn test_dict_persistence(tree: &Database, subtree_name: &str) -> Vec<ID> {
    let mut entry_ids = Vec::new();

    // Txn 1: Initial data
    let txn1 = tree.new_transaction().await.unwrap();
    {
        let dict = txn1.get_store::<DocStore>(subtree_name).await.unwrap();
        dict.set("key_a", "val_a").await.unwrap();
        dict.set("key_b", "val_b").await.unwrap();
    }
    entry_ids.push(txn1.commit().await.unwrap());

    // Txn 2: Update one, add another
    let txn2 = tree.new_transaction().await.unwrap();
    {
        let dict = txn2.get_store::<DocStore>(subtree_name).await.unwrap();
        dict.set("key_b", "val_b_updated").await.unwrap();
        dict.set("key_c", "val_c").await.unwrap();
    }
    entry_ids.push(txn2.commit().await.unwrap());

    entry_ids
}

// ===== DICT VERIFICATION HELPERS =====

/// Verify Doc has expected key-value pairs using viewer
pub async fn assert_dict_viewer_data(
    tree: &Database,
    subtree_name: &str,
    expected_data: &[(&str, &str)],
) {
    let viewer = tree
        .get_store_viewer::<DocStore>(subtree_name)
        .await
        .unwrap();
    for (key, expected_value) in expected_data {
        assert_dict_value(&viewer, key, expected_value).await;
    }
}

/// Verify Doc viewer shows correct number of entries
pub async fn assert_dict_viewer_count(tree: &Database, subtree_name: &str, expected_count: usize) {
    let viewer = tree
        .get_store_viewer::<DocStore>(subtree_name)
        .await
        .unwrap();
    let all_data = viewer.get_all().await.unwrap();
    assert_eq!(all_data.len(), expected_count);
}

/// Verify Doc viewer List operations
pub async fn assert_dict_list_data(
    tree: &Database,
    subtree_name: &str,
    list_key: &str,
    expected_items: &[&str],
) {
    let viewer = tree
        .get_store_viewer::<DocStore>(subtree_name)
        .await
        .unwrap();
    let list = viewer.get_as::<List>(list_key).await.unwrap();

    assert_eq!(list.len(), expected_items.len());
    for (i, expected_item) in expected_items.iter().enumerate() {
        assert_eq!(list.get(i), Some(&Value::Text(expected_item.to_string())));
    }
}

/// Verify nested Map structure in DocStore
pub async fn assert_dict_nested_map(dict: &DocStore, map_key: &str, nested_data: &[(&str, &str)]) {
    match dict.get(map_key).await.unwrap() {
        Value::Doc(map) => {
            for (key, expected_value) in nested_data {
                match map.get(key) {
                    Some(Value::Text(value)) => assert_eq!(value, *expected_value),
                    _ => panic!("Expected string value for nested key '{key}'"),
                }
            }
        }
        _ => panic!("Expected map value for key '{map_key}'"),
    }
}

// ===== Y-CRDT HELPERS =====

#[cfg(feature = "y-crdt")]
/// Create YDoc operation with text content
pub async fn create_ydoc_text_operation(
    tree: &Database,
    subtree_name: &str,
    text_content: &str,
) -> ID {
    let txn = tree.new_transaction().await.unwrap();
    let ydoc = txn.get_store::<YDoc>(subtree_name).await.unwrap();

    ydoc.with_doc_mut(|doc| {
        let text = doc.get_or_insert_text("document");
        let mut txn = doc.transact_mut();
        text.insert(&mut txn, 0, text_content);
        Ok(())
    })
    .await
    .unwrap();

    txn.commit().await.unwrap()
}

#[cfg(feature = "y-crdt")]
/// Create YDoc operation with map data
pub async fn create_ydoc_map_operation(
    tree: &Database,
    subtree_name: &str,
    map_data: &[(&str, &str)],
) -> ID {
    let txn = tree.new_transaction().await.unwrap();
    let ydoc = txn.get_store::<YDoc>(subtree_name).await.unwrap();

    ydoc.with_doc_mut(|doc| {
        let map = doc.get_or_insert_map("root");
        let mut txn = doc.transact_mut();
        for (key, value) in map_data {
            map.insert(&mut txn, *key, *value);
        }
        Ok(())
    })
    .await
    .unwrap();

    txn.commit().await.unwrap()
}

#[cfg(feature = "y-crdt")]
/// Test incremental YDoc updates and verify diff sizes
pub async fn test_ydoc_incremental_updates(tree: &Database, subtree_name: &str) -> (usize, usize) {
    // Large initial content
    let txn1 = tree.new_transaction().await.unwrap();
    let first_diff_size = {
        let ydoc = txn1.get_store::<YDoc>(subtree_name).await.unwrap();
        ydoc.with_doc_mut(|doc| {
            let text = doc.get_or_insert_text("document");
            let mut txn = doc.transact_mut();
            let large_content =
                "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(200);
            text.insert(&mut txn, 0, &large_content);
            Ok(())
        })
        .await
        .unwrap();

        let local_diff: YrsBinary = txn1
            .get_local_data(subtree_name)
            .expect("no error")
            .expect("data should be staged");
        local_diff.as_bytes().len()
    };
    txn1.commit().await.unwrap();

    // Small incremental change
    let txn2 = tree.new_transaction().await.unwrap();
    let second_diff_size = {
        let ydoc = txn2.get_store::<YDoc>(subtree_name).await.unwrap();
        ydoc.with_doc_mut(|doc| {
            let text = doc.get_or_insert_text("document");
            let mut txn = doc.transact_mut();
            text.insert(&mut txn, 12, " SMALL_CHANGE");
            Ok(())
        })
        .await
        .unwrap();

        let local_diff: YrsBinary = txn2
            .get_local_data(subtree_name)
            .expect("no error")
            .expect("data should be staged");
        local_diff.as_bytes().len()
    };
    txn2.commit().await.unwrap();

    (first_diff_size, second_diff_size)
}

#[cfg(feature = "y-crdt")]
/// Verify YDoc text content using viewer
pub async fn assert_ydoc_text_content(tree: &Database, subtree_name: &str, expected_text: &str) {
    let viewer = tree.get_store_viewer::<YDoc>(subtree_name).await.unwrap();
    viewer
        .with_doc(|doc| {
            let text = doc.get_or_insert_text("document");
            let txn = doc.transact();
            let content = text.get_string(&txn);
            assert_eq!(content, expected_text);
            Ok(())
        })
        .await
        .unwrap();
}

#[cfg(feature = "y-crdt")]
/// Verify YDoc map content using viewer
pub async fn assert_ydoc_map_content(
    tree: &Database,
    subtree_name: &str,
    expected_data: &[(&str, &str)],
) {
    let viewer = tree.get_store_viewer::<YDoc>(subtree_name).await.unwrap();
    viewer
        .with_doc(|doc| {
            let map = doc.get_or_insert_map("root");
            let txn = doc.transact();

            for (key, expected_value) in expected_data {
                let val = map
                    .get(&txn, key)
                    .unwrap_or_else(|| panic!("Key '{key}' should exist"));
                assert_eq!(val.to_string(&txn), *expected_value);
            }
            Ok(())
        })
        .await
        .unwrap();
}

#[cfg(feature = "y-crdt")]
/// Create external YDoc update for testing
pub fn create_external_ydoc_update(content: &str) -> Vec<u8> {
    let external_doc = yrs::Doc::new();
    let text = external_doc.get_or_insert_text("shared_doc");
    let mut txn = external_doc.transact_mut();
    text.insert(&mut txn, 0, content);
    drop(txn);

    let txn = external_doc.transact();
    txn.encode_state_as_update_v1(&yrs::StateVector::default())
}

// ===== TABLE OPERATION HELPERS =====

/// Create and commit a Table operation with TestRecord data
pub async fn create_table_operation(
    tree: &Database,
    subtree_name: &str,
    records: &[TestRecord],
) -> Vec<String> {
    let txn = tree.new_transaction().await.unwrap();
    let table = txn
        .get_store::<Table<TestRecord>>(subtree_name)
        .await
        .unwrap();

    let mut keys = Vec::new();
    for record in records {
        let key = table.insert(record.clone()).await.unwrap();
        keys.push(key);
    }

    txn.commit().await.unwrap();
    keys
}

/// Create Table operation with SimpleRecord data
pub async fn create_simple_table_operation(
    tree: &Database,
    subtree_name: &str,
    values: &[i32],
) -> Vec<String> {
    let txn = tree.new_transaction().await.unwrap();
    let table = txn
        .get_store::<Table<SimpleRecord>>(subtree_name)
        .await
        .unwrap();

    let mut keys = Vec::new();
    for value in values {
        let record = SimpleRecord { value: *value };
        let key = table.insert(record).await.unwrap();
        keys.push(key);
    }

    txn.commit().await.unwrap();
    keys
}

/// Test Table multi-operation workflow
pub async fn test_table_multi_operations(
    tree: &Database,
    subtree_name: &str,
) -> (String, String, String) {
    // Txn 1: Insert initial records
    let txn1 = tree.new_transaction().await.unwrap();
    let (key1, key2) = {
        let table = txn1
            .get_store::<Table<TestRecord>>(subtree_name)
            .await
            .unwrap();

        let record1 = TestRecord {
            name: "Initial User 1".to_string(),
            age: 20,
            email: "user1@initial.com".to_string(),
        };
        let record2 = TestRecord {
            name: "Initial User 2".to_string(),
            age: 25,
            email: "user2@initial.com".to_string(),
        };

        let k1 = table.insert(record1).await.unwrap();
        let k2 = table.insert(record2).await.unwrap();
        (k1, k2)
    };
    txn1.commit().await.unwrap();

    // Txn 2: Update and add
    let txn2 = tree.new_transaction().await.unwrap();
    let key3 = {
        let table = txn2
            .get_store::<Table<TestRecord>>(subtree_name)
            .await
            .unwrap();

        // Update existing
        let updated_record1 = TestRecord {
            name: "Updated User 1".to_string(),
            age: 21,
            email: "user1@updated.com".to_string(),
        };
        table.set(&key1, updated_record1).await.unwrap();

        // Add new
        let record3 = TestRecord {
            name: "New User 3".to_string(),
            age: 30,
            email: "user3@new.com".to_string(),
        };
        table.insert(record3).await.unwrap()
    };
    txn2.commit().await.unwrap();

    (key1, key2, key3)
}

// ===== TABLE VERIFICATION HELPERS =====

/// Verify Table record using viewer
pub async fn assert_table_record(
    tree: &Database,
    subtree_name: &str,
    key: &str,
    expected_record: &TestRecord,
) {
    let viewer = tree
        .get_store_viewer::<Table<TestRecord>>(subtree_name)
        .await
        .unwrap();
    let record = viewer.get(key).await.unwrap();
    assert_eq!(record, *expected_record);
}

/// Verify Table search results
pub async fn assert_table_search_count<F>(
    tree: &Database,
    subtree_name: &str,
    predicate: F,
    expected_count: usize,
) where
    F: Fn(&TestRecord) -> bool,
{
    let viewer = tree
        .get_store_viewer::<Table<TestRecord>>(subtree_name)
        .await
        .unwrap();
    let results = viewer.search(predicate).await.unwrap();
    assert_eq!(results.len(), expected_count);
}

/// Verify UUID format and uniqueness
pub fn assert_valid_uuids(keys: &[String]) {
    let mut seen = std::collections::HashSet::new();
    for key in keys {
        // Verify UUID format (36 characters with 4 hyphens)
        assert_eq!(key.len(), 36);
        assert_eq!(key.chars().filter(|&c| c == '-').count(), 4);

        // Verify uniqueness
        assert!(seen.insert(key.clone()), "Duplicate UUID: {key}");
    }
}

/// Verify that a Table record does not exist (has been deleted)
pub async fn assert_table_record_deleted(tree: &Database, subtree_name: &str, key: &str) {
    let viewer = tree
        .get_store_viewer::<Table<TestRecord>>(subtree_name)
        .await
        .unwrap();
    assert!(
        viewer.get(key).await.is_err(),
        "Record with key '{key}' should have been deleted"
    );
}

// ===== INTEGRATION HELPERS =====

/// Create test data for sample records
pub fn create_test_records() -> Vec<TestRecord> {
    vec![
        TestRecord {
            name: "Alice Johnson".to_string(),
            age: 25,
            email: "alice@example.com".to_string(),
        },
        TestRecord {
            name: "Bob Smith".to_string(),
            age: 30,
            email: "bob@company.com".to_string(),
        },
        TestRecord {
            name: "Charlie Brown".to_string(),
            age: 25,
            email: "charlie@example.com".to_string(),
        },
        TestRecord {
            name: "Diana Prince".to_string(),
            age: 35,
            email: "diana@hero.org".to_string(),
        },
    ]
}

/// Test concurrent Table modifications with merging
pub async fn test_table_concurrent_modifications(
    tree: &Database,
    subtree_name: &str,
) -> (String, TestRecord) {
    // Create base entry
    let txn_base = tree.new_transaction().await.unwrap();
    let key1 = {
        let table = txn_base
            .get_store::<Table<TestRecord>>(subtree_name)
            .await
            .unwrap();
        let record = TestRecord {
            name: "Original User".to_string(),
            age: 25,
            email: "original@test.com".to_string(),
        };
        table.insert(record).await.unwrap()
    };
    let base_entry_id = txn_base.commit().await.unwrap();

    // Branch A: Concurrent modification
    let op_branch_a = tree
        .new_transaction_with_tips([base_entry_id.clone()])
        .await
        .unwrap();
    {
        let table = op_branch_a
            .get_store::<Table<TestRecord>>(subtree_name)
            .await
            .unwrap();
        let updated_record = TestRecord {
            name: "Updated by Branch A".to_string(),
            age: 26,
            email: "updated_a@test.com".to_string(),
        };
        table.set(&key1, updated_record).await.unwrap();
        op_branch_a.commit().await.unwrap();
    }

    // Branch B: Parallel modification
    let op_branch_b = tree
        .new_transaction_with_tips([base_entry_id])
        .await
        .unwrap();
    {
        let table = op_branch_b
            .get_store::<Table<TestRecord>>(subtree_name)
            .await
            .unwrap();
        let updated_record = TestRecord {
            name: "Updated by Branch B".to_string(),
            age: 27,
            email: "updated_b@test.com".to_string(),
        };
        table.set(&key1, updated_record).await.unwrap();
        op_branch_b.commit().await.unwrap();
    }

    // Get merged result
    let op_merge = tree.new_transaction().await.unwrap();
    let merged_record = {
        let table = op_merge
            .get_store::<Table<TestRecord>>(subtree_name)
            .await
            .unwrap();
        table.get(&key1).await.unwrap()
    };
    op_merge.commit().await.unwrap();

    (key1, merged_record)
}

// ===== PASSWORD STORE HELPERS =====

/// Initialize a PasswordStore with DocStore wrapper
pub async fn init_password_store_docstore(tree: &Database, store_name: &str, password: &str) {
    let tx = tree.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>(store_name).await.unwrap();
    encrypted
        .initialize(password, DocStore::type_id(), Doc::new())
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// Open an existing PasswordStore and return it
/// Caller must commit the transaction when done
pub async fn open_password_store(
    tx: &Transaction,
    store_name: &str,
    password: &str,
) -> PasswordStore {
    let mut encrypted = tx.get_store::<PasswordStore>(store_name).await.unwrap();
    encrypted.open(password).unwrap();
    encrypted
}

/// Create and initialize a PasswordStore wrapping DocStore, add data, and commit
pub async fn create_password_docstore_with_data(
    tree: &Database,
    store_name: &str,
    password: &str,
    data: &[(&str, &str)],
) -> ID {
    let tx = tree.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>(store_name).await.unwrap();
    encrypted
        .initialize(password, DocStore::type_id(), Doc::new())
        .await
        .unwrap();

    let docstore = encrypted.unwrap::<DocStore>().await.unwrap();
    for (key, value) in data {
        docstore.set(*key, *value).await.unwrap();
    }

    tx.commit().await.unwrap()
}

/// Open a PasswordStore, add data to the wrapped DocStore, and commit
pub async fn add_data_to_password_docstore(
    tree: &Database,
    store_name: &str,
    password: &str,
    data: &[(&str, &str)],
) -> ID {
    let tx = tree.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>(store_name).await.unwrap();
    encrypted.open(password).unwrap();

    let docstore = encrypted.unwrap::<DocStore>().await.unwrap();
    for (key, value) in data {
        docstore.set(*key, *value).await.unwrap();
    }

    tx.commit().await.unwrap()
}

/// Verify data in an encrypted DocStore
pub async fn assert_password_docstore_data(
    tree: &Database,
    store_name: &str,
    password: &str,
    expected_data: &[(&str, &str)],
) {
    let tx = tree.new_transaction().await.unwrap();
    let mut encrypted = tx.get_store::<PasswordStore>(store_name).await.unwrap();
    encrypted.open(password).unwrap();

    let docstore = encrypted.unwrap::<DocStore>().await.unwrap();
    for (key, expected_value) in expected_data {
        let value = docstore.get(key).await.unwrap();
        assert_eq!(value.as_text(), Some(*expected_value));
    }
}

/// Set invalid PasswordStore config in the index for error testing
pub async fn set_invalid_password_store_config(
    tree: &Database,
    store_name: &str,
    invalid_config: &str,
) {
    use eidetica::crdt::{Doc, doc::Value};
    let tx = tree.new_transaction().await.unwrap();
    let index_store = tx.get_index().await.unwrap();
    let mut config_doc = Doc::new();
    config_doc.set("data", Value::Text(invalid_config.to_string()));
    index_store
        .set_entry(store_name, PasswordStore::type_id(), config_doc)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

// Common invalid configs for error testing
pub mod invalid_configs {
    pub const INVALID_SALT: &str = r#"{
        "wrapped_config": {
            "ciphertext": [1, 2, 3],
            "nonce": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
        },
        "encryption": {
            "algorithm": "aes-256-gcm",
            "kdf": "argon2id",
            "salt": "not!!!valid!!!base64",
            "version": "1"
        }
    }"#;

    pub const INVALID_NONCE_LENGTH: &str = r#"{
        "wrapped_config": {
            "ciphertext": [1, 2, 3],
            "nonce": [1, 2, 3]
        },
        "encryption": {
            "algorithm": "aes-256-gcm",
            "kdf": "argon2id",
            "salt": "abcdefghijklmnop",
            "version": "1"
        }
    }"#;

    pub const CORRUPTED_CIPHERTEXT: &str = r#"{
        "wrapped_config": {
            "ciphertext": [255, 255, 255],
            "nonce": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
        },
        "encryption": {
            "algorithm": "aes-256-gcm",
            "kdf": "argon2id",
            "salt": "abcdefghijklmnop",
            "version": "1"
        }
    }"#;

    pub const UNSUPPORTED_ALGORITHM: &str = r#"{
        "wrapped_config": {
            "ciphertext": [1, 2, 3],
            "nonce": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
        },
        "encryption": {
            "algorithm": "aes-128-gcm",
            "kdf": "argon2id",
            "salt": "c29tZXNhbHQ=",
            "version": "1"
        }
    }"#;

    pub const UNSUPPORTED_KDF: &str = r#"{
        "wrapped_config": {
            "ciphertext": [1, 2, 3],
            "nonce": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
        },
        "encryption": {
            "algorithm": "aes-256-gcm",
            "kdf": "pbkdf2",
            "salt": "c29tZXNhbHQ=",
            "version": "1"
        }
    }"#;

    pub const MALFORMED_JSON: &str = r#"{ this is not valid json! }"#;
}
