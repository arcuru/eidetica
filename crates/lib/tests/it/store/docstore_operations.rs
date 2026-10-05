//! DocStore subtree operation tests
//!
//! This module contains tests for DocStore subtree functionality including
//! basic CRUD operations, path-based access, nested values, and persistence.

use eidetica::{
    crdt::{
        Doc,
        doc::{List, Value, path},
    },
    store::DocStore,
};

mod contains;
mod paths;

use super::helpers::*;
use crate::helpers::*;

#[tokio::test]
async fn test_dict_set_and_get_via_op() {
    let ctx = TestContext::new().with_database().await;

    // Use helper to create initial data
    let initial_data = &[("key1", "value1"), ("key2", "value2")];
    create_dict_operation(ctx.database(), "my_kv", initial_data).await;

    // Test operation-level modifications
    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");
    let dict = txn
        .get_store::<DocStore>("my_kv")
        .await
        .expect("Failed to get Doc");

    // Verify initial values are accessible
    assert_dict_value(&dict, "key1", "value1").await;
    assert_dict_value(&dict, "key2", "value2").await;

    // Test get_string convenience method
    assert_eq!(
        dict.get_string("key1")
            .await
            .expect("Failed get_string key1"),
        "value1"
    );
    assert_eq!(
        dict.get_string("key2")
            .await
            .expect("Failed get_string key2"),
        "value2"
    );

    // Test overwrite
    dict.set("key1", "value1_updated")
        .await
        .expect("Failed to overwrite key1");
    assert_dict_value(&dict, "key1", "value1_updated").await;

    // Test non-existent key
    assert_key_not_found(dict.get("non_existent").await);

    txn.commit().await.expect("Failed to commit transaction");

    // Verify final state using helper
    let expected_final = &[("key1", "value1_updated"), ("key2", "value2")];
    assert_dict_viewer_data(ctx.database(), "my_kv", expected_final).await;
}

#[tokio::test]
async fn test_docstore_set_stages_typed_values_and_paths() {
    let ctx = TestContext::new().with_database().await;
    let txn = ctx.database().new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();

    // Plain keys, overwrites, typed values and dotted paths, in the same order.
    let writes: Vec<(&str, Value)> = vec![
        ("name", "Alice".into()),
        ("count", 1.into()),
        ("name", "Bob".into()),
        ("user.profile.email", "bob@example.com".into()),
        ("user.verified", true.into()),
        ("user.profile.email", "bob@example.org".into()),
        ("count", Value::Deleted),
    ];
    for (key, value) in writes {
        store.set(key, value).await.unwrap();
    }

    let staged = store.get_all().await.unwrap();
    assert_eq!(staged.get("name"), Some(&Value::Text("Bob".to_string())));
    assert!(staged.is_tombstone("count"));
    assert_eq!(
        store.get("name").await.unwrap(),
        Value::Text("Bob".to_string())
    );
    assert_eq!(
        store.get_path(path!("user.profile.email")).await.unwrap(),
        Value::Text("bob@example.org".to_string())
    );
    assert_eq!(
        store.get_path(path!("user.verified")).await.unwrap(),
        Value::Bool(true)
    );
    assert_key_not_found(store.get("count").await);
    assert_key_not_found(store.get("missing").await);

    // Staged writes stay invisible until commit.
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("data")
        .await
        .unwrap();
    assert_key_not_found(viewer.get("name").await);
    txn.commit().await.unwrap();

    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("data")
        .await
        .unwrap();
    assert_eq!(viewer.get_all().await.unwrap(), staged);
    assert_eq!(
        viewer.get("name").await.unwrap(),
        Value::Text("Bob".to_string())
    );
    assert_key_not_found(viewer.get("count").await);
}

