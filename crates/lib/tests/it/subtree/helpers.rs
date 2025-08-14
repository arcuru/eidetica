//! Comprehensive helper functions for subtree testing
//!
//! This module provides utilities for testing Doc, YDoc, and Table subtree functionality
//! including basic operations, CRUD operations, search functionality, and integration scenarios.

use crate::helpers::*;
use eidetica::crdt::Doc;
use eidetica::crdt::map::Value;
use eidetica::subtree::{Dict, Table};
use serde::{Deserialize, Serialize};

#[cfg(feature = "y-crdt")]
use eidetica::subtree::YDoc;
#[cfg(feature = "y-crdt")]
use yrs::{Doc as YrsDoc, GetString, Map as YrsMapTrait, ReadTxn, Text, Transact};

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
pub fn create_dict_operation(
    tree: &eidetica::Tree,
    subtree_name: &str,
    data: &[(&str, &str)],
) -> eidetica::entry::ID {
    let op = tree.new_operation().unwrap();
    let dict = op.get_subtree::<Dict>(subtree_name).unwrap();

    for (key, value) in data {
        dict.set(*key, *value).unwrap();
    }

    op.commit().unwrap()
}

/// Create Doc operation with nested Map values
pub fn create_dict_with_nested_map(
    tree: &eidetica::Tree,
    subtree_name: &str,
) -> eidetica::entry::ID {
    let op = tree.new_operation().unwrap();
    let dict = op.get_subtree::<Dict>(subtree_name).unwrap();

    // Set regular string
    dict.set("key1", "value1").unwrap();

    // Set nested map
    let mut nested = Doc::new();
    nested.set_string("inner", "nested_value");
    dict.set_value("key2", Value::Node(nested.into())).unwrap();

    op.commit().unwrap()
}

/// Create Doc operation with List values
pub fn create_dict_with_list(
    tree: &eidetica::Tree,
    subtree_name: &str,
    list_items: &[&str],
) -> eidetica::entry::ID {
    let op = tree.new_operation().unwrap();
    let dict = op.get_subtree::<Dict>(subtree_name).unwrap();

    let mut fruits = eidetica::crdt::map::List::new();
    for item in list_items {
        fruits.push(Value::Text(item.to_string()));
    }

    dict.set_list("fruits", fruits).unwrap();
    op.commit().unwrap()
}

/// Test multiple Doc operations across commits
pub fn test_dict_persistence(
    tree: &eidetica::Tree,
    subtree_name: &str,
) -> Vec<eidetica::entry::ID> {
    let mut entry_ids = Vec::new();

    // Op 1: Initial data
    let op1 = tree.new_operation().unwrap();
    {
        let dict = op1.get_subtree::<Dict>(subtree_name).unwrap();
        dict.set("key_a", "val_a").unwrap();
        dict.set("key_b", "val_b").unwrap();
    }
    entry_ids.push(op1.commit().unwrap());

    // Op 2: Update one, add another
    let op2 = tree.new_operation().unwrap();
    {
        let dict = op2.get_subtree::<Dict>(subtree_name).unwrap();
        dict.set("key_b", "val_b_updated").unwrap();
        dict.set("key_c", "val_c").unwrap();
    }
    entry_ids.push(op2.commit().unwrap());

    entry_ids
}

// ===== DICT VERIFICATION HELPERS =====

/// Verify Doc has expected key-value pairs using viewer
pub fn assert_dict_viewer_data(
    tree: &eidetica::Tree,
    subtree_name: &str,
    expected_data: &[(&str, &str)],
) {
    let viewer = tree.get_subtree_viewer::<Dict>(subtree_name).unwrap();
    for (key, expected_value) in expected_data {
        assert_dict_value(&viewer, key, expected_value);
    }
}

/// Verify Doc viewer shows correct number of entries
pub fn assert_dict_viewer_count(tree: &eidetica::Tree, subtree_name: &str, expected_count: usize) {
    let viewer = tree.get_subtree_viewer::<Dict>(subtree_name).unwrap();
    let all_data = viewer.get_all().unwrap();
    assert_eq!(all_data.as_hashmap().len(), expected_count);
}

/// Verify Doc viewer List operations
pub fn assert_dict_list_data(
    tree: &eidetica::Tree,
    subtree_name: &str,
    list_key: &str,
    expected_items: &[&str],
) {
    let viewer = tree.get_subtree_viewer::<Dict>(subtree_name).unwrap();
    let list = viewer.get_list(list_key).unwrap();

    assert_eq!(list.len(), expected_items.len());
    for (i, expected_item) in expected_items.iter().enumerate() {
        assert_eq!(list.get(i), Some(&Value::Text(expected_item.to_string())));
    }
}

