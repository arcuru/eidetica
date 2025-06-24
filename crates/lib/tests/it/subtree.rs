use crate::helpers::*;
use eidetica::data::{KVNested, NestedValue};
use eidetica::subtree::{KVStore, RowStore};
use serde::{Deserialize, Serialize};

#[cfg(feature = "y-crdt")]
use eidetica::subtree::YrsStore;
#[cfg(feature = "y-crdt")]
use yrs::{Doc, GetString, Map, ReadTxn, Text, Transact};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct TestRecord {
    name: String,
    age: u32,
    email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct SimpleRecord {
    value: i32,
}

#[test]
fn test_kvstore_set_and_get_via_op() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let kv_store = op
            .get_subtree::<KVStore>("my_kv")
            .expect("Failed to get KVStore");

        // Set initial values
        kv_store.set("key1", "value1").expect("Failed to set key1");
        kv_store.set("key2", "value2").expect("Failed to set key2");

        // Get staged values within the same operation
        assert_kvstore_value(&kv_store, "key1", "value1");
        assert_kvstore_value(&kv_store, "key2", "value2");

        // Using get_string convenience method
        assert_eq!(
            kv_store
                .get_string("key1")
                .expect("Failed get_string staged key1"),
            "value1"
        );
        assert_eq!(
            kv_store
                .get_string("key2")
                .expect("Failed get_string staged key2"),
            "value2"
        );

        // Overwrite a value
        kv_store
            .set("key1", "value1_updated")
            .expect("Failed to overwrite key1");

        assert_kvstore_value(&kv_store, "key1", "value1_updated");

        // Get non-existent key
        assert_key_not_found(kv_store.get("non_existent"));
    }

    // Commit the operation
    op.commit().expect("Failed to commit operation");

    // Verify final state with a viewer
    let viewer = tree
        .get_subtree_viewer::<KVStore>("my_kv")
        .expect("Failed to get viewer");

    assert_kvstore_value(&viewer, "key1", "value1_updated");
    assert_kvstore_value(&viewer, "key2", "value2");
    assert_key_not_found(viewer.get("non_existent"));
}

#[test]
fn test_kvstore_get_all_via_viewer() {
    let tree = setup_tree();

    // Op 1: Set initial data
    let op1 = tree.new_operation().expect("Op1: Failed start");
    {
        let kv_store = op1
            .get_subtree::<KVStore>("my_kv")
            .expect("Op1: Failed get");
        kv_store.set("key_a", "val_a").expect("Op1: Failed set a");
        kv_store.set("key_b", "val_b").expect("Op1: Failed set b");
    }
    op1.commit().expect("Op1: Failed commit");

    // Op 2: Update one, add another
    let op2 = tree.new_operation().expect("Op2: Failed start");
    {
        let kv_store = op2
            .get_subtree::<KVStore>("my_kv")
            .expect("Op2: Failed get");
        kv_store
            .set("key_b", "val_b_updated")
            .expect("Op2: Failed update b");
        kv_store.set("key_c", "val_c").expect("Op2: Failed set c");
    }
    op2.commit().expect("Op2: Failed commit");

    // Verify get_all using a viewer
    let viewer = tree
        .get_subtree_viewer::<KVStore>("my_kv")
        .expect("Failed to get viewer");
    let all_data_crdt = viewer.get_all().expect("Failed to get all data");
    let all_data_map = all_data_crdt.as_hashmap();

    assert_eq!(all_data_map.len(), 3);
    assert_eq!(
        all_data_map.get("key_a"),
        Some(&NestedValue::String("val_a".to_string()))
    );
    assert_eq!(
        all_data_map.get("key_b"),
        Some(&NestedValue::String("val_b_updated".to_string()))
    );
    assert_eq!(
        all_data_map.get("key_c"),
        Some(&NestedValue::String("val_c".to_string()))
    );
}

#[test]
fn test_kvstore_get_all_empty() {
    let tree = setup_tree();

    // Get viewer for a non-existent subtree
    let viewer = tree
        .get_subtree_viewer::<KVStore>("empty_kv")
        .expect("Failed to get viewer for empty");
    let all_data_crdt = viewer.get_all().expect("Failed to get all data from empty");
    let all_data_map = all_data_crdt.as_hashmap();

    assert!(all_data_map.is_empty());
}

#[test]
fn test_kvstore_delete() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let kv_store = op
            .get_subtree::<KVStore>("my_kv")
            .expect("Failed to get KVStore");

        // Set initial values
        kv_store.set("key1", "value1").expect("Failed to set key1");
        kv_store.set("key2", "value2").expect("Failed to set key2");

        // Delete a key
        kv_store.delete("key1").expect("Failed to delete key1");

        // Verify key1 is deleted
        assert_key_not_found(kv_store.get("key1"));

        // key2 should still be accessible
        assert_kvstore_value(&kv_store, "key2", "value2");
    }

    // Commit the operation
    op.commit().expect("Failed to commit operation");

    // Verify the deletion persisted
    let viewer = tree
        .get_subtree_viewer::<KVStore>("my_kv")
        .expect("Failed to get viewer");
    assert_key_not_found(viewer.get("key1"));

    assert_kvstore_value(&viewer, "key2", "value2");
}

