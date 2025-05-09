use eideticadb::backend::InMemoryBackend;
use eideticadb::basedb::BaseDB;
use eideticadb::data::{KVNested, NestedValue};
use eideticadb::subtree::KVStore;

// Helper function to set up a tree for testing
fn setup_tree() -> eideticadb::Tree {
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);
    db.new_tree_default()
        .expect("Failed to create tree for setup")
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
        match kv_store.get("key1").expect("Failed get staged key1") {
            NestedValue::String(value) => assert_eq!(value, "value1"),
            _ => panic!("Expected string value for key1"),
        }

        match kv_store.get("key2").expect("Failed get staged key2") {
            NestedValue::String(value) => assert_eq!(value, "value2"),
            _ => panic!("Expected string value for key2"),
        }

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

        match kv_store
            .get("key1")
            .expect("Failed get staged overwritten key1")
        {
            NestedValue::String(value) => assert_eq!(value, "value1_updated"),
            _ => panic!("Expected string value for updated key1"),
        }

        // Get non-existent key
        match kv_store.get("non_existent") {
            Err(eideticadb::Error::NotFound) => (), // Expected
            other => panic!("Expected NotFound, got {:?}", other),
        }
    }

    // Commit the operation
    op.commit().expect("Failed to commit operation");

    // Verify final state with a viewer
    let viewer = tree
        .get_subtree_viewer::<KVStore>("my_kv")
        .expect("Failed to get viewer");

    match viewer.get("key1").expect("Viewer: Failed get key1") {
        NestedValue::String(value) => assert_eq!(value, "value1_updated"),
        _ => panic!("Expected string value for key1 from viewer"),
    }

    match viewer.get("key2").expect("Viewer: Failed get key2") {
        NestedValue::String(value) => assert_eq!(value, "value2"),
        _ => panic!("Expected string value for key2 from viewer"),
    }

    match viewer.get("non_existent") {
        Err(eideticadb::Error::NotFound) => (), // Expected
        other => panic!("Viewer: Expected NotFound, got {:?}", other),
    }
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
        match kv_store.get("key1") {
            Err(eideticadb::Error::NotFound) => (), // Expected
            Ok(NestedValue::Deleted) => (),         // This is also acceptable
            other => panic!("Expected NotFound or Deleted after delete, got {:?}", other),
        }

        // key2 should still be accessible
        match kv_store.get("key2").expect("Failed get key2 after delete") {
            NestedValue::String(value) => assert_eq!(value, "value2"),
            _ => panic!("Expected string value for key2"),
        }
    }

    // Commit the operation
    op.commit().expect("Failed to commit operation");

    // Verify the deletion persisted
    let viewer = tree
        .get_subtree_viewer::<KVStore>("my_kv")
        .expect("Failed to get viewer");
    match viewer.get("key1") {
        Err(eideticadb::Error::NotFound) => (), // Expected
        Ok(NestedValue::Deleted) => (),         // This is also acceptable
        other => panic!("Expected NotFound or Deleted after commit, got {:?}", other),
    }

    match viewer.get("key2").expect("Viewer: Failed get key2") {
        NestedValue::String(value) => assert_eq!(value, "value2"),
        _ => panic!("Expected string value for key2 from viewer"),
    }

    // Check the tombstone in get_all
    let all_data = viewer.get_all().expect("Failed to get all data");
    let all_data_map = all_data.as_hashmap();

    // Should have two keys (one with value, one with tombstone)
    assert_eq!(all_data_map.len(), 2);
    assert_eq!(all_data_map.get("key1"), Some(&NestedValue::Deleted));
    assert_eq!(
        all_data_map.get("key2"),
        Some(&NestedValue::String("value2".to_string()))
    );
}

#[test]
fn test_kvstore_set_value() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let kv_store = op
            .get_subtree::<KVStore>("my_kv")
            .expect("Failed to get KVStore");

        // Set a string value
        kv_store
            .set("key1", "value1")
            .expect("Failed to set string");

        // Set a nested map value
        let mut nested = KVNested::new();
        nested.set_string("inner".to_string(), "nested_value".to_string());
        kv_store
            .set_value("key2", NestedValue::Map(nested.clone()))
            .expect("Failed to set map");

        // Verify string value
        match kv_store.get("key1").expect("Failed to get key1") {
            NestedValue::String(value) => assert_eq!(value, "value1"),
            _ => panic!("Expected string value for key1"),
        }

        // Verify map value
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

    // Verify with a viewer
    let viewer = tree
        .get_subtree_viewer::<KVStore>("my_kv")
        .expect("Failed to get viewer");

    // Check string value persisted
    match viewer.get("key1").expect("Failed to get key1 from viewer") {
        NestedValue::String(value) => assert_eq!(value, "value1"),
        _ => panic!("Expected string value for key1 from viewer"),
    }

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
    let backend = Box::new(InMemoryBackend::new());
    let db = BaseDB::new(backend);
    let tree = db.new_tree_default().expect("Failed to create tree");

    // Create an operation
    let op = tree.new_operation().expect("Failed to start operation");

    // Get a KVStore subtree
    let subtree = op
        .get_subtree::<KVStore>("test_subtree")
        .expect("Failed to get KVStore subtree");

    // Set some values
    subtree.set("key1", "value1").expect("Failed to set key1");
    subtree.set("key2", "value2").expect("Failed to set key2");

    // Add a nested map
    let mut nested = KVNested::new();
    nested.set_string("nested_key1".to_string(), "nested_value1".to_string());
    subtree
        .set_value("nested", NestedValue::Map(nested.clone()))
        .expect("Failed to set nested map");

    // Commit the operation
    op.commit().expect("Failed to commit operation");

    // Verify values can be read with a viewer
    let viewer = tree
        .get_subtree_viewer::<KVStore>("test_subtree")
        .expect("Failed to get viewer");

    // Check string values
    assert_eq!(
        viewer.get_string("key1").expect("Failed to get key1"),
        "value1"
    );
    assert_eq!(
        viewer.get_string("key2").expect("Failed to get key2"),
        "value2"
    );

    // Check nested map
    match viewer.get("nested").expect("Failed to get nested map") {
        NestedValue::Map(map) => match map.get("nested_key1") {
            Some(NestedValue::String(val)) => assert_eq!(val, "nested_value1"),
            _ => panic!("Expected string value for nested_key1"),
        },
        _ => panic!("Expected map value for 'nested'"),
    }

    // Check non-existent key
    match viewer.get("non_existent") {
        Err(eideticadb::Error::NotFound) => (), // Expected
        other => panic!("Expected NotFound for non-existent key, got {:?}", other),
    }
}
