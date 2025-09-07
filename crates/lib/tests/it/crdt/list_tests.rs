//! List CRDT integration tests
//!
//! This module contains comprehensive tests for the List CRDT implementation,
//! including basic operations, position handling, merging, insertion, and
//! JSON serialization.

use eidetica::crdt::{
    CRDTError, Doc,
    doc::{List, Value, list::Position},
};

use crate::crdt::helpers::*;

// ===== BASIC LIST OPERATIONS =====

#[test]
fn test_list_basic_operations() {
    let mut list = List::new();

    assert!(list.is_empty());
    assert_eq!(list.len(), 0);

    // Test push with flexible input
    let idx1 = list.push("hello");
    let idx2 = list.push(42);
    let idx3 = list.push(true);

    assert!(!list.is_empty());
    assert_eq!(list.len(), 3);

    // Test get
    assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("hello"));
    assert_eq!(list.get(1).and_then(|v| v.as_int()), Some(42));
    assert_eq!(list.get(2).and_then(|v| v.as_bool()), Some(true));
    assert!(list.get(3).is_none());

    // Test indexes returned by push
    assert_eq!(idx1, 0);
    assert_eq!(idx2, 1);
    assert_eq!(idx3, 2);
}

#[test]
fn test_list_set_operations() {
    let mut list = List::new();

    list.push("original");
    list.push(100);

    // Test set with flexible input
    let old_val = list.set(0, "modified");
    assert_eq!(old_val.as_ref().and_then(|v| v.as_text()), Some("original"));
    assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("modified"));

    let old_val2 = list.set(1, 200);
    assert_eq!(old_val2.as_ref().and_then(|v| v.as_int()), Some(100));
    assert_eq!(list.get(1).and_then(|v| v.as_int()), Some(200));

    // Test set on non-existent index
    let result = list.set(10, "nonexistent");
    assert!(result.is_none());
}

#[test]
fn test_list_remove_operations() {
    let mut list = List::new();

    list.push("first");
    list.push("second");
    list.push("third");

    // Test remove
    let removed = list.remove(1);
    assert_eq!(removed.as_ref().and_then(|v| v.as_text()), Some("second"));
    assert_eq!(list.len(), 2);

    // Verify remaining elements
    assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("first"));
    assert_eq!(list.get(1).and_then(|v| v.as_text()), Some("third"));

    // Test remove on non-existent index
    let result = list.remove(10);
    assert!(result.is_none());
}

// ===== POSITION-BASED OPERATIONS =====

#[test]
fn test_list_insert_at_position() {
    let mut list = List::new();

    let pos1 = Position::new(10, 1);
    let pos2 = Position::new(20, 1);
    let pos3 = Position::new(15, 1); // Between pos1 and pos2

    list.insert_at_position(pos1, "first");
    list.insert_at_position(pos2, "third");
    list.insert_at_position(pos3, "second");

    // Should be ordered by position
    assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("first"));
    assert_eq!(list.get(1).and_then(|v| v.as_text()), Some("second"));
    assert_eq!(list.get(2).and_then(|v| v.as_text()), Some("third"));
}

#[test]
fn test_list_position_ordering() {
    let pos1 = Position::new(1, 2); // 0.5
    let pos2 = Position::new(3, 4); // 0.75
    let pos3 = Position::new(1, 1); // 1.0

    assert!(pos1 < pos2);
    assert!(pos2 < pos3);
    assert!(pos1 < pos3);

    // Test between
    let between = Position::between(&pos1, &pos3);
    assert!(pos1 < between);
    assert!(between < pos3);
}

#[test]
fn test_list_position_beginning_end() {
    let beginning = Position::beginning();
    let end = Position::end();
    let middle = Position::new(100, 1);

    assert!(beginning < middle);
    assert!(middle < end);
    assert!(beginning < end);
}

// ===== ITERATORS =====

