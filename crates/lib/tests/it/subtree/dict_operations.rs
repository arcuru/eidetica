//! Dict subtree operation tests
//!
//! This module contains tests for Dict subtree functionality including
//! basic CRUD operations, List operations, nested values, and persistence.

use super::helpers::*;
use crate::helpers::*;
use eidetica::crdt::Map;
use eidetica::crdt::map::Value;
use eidetica::subtree::Dict;

#[test]
fn test_dict_set_and_get_via_op() {
    let tree = setup_tree();

    // Use helper to create initial data
    let initial_data = &[("key1", "value1"), ("key2", "value2")];
    create_dict_operation(&tree, "my_kv", initial_data);

    // Test operation-level modifications
    let op = tree.new_operation().expect("Failed to start operation");
    let dict = op.get_subtree::<Dict>("my_kv").expect("Failed to get Dict");

    // Verify initial values are accessible
    assert_dict_value(&dict, "key1", "value1");
    assert_dict_value(&dict, "key2", "value2");

    // Test get_string convenience method
    assert_eq!(
        dict.get_string("key1").expect("Failed get_string key1"),
        "value1"
    );
    assert_eq!(
        dict.get_string("key2").expect("Failed get_string key2"),
        "value2"
    );

    // Test overwrite
    dict.set("key1", "value1_updated")
        .expect("Failed to overwrite key1");
    assert_dict_value(&dict, "key1", "value1_updated");

    // Test non-existent key
    assert_key_not_found(dict.get("non_existent"));

    op.commit().expect("Failed to commit operation");

    // Verify final state using helper
    let expected_final = &[("key1", "value1_updated"), ("key2", "value2")];
    assert_dict_viewer_data(&tree, "my_kv", expected_final);
}

#[test]
fn test_dict_get_all_via_viewer() {
    let tree = setup_tree();

    // Test dict persistence helper
    test_dict_persistence(&tree, "my_kv");

    // Verify get_all using a viewer
    let viewer = tree
        .get_subtree_viewer::<Dict>("my_kv")
        .expect("Failed to get viewer");
    let all_data_crdt = viewer.get_all().expect("Failed to get all data");
    let all_data_map = all_data_crdt.as_hashmap();

    assert_eq!(all_data_map.len(), 3);
    assert_eq!(
        all_data_map.get("key_a"),
        Some(&Value::Text("val_a".to_string()))
    );
    assert_eq!(
        all_data_map.get("key_b"),
        Some(&Value::Text("val_b_updated".to_string()))
    );
    assert_eq!(
        all_data_map.get("key_c"),
        Some(&Value::Text("val_c".to_string()))
    );
}

#[test]
fn test_dict_get_all_empty() {
    let tree = setup_tree();

    // Get viewer for a non-existent subtree
    let viewer = tree
        .get_subtree_viewer::<Dict>("empty_kv")
        .expect("Failed to get viewer for empty");
    let all_data_crdt = viewer.get_all().expect("Failed to get all data from empty");
    let all_data_map = all_data_crdt.as_hashmap();

    assert!(all_data_map.is_empty());
}

#[test]
fn test_dict_delete() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let dict = op.get_subtree::<Dict>("my_kv").expect("Failed to get Dict");

        // Set initial values
        dict.set("key1", "value1").expect("Failed to set key1");
        dict.set("key2", "value2").expect("Failed to set key2");

        // Delete a key
        dict.delete("key1").expect("Failed to delete key1");

        // Verify key1 is deleted
        assert_key_not_found(dict.get("key1"));

        // key2 should still be accessible
        assert_dict_value(&dict, "key2", "value2");
    }

    // Commit the operation
    op.commit().expect("Failed to commit operation");

    // Verify the deletion persisted
    let viewer = tree
        .get_subtree_viewer::<Dict>("my_kv")
        .expect("Failed to get viewer");
    assert_key_not_found(viewer.get("key1"));

    assert_dict_value(&viewer, "key2", "value2");
}

