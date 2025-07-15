//! Serialization tests for CRDT types
//!
//! This module contains tests specifically focused on JSON serialization,
//! serde functionality, and serialization/deserialization behavior for
//! Map, List, and Value types.

use crate::crdt::helpers::*;
use eidetica::crdt::Map;
use eidetica::crdt::map::list::Position;
use eidetica::crdt::map::{List, Value};

// ===== JSON STRING SERIALIZATION TESTS =====

#[test]
fn test_map_to_json_string_basic() {
    let mut map = Map::new();
    map.set("name", "Alice");
    map.set("age", 30);
    map.set("active", true);

    let json = map.to_json_string();

    // Parse as JSON to verify validity
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    // Verify content (order may vary in HashMap)
    assert_eq!(parsed["name"], "Alice");
    assert_eq!(parsed["age"], 30);
    assert_eq!(parsed["active"], true);
}

#[test]
fn test_map_to_json_string_empty() {
    let map = Map::new();
    assert_eq!(map.to_json_string(), "{}");
}

#[test]
fn test_map_to_json_string_nested() {
    let mut inner_map = Map::new();
    inner_map.set("city", "NYC");
    inner_map.set("zip", 10001);

    let mut outer_map = Map::new();
    outer_map.set("name", "Alice");
    outer_map.set("address", inner_map);

    let json = outer_map.to_json_string();

    // Parse as JSON to verify validity and structure
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed["name"], "Alice");
    assert_eq!(parsed["address"]["city"], "NYC");
    assert_eq!(parsed["address"]["zip"], 10001);
}

#[test]
fn test_list_to_json_string_via_value() {
    let mut list = List::new();
    list.push("first");
    list.push(42);
    list.push(true);

    let list_value = Value::List(list);
    let json = list_value.to_json_string();

    // Parse as JSON to verify validity
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed[0], "first");
    assert_eq!(parsed[1], 42);
    assert_eq!(parsed[2], true);
}

#[test]
fn test_list_to_json_string_empty() {
    let list = List::new();
    let list_value = Value::List(list);
    assert_eq!(list_value.to_json_string(), "[]");
}

#[test]
fn test_list_to_json_string_nested() {
    let mut inner_list = List::new();
    inner_list.push(1);
    inner_list.push(2);

    let mut outer_list = List::new();
    outer_list.push("start");
    outer_list.push(Value::List(inner_list));
    outer_list.push("end");

    let list_value = Value::List(outer_list);
    let json = list_value.to_json_string();

    // Parse as JSON to verify validity and structure
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed[0], "start");
    assert_eq!(parsed[1][0], 1);
    assert_eq!(parsed[1][1], 2);
    assert_eq!(parsed[2], "end");
}

#[test]
fn test_complex_nested_structure_to_json() {
    let mut users_list = List::new();

    // Create first user
    let mut user1 = Map::new();
    user1.set("name", "Alice");
    user1.set("age", 30);

    let mut tags1 = List::new();
    tags1.push("developer");
    tags1.push("rust");
    user1.set("tags", tags1);

    // Create second user
    let mut user2 = Map::new();
    user2.set("name", "Bob");
    user2.set("age", 25);

    let mut tags2 = List::new();
    tags2.push("designer");
    user2.set("tags", tags2);

    users_list.push(Value::Map(user1));
    users_list.push(Value::Map(user2));

    // Create root structure
    let mut root = Map::new();
    root.set("users", Value::List(users_list));
    root.set("total", 2);

    let json = root.to_json_string();

    // Parse as JSON to verify validity and structure
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed["total"], 2);
    assert_eq!(parsed["users"][0]["name"], "Alice");
    assert_eq!(parsed["users"][0]["age"], 30);
    assert_eq!(parsed["users"][0]["tags"][0], "developer");
    assert_eq!(parsed["users"][0]["tags"][1], "rust");
    assert_eq!(parsed["users"][1]["name"], "Bob");
    assert_eq!(parsed["users"][1]["age"], 25);
    assert_eq!(parsed["users"][1]["tags"][0], "designer");
}

#[test]
fn test_to_json_string_with_tombstones() {
    let mut map = Map::new();
    map.set("name", "Alice");
    map.set("age", 30);
    map.set("temp", "delete_me");

    // Remove a key (creates tombstone)
    map.remove("temp");

    let json = map.to_json_string();

    // Parse as JSON - tombstones should not appear in output
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed["name"], "Alice");
    assert_eq!(parsed["age"], 30);
    assert!(!parsed.as_object().unwrap().contains_key("temp"));

    // Test deleted value directly
    assert_eq!(Value::Deleted.to_json_string(), "null");
}

