//! Comprehensive helper functions for Transaction testing
//!
//! This module provides utilities for testing Transaction functionality including
//! operations, custom tips, diamond patterns, and data isolation scenarios.

use eidetica::{
    Database, Instance,
    crdt::{Doc, doc::Value},
    entry::ID,
    store::DocStore,
};

// Type alias for local usage
type Map = Doc;

use crate::helpers::*;

// ===== BASIC OPERATION HELPERS =====

/// Create and commit a simple operation with one Doc subtree
pub async fn create_simple_operation(
    tree: &Database,
    subtree_name: &str,
    key: &str,
    value: &str,
) -> ID {
    let operation = tree.new_transaction().await.unwrap();
    let dict = operation.get_store::<DocStore>(subtree_name).await.unwrap();
    dict.set(key, value).await.unwrap();
    operation.commit().await.unwrap()
}

/// Create an operation with multiple subtrees and data
pub async fn create_multi_subtree_operation(
    tree: &Database,
    subtree_data: &[(&str, &[(&str, &str)])],
) -> ID {
    let operation = tree.new_transaction().await.unwrap();

    for (subtree_name, data) in subtree_data {
        let dict = operation
            .get_store::<DocStore>(*subtree_name)
            .await
            .unwrap();
        for (key, value) in *data {
            dict.set(*key, *value).await.unwrap();
        }
    }

    operation.commit().await.unwrap()
}

/// Setup a tree with initial data across multiple subtrees
///
/// Note: Returns the Instance along with the Database because Database holds a weak reference.
/// If the Instance is dropped, operations on the Database will fail with InstanceDropped.
pub async fn setup_tree_with_data(
    subtree_data: &[(&str, &[(&str, &str)])],
) -> (Instance, Database) {
    let (instance, tree) = setup_tree().await;
    create_multi_subtree_operation(&tree, subtree_data).await;
    (instance, tree)
}

// ===== CUSTOM TIPS HELPERS =====

/// Create a diamond pattern: base -> (left, right) -> merge
pub async fn create_diamond_pattern(tree: &Database) -> DiamondIds {
    // Create base
    let txn_base = tree.new_transaction().await.unwrap();
    let base_store = txn_base.get_store::<DocStore>("data").await.unwrap();
    base_store.set("base", "initial").await.unwrap();
    let base_id = txn_base.commit().await.unwrap();

    // Create left branch
    let left_op = tree
        .new_transaction_with_tips(std::slice::from_ref(&base_id))
        .await
        .unwrap();
    let left_store = left_op.get_store::<DocStore>("data").await.unwrap();
    left_store.set("left", "left_value").await.unwrap();
    left_store.set("shared", "left_version").await.unwrap();
    let left_id = left_op.commit().await.unwrap();

    // Create right branch
    let right_op = tree
        .new_transaction_with_tips([base_id.clone()])
        .await
        .unwrap();
    let right_store = right_op.get_store::<DocStore>("data").await.unwrap();
    right_store.set("right", "right_value").await.unwrap();
    right_store.set("shared", "right_version").await.unwrap();
    let right_id = right_op.commit().await.unwrap();

    DiamondIds {
        base: base_id,
        left: left_id,
        right: right_id,
    }
}

/// IDs for diamond pattern testing
pub struct DiamondIds {
    pub base: ID,
    pub left: ID,
    pub right: ID,
}

/// Create a merge operation from diamond pattern
pub async fn create_merge_from_diamond(tree: &Database, diamond: &DiamondIds) -> ID {
    let merge_op = tree
        .new_transaction_with_tips([diamond.left.clone(), diamond.right.clone()])
        .await
        .unwrap();
    let merge_store = merge_op.get_store::<DocStore>("data").await.unwrap();
    merge_store.set("merged", "merge_value").await.unwrap();
    merge_op.commit().await.unwrap()
}

// ===== DATA VALIDATION HELPERS =====

/// Verify that a DocStore contains expected key-value pairs
pub async fn assert_dict_contains(dict: &DocStore, expected_data: &[(&str, &str)]) {
    for (key, expected_value) in expected_data {
        assert_dict_value(dict, key, expected_value).await;
    }
}

/// Get all data from a DocStore as a Map for detailed inspection
pub async fn get_dict_data(dict: &DocStore) -> Map {
    dict.get_all().await.unwrap()
}

/// Verify that all expected data exists in a Doc's Map
pub fn assert_map_data(map: &Map, expected_data: &[(&str, &str)]) {
    for (key, expected_value) in expected_data {
        match map.get(key) {
            Some(Value::Text(actual_value)) => {
                assert_eq!(
                    actual_value, *expected_value,
                    "Value mismatch for key '{key}'"
                );
            }
            Some(other) => panic!("Expected text value for key '{key}', got: {other:?}"),
            None => panic!("Key '{key}' not found in map"),
        }
    }
}

// ===== TOMBSTONE AND DELETE HELPERS =====