#[test]
fn test_list_iterators() {
    let mut list = List::new();

    list.push("a");
    list.push("b");
    list.push("c");

    // Test iter
    let values: Vec<_> = list.iter().collect();
    assert_eq!(values.len(), 3);

    // Test iter_with_positions
    let pairs: Vec<_> = list.iter_with_positions().collect();
    assert_eq!(pairs.len(), 3);

    // Test iter_mut
    for value in list.iter_mut() {
        if let Value::Text(s) = value {
            s.push_str("_modified");
        }
    }

    assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("a_modified"));
    assert_eq!(list.get(1).and_then(|v| v.as_text()), Some("b_modified"));
    assert_eq!(list.get(2).and_then(|v| v.as_text()), Some("c_modified"));
}

// ===== MERGING =====

#[test]
fn test_list_merge() {
    let mut list1 = List::new();
    let mut list2 = List::new();

    let pos1 = Position::new(10, 1);
    let pos2 = Position::new(20, 1);
    let pos3 = Position::new(15, 1);

    list1.insert_at_position(pos1.clone(), "first");
    list1.insert_at_position(pos2.clone(), "second");

    list2.insert_at_position(pos2.clone(), "second_modified"); // Conflict
    list2.insert_at_position(pos3.clone(), "middle");

    list1.merge(&list2);

    // Should have merged
    assert_eq!(list1.len(), 3);
    assert_eq!(
        list1.get_by_position(&pos1).and_then(|v| v.as_text()),
        Some("first")
    );
    assert_eq!(
        list1.get_by_position(&pos2).and_then(|v| v.as_text()),
        Some("second_modified")
    );
    assert_eq!(
        list1.get_by_position(&pos3).and_then(|v| v.as_text()),
        Some("middle")
    );
}

// ===== FROM ITERATOR =====

#[test]
fn test_list_from_iterator() {
    let values = vec![
        Value::Text("a".to_string()),
        Value::Int(42),
        Value::Bool(true),
    ];

    let list: List = values.into_iter().collect();
    assert_eq!(list.len(), 3);
    assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("a"));
    assert_eq!(list.get(1).and_then(|v| v.as_int()), Some(42));
    assert_eq!(list.get(2).and_then(|v| v.as_bool()), Some(true));
}

// ===== PUSH OPERATIONS =====

#[test]
fn test_list_push_returns_index() {
    let mut list = List::new();

    // Test push returns correct sequential indices
    let idx1 = list.push("first");
    let idx2 = list.push("second");
    let idx3 = list.push("third");

    assert_eq!(idx1, 0);
    assert_eq!(idx2, 1);
    assert_eq!(idx3, 2);
    assert_eq!(list.len(), 3);

    // Verify values are accessible by returned indices
    assert_eq!(list.get(idx1).unwrap().as_text(), Some("first"));
    assert_eq!(list.get(idx2).unwrap().as_text(), Some("second"));
    assert_eq!(list.get(idx3).unwrap().as_text(), Some("third"));
}

#[test]
fn test_list_push_different_types() {
    let mut list = List::new();

    let idx1 = list.push("hello");
    let idx2 = list.push(42);
    let idx3 = list.push(true);
    let idx4 = list.push(3.13); // Use non-pi value to avoid clippy warning

    assert_eq!(idx1, 0);
    assert_eq!(idx2, 1);
    assert_eq!(idx3, 2);
    assert_eq!(idx4, 3);

    assert_eq!(list.get(0).unwrap().as_text(), Some("hello"));
    assert_eq!(list.get(1).unwrap().as_int(), Some(42));
    assert_eq!(list.get(2).unwrap().as_bool(), Some(true));
    assert_eq!(list.get(3).unwrap().as_int(), Some(3)); // float converted to int
}

#[test]
fn test_list_push_after_removals() {
    let mut list = List::new();

    // Add items
    list.push("a");
    list.push("b");
    list.push("c");

    // Remove middle item
    list.remove(1);
    assert_eq!(list.len(), 2);

    // Push should still return correct index
    let idx = list.push("d");
    assert_eq!(idx, 2);
    assert_eq!(list.len(), 3);
    assert_eq!(list.get(2).unwrap().as_text(), Some("d"));
}