#[test]
fn test_to_json_string_with_list_tombstones() {
    let mut list = List::new();
    list.push("keep1");
    list.push("remove_me");
    list.push("keep2");

    // Remove middle element (creates tombstone)
    list.remove(1);

    let list_value = Value::List(list);
    let json = list_value.to_json_string();

    // Parse as JSON - should only contain non-tombstone elements
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    // Only 2 elements should remain
    assert_eq!(parsed.as_array().unwrap().len(), 2);
    assert_eq!(parsed[0], "keep1");
    assert_eq!(parsed[1], "keep2");
}

#[test]
fn test_json_round_trip_validation() {
    // Test that our JSON output is valid and can be parsed
    let mut map = Map::new();
    map.set("text", "hello \"world\"");
    map.set("number", 42);
    map.set("boolean", true);
    map.set("null_val", Value::Null);

    let mut inner_list = List::new();
    inner_list.push(1);
    inner_list.push("test");
    inner_list.push(false);

    map.set("list", Value::List(inner_list));

    let json = map.to_json_string();

    // Should be valid JSON
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    // Verify all types are preserved correctly
    assert_eq!(parsed["text"], "hello \"world\"");
    assert_eq!(parsed["number"], 42);
    assert_eq!(parsed["boolean"], true);
    assert_eq!(parsed["null_val"], serde_json::Value::Null);
    assert_eq!(parsed["list"][0], 1);
    assert_eq!(parsed["list"][1], "test");
    assert_eq!(parsed["list"][2], false);
}

#[test]
fn test_map_to_json_string_key_ordering() {
    let mut map = Map::new();
    map.set("zebra", 1);
    map.set("alpha", 2);
    map.set("beta", 3);

    let json = map.to_json_string();

    // Should be valid JSON regardless of key order
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed["zebra"], 1);
    assert_eq!(parsed["alpha"], 2);
    assert_eq!(parsed["beta"], 3);

    // Ensure we have exactly 3 keys
    assert_eq!(parsed.as_object().unwrap().len(), 3);
}

// ===== SERDE JSON ROUND-TRIP TESTS =====

#[test]
fn test_serde_json_round_trip_map() {
    // Test round-trip using serde JSON serialization/deserialization
    let mut original_map = Map::new();
    original_map.set("name", "Alice");
    original_map.set("age", 30);
    original_map.set("active", true);
    original_map.set("score", 95.5); // Will be converted to int

    let mut nested_map = Map::new();
    nested_map.set("city", "NYC");
    nested_map.set("zip", 10001);
    original_map.set("address", nested_map);

    // Serialize to JSON
    let json = serde_json::to_string(&original_map).unwrap();

    // Deserialize back to Map
    let deserialized_map: Map = serde_json::from_str(&json).unwrap();

    // Compare the maps
    assert_eq!(
        original_map.get_text("name"),
        deserialized_map.get_text("name")
    );
    assert_eq!(original_map.get_int("age"), deserialized_map.get_int("age"));
    assert_eq!(
        original_map.get_bool("active"),
        deserialized_map.get_bool("active")
    );
    assert_eq!(
        original_map.get_int("score"),
        deserialized_map.get_int("score")
    ); // 95.5 -> 95

    // Test nested map
    let orig_nested = original_map.get_node("address").unwrap();
    let deser_nested = deserialized_map.get_node("address").unwrap();
    assert_eq!(orig_nested.get_text("city"), deser_nested.get_text("city"));
    assert_eq!(orig_nested.get_int("zip"), deser_nested.get_int("zip"));

    // Test map equality (should be equal)
    assert_eq!(original_map, deserialized_map);
}

#[test]
fn test_serde_json_round_trip_list() {
    // Test round-trip for List using serde
    let mut original_list = List::new();
    original_list.push("first");
    original_list.push(42);
    original_list.push(true);
    original_list.push("last");

    // Serialize to JSON
    let json = serde_json::to_string(&original_list).unwrap();

    // Deserialize back to List
    let deserialized_list: List = serde_json::from_str(&json).unwrap();

    // Compare lengths
    assert_eq!(original_list.len(), deserialized_list.len());

    // Compare each element
    for i in 0..original_list.len() {
        let orig_val = original_list.get(i).unwrap();
        let deser_val = deserialized_list.get(i).unwrap();
        assert_eq!(orig_val, deser_val);
    }

    // Test list equality
    assert_eq!(original_list, deserialized_list);
}

