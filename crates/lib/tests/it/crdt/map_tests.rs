//! Map-specific integration tests
//!
//! This module contains tests extracted from the map tests that focus specifically
//! on Map functionality, including basic operations, path operations, iterators,
//! builder pattern, CRDT merge operations, tombstone handling, and JSON serialization.

use eidetica::crdt::map::{List, Node, Value};
use eidetica::crdt::{CRDT, Doc};

// ===== BASIC MAP OPERATIONS =====

#[test]
fn test_map_basic_operations() {
    let mut map = Doc::new();

    assert!(map.is_empty());
    assert_eq!(map.len(), 0);

    // Test set with flexible input
    let old_val = map.set("name", "Alice");
    assert!(old_val.is_none());
    assert!(!map.is_empty());
    assert_eq!(map.len(), 1);

    let old_val2 = map.set("age", 30);
    assert!(old_val2.is_none());
    assert_eq!(map.len(), 2);

    // Test contains_key with flexible input
    assert!(map.contains_key("name"));
    assert!(map.contains_key("age"));
    assert!(!map.contains_key("nonexistent"));

    // Test get with flexible input
    assert_eq!(map.get_text("name"), Some("Alice"));
    assert_eq!(map.get_int("age"), Some(30));
    assert!(map.get("nonexistent").is_none());
}

#[test]
fn test_map_overwrite_values() {
    let mut map = Doc::new();

    map.set("key", "original");
    let old_val = map.set("key", "modified");

    assert_eq!(old_val.as_ref().and_then(|v| v.as_text()), Some("original"));
    assert_eq!(map.get_text("key"), Some("modified"));
    assert_eq!(map.len(), 1); // Should still be 1
}

#[test]
fn test_map_remove_operations() {
    let mut map = Doc::new();

    map.set("name", "Alice");
    map.set("age", 30);
    map.set("active", true);

    // Test remove with flexible input
    let removed = map.remove("age");
    assert_eq!(removed.as_ref().and_then(|v| v.as_int()), Some(30));
    assert!(!map.contains_key("age")); // Key no longer exists (tombstone hidden)
    assert!(map.get("age").is_none()); // get returns None
    assert_eq!(map.len(), 2); // Tombstones excluded from len

    // Test remove on non-existent key
    let result = map.remove("nonexistent");
    assert!(result.is_none());
    assert!(!map.contains_key("nonexistent")); // Tombstone hidden from API
    assert_eq!(map.len(), 2); // Tombstones excluded from len
}

#[test]
fn test_map_delete_operations() {
    let mut map = Doc::new();

    map.set("name", "Alice");
    map.set("age", 30);

    // Test delete with flexible input
    let result = map.delete("age");
    assert!(result);
    assert!(!map.contains_key("age")); // Key no longer exists (tombstone hidden)
    assert!(map.get("age").is_none()); // Returns None (filtered out)

    // Test delete on non-existent key
    let result2 = map.delete("nonexistent");
    assert!(!result2);
}

#[test]
fn test_map_get_mut() {
    let mut map = Doc::new();

    map.set("name", "Alice");
    map.set("age", 30);

    // Test get_mut with flexible input
    if let Some(Value::Text(name)) = map.get_mut("name") {
        name.push_str(" Smith");
    }

    assert_eq!(map.get_text("name"), Some("Alice Smith"));

    // Test get_mut on non-existent key
    assert!(map.get_mut("nonexistent").is_none());
}

// ===== PATH OPERATIONS =====

#[test]
fn test_map_path_operations() {
    let mut map = Doc::new();

    // Test set_path creating intermediate nodes
    let result = map.set_path("user.profile.name", "Alice");
    assert!(result.is_ok());

    let result2 = map.set_path("user.profile.age", 30);
    assert!(result2.is_ok());

    let result3 = map.set_path("user.settings.theme", "dark");
    assert!(result3.is_ok());

    // Test get_path
    assert_eq!(map.get_text_at_path("user.profile.name"), Some("Alice"));
    assert_eq!(map.get_int_at_path("user.profile.age"), Some(30));
    assert_eq!(map.get_text_at_path("user.settings.theme"), Some("dark"));
    assert!(map.get_path("nonexistent.path").is_none());

    // Test get_path_mut
    if let Some(Value::Text(name)) = map.get_path_mut("user.profile.name") {
        name.push_str(" Smith");
    }

    assert_eq!(
        map.get_text_at_path("user.profile.name"),
        Some("Alice Smith")
    );
}