#[test]
fn test_kvstore_set_value() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let kv_store = op
            .get_subtree::<KVStore>("my_kv")
            .expect("Failed to get KVStore");

        // Set string value
        kv_store.set("key1", "value1").expect("Failed to set key1");

        // Set map value
        let mut nested = KVNested::new();
        nested.set_string("inner", "nested_value");
        kv_store
            .set_value("key2", NestedValue::Map(nested.clone()))
            .expect("Failed to set key2");

        // Verify string value
        assert_kvstore_value(&kv_store, "key1", "value1");

        // Verify map value exists and has correct structure
        match kv_store.get("key2").expect("Failed to get key2") {
            NestedValue::Map(map) => match map.get("inner") {
                Some(NestedValue::String(value)) => assert_eq!(value, "nested_value"),
                _ => panic!("Expected string value in nested map"),
            },
            _ => panic!("Expected map value for key2"),
        }
    }

    // Commit the operation
    op.commit().expect("Failed to commit operation");

    // Get viewer to verify persistence
    let viewer = tree
        .get_subtree_viewer::<KVStore>("my_kv")
        .expect("Failed to get viewer");

    // Check string value persisted
    assert_kvstore_value(&viewer, "key1", "value1");

    // Check map value persisted and can be accessed
    match viewer.get("key2").expect("Failed to get key2 from viewer") {
        NestedValue::Map(map) => match map.get("inner") {
            Some(NestedValue::String(value)) => assert_eq!(value, "nested_value"),
            _ => panic!("Expected string value in nested map from viewer"),
        },
        _ => panic!("Expected map value for key2 from viewer"),
    }
}

#[test]
fn test_kvstore_array_basic_operations() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    let mut element_ids = Vec::new();

    {
        let kv_store = op
            .get_subtree::<KVStore>("my_kv")
            .expect("Failed to get KVStore");

        // Test array operations - add strings as NestedValue
        let element_id1 = kv_store
            .array_add("fruits", NestedValue::String("apple".to_string()))
            .expect("Failed to add apple");
        let element_id2 = kv_store
            .array_add("fruits", NestedValue::String("banana".to_string()))
            .expect("Failed to add banana");
        let element_id3 = kv_store
            .array_add("fruits", NestedValue::String("orange".to_string()))
            .expect("Failed to add orange");

        element_ids.push(element_id1.clone());
        element_ids.push(element_id2.clone());
        element_ids.push(element_id3.clone());

        // Test array length
        assert_eq!(
            kv_store
                .array_len("fruits")
                .expect("Failed to get array length"),
            3
        );

        // Test getting elements by ID
        let apple = kv_store
            .array_get("fruits", &element_id1)
            .expect("Failed to get apple");
        assert_eq!(apple, Some(NestedValue::String("apple".to_string())));

        // Test getting all IDs (should be in UUID-sorted order)
        let ids = kv_store
            .array_ids("fruits")
            .expect("Failed to get array IDs");
        assert_eq!(ids.len(), 3);

        // Verify IDs are sorted
        let mut sorted_ids = ids.clone();
        sorted_ids.sort();
        assert_eq!(ids, sorted_ids);

        // Test remove by ID
        let removed = kv_store
            .array_remove("fruits", &element_id3)
            .expect("Failed to remove orange");
        assert!(removed);

        // Verify removal
        assert_eq!(
            kv_store
                .array_len("fruits")
                .expect("Failed to get array length after remove"),
            2
        );
        let orange_removed = kv_store
            .array_get("fruits", &element_id3)
            .expect("Failed to check orange");
        assert_eq!(orange_removed, None);
    }

    // Commit the operation
    op.commit().expect("Failed to commit operation");

    // Verify with viewer
    let viewer = tree
        .get_subtree_viewer::<KVStore>("my_kv")
        .expect("Failed to get viewer");

    assert_eq!(
        viewer
            .array_len("fruits")
            .expect("Failed to get array length from viewer"),
        2
    );
    let apple = viewer
        .array_get("fruits", &element_ids[0])
        .expect("Failed to get apple from viewer");
    assert_eq!(apple, Some(NestedValue::String("apple".to_string())));
}

#[test]
fn test_kvstore_array_nonexistent_key() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let kv_store = op
            .get_subtree::<KVStore>("my_kv")
            .expect("Failed to get KVStore");

        // Test operations on non-existent array
        let empty_ids = kv_store
            .array_ids("nonexistent")
            .expect("Failed to get nonexistent array IDs");
        assert!(empty_ids.is_empty());

        assert_eq!(
            kv_store
                .array_len("nonexistent")
                .expect("Failed to get length"),
            0
        );
        assert!(
            kv_store
                .array_is_empty("nonexistent")
                .expect("Failed to check empty")
        );

        // Test getting from non-existent array
        let nothing = kv_store
            .array_get("nonexistent", "fake-uuid")
            .expect("Failed to get from nonexistent array");
        assert_eq!(nothing, None);

        // Remove from non-existent array should return false
        let removed = kv_store
            .array_remove("nonexistent", "fake-uuid")
            .expect("Failed to remove from nonexistent");
        assert!(!removed);

        // Add to non-existent array should create it
        let first_id = kv_store
            .array_add("new_array", NestedValue::String("first_item".to_string()))
            .expect("Failed to add to new array");

        assert_eq!(
            kv_store
                .array_len("new_array")
                .expect("Failed to get new array length"),
            1
        );
        let first_item = kv_store
            .array_get("new_array", &first_id)
            .expect("Failed to get first item");
        assert_eq!(
            first_item,
            Some(NestedValue::String("first_item".to_string()))
        );
    }
}