#[test]
fn test_serde_json_round_trip_complex_structure() {
    // Test round-trip with complex nested structure
    let mut root = Map::new();

    // Add simple values
    root.set("app_name", "Eidetica");
    root.set("version", 1);
    root.set("enabled", true);

    // Add nested map
    let mut config = Map::new();
    config.set("timeout", 30);
    config.set("debug", false);
    root.set("config", config);

    // Add list with mixed types
    let mut features = List::new();
    features.push("auth");
    features.push("crdt");
    features.push("distributed");
    root.set("features", Value::List(features));

    // Add list of maps
    let mut users = List::new();

    let mut user1 = Map::new();
    user1.set("id", 1);
    user1.set("name", "Alice");
    user1.set("admin", true);

    let mut user2 = Map::new();
    user2.set("id", 2);
    user2.set("name", "Bob");
    user2.set("admin", false);

    users.push(Value::Map(user1));
    users.push(Value::Map(user2));
    root.set("users", Value::List(users));

    // Serialize to JSON
    let json = serde_json::to_string(&root).unwrap();

    // Deserialize back to Map
    let deserialized_root: Map = serde_json::from_str(&json).unwrap();

    // Compare the entire structure
    assert_eq!(root, deserialized_root);

    // Verify specific nested access works
    assert_eq!(deserialized_root.get_text("app_name"), Some("Eidetica"));
    assert_eq!(
        deserialized_root.get_int_at_path("config.timeout"),
        Some(30)
    );
    assert_eq!(
        deserialized_root.get_bool_at_path("config.debug"),
        Some(false)
    );

    // Verify list access
    let features_list = deserialized_root.get_list("features").unwrap();
    assert_eq!(features_list.len(), 3);
    assert_eq!(features_list.get(0).unwrap().as_text(), Some("auth"));

    // Verify nested list of maps
    let users_list = deserialized_root.get_list("users").unwrap();
    assert_eq!(users_list.len(), 2);

    let first_user = users_list.get(0).unwrap().as_node().unwrap();
    assert_eq!(first_user.get_text("name"), Some("Alice"));
    assert_eq!(first_user.get_bool("admin"), Some(true));
}

#[test]
fn test_serde_json_round_trip_with_tombstones() {
    // Test that tombstones are preserved during round-trip
    let mut original_map = Map::new();
    original_map.set("keep", "this");
    original_map.set("remove", "this");

    // Verify initial state
    assert!(!original_map.is_tombstone("keep"));
    assert!(!original_map.is_tombstone("remove"));
    assert!(!original_map.is_tombstone("nonexistent"));

    // Create tombstone
    original_map.remove("remove");

    // Verify tombstone exists in original
    assert!(original_map.is_tombstone("remove"));
    assert!(!original_map.is_tombstone("keep"));

    // Serialize to JSON
    let json = serde_json::to_string(&original_map).unwrap();

    // Deserialize back to Map
    let deserialized_map: Map = serde_json::from_str(&json).unwrap();

    // The deserialized map should be equal to the original (including tombstones)
    assert_eq!(original_map, deserialized_map);

    // Verify that tombstones are preserved after round-trip
    assert!(deserialized_map.is_tombstone("remove")); // Tombstone preserved!
    assert!(!deserialized_map.is_tombstone("keep")); // Non-tombstone preserved!
    assert!(!deserialized_map.is_tombstone("nonexistent")); // Still doesn't exist

    // Verify that tombstones are properly hidden from public API
    assert_eq!(
        deserialized_map.get("keep").unwrap().as_text(),
        Some("this")
    );
    assert!(deserialized_map.get("remove").is_none()); // Tombstone hidden from get()
    assert!(!deserialized_map.contains_key("remove")); // Tombstone hidden from contains_key()
    assert_eq!(deserialized_map.len(), 1); // Only counts non-tombstones
}