#[tokio::test]
async fn test_docstore_get_deleted_keys_are_not_found() {
    let ctx = TestContext::new().with_database().await;
    create_dict_operation(
        ctx.database(),
        "data",
        &[("deleted", "old"), ("kept", "value")],
    )
    .await;
    let txn = ctx.database().new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();

    assert_dict_value(&store, "deleted", "old").await;
    assert!(store.delete("deleted").await.unwrap());
    // The staged tombstone must hide the historical value, not return Deleted.
    assert_key_not_found(store.get("deleted").await);
    assert_key_not_found(store.get_string("deleted").await.map(Value::from));
    assert_key_not_found(store.get("missing").await);
    assert_dict_value(&store, "kept", "value").await;

    // Another view still sees the historical value until the deletion commits.
    assert_dict_viewer_data(ctx.database(), "data", &[("deleted", "old")]).await;
    txn.commit().await.unwrap();
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("data")
        .await
        .unwrap();
    assert_key_not_found(viewer.get("deleted").await);
    assert_key_not_found(viewer.get_string("deleted").await.map(Value::from));
    assert_key_not_found(viewer.get("missing").await);
    assert_dict_value(&viewer, "kept", "value").await;

    // A default can replace a staged tombstone through canonical get/set.
    let txn = ctx.database().new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("deleted", "restored").await.unwrap();
    assert_dict_value(&store, "deleted", "restored").await;
    assert!(store.delete("deleted").await.unwrap());
    assert_eq!(
        store
            .get_or_insert("deleted", String::from("default"))
            .await
            .unwrap(),
        "default"
    );
    assert_dict_value(&store, "deleted", "default").await;
    txn.commit().await.unwrap();
    assert_dict_viewer_data(
        ctx.database(),
        "data",
        &[("deleted", "default"), ("kept", "value")],
    )
    .await;
}

#[tokio::test]
async fn test_dict_get_all_via_viewer() {
    let ctx = TestContext::new().with_database().await;

    // Test dict persistence helper
    test_dict_persistence(ctx.database(), "my_kv").await;

    // Verify get_all using a viewer
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("my_kv")
        .await
        .expect("Failed to get viewer");
    let all_data_crdt = viewer.get_all().await.expect("Failed to get all data");

    assert_eq!(all_data_crdt.len(), 3);
    assert_eq!(
        all_data_crdt.get("key_a"),
        Some(&Value::Text("val_a".to_string()))
    );
    assert_eq!(
        all_data_crdt.get("key_b"),
        Some(&Value::Text("val_b_updated".to_string()))
    );
    assert_eq!(
        all_data_crdt.get("key_c"),
        Some(&Value::Text("val_c".to_string()))
    );
}

#[tokio::test]
async fn test_dict_get_all_empty() {
    let ctx = TestContext::new().with_database().await;

    // Get viewer for a non-existent subtree
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("empty_kv")
        .await
        .expect("Failed to get viewer for empty");
    let all_data_crdt = viewer
        .get_all()
        .await
        .expect("Failed to get all data from empty");

    assert!(all_data_crdt.is_empty());
}

#[tokio::test]
async fn test_dict_delete() {
    let ctx = TestContext::new().with_database().await;
    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");

    {
        let dict = txn
            .get_store::<DocStore>("my_kv")
            .await
            .expect("Failed to get Doc");

        // Set initial values
        dict.set("key1", "value1")
            .await
            .expect("Failed to set key1");
        dict.set("key2", "value2")
            .await
            .expect("Failed to set key2");

        // Delete a key
        dict.delete("key1").await.expect("Failed to delete key1");

        // Verify key1 is deleted
        assert_key_not_found(dict.get("key1").await);

        // key2 should still be accessible
        assert_dict_value(&dict, "key2", "value2").await;
    }

    // Commit the operation
    txn.commit().await.expect("Failed to commit transaction");

    // Verify the deletion persisted
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("my_kv")
        .await
        .expect("Failed to get viewer");
    assert_key_not_found(viewer.get("key1").await);

    assert_dict_value(&viewer, "key2", "value2").await;
}

