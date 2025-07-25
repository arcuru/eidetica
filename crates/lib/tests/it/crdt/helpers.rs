//! Comprehensive helper functions for CRDT testing
//!
//! This module provides utilities for testing Map, List, Value, and serialization
//! functionality of the CRDT system.

// Helper functions are self-contained and don't need external imports
use eidetica::crdt::map::list::Position;
use eidetica::crdt::map::{List, Value};
use eidetica::crdt::{CRDT, Map};

// ===== MAP HELPERS =====

/// Create a Map with string key-value pairs
pub fn create_map_with_values(pairs: &[(&str, &str)]) -> Map {
    let mut map = Map::new();
    for (key, value) in pairs {
        map.set_string(*key, *value);
    }
    map
}

/// Create a nested Map structure with multiple levels
pub fn create_nested_map(nested_data: &[(&str, &[(&str, &str)])]) -> Map {
    let mut map = Map::new();
    for (outer_key, inner_pairs) in nested_data {
        let inner_map = create_map_with_values(inner_pairs);
        map.set_map(*outer_key, inner_map);
    }
    map
}

/// Create two maps for merge testing with specified overlap
pub fn create_merge_test_maps(
    map1_data: &[(&str, &str)],
    map2_data: &[(&str, &str)],
) -> (Map, Map) {
    (
        create_map_with_values(map1_data),
        create_map_with_values(map2_data),
    )
}

/// Test merge operation and verify expected results
pub fn test_merge_result(
    map1: &Map,
    map2: &Map,
    expected_values: &[(&str, &str)],
) -> eidetica::Result<Map> {
    let merged = map1.merge(map2)?;

    for (key, expected_value) in expected_values {
        assert_text_value(
            merged.get(key).expect("Key should exist after merge"),
            expected_value,
        );
    }

    Ok(merged)
}

/// Assert that a string value matches expected content
pub fn assert_text_value(value: &Value, expected: &str) {
    match value {
        Value::Text(actual) => assert_eq!(actual, expected),
        _ => panic!("Expected text value '{expected}', got {value:?}"),
    }
}