/// Verify nested Map structure in Doc
pub fn assert_dict_nested_map(dict: &Doc, map_key: &str, nested_data: &[(&str, &str)]) {
    match dict.get(map_key).unwrap() {
        Value::Node(map) => {
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
pub fn create_ydoc_text_operation(
    tree: &eidetica::Tree,
    subtree_name: &str,
    text_content: &str,
) -> eidetica::entry::ID {
    let op = tree.new_operation().unwrap();
    let ydoc = op.get_subtree::<YDoc>(subtree_name).unwrap();

    ydoc.with_doc_mut(|doc| {
        let text = doc.get_or_insert_text("document");
        let mut txn = doc.transact_mut();
        text.insert(&mut txn, 0, text_content);
        Ok(())
    })
    .unwrap();

    op.commit().unwrap()
}

#[cfg(feature = "y-crdt")]
/// Create YDoc operation with map data
pub fn create_ydoc_map_operation(
    tree: &eidetica::Tree,
    subtree_name: &str,
    map_data: &[(&str, &str)],
) -> eidetica::entry::ID {
    let op = tree.new_operation().unwrap();
    let ydoc = op.get_subtree::<YDoc>(subtree_name).unwrap();

    ydoc.with_doc_mut(|doc| {
        let map = doc.get_or_insert_map("root");
        let mut txn = doc.transact_mut();
        for (key, value) in map_data {
            map.insert(&mut txn, *key, *value);
        }
        Ok(())
    })
    .unwrap();

    op.commit().unwrap()
}

#[cfg(feature = "y-crdt")]
/// Test incremental YDoc updates and verify diff sizes
pub fn test_ydoc_incremental_updates(tree: &eidetica::Tree, subtree_name: &str) -> (usize, usize) {
    // Large initial content
    let op1 = tree.new_operation().unwrap();
    let first_diff_size = {
        let ydoc = op1.get_subtree::<YDoc>(subtree_name).unwrap();
        ydoc.with_doc_mut(|doc| {
            let text = doc.get_or_insert_text("document");
            let mut txn = doc.transact_mut();
            let large_content =
                "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(200);
            text.insert(&mut txn, 0, &large_content);
            Ok(())
        })
        .unwrap();

        let local_diff: eidetica::subtree::YrsBinary = op1.get_local_data(subtree_name).unwrap();
        local_diff.as_bytes().len()
    };
    op1.commit().unwrap();

    // Small incremental change
    let op2 = tree.new_operation().unwrap();
    let second_diff_size = {
        let ydoc = op2.get_subtree::<YDoc>(subtree_name).unwrap();
        ydoc.with_doc_mut(|doc| {
            let text = doc.get_or_insert_text("document");
            let mut txn = doc.transact_mut();
            text.insert(&mut txn, 12, " SMALL_CHANGE");
            Ok(())
        })
        .unwrap();

        let local_diff: eidetica::subtree::YrsBinary = op2.get_local_data(subtree_name).unwrap();
        local_diff.as_bytes().len()
    };
    op2.commit().unwrap();

    (first_diff_size, second_diff_size)
}

#[cfg(feature = "y-crdt")]
/// Verify YDoc text content using viewer
pub fn assert_ydoc_text_content(tree: &eidetica::Tree, subtree_name: &str, expected_text: &str) {
    let viewer = tree.get_subtree_viewer::<YDoc>(subtree_name).unwrap();
    viewer
        .with_doc(|doc| {
            let text = doc.get_or_insert_text("document");
            let txn = doc.transact();
            let content = text.get_string(&txn);
            assert_eq!(content, expected_text);
            Ok(())
        })
        .unwrap();
}

#[cfg(feature = "y-crdt")]
/// Verify YDoc map content using viewer
pub fn assert_ydoc_map_content(
    tree: &eidetica::Tree,
    subtree_name: &str,
    expected_data: &[(&str, &str)],
) {
    let viewer = tree.get_subtree_viewer::<YDoc>(subtree_name).unwrap();
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
        .unwrap();
}

#[cfg(feature = "y-crdt")]
/// Create external YDoc update for testing
pub fn create_external_ydoc_update(content: &str) -> Vec<u8> {
    let external_doc = YrsDoc::new();
    let text = external_doc.get_or_insert_text("shared_doc");
    let mut txn = external_doc.transact_mut();
    text.insert(&mut txn, 0, content);
    drop(txn);

    let txn = external_doc.transact();
    txn.encode_state_as_update_v1(&yrs::StateVector::default())
}

// ===== TABLE OPERATION HELPERS =====

/// Create and commit a Table operation with TestRecord data
pub fn create_table_operation(
    tree: &eidetica::Tree,
    subtree_name: &str,
    records: &[TestRecord],
) -> Vec<String> {
    let op = tree.new_operation().unwrap();
    let table = op.get_subtree::<Table<TestRecord>>(subtree_name).unwrap();

    let mut keys = Vec::new();
    for record in records {
        let key = table.insert(record.clone()).unwrap();
        keys.push(key);
    }

    op.commit().unwrap();
    keys
}

/// Create Table operation with SimpleRecord data
pub fn create_simple_table_operation(
    tree: &eidetica::Tree,
    subtree_name: &str,
    values: &[i32],
) -> Vec<String> {
    let op = tree.new_operation().unwrap();
    let table = op.get_subtree::<Table<SimpleRecord>>(subtree_name).unwrap();

    let mut keys = Vec::new();
    for value in values {
        let record = SimpleRecord { value: *value };
        let key = table.insert(record).unwrap();
        keys.push(key);
    }

    op.commit().unwrap();
    keys
}

/// Test Table multi-operation workflow
pub fn test_table_multi_operations(
    tree: &eidetica::Tree,
    subtree_name: &str,
) -> (String, String, String) {
    // Op 1: Insert initial records
    let op1 = tree.new_operation().unwrap();
    let (key1, key2) = {
        let table = op1.get_subtree::<Table<TestRecord>>(subtree_name).unwrap();

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

        let k1 = table.insert(record1).unwrap();
        let k2 = table.insert(record2).unwrap();
        (k1, k2)
    };
    op1.commit().unwrap();

    // Op 2: Update and add
    let op2 = tree.new_operation().unwrap();
    let key3 = {
        let table = op2.get_subtree::<Table<TestRecord>>(subtree_name).unwrap();

        // Update existing
        let updated_record1 = TestRecord {
            name: "Updated User 1".to_string(),
            age: 21,
            email: "user1@updated.com".to_string(),
        };
        table.set(&key1, updated_record1).unwrap();

        // Add new
        let record3 = TestRecord {
            name: "New User 3".to_string(),
            age: 30,
            email: "user3@new.com".to_string(),
        };
        table.insert(record3).unwrap()
    };
    op2.commit().unwrap();

    (key1, key2, key3)
}

// ===== TABLE VERIFICATION HELPERS =====

/// Verify Table record using viewer
pub fn assert_table_record(
    tree: &eidetica::Tree,
    subtree_name: &str,
    key: &str,
    expected_record: &TestRecord,
) {
    let viewer = tree
        .get_subtree_viewer::<Table<TestRecord>>(subtree_name)
        .unwrap();
    let record = viewer.get(key).unwrap();
    assert_eq!(record, *expected_record);
}

/// Verify Table search results
pub fn assert_table_search_count<F>(
    tree: &eidetica::Tree,
    subtree_name: &str,
    predicate: F,
    expected_count: usize,
) where
    F: Fn(&TestRecord) -> bool,
{
    let viewer = tree
        .get_subtree_viewer::<Table<TestRecord>>(subtree_name)
        .unwrap();
    let results = viewer.search(predicate).unwrap();
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
pub fn test_table_concurrent_modifications(
    tree: &eidetica::Tree,
    subtree_name: &str,
) -> (String, TestRecord) {
    // Create base entry
    let op_base = tree.new_operation().unwrap();
    let key1 = {
        let table = op_base
            .get_subtree::<Table<TestRecord>>(subtree_name)
            .unwrap();
        let record = TestRecord {
            name: "Original User".to_string(),
            age: 25,
            email: "original@test.com".to_string(),
        };
        table.insert(record).unwrap()
    };
    let base_entry_id = op_base.commit().unwrap();

    // Branch A: Concurrent modification
    let op_branch_a = tree
        .new_operation_with_tips([base_entry_id.clone()])
        .unwrap();
    {
        let table = op_branch_a
            .get_subtree::<Table<TestRecord>>(subtree_name)
            .unwrap();
        let updated_record = TestRecord {
            name: "Updated by Branch A".to_string(),
            age: 26,
            email: "updated_a@test.com".to_string(),
        };
        table.set(&key1, updated_record).unwrap();
        op_branch_a.commit().unwrap();
    }

    // Branch B: Parallel modification
    let op_branch_b = tree.new_operation_with_tips([base_entry_id]).unwrap();
    {
        let table = op_branch_b
            .get_subtree::<Table<TestRecord>>(subtree_name)
            .unwrap();
        let updated_record = TestRecord {
            name: "Updated by Branch B".to_string(),
            age: 27,
            email: "updated_b@test.com".to_string(),
        };
        table.set(&key1, updated_record).unwrap();
        op_branch_b.commit().unwrap();
    }

    // Get merged result
    let op_merge = tree.new_operation().unwrap();
    let merged_record = {
        let table = op_merge
            .get_subtree::<Table<TestRecord>>(subtree_name)
            .unwrap();
        table.get(&key1).unwrap()
    };
    op_merge.commit().unwrap();

    (key1, merged_record)
}
