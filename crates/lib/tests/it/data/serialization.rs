//! Tests for CRDT Doc serialization and deserialization
//!
//! This module tests the serialization behavior of Doc structures,
//! including handling of nested documents and tombstones.

use eidetica::crdt::{Doc, doc::Value};

// Type alias for local usage

use super::helpers::*;

#[tokio::test]
async fn test_doc_serialization() {
    // Test serialization and deserialization of Doc
    let mut map = eidetica::crdt::Doc::new();

    // Add various value types
    map.set("string_key", "string_value");

    let mut nested = eidetica::crdt::Doc::new();
    nested.set("inner", "inner_value");
    map.set("map_key", nested);

    // Create a tombstone
    map.remove("deleted_key");

    // Test serialization roundtrip
    test_serialization_roundtrip(&map).expect("Serialization roundtrip failed");

    // Verify specific values survived serialization
    let serialized = serde_json::to_string(&map).expect("Serialization failed");
    let deserialized: Doc = serde_json::from_str(&serialized).expect("Deserialization failed");

    // Verify string survived
    assert_text_value(deserialized.get("string_key").unwrap(), "string_value");

    // Verify nested map survived
    match deserialized.get("map_key").unwrap() {
        Value::Doc(m) => assert_text_value(m.get("inner").unwrap(), "inner_value"),
        _ => panic!("Expected map value"),
    }

    // Verify tombstone survived - use is_tombstone (contains_key returns false for tombstones)
    assert!(deserialized.is_tombstone("deleted_key"));
    assert_path_deleted(&deserialized, &["deleted_key"]);
}

#[tokio::test]
async fn test_serialization_complex_nested_structure() {
    let complex_map = create_complex_nested_structure();

    // Test roundtrip on complex structure
    test_serialization_roundtrip(&complex_map).expect("Complex serialization failed");

    // Verify structure integrity after serialization
    let serialized = serde_json::to_string(&complex_map).expect("Serialization failed");
    let deserialized: Doc = serde_json::from_str(&serialized).expect("Deserialization failed");

    // Verify nested structure preserved
    assert_nested_value(
        &deserialized,
        &["level2", "level3", "level3_key1"],
        "level3_value1",
    );
    assert_text_value(deserialized.get("top_key").unwrap(), "top_value");
}

#[tokio::test]
async fn test_serialization_mixed_doc() {
    let mixed_map = create_mixed_map();

    // Test roundtrip
    test_serialization_roundtrip(&mixed_map).expect("Mixed doc serialization failed");

    // Verify all types preserved
    let serialized = serde_json::to_string(&mixed_map).expect("Serialization failed");
    let deserialized: Doc = serde_json::from_str(&serialized).expect("Deserialization failed");

    // Check string value
    assert_text_value(deserialized.get("string_val").unwrap(), "test_string");

    // Check nested map
    match deserialized.get("map_val").unwrap() {
        Value::Doc(nested) => assert_text_value(nested.get("nested_key").unwrap(), "nested_value"),
        _ => panic!("Expected nested map"),
    }

    // Check tombstone
    assert_path_deleted(&deserialized, &["deleted_val"]);
}

#[tokio::test]
async fn test_serialization_empty_doc() {
    let empty_map = eidetica::crdt::Doc::new();

    test_serialization_roundtrip(&empty_map).expect("Empty doc serialization failed");

    let serialized = serde_json::to_string(&empty_map).expect("Serialization failed");
    let deserialized: Doc = serde_json::from_str(&serialized).expect("Deserialization failed");

    assert_eq!(deserialized.len(), 0, "Empty map should remain empty");
}

#[tokio::test]
async fn test_serialization_tombstone_only_doc() {
    let mut tombstone_map = eidetica::crdt::Doc::new();
    tombstone_map.remove("tombstone1");
    tombstone_map.remove("tombstone2");
    tombstone_map.set("direct_tombstone", Value::Deleted);

    test_serialization_roundtrip(&tombstone_map).expect("Tombstone-only doc serialization failed");

    let serialized = serde_json::to_string(&tombstone_map).expect("Serialization failed");
    let deserialized: Doc = serde_json::from_str(&serialized).expect("Deserialization failed");

    // Verify all tombstones preserved
    assert_path_deleted(&deserialized, &["tombstone1"]);
    assert_path_deleted(&deserialized, &["tombstone2"]);
    assert_path_deleted(&deserialized, &["direct_tombstone"]);

    // Verify no accessible values
    assert_eq!(deserialized.get("tombstone1"), None);
    assert_eq!(deserialized.get("tombstone2"), None);
    assert_eq!(deserialized.get("direct_tombstone"), None);
}