#[test]
fn test_kvstore_array_persistence() {
    let tree = setup_tree();

    let mut color_ids = Vec::new();

    // Create array in first operation
    let op1 = tree.new_operation().expect("Failed to start op1");
    {
        let kv_store = op1
            .get_subtree::<KVStore>("my_kv")
            .expect("Failed to get KVStore");

        let red_id = kv_store
            .array_add("colors", NestedValue::String("red".to_string()))
            .expect("Failed to add red");
        let green_id = kv_store
            .array_add("colors", NestedValue::String("green".to_string()))
            .expect("Failed to add green");
        color_ids.push(red_id);
        color_ids.push(green_id);
    }
    op1.commit().expect("Failed to commit op1");

    // Modify array in second operation
    let op2 = tree.new_operation().expect("Failed to start op2");
    {
        let kv_store = op2
            .get_subtree::<KVStore>("my_kv")
            .expect("Failed to get KVStore");

        // Array should persist from previous operation
        assert_eq!(
            kv_store
                .array_len("colors")
                .expect("Failed to get colors length"),
            2
        );

        let red = kv_store
            .array_get("colors", &color_ids[0])
            .expect("Failed to get red");
        assert_eq!(red, Some(NestedValue::String("red".to_string())));

        let green = kv_store
            .array_get("colors", &color_ids[1])
            .expect("Failed to get green");
        assert_eq!(green, Some(NestedValue::String("green".to_string())));

        // Add more items and remove one
        let blue_id = kv_store
            .array_add("colors", NestedValue::String("blue".to_string()))
            .expect("Failed to add blue");
        color_ids.push(blue_id);

        let removed = kv_store
            .array_remove("colors", &color_ids[0])
            .expect("Failed to remove red");
        assert!(removed);
    }
    op2.commit().expect("Failed to commit op2");

    // Verify final state
    let viewer = tree
        .get_subtree_viewer::<KVStore>("my_kv")
        .expect("Failed to get viewer");

    assert_eq!(
        viewer
            .array_len("colors")
            .expect("Failed to get final colors length"),
        2
    );

    let red_removed = viewer
        .array_get("colors", &color_ids[0])
        .expect("Failed to check red removed");
    assert_eq!(red_removed, None);

    let green = viewer
        .array_get("colors", &color_ids[1])
        .expect("Failed to get final green");
    assert_eq!(green, Some(NestedValue::String("green".to_string())));

    let blue = viewer
        .array_get("colors", &color_ids[2])
        .expect("Failed to get final blue");
    assert_eq!(blue, Some(NestedValue::String("blue".to_string())));
}

#[test]
fn test_subtree_basic() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let kv_store = op
            .get_subtree::<KVStore>("test_store")
            .expect("Failed to get KVStore");

        // Set basic string values
        kv_store.set("key1", "value1").expect("Failed to set key1");
        kv_store.set("key2", "value2").expect("Failed to set key2");

        // Set a nested map value
        let mut nested = KVNested::new();
        nested.set_string("nested_key1", "nested_value1");
        nested.set_string("nested_key2", "nested_value2");
        kv_store
            .set_value("nested", NestedValue::Map(nested.clone()))
            .expect("Failed to set nested map");
    }

    // Commit the operation
    op.commit().expect("Failed to commit operation");

    // Get a viewer to check the subtree
    let viewer = tree
        .get_subtree_viewer::<KVStore>("test_store")
        .expect("Failed to get viewer");

    // Check string values
    assert_kvstore_value(&viewer, "key1", "value1");
    assert_kvstore_value(&viewer, "key2", "value2");

    // Check nested map
    match viewer.get("nested").expect("Failed to get nested map") {
        NestedValue::Map(map) => {
            // Check nested values
            match map.get("nested_key1") {
                Some(NestedValue::String(value)) => assert_eq!(value, "nested_value1"),
                _ => panic!("Expected string value for nested_key1"),
            }
            match map.get("nested_key2") {
                Some(NestedValue::String(value)) => assert_eq!(value, "nested_value2"),
                _ => panic!("Expected string value for nested_key2"),
            }
        }
        _ => panic!("Expected map value for 'nested'"),
    }

    // Check non-existent key
    assert_key_not_found(viewer.get("non_existent"));
}

#[test]
fn test_kvstore_update_nested_value() {
    let tree = setup_tree();

    // First operation: Create initial nested structure
    let op1 = tree.new_operation().expect("Op1: Failed to start");
    {
        let kv_store = op1
            .get_subtree::<KVStore>("nested_test")
            .expect("Op1: Failed to get KVStore");

        // Create level1 -> level2_str structure
        let mut l1_map = KVNested::new();
        l1_map.set_string("level2_str", "initial_value");
        kv_store
            .set_value("level1", NestedValue::Map(l1_map))
            .expect("Op1: Failed to set level1");
    }
    op1.commit().expect("Op1: Failed to commit");

    // Second operation: Update with another structure
    let op2 = tree.new_operation().expect("Op2: Failed to start");
    {
        let kv_store = op2
            .get_subtree::<KVStore>("nested_test")
            .expect("Op2: Failed to get KVStore");

        // Create an entirely new map structure that will replace the old one
        let mut l2_map = KVNested::new();
        l2_map.set_string("deep_key", "deep_value");

        let mut new_l1_map = KVNested::new();
        new_l1_map.set_map("level2_map", l2_map);

        // Completely replace the previous value at level1
        kv_store
            .set_value("level1", NestedValue::Map(new_l1_map.clone()))
            .expect("Op2: Failed to overwrite level1");

        // Verify the update within the same operation
        match kv_store.get("level1").expect("Failed to get level1") {
            NestedValue::Map(retrieved_l1_map) => {
                // Check if level2_map exists with the expected content
                match retrieved_l1_map.get("level2_map") {
                    Some(NestedValue::Map(retrieved_l2_map)) => {
                        match retrieved_l2_map.get("deep_key") {
                            Some(NestedValue::String(val)) => assert_eq!(val, "deep_value"),
                            _ => panic!("Expected string 'deep_value' at deep_key"),
                        }
                    }
                    _ => panic!("Expected 'level2_map' to be a map"),
                }
            }
            _ => panic!("Expected 'level1' to be a map"),
        }
    }
    op2.commit().expect("Op2: Failed to commit");

    // Verify the update persists after commit
    let viewer = tree
        .get_subtree_viewer::<KVStore>("nested_test")
        .expect("Failed to get viewer");

    // Verify the structure after commit
    match viewer.get("level1").expect("Viewer: Failed to get level1") {
        NestedValue::Map(retrieved_l1_map) => {
            // Check if level2_map exists with expected content
            match retrieved_l1_map.get("level2_map") {
                Some(NestedValue::Map(retrieved_l2_map)) => {
                    match retrieved_l2_map.get("deep_key") {
                        Some(NestedValue::String(val)) => assert_eq!(val, "deep_value"),
                        _ => panic!("Viewer: Expected string 'deep_value' at deep_key"),
                    }
                }
                _ => panic!("Viewer: Expected 'level2_map' to be a map"),
            }
        }
        _ => panic!("Viewer: Expected 'level1' to be a map"),
    }
}