#[test]
fn test_dict_set_value() {
    let tree = setup_tree();

    // Use helper to create nested map operation
    create_dict_with_nested_map(&tree, "my_kv");

    // Get viewer to verify persistence
    let viewer = tree
        .get_subtree_viewer::<Dict>("my_kv")
        .expect("Failed to get viewer");

    // Check string value persisted
    assert_dict_value(&viewer, "key1", "value1");

    // Check nested map structure
    assert_dict_nested_map(&viewer, "key2", &[("inner", "nested_value")]);
}

#[test]
fn test_dict_list_basic_operations() {
    let tree = setup_tree();

    // Use helper to create Dict with list
    let list_items = &["apple", "banana", "orange"];
    create_dict_with_list(&tree, "my_kv", list_items);

    // Verify with viewer
    assert_dict_list_data(&tree, "my_kv", "fruits", list_items);
}

#[test]
fn test_dict_list_nonexistent_key() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let dict = op.get_subtree::<Dict>("my_kv").expect("Failed to get Dict");

        // Test getting non-existent list should return NotFound error
        assert_key_not_found(dict.get("nonexistent"));

        // Test getting non-existent list with get_list should also return NotFound
        let list_result = dict.get_list("nonexistent");
        assert!(list_result.is_err());

        // Create a new list
        let mut new_list = eidetica::crdt::map::List::new();
        new_list.push(Value::Text("first_item".to_string()));

        dict.set_list("new_list", new_list)
            .expect("Failed to set new list");

        // Verify the new list was created
        let retrieved_list = dict.get_list("new_list").expect("Failed to get new list");
        assert_eq!(retrieved_list.len(), 1);
        assert_eq!(
            retrieved_list.get(0),
            Some(&Value::Text("first_item".to_string()))
        );
    }
}

#[test]
fn test_dict_list_persistence() {
    let tree = setup_tree();

    // Create list in first operation
    let op1 = tree.new_operation().expect("Failed to start op1");
    {
        let dict = op1
            .get_subtree::<Dict>("my_kv")
            .expect("Failed to get Dict");

        let mut colors = eidetica::crdt::map::List::new();
        colors.push(Value::Text("red".to_string()));
        colors.push(Value::Text("green".to_string()));

        dict.set_list("colors", colors)
            .expect("Failed to set colors list");
    }
    op1.commit().expect("Failed to commit op1");

    // Modify list in second operation
    let op2 = tree.new_operation().expect("Failed to start op2");
    {
        let dict = op2
            .get_subtree::<Dict>("my_kv")
            .expect("Failed to get Dict");

        // List should persist from previous operation
        let colors = dict.get_list("colors").expect("Failed to get colors list");
        assert_eq!(colors.len(), 2);
        assert_eq!(colors.get(0), Some(&Value::Text("red".to_string())));
        assert_eq!(colors.get(1), Some(&Value::Text("green".to_string())));

        // Modify the list - remove first element and add blue
        let mut updated_colors = colors.clone();
        updated_colors.remove(0); // Remove red
        updated_colors.push(Value::Text("blue".to_string())); // Add blue

        dict.set_list("colors", updated_colors)
            .expect("Failed to update colors list");
    }
    op2.commit().expect("Failed to commit op2");

    // Verify final state
    assert_dict_list_data(&tree, "my_kv", "colors", &["green", "blue"]);
}

