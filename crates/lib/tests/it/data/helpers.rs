//! Helper functions for data module tests
//!
//! This module provides utility functions for testing CRDT Doc operations,
//! value editors, path operations, and merge scenarios.

use eidetica::{
    Database, Instance, Result, Transaction,
    crdt::{Doc, doc::Value},
    store::DocStore,
};

use crate::helpers::test_instance;
pub use crate::helpers::{
    assert_nested_value, assert_path_deleted, assert_text_value, create_complex_nested_structure,
};

// Type alias for local usage
type Node = Doc;

// ===== BASIC SETUP HELPERS =====

/// Create a database with a test key and return both Instance and tree
pub async fn setup_db_and_tree() -> Result<(Instance, Database)> {
    let instance = test_instance().await;
    crate::helpers::create_account(&instance, "test_user", None).await?;
    let mut user = instance.login_account("test_user", None).await?;
    let default_key = user.get_default_key()?;

    let mut settings = Doc::new();
    settings.set("name", "test_tree");

    let tree = user.create_database(settings, &default_key).await?;
    Ok((instance, tree))
}

/// Setup a Doc subtree for testing
pub async fn setup_dict_subtree(txn: &Transaction, subtree_name: &str) -> Result<DocStore> {
    txn.get_store::<DocStore>(subtree_name).await
}

/// Create a complete test environment with DB, tree, operation, and Doc
pub async fn setup_complete_test_env(
    subtree_name: &str,
) -> Result<(Instance, Database, Transaction, DocStore)> {
    let (db, tree) = setup_db_and_tree().await?;
    let txn = tree.new_transaction().await?;
    let dict = setup_dict_subtree(&txn, subtree_name).await?;
    Ok((db, tree, txn, dict))
}

// ===== MAP CREATION HELPERS =====

// ===== ASSERTION HELPERS =====

/// Create a Map with mixed value types
pub fn create_mixed_map() -> Node {
    let mut map = Doc::new();
    map.set("string_val", "test_string");

    let mut nested = Doc::new();
    nested.set("nested_key", "nested_value");
    map.set("map_val", nested);

    // Create a tombstone
    map.remove("deleted_val");

    map
}

/// Test serialization roundtrip for a Node
pub fn assert_serialization_roundtrip(map: &Node) -> Result<Doc> {
    let serialized = serde_json::to_string(map).expect("Serialization failed");
    let deserialized: Doc = serde_json::from_str(&serialized).expect("Deserialization failed");

    // Compare using the simplified API
    assert_eq!(
        map.len(),
        deserialized.len(),
        "Serialization changed map size"
    );

    for (key, value) in map.iter() {
        assert_eq!(
            deserialized.get(key),
            Some(value),
            "Serialization changed value for key '{key}'"
        );
    }

    Ok(deserialized)
}

/// Assert that a value is a Map with expected content
pub fn assert_doc_value_has_keys(value: &Value, expected_keys: &[&str]) {
    match value {
        Value::Doc(map) => {
            for &key in expected_keys {
                assert!(map.contains_key(key), "Map should contain key '{key}'");
            }
        }
        _ => panic!("Expected Map value, got {value:?}"),
    }
}

// ===== MERGE TESTING HELPERS =====

// ===== VALUE EDITOR HELPERS =====

/// Setup a Doc for path operation tests
pub async fn setup_path_test_dict(txn: &Transaction) -> Result<DocStore> {
    setup_dict_subtree(txn, "path_test_store").await
}

/// Test value editor basic functionality
pub async fn assert_editor_set_get(dict: &DocStore, key: &str, value: Value) -> Result<()> {
    let editor = dict.get_value_mut(key);
    editor.set(value.clone()).await?;

    let retrieved = editor.get().await?;
    assert_eq!(retrieved, value, "Editor set/get mismatch for key '{key}'");

    Ok(())
}

/// Test nested editor operations
pub async fn assert_nested_editor_set_get(
    dict: &DocStore,
    path: &[&str],
    value: Value,
) -> Result<()> {
    // Navigate to the target path using chained editors
    let mut editor = dict.get_value_mut(path[0]);
    for &segment in &path[1..] {
        editor = editor.get_value_mut(segment);
    }

    editor.set(value.clone()).await?;
    let retrieved = editor.get().await?;
    assert_eq!(
        retrieved, value,
        "Nested editor set/get mismatch at path {path:?}"
    );

    Ok(())
}

/// Test path-based operations
pub async fn assert_path_set_get(dict: &DocStore, path: &[&str], value: Value) -> Result<()> {
    dict.set_at_path(path, value.clone()).await?;
    let retrieved = dict.get_at_path(path).await?;
    assert_eq!(retrieved, value, "Path operation mismatch at {path:?}");

    Ok(())
}

// ===== SERIALIZATION HELPERS =====

// ===== ERROR TESTING HELPERS =====

/// Test that an operation fails with a specific error type
pub fn assert_error_type<T, E: std::fmt::Debug>(
    result: Result<T, E>,
    check_fn: fn(&E) -> bool,
    error_description: &str,
) {
    match result {
        Ok(_) => panic!("Expected {error_description}, but operation succeeded"),
        Err(e) => assert!(check_fn(&e), "Expected {error_description}, got {e:?}"),
    }
}

/// Test that a not found error occurs
pub fn assert_not_found_error<T>(result: Result<T>) {
    assert_error_type(result, |e| e.is_not_found(), "NotFound error");
}

/// Test that a type error occurs
pub fn assert_type_error<T>(result: Result<T>) {
    assert_error_type(result, |e| e.is_type_error(), "Type error");
}

// ===== TEST DATA BUILDERS =====

// ===== MACROS =====

/// Macro for creating test value assertions
#[macro_export]
macro_rules! assert_value_eq {
    ($actual:expr, text: $expected:expr) => {
        match $actual {
            Value::Text(s) => assert_eq!(s, $expected),
            other => panic!("Expected text value '{}', got {:?}", $expected, other),
        }
    };
    ($actual:expr, map: $expected_keys:expr) => {
        match $actual {
            Value::Map(map) => {
                for key in $expected_keys {
                    assert!(map.contains_key(key), "Map should contain key '{}'", key);
                }
            }
            other => panic!("Expected map value, got {:?}", other),
        }
    };
    ($actual:expr, deleted) => {
        match $actual {
            Value::Deleted => (),
            other => panic!("Expected deleted value, got {:?}", other),
        }
    };
}