#[cfg(feature = "y-crdt")]
#[test]
fn test_yrsstore_basic_text_operations() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let yrs_store = op
            .get_subtree::<YrsStore>("yrs_text")
            .expect("Failed to get YrsStore");

        // Perform text operations within a single operation
        yrs_store
            .with_doc_mut(|doc| {
                let text = doc.get_or_insert_text("document");
                let mut txn = doc.transact_mut();
                text.insert(&mut txn, 0, "Hello, World!");
                Ok(())
            })
            .expect("Failed to perform text operations");
    }

    // Commit the operation
    op.commit().expect("Failed to commit operation");

    // Verify the text content persisted
    let viewer = tree
        .get_subtree_viewer::<YrsStore>("yrs_text")
        .expect("Failed to get YrsStore viewer");

    viewer
        .with_doc(|doc| {
            let text = doc.get_or_insert_text("document");
            let txn = doc.transact();
            let content = text.get_string(&txn);
            assert_eq!(content, "Hello, World!");
            Ok(())
        })
        .expect("Failed to verify text content");
}

#[cfg(feature = "y-crdt")]
#[test]
fn test_yrsstore_incremental_updates_save_diffs_only() {
    let tree = setup_tree();

    // Operation 1: Create initial large text content
    let op1 = tree.new_operation().expect("Op1: Failed to start");
    let first_diff_size = {
        let yrs_store = op1
            .get_subtree::<YrsStore>("yrs_diff_test")
            .expect("Op1: Failed to get YrsStore");

        yrs_store
            .with_doc_mut(|doc| {
                let text = doc.get_or_insert_text("document");
                let mut txn = doc.transact_mut();

                // Create a large initial document (about 10KB of text)
                let large_content =
                    "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(200);
                text.insert(&mut txn, 0, &large_content);
                Ok(())
            })
            .expect("Op1: Failed to perform text operations");

        // Get the actual diff stored in the atomic operation (not the full document state)
        let local_diff: eidetica::subtree::YrsBinary = op1
            .get_local_data("yrs_diff_test")
            .expect("Op1: Failed to get local diff data");

        local_diff.as_bytes().len()
    };
    op1.commit().expect("Op1: Failed to commit");

    // Operation 2: Add a small change (this should only save the diff)
    let op2 = tree.new_operation().expect("Op2: Failed to start");
    let second_diff_size = {
        let yrs_store = op2
            .get_subtree::<YrsStore>("yrs_diff_test")
            .expect("Op2: Failed to get YrsStore");

        yrs_store
            .with_doc_mut(|doc| {
                let text = doc.get_or_insert_text("document");
                let mut txn = doc.transact_mut();
                // Add just a small amount of text at a specific position
                text.insert(&mut txn, 12, " SMALL_CHANGE");
                Ok(())
            })
            .expect("Op2: Failed to perform text operations");

        // Get the actual diff stored in the atomic operation
        let local_diff: eidetica::subtree::YrsBinary = op2
            .get_local_data("yrs_diff_test")
            .expect("Op2: Failed to get local diff data");

        local_diff.as_bytes().len()
    };
    op2.commit().expect("Op2: Failed to commit");

    // Print the actual diff sizes for verification
    println!("First diff size: {first_diff_size}, Second diff size: {second_diff_size}");

    // Assert that the second diff is significantly smaller than the first
    // The first diff contains ~10KB of content, the second should be just a few bytes
    assert!(
        second_diff_size < first_diff_size / 10,
        "Second diff size ({second_diff_size}) should be much smaller than first diff size ({first_diff_size})"
    );

    // The second diff should be smaller than 200 bytes for such a small change
    assert!(
        second_diff_size < 200,
        "Second diff size ({second_diff_size}) should be much smaller for just adding a few characters"
    );

    // Verify final content is correct
    let viewer = tree
        .get_subtree_viewer::<YrsStore>("yrs_diff_test")
        .expect("Failed to get YrsStore viewer");

    viewer
        .with_doc(|doc| {
            let text = doc.get_or_insert_text("document");
            let txn = doc.transact();
            let content = text.get_string(&txn);

            // Verify the small change was inserted at the correct position
            assert!(
                content.contains(" SMALL_CHANGE"),
                "Content should contain the inserted text"
            );

            // Verify the content is still large (confirming we didn't lose the original)
            assert!(
                content.len() > 10000,
                "Content should still be large after the small change"
            );

            Ok(())
        })
        .expect("Failed to verify final text content");
}

