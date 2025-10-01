//! Value-specific integration tests
//!
//! This module contains tests extracted from the doc tests that focus specifically
//! on Value enum functionality, including type checking, accessors, merging,
//! and PartialEq implementations.

use eidetica::{
    crdt::{
        Doc,
        doc::{List, Node, Value},
    },
    path,
};

use super::helpers::*;

// ===== VALUE TYPE TESTS =====

#[test]
fn test_map_value_basic_types() {
    let null_val = Value::Null;
    let bool_val = Value::Bool(true);
    let int_val = Value::Int(42);
    let text_val = Value::Text("hello".to_string());
    let deleted_val = Value::Deleted;

    assert!(null_val.is_leaf());
    assert!(bool_val.is_leaf());
    assert!(int_val.is_leaf());
    assert!(text_val.is_leaf());
    assert!(deleted_val.is_leaf());

    assert!(!null_val.is_branch());
    assert!(!bool_val.is_branch());
    assert!(!int_val.is_branch());
    assert!(!text_val.is_branch());
    assert!(!deleted_val.is_branch());

    assert!(null_val.is_null());
    assert!(!bool_val.is_null());
    assert!(!int_val.is_null());
    assert!(!text_val.is_null());
    assert!(!deleted_val.is_null());

    assert!(!null_val.is_deleted());
    assert!(!bool_val.is_deleted());
    assert!(!int_val.is_deleted());
    assert!(!text_val.is_deleted());
    assert!(deleted_val.is_deleted());
}

#[test]
fn test_map_value_branch_types() {
    let map_val = Value::Doc(Node::new());
    let list_val = Value::List(List::new());

    assert!(!map_val.is_leaf());
    assert!(!list_val.is_leaf());

    assert!(map_val.is_branch());
    assert!(list_val.is_branch());

    assert!(!map_val.is_null());
    assert!(!list_val.is_null());

    assert!(!map_val.is_deleted());
    assert!(!list_val.is_deleted());
}

#[test]
fn test_map_value_type_names() {
    assert_eq!(Value::Null.type_name(), "null");
    assert_eq!(Value::Bool(true).type_name(), "bool");
    assert_eq!(Value::Int(42).type_name(), "int");
    assert_eq!(Value::Text("hello".to_string()).type_name(), "text");
    assert_eq!(Value::Doc(Node::new()).type_name(), "doc");
    assert_eq!(Value::List(List::new()).type_name(), "list");
    assert_eq!(Value::Deleted.type_name(), "deleted");
}

#[test]
fn test_map_value_accessors() {
    let bool_val = Value::Bool(true);
    let int_val = Value::Int(42);
    let text_val = Value::Text("hello".to_string());
    let map_val = Value::Doc(Node::new());
    let list_val = Value::List(List::new());

    // Test as_bool
    assert_eq!(bool_val.as_bool(), Some(true));
    assert_eq!(int_val.as_bool(), None);

    // Test as_int
    assert_eq!(int_val.as_int(), Some(42));
    assert_eq!(bool_val.as_int(), None);

    // Test as_text
    assert_eq!(text_val.as_text(), Some("hello"));
    assert_eq!(bool_val.as_text(), None);

    // Test direct comparisons
    assert!(bool_val == true);
    assert!(int_val == 42);
    assert!(text_val == "hello");

    // Test as_node
    assert!(map_val.as_node().is_some());
    assert!(bool_val.as_node().is_none());

    // Test as_list
    assert!(list_val.as_list().is_some());
    assert!(bool_val.as_list().is_none());
}

#[test]
fn test_map_value_from_impls() {
    let from_bool: Value = true.into();
    let from_i64: Value = 42i64.into();
    let from_string: Value = "hello".into();
    let from_node: Value = Node::new().into();
    let from_list: Value = List::new().into();

    assert_eq!(from_bool.as_bool(), Some(true));
    assert_eq!(from_i64.as_int(), Some(42));
    assert_eq!(from_string.as_text(), Some("hello"));
    assert!(from_node.as_node().is_some());
    assert!(from_list.as_list().is_some());
}

// ===== VALUE MERGE TESTS =====

#[test]
fn test_map_value_merge_leafs() {
    let mut val1 = Value::Int(42);
    let val2 = Value::Int(100);

    val1.merge(&val2);
    assert_eq!(val1.as_int(), Some(100)); // Last write wins

    let mut val3 = Value::Text("hello".to_string());
    let val4 = Value::Text("world".to_string());

    val3.merge(&val4);
    assert_eq!(val3.as_text(), Some("world")); // Last write wins
}

#[test]
fn test_map_value_merge_with_deleted() {
    let mut val1 = Value::Int(42);
    let val2 = Value::Deleted;

    val1.merge(&val2);
    assert!(val1.is_deleted()); // Deletion wins

    let mut val3 = Value::Deleted;
    let val4 = Value::Int(100);

    val3.merge(&val4);
    assert_eq!(val3.as_int(), Some(100)); // Resurrection
}

// ===== PARTIAL EQ TESTS =====