#[test]
fn test_map_path_with_lists() {
    let mut map = Doc::new();

    // Create a node with a list
    let mut list = List::new();
    list.push("item1");
    list.push("item2");
    map.set("items", list);

    // Test path access with list indices
    assert_eq!(map.get_text_at_path("items.0"), Some("item1"));
    assert_eq!(map.get_text_at_path("items.1"), Some("item2"));
    assert!(map.get_path("items.2").is_none());
    assert!(map.get_path("items.invalid").is_none());
}

#[test]
fn test_map_path_errors() {
    let mut map = Doc::new();

    map.set("scalar", "value");

    // Test setting path through scalar value
    let result = map.set_path("scalar.nested", "should_fail");
    assert!(result.is_err());

    // Test empty path - this actually works, it just sets at root level
    let result2 = map.set_path("", "value");
    assert!(result2.is_ok()); // Empty path is treated as root level

    // Test path with single component
    let result3 = map.set_path("single", "value");
    assert!(result3.is_ok());
    assert_eq!(map.get_text("single"), Some("value"));
}

// ===== ITERATORS =====

#[test]
fn test_map_iterators() {
    let mut map = Doc::new();

    map.set("name", "Alice");
    map.set("age", 30);
    map.set("active", true);

    // Test iter
    let pairs: Vec<_> = map.iter().collect();
    assert_eq!(pairs.len(), 3);

    // Test keys
    let keys: Vec<_> = map.keys().collect();
    assert_eq!(keys.len(), 3);
    assert!(keys.contains(&&"name".to_string()));
    assert!(keys.contains(&&"age".to_string()));
    assert!(keys.contains(&&"active".to_string()));

    // Test values
    let values: Vec<_> = map.values().collect();
    assert_eq!(values.len(), 3);

    // Test iter_mut
    for (key, value) in map.iter_mut() {
        if key == "name"
            && let Value::Text(s) = value
        {
            s.push_str(" Smith");
        }
    }

    assert_eq!(map.get_text("name"), Some("Alice Smith"));
}

// ===== BUILDER PATTERN =====

#[test]
fn test_map_builder_pattern() {
    let map = Doc::new()
        .with_text("name", "Alice")
        .with_int("age", 30)
        .with_bool("active", true)
        .with_node("profile", Doc::new().with_text("bio", "Developer"))
        .with_list("tags", List::new());

    assert_eq!(map.get_text("name"), Some("Alice"));
    assert_eq!(map.get_int("age"), Some(30));
    assert_eq!(map.get_bool("active"), Some(true));
    assert!(map.get_node("profile").is_some());
    assert!(map.get_list("tags").is_some());

    // Test nested access
    assert_eq!(map.get_text_at_path("profile.bio"), Some("Developer"));
}

#[test]
fn test_map_clear() {
    let mut map = Doc::new();

    map.set("name", "Alice");
    map.set("age", 30);

    assert_eq!(map.len(), 2);

    map.clear();

    assert!(map.is_empty());
    assert_eq!(map.len(), 0);
}

// ===== CRDT MERGE OPERATIONS =====

#[test]
fn test_map_crdt_merge() {
    let mut map1 = Doc::new();
    let mut map2 = Doc::new();

    map1.set("name", "Alice");
    map1.set("age", 30);

    map2.set("name", "Bob"); // Conflict
    map2.set("city", "NYC");

    let merged = map1.merge(&map2).unwrap();

    assert_eq!(merged.get_text("name"), Some("Bob")); // Last write wins
    assert_eq!(merged.get_int("age"), Some(30));
    assert_eq!(merged.get_text("city"), Some("NYC"));
}

#[test]
fn test_map_from_iterator() {
    let pairs = vec![
        ("name".to_string(), Value::Text("Alice".to_string())),
        ("age".to_string(), Value::Int(30)),
        ("active".to_string(), Value::Bool(true)),
    ];

    let map: Node = pairs.into_iter().collect();

    assert_eq!(map.get_text("name"), Some("Alice"));
    assert_eq!(map.get_int("age"), Some(30));
    assert_eq!(map.get_bool("active"), Some(true));
}