#[tokio::test]
async fn test_dict_set_nested_value() {
    let ctx = TestContext::new().with_database().await;

    // Use helper to create nested map operation
    create_dict_with_nested_map(ctx.database(), "my_kv").await;

    // Get viewer to verify persistence
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("my_kv")
        .await
        .expect("Failed to get viewer");

    // Check string value persisted
    assert_dict_value(&viewer, "key1", "value1").await;

    // Check nested map structure
    assert_dict_nested_map(&viewer, "key2", &[("inner", "nested_value")]).await;
}

#[tokio::test]
async fn test_dict_list_basic_operations() {
    let ctx = TestContext::new().with_database().await;

    // Use helper to create Doc with list
    let list_items = &["apple", "banana", "orange"];
    create_dict_with_list(ctx.database(), "my_kv", list_items).await;

    // Verify with viewer
    assert_dict_list_data(ctx.database(), "my_kv", "fruits", list_items).await;
}

#[tokio::test]
async fn test_dict_list_nonexistent_key() {
    let ctx = TestContext::new().with_database().await;
    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");

    {
        let dict = txn
            .get_store::<DocStore>("my_kv")
            .await
            .expect("Failed to get Doc");

        // Test getting non-existent list should return NotFound error
        assert_key_not_found(dict.get("nonexistent").await);

        // Test getting non-existent list with get_list should also return NotFound
        let list_result = dict.get_as::<List>("nonexistent").await;
        assert!(list_result.is_err());

        // Create a new list
        let mut new_list = List::new();
        new_list.push(Value::Text("first_item".to_string()));

        dict.set_list("new_list", new_list)
            .await
            .expect("Failed to set new list");

        // Verify the new list was created
        let retrieved_list = dict
            .get_as::<List>("new_list")
            .await
            .expect("Failed to get new list");
        assert_eq!(retrieved_list.len(), 1);
        assert_eq!(
            retrieved_list.get(0),
            Some(&Value::Text("first_item".to_string()))
        );
    }
}

#[tokio::test]
async fn test_dict_list_persistence() {
    let ctx = TestContext::new().with_database().await;

    // Create list in first transaction
    let txn1 = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start op1");
    {
        let dict = txn1
            .get_store::<DocStore>("my_kv")
            .await
            .expect("Failed to get Doc");

        let mut colors = List::new();
        colors.push(Value::Text("red".to_string()));
        colors.push(Value::Text("green".to_string()));

        dict.set_list("colors", colors)
            .await
            .expect("Failed to set colors list");
    }
    txn1.commit().await.expect("Failed to commit op1");

    // Modify list in second transaction
    let txn2 = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start op2");
    {
        let dict = txn2
            .get_store::<DocStore>("my_kv")
            .await
            .expect("Failed to get Doc");

        // List should persist from previous operation
        let colors = dict
            .get_as::<List>("colors")
            .await
            .expect("Failed to get colors list");
        assert_eq!(colors.len(), 2);
        assert_eq!(colors.get(0), Some(&Value::Text("red".to_string())));
        assert_eq!(colors.get(1), Some(&Value::Text("green".to_string())));

        // Modify the list - remove first element and add blue
        let mut updated_colors = colors.clone();
        updated_colors.remove(0); // Remove red
        updated_colors.push(Value::Text("blue".to_string())); // Add blue

        dict.set_list("colors", updated_colors)
            .await
            .expect("Failed to update colors list");
    }
    txn2.commit().await.expect("Failed to commit op2");

    // Verify final state
    assert_dict_list_data(ctx.database(), "my_kv", "colors", &["green", "blue"]).await;
}