#[cfg(feature = "y-crdt")]
#[test]
fn test_yrsstore_map_operations() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let yrs_store = op
            .get_subtree::<YrsStore>("yrs_map")
            .expect("Failed to get YrsStore");

        // Perform map operations within a single operation
        yrs_store
            .with_doc_mut(|doc| {
                let map = doc.get_or_insert_map("root");
                let mut txn = doc.transact_mut();
                map.insert(&mut txn, "key1", "value1");
                map.insert(&mut txn, "key2", 42);
                map.insert(&mut txn, "key3", true);
                Ok(())
            })
            .expect("Failed to perform map operations");
    }

    // Commit the operation
    op.commit().expect("Failed to commit operation");

    // Verify the map content persisted
    let viewer = tree
        .get_subtree_viewer::<YrsStore>("yrs_map")
        .expect("Failed to get YrsStore viewer");

    viewer
        .with_doc(|doc| {
            let map = doc.get_or_insert_map("root");
            let txn = doc.transact();

            // Check string value
            let val1 = map.get(&txn, "key1").expect("key1 should exist");
            assert_eq!(val1.to_string(&txn), "value1");

            // Check integer value
            let val2 = map.get(&txn, "key2").expect("key2 should exist");
            assert_eq!(val2.to_string(&txn), "42");

            // Check boolean value
            let val3 = map.get(&txn, "key3").expect("key3 should exist");
            assert_eq!(val3.to_string(&txn), "true");

            Ok(())
        })
        .expect("Failed to verify map content");
}

#[cfg(feature = "y-crdt")]
#[test]
fn test_yrsstore_multiple_operations_with_diffs() {
    let tree = setup_tree();

    // Operation 1: Create initial state
    let op1 = tree.new_operation().expect("Op1: Failed to start");
    {
        let yrs_store = op1
            .get_subtree::<YrsStore>("yrs_multi")
            .expect("Op1: Failed to get YrsStore");

        yrs_store
            .with_doc_mut(|doc| {
                let map = doc.get_or_insert_map("data");
                let text = doc.get_or_insert_text("notes");

                let mut txn = doc.transact_mut();
                map.insert(&mut txn, "version", 1);
                text.insert(&mut txn, 0, "Version 1 notes");
                Ok(())
            })
            .expect("Op1: Failed to perform operations");
    }
    op1.commit().expect("Op1: Failed to commit");

    // Operation 2: Update existing data
    let op2 = tree.new_operation().expect("Op2: Failed to start");
    {
        let yrs_store = op2
            .get_subtree::<YrsStore>("yrs_multi")
            .expect("Op2: Failed to get YrsStore");

        yrs_store
            .with_doc_mut(|doc| {
                let map = doc.get_or_insert_map("data");
                let text = doc.get_or_insert_text("notes");

                let mut txn = doc.transact_mut();
                map.insert(&mut txn, "version", 2);
                map.insert(&mut txn, "author", "test_user");
                let text_len = text.len(&txn);
                text.insert(&mut txn, text_len, " - Updated in v2");
                Ok(())
            })
            .expect("Op2: Failed to perform operations");
    }
    op2.commit().expect("Op2: Failed to commit");

    // Operation 3: Add more data
    let op3 = tree.new_operation().expect("Op3: Failed to start");
    {
        let yrs_store = op3
            .get_subtree::<YrsStore>("yrs_multi")
            .expect("Op3: Failed to get YrsStore");

        yrs_store
            .with_doc_mut(|doc| {
                let map = doc.get_or_insert_map("data");

                let mut txn = doc.transact_mut();
                map.insert(&mut txn, "features", vec!["diff_saving", "crdt_support"]);
                Ok(())
            })
            .expect("Op3: Failed to perform operations");
    }
    op3.commit().expect("Op3: Failed to commit");

    // Verify final state
    let viewer = tree
        .get_subtree_viewer::<YrsStore>("yrs_multi")
        .expect("Failed to get YrsStore viewer");

    viewer
        .with_doc(|doc| {
            let map = doc.get_or_insert_map("data");
            let text = doc.get_or_insert_text("notes");
            let txn = doc.transact();

            // Check map values
            let version = map.get(&txn, "version").expect("version should exist");
            assert_eq!(version.to_string(&txn), "2");

            let author = map.get(&txn, "author").expect("author should exist");
            assert_eq!(author.to_string(&txn), "test_user");

            // Check text content
            let notes_content = text.get_string(&txn);
            assert_eq!(notes_content, "Version 1 notes - Updated in v2");

            Ok(())
        })
        .expect("Failed to verify final state");
}

#[cfg(feature = "y-crdt")]
#[test]
fn test_yrsstore_apply_external_update() {
    let tree = setup_tree();

    // Create a document externally to simulate remote changes
    let external_doc = Doc::new();
    let external_update = {
        let text = external_doc.get_or_insert_text("shared_doc");
        let mut txn = external_doc.transact_mut();
        text.insert(&mut txn, 0, "External change");
        drop(txn);

        let txn = external_doc.transact();
        txn.encode_state_as_update_v1(&yrs::StateVector::default())
    };

    // Apply the external update to our YrsStore
    let op = tree.new_operation().expect("Failed to start operation");
    {
        let yrs_store = op
            .get_subtree::<YrsStore>("yrs_external")
            .expect("Failed to get YrsStore");

        yrs_store
            .apply_update(&external_update)
            .expect("Failed to apply external update");
    }
    op.commit().expect("Failed to commit operation");

    // Verify the external update was applied
    let viewer = tree
        .get_subtree_viewer::<YrsStore>("yrs_external")
        .expect("Failed to get YrsStore viewer");

    viewer
        .with_doc(|doc| {
            let text = doc.get_or_insert_text("shared_doc");
            let txn = doc.transact();
            let content = text.get_string(&txn);
            assert_eq!(content, "External change");
            Ok(())
        })
        .expect("Failed to verify external update");
}