#[test]
fn test_dict_update_nested_value() {
    let tree = setup_tree();

    // First operation: Create initial nested structure
    let op1 = tree.new_operation().expect("Op1: Failed to start");
    {
        let dict = op1
            .get_subtree::<Dict>("nested_test")
            .expect("Op1: Failed to get Dict");

        // Create level1 -> level2_str structure
        let mut l1_map = Map::new();
        l1_map.set_string("level2_str", "initial_value");
        dict.set_value("level1", l1_map)
            .expect("Op1: Failed to set level1");
    }
    op1.commit().expect("Op1: Failed to commit");

    // Second operation: Update with another structure
    let op2 = tree.new_operation().expect("Op2: Failed to start");
    {
        let dict = op2
            .get_subtree::<Dict>("nested_test")
            .expect("Op2: Failed to get Dict");

        // Create an entirely new map structure that will replace the old one
        let mut l2_map = Map::new();
        l2_map.set_string("deep_key", "deep_value");

        let mut new_l1_map = Map::new();
        new_l1_map.set_map("level2_map", l2_map);

        // Completely replace the previous value at level1
        dict.set_value("level1", new_l1_map.clone())
            .expect("Op2: Failed to overwrite level1");

        // Verify the update within the same operation
        match dict.get("level1").expect("Failed to get level1") {
            Value::Node(retrieved_l1_map) => {
                // Check if level2_map exists with the expected content
                match retrieved_l1_map.get("level2_map") {
                    Some(Value::Node(retrieved_l2_map)) => match retrieved_l2_map.get("deep_key") {
                        Some(Value::Text(val)) => assert_eq!(val, "deep_value"),
                        _ => panic!("Expected string 'deep_value' at deep_key"),
                    },
                    _ => panic!("Expected 'level2_map' to be a map"),
                }
            }
            _ => panic!("Expected 'level1' to be a map"),
        }
    }
    op2.commit().expect("Op2: Failed to commit");

    // Verify the update persists after commit
    let viewer = tree
        .get_subtree_viewer::<Dict>("nested_test")
        .expect("Failed to get viewer");

    // Verify the structure after commit
    match viewer.get("level1").expect("Viewer: Failed to get level1") {
        Value::Node(retrieved_l1_map) => {
            // Check if level2_map exists with expected content
            match retrieved_l1_map.get("level2_map") {
                Some(Value::Node(retrieved_l2_map)) => match retrieved_l2_map.get("deep_key") {
                    Some(Value::Text(val)) => assert_eq!(val, "deep_value"),
                    _ => panic!("Viewer: Expected string 'deep_value' at deep_key"),
                },
                _ => panic!("Viewer: Expected 'level2_map' to be a map"),
            }
        }
        _ => panic!("Viewer: Expected 'level1' to be a map"),
    }
}

#[test]
fn test_dict_comprehensive_operations() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let dict = op
            .get_subtree::<Dict>("test_store")
            .expect("Failed to get Dict");

        // Set basic string values
        dict.set("key1", "value1").expect("Failed to set key1");
        dict.set("key2", "value2").expect("Failed to set key2");

        // Set a nested map value
        let mut nested = Map::new();
        nested.set_string("nested_key1", "nested_value1");
        nested.set_string("nested_key2", "nested_value2");
        dict.set_value("nested", Value::Node(nested.clone().into()))
            .expect("Failed to set nested map");
    }

    // Commit the operation
    op.commit().expect("Failed to commit operation");

    // Get a viewer to check the subtree
    let viewer = tree
        .get_subtree_viewer::<Dict>("test_store")
        .expect("Failed to get viewer");

    // Check string values
    assert_dict_value(&viewer, "key1", "value1");
    assert_dict_value(&viewer, "key2", "value2");

    // Check nested map
    assert_dict_nested_map(
        &viewer,
        "nested",
        &[
            ("nested_key1", "nested_value1"),
            ("nested_key2", "nested_value2"),
        ],
    );

    // Check non-existent key
    assert_key_not_found(viewer.get("non_existent"));
}

#[test]
fn test_empty_dict_behavior() {
    let tree = setup_tree();

    // Test empty Dict behavior
    assert_dict_viewer_count(&tree, "empty_dict", 0);

    let dict_viewer = tree
        .get_subtree_viewer::<Dict>("empty_dict")
        .expect("Failed to get empty Dict viewer");
    assert_key_not_found(dict_viewer.get("any_key"));
}
