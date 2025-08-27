//! Data operation tests for AtomicOp
//!
//! This module contains tests focused on data manipulation including
//! deletes, nested values, and staging isolation.

use super::helpers::*;
use crate::helpers::*;
use eidetica::constants::SETTINGS;
use eidetica::crdt::Doc;
use eidetica::crdt::doc::Value;
use eidetica::subtree::{DocStore, SubTree};

#[test]
fn test_atomicop_with_delete() {
    // Create a backend and a tree
    let tree = setup_tree();

    // Create an operation and add some data
    let op1 = tree.new_operation().unwrap();
    let store1 = DocStore::new(&op1, "data").unwrap();
    store1.set("key1", "value1").unwrap();
    store1.set("key2", "value2").unwrap();
    op1.commit().unwrap();

    // Create another operation to delete a key
    let op2 = tree.new_operation().unwrap();
    let store2 = DocStore::new(&op2, "data").unwrap();
    store2.delete("key1").unwrap();
    op2.commit().unwrap();

    // Verify with a third operation
    let op3 = tree.new_operation().unwrap();
    let store3 = DocStore::new(&op3, "data").unwrap();

    // key1 should be deleted
    assert_key_not_found(store3.get("key1"));

    // key2 should still exist
    assert_dict_value(&store3, "key2", "value2");

    // Check the full state with tombstone using helpers
    let all_data = get_dict_data(&store3);
    assert_has_tombstone(&all_data, "key1");
    assert_tombstone_hidden(&all_data, "key1");
    assert_map_data(&all_data, &[("key2", "value2")]);
}

#[test]
fn test_atomicop_nested_values() {
    const TEST_KEY: &str = "test_key";
    let (_db, tree) = setup_db_and_tree_with_key(TEST_KEY);

    // Create an operation
    let op1 = tree.new_operation().unwrap();
    let store1 = DocStore::new(&op1, "data").unwrap();

    // Set a regular string value
    store1.set("string_key", "string_value").unwrap();

    // Create and set a nested map value
    let mut nested = Doc::new();
    nested.set_string("inner1".to_string(), "value1".to_string());
    nested.set_string("inner2".to_string(), "value2".to_string());

    // Use the new set_value method to store a map
    store1.set_value("map_key", nested).unwrap();

    // Commit the operation
    op1.commit().unwrap();

    // Verify with a new operation
    let op2 = tree.new_operation().unwrap();
    let store2 = DocStore::new(&op2, "data").unwrap();

    // Check the string value
    match store2.get("string_key").unwrap() {
        Value::Text(value) => assert_eq!(value, "string_value"),
        _ => panic!("Expected string value"),
    }

    // Check the nested map
    match store2.get("map_key").unwrap() {
        Value::Node(map) => {
            match map.get("inner1") {
                Some(Value::Text(value)) => assert_eq!(value, "value1"),
                _ => panic!("Expected string value for inner1"),
            }
            match map.get("inner2") {
                Some(Value::Text(value)) => assert_eq!(value, "value2"),
                _ => panic!("Expected string value for inner2"),
            }
        }
        _ => panic!("Expected map value"),
    }
}

#[test]
fn test_atomicop_staged_data_isolation() {
    let tree = setup_tree();

    // Create initial data
    let op1 = tree.new_operation().unwrap();
    let store1 = op1.get_subtree::<DocStore>("data").unwrap();
    store1.set("key1", "committed_value").unwrap();
    let entry1_id = op1.commit().unwrap();

    // Create operation from entry1
    let op2 = tree
        .new_operation_with_tips(std::slice::from_ref(&entry1_id))
        .unwrap();
    let store2 = op2.get_subtree::<DocStore>("data").unwrap();

    // Initially should see committed data
    assert_dict_value(&store2, "key1", "committed_value");

    // Stage new data (not yet committed)
    store2.set("key1", "staged_value").unwrap();
    store2.set("key2", "new_staged").unwrap();

    // Should now see staged data
    assert_dict_value(&store2, "key1", "staged_value");
    assert_dict_value(&store2, "key2", "new_staged");

    // Create another operation from same tip - should not see staged data
    let op3 = tree.new_operation_with_tips([entry1_id]).unwrap();
    let store3 = op3.get_subtree::<DocStore>("data").unwrap();

    // Should see original committed data, not staged data from op2
    assert_dict_value(&store3, "key1", "committed_value");
    assert_key_not_found(store3.get("key2"));

    // Commit op2
    let entry2_id = op2.commit().unwrap();

    // Create operation from entry2 - should see committed staged data
    let op4 = tree.new_operation_with_tips([entry2_id]).unwrap();
    let store4 = op4.get_subtree::<DocStore>("data").unwrap();

    assert_dict_value(&store4, "key1", "staged_value");
    assert_dict_value(&store4, "key2", "new_staged");
}

