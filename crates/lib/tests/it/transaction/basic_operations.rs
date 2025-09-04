//! Basic AtomicOp operation tests
//!
//! This module contains tests for fundamental AtomicOp functionality including
//! Doc operations, multiple subtrees, empty subtree handling, and parent relationships.

use super::helpers::*;
use crate::helpers::*;
use eidetica::store::{DocStore, Store};

#[test]
fn test_atomicop_through_dict() {
    // Create a backend and a tree
    let tree = setup_tree();

    // Create a new operation
    let operation = tree.new_operation().unwrap();

    // Get a Doc subtree, which will use AtomicOp internally
    let dict = DocStore::new(&operation, "test").unwrap();

    // Set a value in the Doc, which will use AtomicOp::update_subtree internally
    dict.set("key", "value").unwrap();

    // Commit the operation
    operation.commit().unwrap();

    // Use a new operation to read the data
    let read_op = tree.new_operation().unwrap();
    let read_store = DocStore::new(&read_op, "test").unwrap();

    // Verify the value was set correctly
    assert_dict_value(&read_store, "key", "value");

    // Also test the get_string convenience method
    assert_eq!(read_store.get_string("key").unwrap(), "value");
}

#[test]
fn test_atomicop_multiple_subtrees() {
    // Create a backend and a tree
    let tree = setup_tree();

    // Create a new operation
    let operation = tree.new_operation().unwrap();

    // Create two different Doc subtrees
    let store1 = DocStore::new(&operation, "store1").unwrap();
    let store2 = DocStore::new(&operation, "store2").unwrap();

    // Set values in each store
    store1.set("key1", "value1").unwrap();
    store2.set("key2", "value2").unwrap();

    // Update a value in store1
    store1.set("key1", "updated").unwrap();

    // Commit the operation
    operation.commit().unwrap();

    // Create a new operation to read the data
    let read_op = tree.new_operation().unwrap();
    let store1_read = DocStore::new(&read_op, "store1").unwrap();
    let store2_read = DocStore::new(&read_op, "store2").unwrap();

    // Verify values in both stores using helpers
    assert_dict_contains(&store1_read, &[("key1", "updated")]);
    assert_dict_contains(&store2_read, &[("key2", "value2")]);
}

#[test]
fn test_atomicop_empty_subtree_removal() {
    // Create a backend and a tree
    let tree = setup_tree();

    // Create a new operation
    let operation = tree.new_operation().unwrap();

    // Create a Doc subtree but don't add any data (will be empty)
    let _empty_store = DocStore::new(&operation, "empty").unwrap();

    // Create another Doc and add data
    let data_store = DocStore::new(&operation, "data").unwrap();
    data_store.set("key", "value").unwrap();

    // Commit the operation - should remove the empty subtree
    operation.commit().unwrap();

    // Create a new operation to check if subtrees exist
    let read_op = tree.new_operation().unwrap();

    // Try to access both subtrees
    let data_result = DocStore::new(&read_op, "data");
    let empty_result = DocStore::new(&read_op, "empty");

    // The data subtree should be accessible
    assert!(data_result.is_ok());

    // The empty subtree should have been removed, but accessing it doesn't fail
    // because Doc creates it if it doesn't exist
    assert!(empty_result.is_ok());

    // However, the empty subtree should not have any data
    let empty_store = empty_result.unwrap();
    // If we try to get any key from the empty store, it should return NotFound
    assert_key_not_found(empty_store.get("any_key"));
}

#[test]
fn test_atomicop_parent_relationships() {
    // Create a backend and a tree
    let tree = setup_tree();

    // Create first operation and set data
    let op1 = tree.new_operation().unwrap();
    let store1 = DocStore::new(&op1, "data").unwrap();
    store1.set("first", "entry").unwrap();
    op1.commit().unwrap();

    // Create second operation that will use the first as parent
    let op2 = tree.new_operation().unwrap();
    let store2 = DocStore::new(&op2, "data").unwrap();
    store2.set("second", "entry").unwrap();
    op2.commit().unwrap();

    // Create a third operation to read all entries
    let op3 = tree.new_operation().unwrap();
    let store3 = DocStore::new(&op3, "data").unwrap();

    // Get all data - should include both entries due to CRDT merge
    let all_data = get_dict_data(&store3);

    // Verify both entries are included in merged data using helpers
    assert_map_data(&all_data, &[("first", "entry"), ("second", "entry")]);
}