// RowStore Tests

#[test]
fn test_rowstore_basic_crud_operations() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    let primary_key = {
        let row_store = op
            .get_subtree::<RowStore<TestRecord>>("test_records")
            .expect("Failed to get RowStore");

        let record = TestRecord {
            name: "John Doe".to_string(),
            age: 30,
            email: "john@example.com".to_string(),
        };

        // Test insert
        let pk = row_store
            .insert(record.clone())
            .expect("Failed to insert record");
        assert!(!pk.is_empty(), "Primary key should not be empty");

        // Test get within same operation
        let retrieved = row_store.get(&pk).expect("Failed to get record");
        assert_eq!(retrieved, record);

        // Test update/set
        let updated_record = TestRecord {
            name: "John Smith".to_string(),
            age: 31,
            email: "john.smith@example.com".to_string(),
        };
        row_store
            .set(&pk, updated_record.clone())
            .expect("Failed to update record");

        // Verify update within same operation
        let retrieved_updated = row_store.get(&pk).expect("Failed to get updated record");
        assert_eq!(retrieved_updated, updated_record);

        pk
    };

    // Commit the operation
    op.commit().expect("Failed to commit operation");

    // Verify persistence after commit
    let viewer = tree
        .get_subtree_viewer::<RowStore<TestRecord>>("test_records")
        .expect("Failed to get RowStore viewer");

    let retrieved_after_commit = viewer
        .get(&primary_key)
        .expect("Failed to get record after commit");
    let expected = TestRecord {
        name: "John Smith".to_string(),
        age: 31,
        email: "john.smith@example.com".to_string(),
    };
    assert_eq!(retrieved_after_commit, expected);
}

#[test]
fn test_rowstore_multiple_records() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    let mut inserted_keys = Vec::new();
    {
        let row_store = op
            .get_subtree::<RowStore<SimpleRecord>>("simple_records")
            .expect("Failed to get RowStore");

        // Insert multiple records
        for i in 1..=5 {
            let record = SimpleRecord { value: i * 10 };
            let key = row_store.insert(record).expect("Failed to insert record");
            inserted_keys.push(key);
        }

        // Verify all records can be retrieved
        for (i, key) in inserted_keys.iter().enumerate() {
            let record = row_store.get(key).expect("Failed to get record");
            assert_eq!(record.value, (i as i32 + 1) * 10);
        }
    }

    op.commit().expect("Failed to commit operation");

    // Verify all records persist after commit
    let viewer = tree
        .get_subtree_viewer::<RowStore<SimpleRecord>>("simple_records")
        .expect("Failed to get RowStore viewer");

    for (i, key) in inserted_keys.iter().enumerate() {
        let record = viewer.get(key).expect("Failed to get record after commit");
        assert_eq!(record.value, (i as i32 + 1) * 10);
    }
}

#[test]
fn test_rowstore_search_functionality() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let row_store = op
            .get_subtree::<RowStore<TestRecord>>("search_records")
            .expect("Failed to get RowStore");

        // Insert test data
        let records = vec![
            TestRecord {
                name: "Alice Johnson".to_string(),
                age: 25,
                email: "alice@example.com".to_string(),
            },
            TestRecord {
                name: "Bob Smith".to_string(),
                age: 30,
                email: "bob@company.com".to_string(),
            },
            TestRecord {
                name: "Charlie Brown".to_string(),
                age: 25,
                email: "charlie@example.com".to_string(),
            },
            TestRecord {
                name: "Diana Prince".to_string(),
                age: 35,
                email: "diana@hero.org".to_string(),
            },
        ];

        for record in records {
            row_store.insert(record).expect("Failed to insert record");
        }

        // Test search by age
        let age_25_results = row_store
            .search(|record| record.age == 25)
            .expect("Failed to search by age");
        assert_eq!(age_25_results.len(), 2);
        for (_, record) in &age_25_results {
            assert_eq!(record.age, 25);
        }

        // Test search by email domain
        let example_domain_results = row_store
            .search(|record| record.email.contains("example.com"))
            .expect("Failed to search by email domain");
        assert_eq!(example_domain_results.len(), 2);
        for (_, record) in &example_domain_results {
            assert!(record.email.contains("example.com"));
        }

        // Test search by name prefix
        let name_starting_with_b = row_store
            .search(|record| record.name.starts_with('B'))
            .expect("Failed to search by name prefix");
        assert_eq!(name_starting_with_b.len(), 1);
        assert_eq!(name_starting_with_b[0].1.name, "Bob Smith");

        // Test search with no matches
        let no_matches = row_store
            .search(|record| record.age > 100)
            .expect("Failed to search with no matches");
        assert_eq!(no_matches.len(), 0);
    }

    op.commit().expect("Failed to commit operation");

    // Test search after commit
    let viewer = tree
        .get_subtree_viewer::<RowStore<TestRecord>>("search_records")
        .expect("Failed to get RowStore viewer");

    let age_30_results = viewer
        .search(|record| record.age == 30)
        .expect("Failed to search after commit");
    assert_eq!(age_30_results.len(), 1);
    assert_eq!(age_30_results[0].1.name, "Bob Smith");
}