#[test]
fn test_metadata_for_settings_entries() {
    let tree = setup_tree_with_settings(&[("name", "test_tree")]);

    // Create a settings update
    let settings_op = tree.new_operation().unwrap();
    let settings_subtree = settings_op.get_subtree::<DocStore>(SETTINGS).unwrap();
    settings_subtree.set("version", "1.0").unwrap();
    let settings_id = settings_op.commit().unwrap();

    // Now create a data entry (not touching settings)
    let data_op = tree.new_operation().unwrap();
    let data_subtree = data_op.get_subtree::<DocStore>("data").unwrap();
    data_subtree.set("key1", "value1").unwrap();
    let data_id = data_op.commit().unwrap();

    // Get both entries from the backend through the tree
    let settings_entry = tree.get_entry(&settings_id).unwrap();
    let data_entry = tree.get_entry(&data_id).unwrap();

    // Verify settings entry has metadata with settings tips
    assert!(settings_entry.metadata().is_some());

    // Verify data entry has metadata with settings_tips field
    let metadata = data_entry.metadata().unwrap();
    let metadata_obj: serde_json::Value = serde_json::from_str(metadata).unwrap();
    assert!(
        metadata_obj.get("settings_tips").is_some(),
        "Metadata should include settings_tips field"
    );
}

#[test]
fn test_delete_operations_with_helpers() {
    let tree = setup_tree_with_data(&[(
        "data",
        &[
            ("keep1", "value1"),
            ("delete_me", "temp"),
            ("keep2", "value2"),
        ] as &[(&str, &str)],
    )]);

    // Test delete operation helper
    let delete_id = test_delete_operation(&tree, "data", "delete_me");
    assert!(!delete_id.to_string().is_empty());

    // Verify deletion with new operation
    let read_op = tree.new_operation().unwrap();
    let store = DocStore::new(&read_op, "data").unwrap();
    let all_data = get_dict_data(&store);

    // Test tombstone helpers
    assert_has_tombstone(&all_data, "delete_me");
    assert_tombstone_hidden(&all_data, "delete_me");

    // Verify other keys still exist
    assert_map_data(&all_data, &[("keep1", "value1"), ("keep2", "value2")]);
}

#[test]
fn test_nested_map_operations() {
    let tree = setup_tree();

    // Test nested map creation helper
    let nested_value = create_nested_map(&[("key1", "val1"), ("key2", "val2")]);

    match nested_value {
        Value::Node(map) => {
            assert_eq!(map.get_text("key1"), Some("val1"));
            assert_eq!(map.get_text("key2"), Some("val2"));
        }
        _ => panic!("Expected Map value"),
    }

    // Test integration with AtomicOp
    let entry_id = create_operation_with_nested_data(&tree);
    assert!(!entry_id.to_string().is_empty());

    // Verify using nested data helper
    let read_op = tree.new_operation().unwrap();
    let store = DocStore::new(&read_op, "data").unwrap();

    assert_nested_data(
        &store,
        "string_key",
        "map_key",
        &[("inner1", "value1"), ("inner2", "value2")],
    );
}

#[test]
fn test_nested_data_operations_with_helpers() {
    let tree = setup_tree();

    // Test nested data creation helper
    let entry_id = create_operation_with_nested_data(&tree);
    assert!(!entry_id.to_string().is_empty());

    // Verify using nested data helper
    let read_op = tree.new_operation().unwrap();
    let store = DocStore::new(&read_op, "data").unwrap();

    assert_nested_data(
        &store,
        "string_key",
        "map_key",
        &[("inner1", "value1"), ("inner2", "value2")],
    );
}