#[tokio::test]
async fn test_dict_update_nested_value() {
    let ctx = TestContext::new().with_database().await;

    // First operation: Create initial nested structure
    let txn1 = ctx
        .database()
        .new_transaction()
        .await
        .expect("Txn1: Failed to start");
    {
        let dict = txn1
            .get_store::<DocStore>("nested_test")
            .await
            .expect("Txn1: Failed to get Doc");

        // Create level1 -> level2_str structure
        let mut l1_map = Doc::new();
        l1_map.set("level2_str", "initial_value");
        dict.set("level1", l1_map)
            .await
            .expect("Txn1: Failed to set level1");
    }
    txn1.commit().await.expect("Txn1: Failed to commit");

    // Second operation: Update with another structure
    let txn2 = ctx
        .database()
        .new_transaction()
        .await
        .expect("Txn2: Failed to start");
    {
        let dict = txn2
            .get_store::<DocStore>("nested_test")
            .await
            .expect("Txn2: Failed to get Doc");

        // Create an entirely new map structure that will replace the old one
        let mut l2_map = Doc::new();
        l2_map.set("deep_key", "deep_value");

        let mut new_l1_map = Doc::new();
        new_l1_map.set("level2_map", l2_map);

        // Completely replace the previous value at level1
        dict.set("level1", new_l1_map.clone())
            .await
            .expect("Txn2: Failed to overwrite level1");

        // Verify the update within the same operation
        match dict.get("level1").await.expect("Failed to get level1") {
            Value::Doc(retrieved_l1_map) => {
                // Check if level2_map exists with the expected content
                match retrieved_l1_map.get("level2_map") {
                    Some(Value::Doc(retrieved_l2_map)) => match retrieved_l2_map.get("deep_key") {
                        Some(Value::Text(val)) => assert_eq!(val, "deep_value"),
                        _ => panic!("Expected string 'deep_value' at deep_key"),
                    },
                    _ => panic!("Expected 'level2_map' to be a map"),
                }
            }
            _ => panic!("Expected 'level1' to be a map"),
        }
    }
    txn2.commit().await.expect("Txn2: Failed to commit");

    // Verify the update persists after commit
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("nested_test")
        .await
        .expect("Failed to get viewer");

    // Verify the structure after commit
    match viewer
        .get("level1")
        .await
        .expect("Viewer: Failed to get level1")
    {
        Value::Doc(retrieved_l1_map) => {
            // Check if level2_map exists with expected content
            match retrieved_l1_map.get("level2_map") {
                Some(Value::Doc(retrieved_l2_map)) => match retrieved_l2_map.get("deep_key") {
                    Some(Value::Text(val)) => assert_eq!(val, "deep_value"),
                    _ => panic!("Viewer: Expected string 'deep_value' at deep_key"),
                },
                _ => panic!("Viewer: Expected 'level2_map' to be a map"),
            }
        }
        _ => panic!("Viewer: Expected 'level1' to be a map"),
    }
}

#[tokio::test]
async fn test_dict_comprehensive_operations() {
    let ctx = TestContext::new().with_database().await;
    let txn = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start transaction");

    {
        let dict = txn
            .get_store::<DocStore>("test_store")
            .await
            .expect("Failed to get Doc");

        // Set basic string values
        dict.set("key1", "value1")
            .await
            .expect("Failed to set key1");
        dict.set("key2", "value2")
            .await
            .expect("Failed to set key2");

        // Set a nested map value
        let mut nested = Doc::new();
        nested.set("nested_key1", "nested_value1");
        nested.set("nested_key2", "nested_value2");
        dict.set("nested", Value::Doc(nested.clone()))
            .await
            .expect("Failed to set nested map");
    }

    // Commit the operation
    txn.commit().await.expect("Failed to commit transaction");

    // Get a viewer to check the subtree
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("test_store")
        .await
        .expect("Failed to get viewer");

    // Check string values
    assert_dict_value(&viewer, "key1", "value1").await;
    assert_dict_value(&viewer, "key2", "value2").await;

    // Check nested map
    assert_dict_nested_map(
        &viewer,
        "nested",
        &[
            ("nested_key1", "nested_value1"),
            ("nested_key2", "nested_value2"),
        ],
    )
    .await;

    // Check non-existent key
    assert_key_not_found(viewer.get("non_existent").await);
}

#[tokio::test]
async fn test_empty_dict_behavior() {
    let ctx = TestContext::new().with_database().await;

    // Test empty Doc behavior
    assert_dict_viewer_count(ctx.database(), "empty_dict", 0).await;

    let dict_viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("empty_dict")
        .await
        .expect("Failed to get empty Doc viewer");
    assert_key_not_found(dict_viewer.get("any_key").await);
}