// ===== INSERT OPERATIONS =====

#[test]
fn test_list_insert_at_valid_indices() {
    let mut list = List::new();

    // Insert at beginning of empty list
    assert!(list.insert(0, "first").is_ok());
    assert_eq!(list.len(), 1);
    assert_eq!(list.get(0).unwrap().as_text(), Some("first"));

    // Insert at end
    assert!(list.insert(1, "last").is_ok());
    assert_eq!(list.len(), 2);
    assert_eq!(list.get(1).unwrap().as_text(), Some("last"));

    // Insert in middle
    assert!(list.insert(1, "middle").is_ok());
    assert_eq!(list.len(), 3);
    assert_eq!(list.get(0).unwrap().as_text(), Some("first"));
    assert_eq!(list.get(1).unwrap().as_text(), Some("middle"));
    assert_eq!(list.get(2).unwrap().as_text(), Some("last"));
}

#[test]
fn test_list_insert_at_beginning() {
    let mut list = List::new();

    list.push("second");
    list.push("third");

    // Insert at beginning
    assert!(list.insert(0, "first").is_ok());
    assert_eq!(list.len(), 3);
    assert_eq!(list.get(0).unwrap().as_text(), Some("first"));
    assert_eq!(list.get(1).unwrap().as_text(), Some("second"));
    assert_eq!(list.get(2).unwrap().as_text(), Some("third"));
}

#[test]
fn test_list_insert_at_end() {
    let mut list = List::new();

    list.push("first");
    list.push("second");

    // Insert at end (equivalent to push)
    assert!(list.insert(2, "third").is_ok());
    assert_eq!(list.len(), 3);
    assert_eq!(list.get(2).unwrap().as_text(), Some("third"));
}

#[test]
fn test_list_insert_index_out_of_bounds() {
    let mut list = List::new();

    // Insert beyond bounds in empty list
    let result = list.insert(1, "invalid");
    assert!(result.is_err());
    match result.unwrap_err() {
        CRDTError::ListIndexOutOfBounds { index, len } => {
            assert_eq!(index, 1);
            assert_eq!(len, 0);
        }
        _ => panic!("Expected ListIndexOutOfBounds error"),
    }

    // Add some items
    list.push("first");
    list.push("second");

    // Insert way beyond bounds
    let result = list.insert(10, "invalid");
    assert!(result.is_err());
    match result.unwrap_err() {
        CRDTError::ListIndexOutOfBounds { index, len } => {
            assert_eq!(index, 10);
            assert_eq!(len, 2);
        }
        _ => panic!("Expected ListIndexOutOfBounds error"),
    }
}

#[test]
fn test_list_insert_mixed_with_push() {
    let mut list = List::new();

    // Mix insert and push operations
    let idx1 = list.push("a");
    assert!(list.insert(1, "c").is_ok());
    assert!(list.insert(1, "b").is_ok());
    let idx4 = list.push("d");

    assert_eq!(idx1, 0);
    assert_eq!(idx4, 3);
    assert_eq!(list.len(), 4);

    // Verify order
    assert_eq!(list.get(0).unwrap().as_text(), Some("a"));
    assert_eq!(list.get(1).unwrap().as_text(), Some("b"));
    assert_eq!(list.get(2).unwrap().as_text(), Some("c"));
    assert_eq!(list.get(3).unwrap().as_text(), Some("d"));
}

#[test]
fn test_list_insert_maintains_stable_ordering() {
    let mut list = List::new();

    // Add initial items
    list.push("first");
    list.push("third");

    // Insert in middle
    assert!(list.insert(1, "second").is_ok());

    // Create another list with same operations
    let mut list2 = List::new();
    list2.push("first");
    list2.push("third");
    assert!(list2.insert(1, "second").is_ok());

    // Both lists should have same order
    assert_eq!(list.len(), list2.len());
    for i in 0..list.len() {
        assert_eq!(
            list.get(i).unwrap().as_text(),
            list2.get(i).unwrap().as_text()
        );
    }
}

