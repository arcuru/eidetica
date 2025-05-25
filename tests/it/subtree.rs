use crate::helpers::*;
use eidetica::data::{KVNested, NestedValue};
use eidetica::subtree::KVStore;

#[cfg(feature = "y-crdt")]
use eidetica::subtree::YrsStore;
#[cfg(feature = "y-crdt")]
use yrs::{Doc, GetString, Map, ReadTxn, Text, Transact};

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