#[test]
fn test_rowstore_error_handling() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let row_store = op
            .get_subtree::<RowStore<TestRecord>>("error_test")
            .expect("Failed to get RowStore");

        // Test get with non-existent key
        let result = row_store.get("non_existent_key");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), eidetica::Error::NotFound));

        // Test get with empty key
        let result = row_store.get("");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), eidetica::Error::NotFound));
    }

    op.commit().expect("Failed to commit operation");

    // Test error handling after commit
    let viewer = tree
        .get_subtree_viewer::<RowStore<TestRecord>>("error_test")
        .expect("Failed to get RowStore viewer");

    let result = viewer.get("still_non_existent");
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), eidetica::Error::NotFound));
}

#[test]
fn test_rowstore_uuid_generation() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    let mut generated_keys = std::collections::HashSet::new();
    {
        let row_store = op
            .get_subtree::<RowStore<SimpleRecord>>("uuid_test")
            .expect("Failed to get RowStore");

        // Generate multiple keys and verify they're unique
        for i in 1..=100 {
            let record = SimpleRecord { value: i };
            let key = row_store.insert(record).expect("Failed to insert record");

            // Verify UUID format (should be 36 characters with hyphens)
            assert_eq!(key.len(), 36);
            assert_eq!(key.chars().filter(|&c| c == '-').count(), 4);

            // Verify uniqueness
            assert!(
                generated_keys.insert(key.clone()),
                "Duplicate UUID generated: {key}"
            );
        }
    }

    op.commit().expect("Failed to commit operation");

    // Verify all records are retrievable with their unique keys
    let viewer = tree
        .get_subtree_viewer::<RowStore<SimpleRecord>>("uuid_test")
        .expect("Failed to get RowStore viewer");

    for key in &generated_keys {
        let record = viewer.get(key).expect("Failed to get record by UUID");
        assert!(record.value >= 1 && record.value <= 100);
    }
}

#[test]
fn test_rowstore_multiple_operations() {
    let tree = setup_tree();

    // Operation 1: Insert initial records
    let op1 = tree.new_operation().expect("Op1: Failed to start");
    let (key1, key2) = {
        let row_store = op1
            .get_subtree::<RowStore<TestRecord>>("multi_op_test")
            .expect("Op1: Failed to get RowStore");

        let record1 = TestRecord {
            name: "Initial User 1".to_string(),
            age: 20,
            email: "user1@initial.com".to_string(),
        };
        let record2 = TestRecord {
            name: "Initial User 2".to_string(),
            age: 25,
            email: "user2@initial.com".to_string(),
        };

        let k1 = row_store
            .insert(record1)
            .expect("Op1: Failed to insert record1");
        let k2 = row_store
            .insert(record2)
            .expect("Op1: Failed to insert record2");
        (k1, k2)
    };
    op1.commit().expect("Op1: Failed to commit");

    // Operation 2: Update existing records and add new ones
    let op2 = tree.new_operation().expect("Op2: Failed to start");
    let key3 = {
        let row_store = op2
            .get_subtree::<RowStore<TestRecord>>("multi_op_test")
            .expect("Op2: Failed to get RowStore");

        // Update existing record
        let updated_record1 = TestRecord {
            name: "Updated User 1".to_string(),
            age: 21,
            email: "user1@updated.com".to_string(),
        };
        row_store
            .set(&key1, updated_record1)
            .expect("Op2: Failed to update record1");

        // Add new record
        let record3 = TestRecord {
            name: "New User 3".to_string(),
            age: 30,
            email: "user3@new.com".to_string(),
        };
        let k3 = row_store
            .insert(record3)
            .expect("Op2: Failed to insert record3");

        // Verify we can see updated and new data within operation
        let retrieved1 = row_store
            .get(&key1)
            .expect("Op2: Failed to get updated record1");
        assert_eq!(retrieved1.name, "Updated User 1");
        assert_eq!(retrieved1.age, 21);

        let retrieved2 = row_store
            .get(&key2)
            .expect("Op2: Failed to get unchanged record2");
        assert_eq!(retrieved2.name, "Initial User 2");
        assert_eq!(retrieved2.age, 25);

        k3
    };
    op2.commit().expect("Op2: Failed to commit");

    // Verify final state
    let viewer = tree
        .get_subtree_viewer::<RowStore<TestRecord>>("multi_op_test")
        .expect("Failed to get RowStore viewer");

    // Check updated record
    let final_record1 = viewer.get(&key1).expect("Failed to get final record1");
    assert_eq!(final_record1.name, "Updated User 1");
    assert_eq!(final_record1.age, 21);
    assert_eq!(final_record1.email, "user1@updated.com");

    // Check unchanged record
    let final_record2 = viewer.get(&key2).expect("Failed to get final record2");
    assert_eq!(final_record2.name, "Initial User 2");
    assert_eq!(final_record2.age, 25);
    assert_eq!(final_record2.email, "user2@initial.com");

    // Check new record
    let final_record3 = viewer.get(&key3).expect("Failed to get final record3");
    assert_eq!(final_record3.name, "New User 3");
    assert_eq!(final_record3.age, 30);
    assert_eq!(final_record3.email, "user3@new.com");

    // Verify search across all records
    let all_records = viewer
        .search(|_| true)
        .expect("Failed to search all records");
    assert_eq!(all_records.len(), 3);
}