#[test]
fn test_partial_eq_nodevalue() {
    let text_val = Value::Text("hello".to_string());
    let int_val = Value::Int(42);
    let bool_val = Value::Bool(true);

    // Test Value comparisons with primitive types
    assert!(text_val == "hello");
    assert!(text_val == "hello");
    assert!(int_val == 42i64);
    assert!(int_val == 42i32);
    assert!(int_val == 42u32);
    assert!(bool_val == true);

    // Test reverse comparisons
    assert!("hello" == text_val);
    assert!("hello" == text_val);
    assert!(42i64 == int_val);
    assert!(42i32 == int_val);
    assert!(42u32 == int_val);
    assert!(true == bool_val);

    // Test non-matching types
    assert!(!(text_val == 42));
    assert!(!(int_val == "hello"));
    assert!(!(bool_val == "hello"));
}

#[test]
fn test_partial_eq_with_unwrap() {
    let mut map = Doc::new();
    map.set("name", "Alice");
    map.set("age", 30);
    map.set("active", true);

    // Test Value comparisons through unwrap
    assert!(*map.get("name").unwrap() == "Alice");
    assert!(*map.get("age").unwrap() == 30i64);
    assert!(*map.get("age").unwrap() == 30i32);
    assert!(*map.get("age").unwrap() == 30u32);
    assert!(*map.get("active").unwrap() == true);

    // Test reverse comparisons
    assert!("Alice" == *map.get("name").unwrap());
    assert!(30i64 == *map.get("age").unwrap());
    assert!(30i32 == *map.get("age").unwrap());
    assert!(30u32 == *map.get("age").unwrap());
    assert!(true == *map.get("active").unwrap());

    // Test non-matching types
    assert!(!(*map.get("name").unwrap() == 42));
    assert!(!(*map.get("age").unwrap() == "Alice"));
    assert!(!(*map.get("active").unwrap() == "Alice"));

    // Test with matches! macro for cleaner pattern
    assert!(matches!(map.get("name"), Some(v) if *v == "Alice"));
    assert!(matches!(map.get("age"), Some(v) if *v == 30));
    assert!(matches!(map.get("active"), Some(v) if *v == true));
    assert!(map.get("nonexistent").is_none());
}

// ===== VALUE CONVENIENCE METHODS TESTS =====

#[test]
fn test_cleaner_api_examples() {
    let mut map = Doc::new();

    // Set some values
    map.set("name", "Alice");
    map.set("age", 30);
    map.set("active", true);

    // Old verbose way (still works)
    assert_eq!(map.get("name").and_then(|v| v.as_text()), Some("Alice"));
    assert_eq!(map.get("age").and_then(|v| v.as_int()), Some(30));
    assert_eq!(map.get("active").and_then(|v| v.as_bool()), Some(true));

    // New clean way with typed getters
    assert_eq!(map.get_text("name"), Some("Alice"));
    assert_eq!(map.get_int("age"), Some(30));
    assert_eq!(map.get_bool("active"), Some(true));

    // Even cleaner with direct comparisons on Value!
    assert!(*map.get("name").unwrap() == "Alice");
    assert!(*map.get("age").unwrap() == 30);
    assert!(*map.get("active").unwrap() == true);

    // Path-based access
    map.set_path(path!("user.profile.bio"), "Developer")
        .unwrap();

    // Old verbose way (still works)
    assert_eq!(
        map.get_path(path!("user.profile.bio"))
            .and_then(|v| v.as_text()),
        Some("Developer")
    );

    // New clean way with typed getters
    assert_eq!(
        map.get_text_at_path(path!("user.profile.bio")),
        Some("Developer")
    );

    // Even cleaner with direct comparisons on Value!
    assert!(*map.get_path(path!("user.profile.bio")).unwrap() == "Developer");

    // Convenience methods for Value
    let value = Value::Text("hello".to_string());
    assert_eq!(value.as_text_or_empty(), "hello");

    let value = Value::Int(42);
    assert_eq!(value.as_int_or_zero(), 42);
    assert!(!value.as_bool_or_false()); // not a bool, returns false
}

// ===== JSON SERIALIZATION TESTS FOR VALUES =====

#[test]
fn test_value_to_json_string_leaf_types() {
    // Test all leaf value types
    assert_eq!(Value::Null.to_json_string(), "null");
    assert_eq!(Value::Bool(true).to_json_string(), "true");
    assert_eq!(Value::Bool(false).to_json_string(), "false");
    assert_eq!(Value::Int(42).to_json_string(), "42");
    assert_eq!(Value::Int(-123).to_json_string(), "-123");
    assert_eq!(Value::Int(0).to_json_string(), "0");
    assert_eq!(
        Value::Text("hello".to_string()).to_json_string(),
        "\"hello\""
    );
    assert_eq!(Value::Text("".to_string()).to_json_string(), "\"\"");
    assert_eq!(Value::Deleted.to_json_string(), "null");
}

