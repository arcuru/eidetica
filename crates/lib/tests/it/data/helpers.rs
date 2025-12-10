//! Helper functions for data module tests
//!
//! This module provides utility functions for testing CRDT Doc operations,
//! value editors, path operations, and merge scenarios.

use eidetica::{
    Database, Instance, Transaction,
    backend::database::InMemory,
    crdt::{Doc, doc::Value},
    store::DocStore,
};

// Type alias for local usage
type Node = Doc;

// ===== BASIC SETUP HELPERS =====

/// Create a database with a test key and return both Instance and tree
pub fn setup_db_and_tree() -> eidetica::Result<(Instance, Database)> {
    let instance = Instance::open(Box::new(InMemory::new()))?;
    instance.create_user("test_user", None)?;
    let mut user = instance.login_user("test_user", None)?;
    let default_key = user.get_default_key()?;

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "test_tree");

    let tree = user.create_database(settings, &default_key)?;
    Ok((instance, tree))
}

/// Setup a Doc subtree for testing
pub fn setup_dict_subtree(op: &Transaction, subtree_name: &str) -> eidetica::Result<DocStore> {
    op.get_store::<DocStore>(subtree_name)
}

/// Create a complete test environment with DB, tree, operation, and Doc
pub fn setup_complete_test_env(
    subtree_name: &str,
) -> eidetica::Result<(Instance, Database, Transaction, DocStore)> {
    let (db, tree) = setup_db_and_tree()?;
    let op = tree.new_transaction()?;
    let dict = setup_dict_subtree(&op, subtree_name)?;
    Ok((db, tree, op, dict))
}

// ===== MAP CREATION HELPERS =====

// ===== ASSERTION HELPERS =====

/// Assert that a string value matches expected content
pub fn assert_text_value(value: &Value, expected: &str) {
    match value {
        Value::Text(actual) => assert_eq!(actual, expected),
        _ => panic!("Expected text value '{expected}', got {value:?}"),
    }
}

/// Assert that a nested value matches expected string
pub fn assert_nested_value(map: &Node, path: &[&str], expected: &str) {
    let mut current = map;

    // Navigate to the parent of the final key
    for &key in &path[..path.len() - 1] {
        match current.get(key) {
            Some(Value::Doc(inner)) => current = inner,
            _ => panic!("Expected map at path segment '{key}' in path {path:?}"),
        }
    }

    // Check the final value
    let final_key = path.last().expect("Path should not be empty");
    match current.get(final_key) {
        Some(Value::Text(s)) => assert_eq!(s, expected, "Value mismatch at path {path:?}"),
        Some(other) => panic!("Expected text value at path {path:?}, got {other:?}"),
        None => panic!("No value found at path {path:?}"),
    }
}

/// Create a complex nested structure for testing
pub fn create_complex_nested_structure() -> Node {
    let mut root = Doc::new();

    // Level 1
    root.set("top_key", "top_value");

    // Level 2
    let mut level2 = Doc::new();
    level2.set("level2_key1", "level2_value1");
    level2.set("shared_key", "original_value");

    // Level 3
    let mut level3 = Doc::new();
    level3.set("level3_key1", "level3_value1");
    level2.set("level3", level3);

    root.set("level2", level2);
    root
}

/// Assert that a path is deleted (tombstone exists)
pub fn assert_path_deleted(map: &Node, path: &[&str]) {
    if path.len() == 1 {
        // Simple case: check directly in this map using is_tombstone
        assert!(
            map.is_tombstone(path[0]),
            "Expected tombstone at '{path:?}'"
        );
    } else {
        // Navigate to parent and check final key
        let mut current = map;
        for &key in &path[..path.len() - 1] {
            match current.get(key) {
                Some(Value::Doc(inner)) => current = inner,
                _ => panic!("Expected map at path segment '{key}' in path {path:?}"),
            }
        }

        let final_key = path.last().expect("Path should not be empty");
        assert!(
            current.is_tombstone(*final_key),
            "Expected tombstone at '{path:?}'"
        );
    }
}

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
pub fn test_serialization_roundtrip(map: &Node) -> eidetica::Result<()> {
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

    Ok(())
}

/// Assert that a value is a Map with expected content
pub fn assert_map_contains(value: &Value, expected_keys: &[&str]) {
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
pub fn setup_path_test_dict(op: &Transaction) -> eidetica::Result<DocStore> {
    setup_dict_subtree(op, "path_test_store")
}

/// Test value editor basic functionality
pub fn test_editor_basic_set_get(dict: &DocStore, key: &str, value: Value) -> eidetica::Result<()> {
    let editor = dict.get_value_mut(key);
    editor.set(value.clone())?;

    let retrieved = editor.get()?;
    assert_eq!(retrieved, value, "Editor set/get mismatch for key '{key}'");

    Ok(())
}

/// Test nested editor operations
pub fn test_nested_editor_operations(
    dict: &DocStore,
    path: &[&str],
    value: Value,
) -> eidetica::Result<()> {
    // Navigate to the target path using chained editors
    let mut editor = dict.get_value_mut(path[0]);
    for &segment in &path[1..] {
        editor = editor.get_value_mut(segment);
    }

    editor.set(value.clone())?;
    let retrieved = editor.get()?;
    assert_eq!(
        retrieved, value,
        "Nested editor set/get mismatch at path {path:?}"
    );

    Ok(())
}

/// Test path-based operations
pub fn test_path_operations(dict: &DocStore, path: &[&str], value: Value) -> eidetica::Result<()> {
    dict.set_at_path(path, value.clone())?;
    let retrieved = dict.get_at_path(path)?;
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
pub fn assert_not_found_error<T>(result: eidetica::Result<T>) {
    assert_error_type(result, |e| e.is_not_found(), "NotFound error");
}

/// Test that a type error occurs
pub fn assert_type_error<T>(result: eidetica::Result<T>) {
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