#[test]
fn test_rowstore_empty_search() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let row_store = op
            .get_subtree::<RowStore<SimpleRecord>>("empty_search_test")
            .expect("Failed to get RowStore");

        // Search in empty store
        let results = row_store
            .search(|_| true)
            .expect("Failed to search empty store");
        assert_eq!(results.len(), 0);
    }

    op.commit().expect("Failed to commit operation");

    // Search in empty store after commit
    let viewer = tree
        .get_subtree_viewer::<RowStore<SimpleRecord>>("empty_search_test")
        .expect("Failed to get RowStore viewer");

    let results = viewer
        .search(|_| true)
        .expect("Failed to search empty store after commit");
    assert_eq!(results.len(), 0);
}

#[test]
fn test_rowstore_with_authenticated_tree() {
    let db = setup_db_with_key("rowstore_auth_key");
    let tree = db
        .new_tree_default("rowstore_auth_key")
        .expect("Failed to create authenticated tree");

    let op = tree.new_operation().expect("Failed to start operation");

    let primary_key = {
        let row_store = op
            .get_subtree::<RowStore<TestRecord>>("auth_records")
            .expect("Failed to get RowStore");

        let record = TestRecord {
            name: "Authenticated User".to_string(),
            age: 28,
            email: "auth@secure.com".to_string(),
        };

        // Insert record in authenticated tree
        let pk = row_store
            .insert(record.clone())
            .expect("Failed to insert authenticated record");

        // Verify retrieval within same operation
        let retrieved = row_store
            .get(&pk)
            .expect("Failed to get authenticated record");
        assert_eq!(retrieved, record);

        pk
    };

    op.commit()
        .expect("Failed to commit authenticated operation");

    // Verify persistence in authenticated tree
    let viewer = tree
        .get_subtree_viewer::<RowStore<TestRecord>>("auth_records")
        .expect("Failed to get RowStore viewer for authenticated tree");

    let retrieved_after_commit = viewer
        .get(&primary_key)
        .expect("Failed to get authenticated record after commit");
    assert_eq!(retrieved_after_commit.name, "Authenticated User");
    assert_eq!(retrieved_after_commit.age, 28);
    assert_eq!(retrieved_after_commit.email, "auth@secure.com");
}

#[test]
fn test_rowstore_complex_data_merging() {
    let tree = setup_tree();

    // Create base entry with initial data
    let op_base = tree.new_operation().expect("Base: Failed to start");
    let key1 = {
        let row_store = op_base
            .get_subtree::<RowStore<TestRecord>>("merge_test")
            .expect("Base: Failed to get RowStore");

        let record = TestRecord {
            name: "Original User".to_string(),
            age: 25,
            email: "original@test.com".to_string(),
        };

        row_store
            .insert(record)
            .expect("Base: Failed to insert record")
    };
    let base_entry_id = op_base.commit().expect("Base: Failed to commit");

    // Branch A: Create concurrent modification from base
    let op_branch_a = tree
        .new_operation_with_tips([base_entry_id.clone()])
        .expect("Branch A: Failed to start with custom tips");
    {
        let row_store = op_branch_a
            .get_subtree::<RowStore<TestRecord>>("merge_test")
            .expect("Branch A: Failed to get RowStore");

        let updated_record = TestRecord {
            name: "Updated by Branch A".to_string(),
            age: 26,
            email: "updated_a@test.com".to_string(),
        };
        row_store
            .set(&key1, updated_record)
            .expect("Branch A: Failed to update record");

        op_branch_a.commit().expect("Branch A: Failed to commit");
    }

    // Branch B: Create concurrent modification from same base (parallel to Branch A)
    let op_branch_b = tree
        .new_operation_with_tips([base_entry_id])
        .expect("Branch B: Failed to start with custom tips");
    {
        let row_store = op_branch_b
            .get_subtree::<RowStore<TestRecord>>("merge_test")
            .expect("Branch B: Failed to get RowStore");

        let updated_record = TestRecord {
            name: "Updated by Branch B".to_string(),
            age: 27,
            email: "updated_b@test.com".to_string(),
        };
        row_store
            .set(&key1, updated_record)
            .expect("Branch B: Failed to update record");

        op_branch_b.commit().expect("Branch B: Failed to commit");
    }

    // Merge operation: Create operation that merges both branches
    let op_merge = tree
        .new_operation()
        .expect("Merge: Failed to start with both branch tips");

    // Read the merged state to trigger CRDT resolution
    let merged_record = {
        let row_store = op_merge
            .get_subtree::<RowStore<TestRecord>>("merge_test")
            .expect("Merge: Failed to get RowStore");

        row_store
            .get(&key1)
            .expect("Merge: Failed to get merged record")
    };

    // With KVOverWrite semantics, one of the concurrent updates should win
    // The exact result depends on the deterministic merge order of the underlying CRDT
    assert!(
        merged_record.name == "Updated by Branch A" || merged_record.name == "Updated by Branch B",
        "Merged record should contain updates from either branch A or B, got: {}",
        merged_record.name
    );

    // Verify the age was also updated according to whichever branch won
    if merged_record.name == "Updated by Branch A" {
        assert_eq!(merged_record.age, 26);
        assert_eq!(merged_record.email, "updated_a@test.com");
    } else {
        assert_eq!(merged_record.age, 27);
        assert_eq!(merged_record.email, "updated_b@test.com");
    }

    // Commit the merge to verify persistence
    op_merge.commit().expect("Merge: Failed to commit");

    // Verify the merged state persists after commit
    let viewer = tree
        .get_subtree_viewer::<RowStore<TestRecord>>("merge_test")
        .expect("Failed to get RowStore viewer");

    let final_record = viewer
        .get(&key1)
        .expect("Failed to get final merged record");
    assert_eq!(
        final_record, merged_record,
        "Final state should match merged state"
    );
}