#[test]
fn test_value_to_json_string_text_escaping() {
    // Test quote escaping
    assert_eq!(
        Value::Text("say \"hello\"".to_string()).to_json_string(),
        "\"say \\\"hello\\\"\""
    );

    // Test various special characters that should be escaped
    assert_eq!(
        Value::Text("quote: \"".to_string()).to_json_string(),
        "\"quote: \\\"\""
    );

    // Test text with no special characters
    assert_eq!(
        Value::Text("simple text".to_string()).to_json_string(),
        "\"simple text\""
    );

    // Test text with numbers and symbols (no escaping needed)
    assert_eq!(
        Value::Text("test123!@#$%^&*()".to_string()).to_json_string(),
        "\"test123!@#$%^&*()\""
    );
}

#[test]
fn test_value_to_json_string_empty_containers() {
    // Test empty Map
    let empty_map = Doc::new();
    assert_eq!(empty_map.to_json_string(), "{}");

    // Test empty List
    let empty_list = Value::List(List::new());
    assert_eq!(empty_list.to_json_string(), "[]");
}

#[test]
fn test_json_string_large_numbers() {
    // Test edge cases with large numbers
    assert_eq!(Value::Int(i64::MAX).to_json_string(), i64::MAX.to_string());
    assert_eq!(Value::Int(i64::MIN).to_json_string(), i64::MIN.to_string());
    assert_eq!(Value::Int(0).to_json_string(), "0");
    assert_eq!(Value::Int(-1).to_json_string(), "-1");
}

#[test]
fn test_json_string_special_text_cases() {
    // Test various text edge cases
    assert_eq!(Value::Text("".to_string()).to_json_string(), "\"\"");
    assert_eq!(Value::Text(" ".to_string()).to_json_string(), "\" \"");
    assert_eq!(
        Value::Text("\t\n\r".to_string()).to_json_string(),
        "\"\t\n\r\""
    );

    // Test text that looks like JSON
    assert_eq!(
        Value::Text("{\"key\": \"value\"}".to_string()).to_json_string(),
        "\"{\\\"key\\\": \\\"value\\\"}\""
    );

    // Test text with numbers
    assert_eq!(Value::Text("123".to_string()).to_json_string(), "\"123\"");
    assert_eq!(Value::Text("true".to_string()).to_json_string(), "\"true\"");
    assert_eq!(
        Value::Text("false".to_string()).to_json_string(),
        "\"false\""
    );
    assert_eq!(Value::Text("null".to_string()).to_json_string(), "\"null\"");
}

// ===== SERDE ROUND TRIP TESTS FOR VALUES =====

#[test]
fn test_serde_json_round_trip_value_types() {
    // Test round-trip for all Value types
    let test_values = vec![
        Value::Null,
        Value::Bool(true),
        Value::Bool(false),
        Value::Int(42),
        Value::Int(-123),
        Value::Int(0),
        Value::Text("hello world".to_string()),
        Value::Text("".to_string()),
        Value::Text("special \"chars\" & symbols!".to_string()),
        Value::Deleted, // This should round-trip as Deleted
    ];

    for original_value in test_values {
        // Use helper function from crdt::helpers for round-trip testing
        test_json_roundtrip(&original_value).expect("Round-trip should succeed");
    }
}

// ===== VALUE COLLECTION HELPER TESTS =====

#[test]
fn test_all_value_types_helper() {
    // Test that our helper creates all expected Value types
    let all_values = create_all_value_types();

    assert_eq!(all_values.len(), 10); // We should have 10 different Value types

    // Verify we have all the expected types
    let type_names: Vec<&str> = all_values.iter().map(|v| v.type_name()).collect();
    assert!(type_names.contains(&"null"));
    assert!(type_names.contains(&"bool"));
    assert!(type_names.contains(&"int"));
    assert!(type_names.contains(&"text"));
    assert!(type_names.contains(&"doc"));
    assert!(type_names.contains(&"list"));
    assert!(type_names.contains(&"deleted"));
}

#[test]
fn test_merge_test_values_helper() {
    // Test the merge helper function
    let (val1, val2) = create_merge_test_values();

    assert_eq!(val1.as_text(), Some("original"));
    assert_eq!(val2.as_text(), Some("updated"));

    // Test that merge works as expected
    let mut merged = val1.clone();
    merged.merge(&val2);
    assert_eq!(merged.as_text(), Some("updated")); // Last write wins
}

// ===== VALUE ASSERTION HELPER TESTS =====

#[test]
fn test_assert_value_content_helper() {
    let text_value = Value::Text("test".to_string());
    let int_value = Value::Int(42);
    let bool_value = Value::Bool(true);

    // Test type checking
    assert_value_content(&text_value, "text", None);
    assert_value_content(&int_value, "int", None);
    assert_value_content(&bool_value, "bool", None);

    // Test equality checking
    let expected_text = Value::Text("test".to_string());
    assert_value_content(&text_value, "text", Some(&expected_text));

    let expected_int = Value::Int(42);
    assert_value_content(&int_value, "int", Some(&expected_int));
}