/// Create operation that deletes a key and verify tombstone behavior
pub async fn test_delete_operation(tree: &Database, subtree_name: &str, key_to_delete: &str) -> ID {
    let txn = tree.new_transaction().await.unwrap();
    let dict = txn.get_store::<DocStore>(subtree_name).await.unwrap();
    dict.delete(key_to_delete).await.unwrap();
    txn.commit().await.unwrap()
}

/// Verify that a map contains a tombstone for a deleted key
pub fn assert_has_tombstone(map: &Map, key: &str) {
    assert!(map.is_tombstone(key), "Expected tombstone for key '{key}'");
}

/// Verify that public API hides tombstone but internal API shows it
pub fn assert_tombstone_hidden(map: &Map, key: &str) {
    // Internal API should show tombstone
    assert_has_tombstone(map, key);

    // Public API should hide tombstone
    assert!(
        map.get(key).is_none(),
        "Public API should hide deleted key '{key}'"
    );
    assert!(
        !map.contains_key(key),
        "contains_key should return false for deleted key '{key}'"
    );
}

// ===== NESTED VALUE HELPERS =====

/// Create a nested Map value for testing complex data structures
pub fn create_nested_map(data: &[(&str, &str)]) -> Value {
    let mut map = Doc::new();
    for (key, value) in data {
        map.set(key, value.to_string());
    }
    Value::Doc(map)
}

/// Setup operation with nested Map values
pub async fn create_operation_with_nested_data(tree: &Database) -> ID {
    let txn = tree.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();

    // Set regular string value
    store.set("string_key", "string_value").await.unwrap();

    // Set nested map value
    let nested = create_nested_map(&[("inner1", "value1"), ("inner2", "value2")]);
    store.set_value("map_key", nested).await.unwrap();

    txn.commit().await.unwrap()
}

/// Verify nested data structure in a DocStore
pub async fn assert_nested_data(
    dict: &DocStore,
    string_key: &str,
    map_key: &str,
    nested_data: &[(&str, &str)],
) {
    // Check string value
    assert_dict_value(dict, string_key, "string_value").await;

    // Check nested map
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

// ===== PATH FINDING HELPERS =====

/// Create complex LCA scenario for path finding tests
pub async fn create_lca_test_scenario(tree: &Database) -> LcaTestIds {
    // Create LCA
    let lca_op = tree.new_transaction().await.unwrap();
    let lca_store = lca_op.get_store::<DocStore>("data").await.unwrap();
    lca_store.set("base", "LCA").await.unwrap();
    let lca_id = lca_op.commit().await.unwrap();

    // Create branch A
    let a_op = tree
        .new_transaction_with_tips(std::slice::from_ref(&lca_id))
        .await
        .unwrap();
    let a_store = a_op.get_store::<DocStore>("data").await.unwrap();
    a_store.set("branch_a", "modification_A").await.unwrap();
    let a_id = a_op.commit().await.unwrap();

    // Create branch B (parallel to A)
    let b_op = tree
        .new_transaction_with_tips(std::slice::from_ref(&lca_id))
        .await
        .unwrap();
    let b_store = b_op.get_store::<DocStore>("data").await.unwrap();
    b_store.set("branch_b", "modification_B").await.unwrap();
    let b_id = b_op.commit().await.unwrap();

    // Create merge tip
    let merge_op = tree
        .new_transaction_with_tips([a_id.clone(), b_id.clone()])
        .await
        .unwrap();
    let merge_store = merge_op.get_store::<DocStore>("data").await.unwrap();
    merge_store.set("tip", "merged").await.unwrap();
    let merge_id = merge_op.commit().await.unwrap();

    // Create independent tip
    let indep_op = tree
        .new_transaction_with_tips([lca_id.clone()])
        .await
        .unwrap();
    let indep_store = indep_op.get_store::<DocStore>("data").await.unwrap();
    indep_store.set("independent", "tip").await.unwrap();
    let indep_id = indep_op.commit().await.unwrap();

    LcaTestIds {
        merge_tip: merge_id,
        independent_tip: indep_id,
    }
}

/// IDs for LCA and path finding testing
pub struct LcaTestIds {
    pub merge_tip: ID,
    pub independent_tip: ID,
}

/// Verify that LCA path finding includes all expected data
pub async fn assert_lca_path_completeness(tree: &Database, tips: &[ID], expected_keys: &[&str]) {
    let txn = tree.new_transaction_with_tips(tips).await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    let state = store.get_all().await.unwrap();

    for key in expected_keys {
        assert!(
            state.get(key).is_some(),
            "Should have key '{key}' from LCA path finding"
        );
    }
}

// ===== OPERATION LIFECYCLE HELPERS =====

/// Test deterministic operation ordering
pub async fn test_deterministic_operations(tree: &Database, tips: &[ID], iterations: usize) {
    let mut results = Vec::new();

    for _i in 0..iterations {
        let txn = tree.new_transaction_with_tips(tips).await.unwrap();
        let store = txn.get_store::<DocStore>("data").await.unwrap();
        let state = store.get_all().await.unwrap();
        results.push(state);
    }

    // All results should be identical
    for (i, result) in results.iter().enumerate().skip(1) {
        assert_eq!(
            &results[0], result,
            "Operation {i} produced different result than operation 0"
        );
    }
}