// ===== LIST INTEGRATION TESTS =====

#[test]
fn test_map_list_serialization() {
    let mut map = Doc::new();

    // Add a list element
    let result = map.list_add("fruits", Value::Text("apple".to_string()));
    assert!(result.is_ok());

    // Check list length before serialization
    let length_before = map.list_len("fruits");
    assert_eq!(length_before, 1);

    // Serialize and deserialize
    let serialized = serde_json::to_string(&map).unwrap();
    let deserialized: Doc = serde_json::from_str(&serialized).unwrap();

    // Check list length after deserialization
    let length_after = deserialized.list_len("fruits");
    assert_eq!(length_after, 1);

    // Check if they're equal
    assert_eq!(length_before, length_after);
}

// ===== JSON SERIALIZATION TESTS =====

#[test]
fn test_map_to_json_string_basic() {
    let mut map = Doc::new();
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
    let map = Doc::new();
    assert_eq!(map.to_json_string(), "{}");
}

#[test]
fn test_map_to_json_string_nested() {
    let mut inner_map = Doc::new();
    inner_map.set("city", "NYC");
    inner_map.set("zip", 10001);

    let mut outer_map = Doc::new();
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
fn test_complex_nested_structure_to_json() {
    let mut users_list = List::new();

    // Create first user
    let mut user1 = Doc::new();
    user1.set("name", "Alice");
    user1.set("age", 30);

    let mut tags1 = List::new();
    tags1.push("developer");
    tags1.push("rust");
    user1.set("tags", tags1);

    // Create second user
    let mut user2 = Doc::new();
    user2.set("name", "Bob");
    user2.set("age", 25);

    let mut tags2 = List::new();
    tags2.push("designer");
    user2.set("tags", tags2);

    users_list.push(user1);
    users_list.push(user2);

    // Create root structure
    let mut root = Doc::new();
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
    let mut map = Doc::new();
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
fn test_json_round_trip_validation() {
    // Test that our JSON output is valid and can be parsed
    let mut map = Doc::new();
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
    let mut map = Doc::new();
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

// ===== SERDE JSON ROUND TRIP TESTS =====

#[test]
fn test_serde_json_round_trip_map() {
    // Test round-trip using serde JSON serialization/deserialization
    let mut original_map = Doc::new();
    original_map.set("name", "Alice");
    original_map.set("age", 30);
    original_map.set("active", true);
    original_map.set("score", 95.5); // Will be converted to int

    let mut nested_map = Doc::new();
    nested_map.set("city", "NYC");
    nested_map.set("zip", 10001);
    original_map.set("address", nested_map);

    // Serialize to JSON
    let json = serde_json::to_string(&original_map).unwrap();

    // Deserialize back to Map
    let deserialized_map: Doc = serde_json::from_str(&json).unwrap();

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
fn test_serde_json_round_trip_complex_structure() {
    // Test round-trip with complex nested structure
    let mut root = Doc::new();

    // Add simple values
    root.set("app_name", "Eidetica");
    root.set("version", 1);
    root.set("enabled", true);

    // Add nested map
    let mut config = Doc::new();
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

    let mut user1 = Doc::new();
    user1.set("id", 1);
    user1.set("name", "Alice");
    user1.set("admin", true);

    let mut user2 = Doc::new();
    user2.set("id", 2);
    user2.set("name", "Bob");
    user2.set("admin", false);

    users.push(user1);
    users.push(user2);
    root.set("users", Value::List(users));

    // Serialize to JSON
    let json = serde_json::to_string(&root).unwrap();

    // Deserialize back to Map
    let deserialized_root: Doc = serde_json::from_str(&json).unwrap();

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

// ===== TOMBSTONE TESTS =====

#[test]
fn test_map_is_tombstone_basic() {
    let mut map = Doc::new();

    // Non-existent keys are not tombstones
    assert!(!map.is_tombstone("nonexistent"));

    // Regular values are not tombstones
    map.set("key1", "value1");
    map.set("key2", 42);
    map.set("key3", true);

    assert!(!map.is_tombstone("key1"));
    assert!(!map.is_tombstone("key2"));
    assert!(!map.is_tombstone("key3"));

    // Remove creates tombstones
    map.remove("key1");
    assert!(map.is_tombstone("key1"));
    assert!(!map.is_tombstone("key2")); // Others still not tombstones
    assert!(!map.is_tombstone("key3"));

    // Remove more keys
    map.remove("key3");
    assert!(map.is_tombstone("key1"));
    assert!(!map.is_tombstone("key2"));
    assert!(map.is_tombstone("key3"));
}

#[test]
fn test_map_is_tombstone_vs_public_api() {
    let mut map = Doc::new();
    map.set("temp", "value");

    // Before removal - visible in public API
    assert!(map.contains_key("temp"));
    assert!(map.get("temp").is_some());
    assert!(!map.is_tombstone("temp"));
    assert_eq!(map.len(), 1);

    // After removal - hidden from public API but visible via is_tombstone
    map.remove("temp");
    assert!(!map.contains_key("temp")); // Hidden from contains_key
    assert!(map.get("temp").is_none()); // Hidden from get
    assert!(map.is_tombstone("temp")); // But detectable via is_tombstone
    assert_eq!(map.len(), 0); // Hidden from len
}

#[test]
fn test_map_is_tombstone_delete_method() {
    let mut map = Doc::new();
    map.set("key", "value");

    // delete() method also creates tombstones
    assert!(!map.is_tombstone("key"));
    map.delete("key");
    assert!(map.is_tombstone("key"));

    // Delete non-existent key doesn't create tombstone
    assert!(!map.is_tombstone("nonexistent"));
    map.delete("nonexistent");
    assert!(!map.is_tombstone("nonexistent")); // Still not a tombstone
}

#[test]
fn test_map_is_tombstone_overwrite_behavior() {
    let mut map = Doc::new();
    map.set("key", "original");

    // Remove to create tombstone
    map.remove("key");
    assert!(map.is_tombstone("key"));

    // Overwrite tombstone with new value
    map.set("key", "new_value");
    assert!(!map.is_tombstone("key")); // No longer a tombstone
    assert!(map.contains_key("key")); // Visible in public API again
    assert_eq!(map.get_text("key"), Some("new_value"));
}

#[test]
fn test_map_is_tombstone_nested_structures() {
    let mut map = Doc::new();

    // Add nested map
    let mut inner_map = Doc::new();
    inner_map.set("inner_key", "inner_value");
    map.set("outer", inner_map);

    // Add list
    let mut list = List::new();
    list.push("item1");
    list.push("item2");
    map.set("list", Value::List(list));

    // Verify nested structures are not tombstones
    assert!(!map.is_tombstone("outer"));
    assert!(!map.is_tombstone("list"));

    // Remove nested structures
    map.remove("outer");
    map.remove("list");

    // Verify they become tombstones
    assert!(map.is_tombstone("outer"));
    assert!(map.is_tombstone("list"));

    // But are hidden from public API
    assert!(map.get("outer").is_none());
    assert!(map.get("list").is_none());
    assert!(!map.contains_key("outer"));
    assert!(!map.contains_key("list"));
}

#[test]
fn test_map_is_tombstone_key_types() {
    let mut map = Doc::new();

    // Test with different key types that can be converted to string references
    map.set("string_key", "value1");
    map.set(String::from("string_owned"), "value2");

    assert!(!map.is_tombstone("string_key"));
    assert!(!map.is_tombstone(String::from("string_owned")));
    assert!(!map.is_tombstone("string_owned")); // Can use &str for String key

    // Remove and test tombstone detection with different key types
    map.remove("string_key");
    map.remove(String::from("string_owned"));

    assert!(map.is_tombstone("string_key"));
    assert!(map.is_tombstone(String::from("string_owned")));
    assert!(map.is_tombstone("string_owned"));
}

#[test]
fn test_serde_json_round_trip_with_tombstones() {
    // Test that tombstones are preserved during round-trip
    let mut original_map = Doc::new();
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
    let deserialized_map: Doc = serde_json::from_str(&json).unwrap();

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