#[test]
fn test_list_position_preservation_round_trip() {
    // Test that List positions are preserved during serde round-trip
    let mut original_list = List::new();

    // Add items with specific positions to test position preservation
    let pos1 = Position::new(10, 1);
    let pos2 = Position::new(5, 1); // Insert before first
    let pos3 = Position::new(15, 1); // Insert after first

    original_list.insert_at_position(pos1.clone(), "middle");
    original_list.insert_at_position(pos2.clone(), "first");
    original_list.insert_at_position(pos3.clone(), "last");

    // Verify order before serialization (should be: first, middle, last)
    assert_eq!(original_list.get(0).unwrap().as_text(), Some("first"));
    assert_eq!(original_list.get(1).unwrap().as_text(), Some("middle"));
    assert_eq!(original_list.get(2).unwrap().as_text(), Some("last"));

    // Serialize to JSON
    let json = serde_json::to_string(&original_list).unwrap();

    // Deserialize back to List
    let deserialized_list: List = serde_json::from_str(&json).unwrap();

    // Verify the lists are equal
    assert_eq!(original_list, deserialized_list);

    // Verify order is preserved
    assert_eq!(deserialized_list.get(0).unwrap().as_text(), Some("first"));
    assert_eq!(deserialized_list.get(1).unwrap().as_text(), Some("middle"));
    assert_eq!(deserialized_list.get(2).unwrap().as_text(), Some("last"));

    // Verify we can still access by original positions
    assert_eq!(
        deserialized_list.get_by_position(&pos1).unwrap().as_text(),
        Some("middle")
    );
    assert_eq!(
        deserialized_list.get_by_position(&pos2).unwrap().as_text(),
        Some("first")
    );
    assert_eq!(
        deserialized_list.get_by_position(&pos3).unwrap().as_text(),
        Some("last")
    );
}

// ===== SERIALIZATION BEHAVIOR TESTS =====

#[test]
fn test_map_list_serialization() {
    let mut map = Map::new();

    // Add a list element
    let result = map.list_add("fruits", Value::Text("apple".to_string()));
    assert!(result.is_ok());

    // Check list length before serialization
    let length_before = map.list_len("fruits");
    assert_eq!(length_before, 1);

    // Serialize and deserialize
    let serialized = serde_json::to_string(&map).unwrap();
    let deserialized: Map = serde_json::from_str(&serialized).unwrap();

    // Check list length after deserialization
    let length_after = deserialized.list_len("fruits");
    assert_eq!(length_after, 1);

    // Check if they're equal
    assert_eq!(length_before, length_after);
}

// ===== HELPER INTEGRATION TESTS =====

#[test]
fn test_serialization_helpers_integration() {
    // Test using the helper functions for serialization testing
    let complex_map = create_complex_map();

    // Test JSON roundtrip using helper
    let _roundtrip_result = test_json_roundtrip(&complex_map).unwrap();

    // Verify the complex map has the expected structure by checking internal API
    assert_eq!(complex_map.get_text("title"), Some("My Document"));
    assert_eq!(complex_map.get_int("priority"), Some(42));
    assert_eq!(complex_map.get_bool("published"), Some(true));
    assert!(complex_map.get_node("metadata").is_some());
    assert!(complex_map.get_list("tags").is_some());

    // Test that serialization doesn't fail
    let json = serde_json::to_string(&complex_map).unwrap();
    assert!(!json.is_empty());

    // Test that it deserializes back correctly
    let deserialized: Map = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.get_text("title"), Some("My Document"));
}

#[test]
fn test_mixed_value_serialization() {
    let mixed_map = create_mixed_value_map();

    // Test that all value types can be serialized and deserialized
    let json = serde_json::to_string(&mixed_map).unwrap();
    let deserialized: Map = serde_json::from_str(&json).unwrap();

    // Check that basic value types are preserved
    assert_eq!(mixed_map.get("null_val"), deserialized.get("null_val"));
    assert_eq!(mixed_map.get("bool_val"), deserialized.get("bool_val"));
    assert_eq!(mixed_map.get("int_val"), deserialized.get("int_val"));
    assert_eq!(mixed_map.get("text_val"), deserialized.get("text_val"));

    // Verify complex types
    assert!(deserialized.get("map_val").unwrap().as_node().is_some());
    assert!(deserialized.get("list_val").unwrap().as_list().is_some());

    // Note: Deleted values should be handled correctly in serialization
    assert_eq!(
        mixed_map.get("deleted_val"),
        deserialized.get("deleted_val")
    );
}

#[test]
fn test_large_structure_serialization() {
    // Test serialization performance and correctness with larger data
    let large_map = create_large_map(100);
    let large_list = create_large_list(100);

    // Test Map serialization
    let map_json = serde_json::to_string(&large_map).unwrap();
    let deserialized_map: Map = serde_json::from_str(&map_json).unwrap();
    assert_eq!(large_map.len(), deserialized_map.len());

    // Test List serialization
    let list_json = serde_json::to_string(&large_list).unwrap();
    let deserialized_list: List = serde_json::from_str(&list_json).unwrap();
    assert_eq!(large_list.len(), deserialized_list.len());

    // Verify a few specific entries
    assert_eq!(large_map.get_text("key_0"), Some("value_0"));
    assert_eq!(large_map.get_text("key_99"), Some("value_99"));
    assert_eq!(deserialized_map.get_text("key_0"), Some("value_0"));
    assert_eq!(deserialized_map.get_text("key_99"), Some("value_99"));
}