#[test]
fn test_list_insert_with_nested_values() {
    let mut list = List::new();

    // Insert nested structures
    let mut nested_map = Doc::new();
    nested_map.set("name", "Alice");
    nested_map.set("age", 30);

    let mut nested_list = List::new();
    nested_list.push(1);
    nested_list.push(2);
    nested_list.push(3);

    assert!(list.insert(0, nested_map).is_ok());
    assert!(list.insert(1, nested_list).is_ok());

    assert_eq!(list.len(), 2);
    assert!(list.get(0).unwrap().as_node().is_some());
    assert!(list.get(1).unwrap().as_list().is_some());

    // Verify nested content
    let map = list.get(0).unwrap().as_node().unwrap();
    assert_eq!(map.get_text("name"), Some("Alice"));
    assert_eq!(map.get_int("age"), Some(30));

    let inner_list = list.get(1).unwrap().as_list().unwrap();
    assert_eq!(inner_list.len(), 3);
    assert_eq!(inner_list.get(0).unwrap().as_int(), Some(1));
}

#[test]
fn test_list_insert_after_removals() {
    let mut list = List::new();

    // Add items
    list.push("a");
    list.push("b");
    list.push("c");

    // Remove middle item
    list.remove(1);
    assert_eq!(list.len(), 2);

    // Insert should work correctly
    assert!(list.insert(1, "new").is_ok());
    assert_eq!(list.len(), 3);
    assert_eq!(list.get(1).unwrap().as_text(), Some("new"));
}

// ===== ERROR HANDLING =====

#[test]
fn test_list_error_integration() {
    let mut list = List::new();

    let result = list.insert(5, "test");
    assert!(result.is_err());

    let error = result.unwrap_err();
    assert!(error.is_list_error());
    assert!(!error.is_merge_error());
    assert!(!error.is_serialization_error());
    assert!(!error.is_type_error());
    assert!(!error.is_list_operation_error());
    assert!(!error.is_doc_error());
    assert!(!error.is_nested_error());
    assert!(!error.is_not_found_error());
}

// ===== JSON SERIALIZATION =====

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

// ===== SERDE ROUND-TRIP TESTS =====

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

// ===== INTEGRATION WITH HELPERS =====

#[test]
fn test_list_using_helpers() {
    // Test using the helper functions
    let list = setup_test_list();
    assert_eq!(list.len(), 3);
    assert_eq!(list.get(0).unwrap().as_text(), Some("first"));
    assert_eq!(list.get(1).unwrap().as_text(), Some("second"));
    assert_eq!(list.get(2).unwrap().as_text(), Some("third"));

    // Test mixed list helper
    let mixed_list = create_mixed_list();
    assert_eq!(mixed_list.len(), 5);
    assert_eq!(mixed_list.get(0).unwrap(), &Value::Null);
    assert_eq!(mixed_list.get(1).unwrap().as_bool(), Some(false));
    assert_eq!(mixed_list.get(2).unwrap().as_int(), Some(456));

    // Test positioned list helper
    let (positioned_list, positions) = create_positioned_list();
    assert_eq!(positioned_list.len(), 3);
    assert_eq!(positions.len(), 3);

    // Test bounds checking helper
    test_list_bounds_checking(&positioned_list);

    // Test JSON roundtrip using helper
    test_json_roundtrip(&positioned_list).unwrap();
}

// ===== CUSTOM MERGE TESTS =====

#[test]
fn test_list_merge_properties() {
    let mut list1 = setup_test_list();
    let list2 = create_mixed_list();

    // Create a backup for commutativity test
    let mut list1_backup = list1.clone();
    let mut list2_backup = list2.clone();

    // Test that merge works (non-destructive test)
    list1.merge(&list2);
    assert!(list1.len() >= list2.len()); // Should have at least as many items

    // Test merge commutativity: list1 ⊕ list2 should have same result as list2 ⊕ list1
    list1_backup.merge(&list2_backup);
    list2_backup.merge(&setup_test_list());

    // The results should be deterministic even if order differs
    assert_eq!(list1_backup.len(), list2_backup.len());
}