/// Assert that a nested value matches expected string
pub fn assert_nested_value(map: &Map, path: &[&str], expected: &str) {
    let mut current = map;

    // Navigate to the parent of the final key
    for &key in &path[..path.len() - 1] {
        match current.get(key) {
            Some(Value::Map(inner)) => current = inner,
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

/// Assert that a path is deleted (tombstone exists)
pub fn assert_path_deleted(map: &Map, path: &[&str]) {
    if path.len() == 1 {
        // Simple case: check directly in this map
        match map.as_hashmap().get(&path[0].to_string()) {
            Some(Value::Deleted) => (),
            Some(other) => panic!("Expected tombstone at '{path:?}', got {other:?}"),
            None => panic!("Expected tombstone at '{path:?}', but key not found"),
        }
    } else {
        // Navigate to parent and check final key
        let mut current = map;
        for &key in &path[..path.len() - 1] {
            match current.get(key) {
                Some(Value::Map(inner)) => current = inner,
                _ => panic!("Expected map at path segment '{key}' in path {path:?}"),
            }
        }

        let final_key = path.last().expect("Path should not be empty");
        match current.as_hashmap().get(&final_key.to_string()) {
            Some(Value::Deleted) => (),
            Some(other) => panic!("Expected tombstone at '{path:?}', got {other:?}"),
            None => panic!("Expected tombstone at '{path:?}', but key not found"),
        }
    }
}

/// Create a complex nested structure for testing
pub fn create_complex_nested_structure() -> Map {
    let mut root = Map::new();

    // Level 1
    root.set_string("top_key", "top_value");

    // Level 2
    let mut level2 = Map::new();
    level2.set_string("level2_key1", "level2_value1");
    level2.set_string("shared_key", "original_value");

    // Level 3
    let mut level3 = Map::new();
    level3.set_string("level3_key1", "level3_value1");
    level2.set_map("level3", level3);

    root.set_map("level2", level2);
    root
}

/// Build test data for multi-generation update scenarios
pub fn build_generation_test_data() -> Vec<(&'static str, Value)> {
    vec![
        ("gen1", Value::Text("original".to_string())),
        ("gen2", Value::Text("updated".to_string())),
        ("gen3", Value::Deleted),
        ("gen4", Value::Text("resurrected".to_string())),
    ]
}

/// Build complex merge scenario data
pub fn build_complex_merge_data() -> (Map, Map) {
    let mut map1 = Map::new();
    let mut level1a = Map::new();
    level1a.set_string("key1", "value1");
    level1a.set_string("to_delete", "will_be_deleted");
    level1a.set_string("to_update", "initial_value");
    map1.set_map("level1", level1a);
    map1.set_string("top_level_key", "top_value");

    let mut map2 = Map::new();
    let mut level1b = Map::new();
    level1b.set_string("key2", "value2");
    level1b.remove("to_delete");
    level1b.set_string("to_update", "updated_value");
    map2.set_map("level1", level1b);
    map2.remove("top_level_key");
    map2.set_string("new_top_key", "new_top_value");

    (map1, map2)
}

/// Create a test Map with some initial data
pub fn setup_test_map() -> Map {
    let mut map = Map::new();
    map.set_string("key1".to_string(), "value1".to_string());
    map.set_string("key2".to_string(), "value2".to_string());
    map
}

/// Create two concurrent Maps with different modifications
pub fn setup_concurrent_maps() -> (Map, Map) {
    let base = setup_test_map();

    let mut map1 = base.clone();
    map1.set_string("branch".to_string(), "left".to_string());
    map1.set_string("unique1".to_string(), "from_map1".to_string());

    let mut map2 = base.clone();
    map2.set_string("branch".to_string(), "right".to_string());
    map2.set_string("unique2".to_string(), "from_map2".to_string());

    (map1, map2)
}

/// Create a complex nested Map structure for testing
pub fn create_complex_map() -> Map {
    let mut map = Map::new();

    // Add basic values
    map.set_string("title".to_string(), "My Document".to_string());
    map.set("priority".to_string(), Value::Int(42));
    map.set("published".to_string(), Value::Bool(true));

    // Add nested map
    let mut metadata = Map::new();
    metadata.set_string("author".to_string(), "Alice".to_string());
    metadata.set_string("version".to_string(), "1.0".to_string());
    map.set("metadata".to_string(), Value::Map(metadata));

    // Add list
    let mut tags = List::new();
    tags.push(Value::Text("important".to_string()));
    tags.push(Value::Text("draft".to_string()));
    map.set("tags".to_string(), Value::List(tags));

    map
}

/// Create a Map with mixed value types for comprehensive testing
pub fn create_mixed_value_map() -> Map {
    let mut map = Map::new();
    map.set("null_val".to_string(), Value::Null);
    map.set("bool_val".to_string(), Value::Bool(true));
    map.set("int_val".to_string(), Value::Int(123));
    map.set("text_val".to_string(), Value::Text("hello".to_string()));
    map.set("map_val".to_string(), Value::Map(Map::new()));
    map.set("list_val".to_string(), Value::List(List::new()));
    map.set("deleted_val".to_string(), Value::Deleted);
    map
}

// ===== LIST HELPERS =====

/// Create a test List with sample data
pub fn setup_test_list() -> List {
    let mut list = List::new();
    list.push(Value::Text("first".to_string()));
    list.push(Value::Text("second".to_string()));
    list.push(Value::Text("third".to_string()));
    list
}

/// Create a List with mixed value types
pub fn create_mixed_list() -> List {
    let mut list = List::new();
    list.push(Value::Null);
    list.push(Value::Bool(false));
    list.push(Value::Int(456));
    list.push(Value::Text("mixed".to_string()));

    let mut nested_map = Map::new();
    nested_map.set_string("nested".to_string(), "value".to_string());
    list.push(Value::Map(nested_map));

    list
}

/// Create a List with positions for testing insertion scenarios
pub fn create_positioned_list() -> (List, Vec<Position>) {
    let mut list = List::new();
    let mut positions = Vec::new();

    // Create specific positions for testing
    let pos1 = Position::new(10, 1);
    let pos2 = Position::new(20, 1);
    let pos3 = Position::new(30, 1);

    list.insert_at_position(pos1.clone(), Value::Text("A".to_string()));
    list.insert_at_position(pos2.clone(), Value::Text("C".to_string()));
    list.insert_at_position(pos3.clone(), Value::Text("E".to_string()));

    positions.extend([pos1, pos2, pos3]);
    (list, positions)
}

// ===== VALUE HELPERS =====

/// Create all basic Value types for testing
pub fn create_all_value_types() -> Vec<Value> {
    vec![
        Value::Null,
        Value::Bool(true),
        Value::Bool(false),
        Value::Int(42),
        Value::Int(-123),
        Value::Text("test".to_string()),
        Value::Text("".to_string()),
        Value::Map(Map::new()),
        Value::List(List::new()),
        Value::Deleted,
    ]
}

/// Create sample Values for merge testing
pub fn create_merge_test_values() -> (Value, Value) {
    let text1 = Value::Text("original".to_string());
    let text2 = Value::Text("updated".to_string());
    (text1, text2)
}

// ===== ASSERTION HELPERS =====

/// Assert that a Map contains expected key-value pairs
pub fn assert_map_contains(map: &Map, expected: &[(&str, &str)]) {
    for (key, expected_value) in expected {
        match map.get(key) {
            Some(Value::Text(actual_value)) => {
                assert_eq!(
                    actual_value, expected_value,
                    "Value mismatch for key '{key}'"
                );
            }
            Some(other) => panic!("Expected text value for key '{key}', got: {other:?}"),
            None => panic!("Key '{key}' not found in map"),
        }
    }
}

/// Assert that a Value is of expected type and content
pub fn assert_value_content(value: &Value, expected_type: &str, test_equality: Option<&Value>) {
    assert_eq!(value.type_name(), expected_type, "Value type mismatch");

    if let Some(expected) = test_equality {
        assert_eq!(value, expected, "Value content mismatch");
    }
}

/// Assert that two Maps are equivalent (same keys and values)
pub fn assert_maps_equivalent(map1: &Map, map2: &Map) {
    let hashmap1 = map1.as_hashmap();
    let hashmap2 = map2.as_hashmap();

    assert_eq!(hashmap1.len(), hashmap2.len(), "Maps have different sizes");

    for (key, value1) in hashmap1 {
        match hashmap2.get(key) {
            Some(value2) => assert_eq!(value1, value2, "Value mismatch for key '{key}'"),
            None => panic!("Key '{key}' missing in second map"),
        }
    }
}

// ===== MERGE TESTING HELPERS =====

/// Test CRDT merge commutativity: A ⊕ B = B ⊕ A
pub fn test_merge_commutativity<T: CRDT + PartialEq + std::fmt::Debug>(
    a: &T,
    b: &T,
) -> eidetica::Result<()> {
    let merge_ab = a.merge(b)?;
    let merge_ba = b.merge(a)?;
    assert_eq!(merge_ab, merge_ba, "Merge is not commutative");
    Ok(())
}

/// Test CRDT merge associativity: (A ⊕ B) ⊕ C = A ⊕ (B ⊕ C)
pub fn test_merge_associativity<T: CRDT + PartialEq + std::fmt::Debug>(
    a: &T,
    b: &T,
    c: &T,
) -> eidetica::Result<()> {
    let left_assoc = a.merge(b)?.merge(c)?;
    let right_assoc = a.merge(&b.merge(c)?)?;
    assert_eq!(left_assoc, right_assoc, "Merge is not associative");
    Ok(())
}

/// Test CRDT merge idempotency: A ⊕ A = A
pub fn test_merge_idempotency<T: CRDT + PartialEq + std::fmt::Debug>(
    a: &T,
) -> eidetica::Result<()> {
    let merged = a.merge(a)?;
    assert_eq!(*a, merged, "Merge is not idempotent");
    Ok(())
}

// ===== SERIALIZATION HELPERS =====

/// Test JSON serialization roundtrip for any serializable type
pub fn test_json_roundtrip<T>(value: &T) -> eidetica::Result<T>
where
    T: serde::Serialize + for<'de> serde::Deserialize<'de> + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(value).expect("Serialization should succeed");
    let deserialized: T = serde_json::from_str(&json).expect("Deserialization should succeed");
    assert_eq!(*value, deserialized, "Roundtrip serialization failed");
    Ok(deserialized)
}

// ===== ERROR TESTING HELPERS =====

/// Test that list index operations handle bounds correctly
pub fn test_list_bounds_checking(list: &List) {
    let len = list.len();

    // Valid indices should work
    if len > 0 {
        assert!(list.get(0).is_some(), "Index 0 should be valid");
        assert!(list.get(len - 1).is_some(), "Last index should be valid");
    }

    // Invalid indices should return None
    assert!(list.get(len).is_none(), "Index == length should be invalid");
    assert!(
        list.get(len + 100).is_none(),
        "Large index should be invalid"
    );
}

// ===== PERFORMANCE HELPERS =====

/// Create a large Map for performance testing
pub fn create_large_map(size: usize) -> Map {
    let mut map = Map::new();
    for i in 0..size {
        map.set(format!("key_{i}"), Value::Text(format!("value_{i}")));
    }
    map
}

/// Create a large List for performance testing  
pub fn create_large_list(size: usize) -> List {
    let mut list = List::new();
    for i in 0..size {
        list.push(Value::Text(format!("item_{i}")));
    }
    list
}
