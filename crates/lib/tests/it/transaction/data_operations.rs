//! Data operation tests for Transaction
//!
//! This module contains tests focused on data manipulation including
//! deletes, nested values, and staging isolation.

use eidetica::{
    constants::SETTINGS,
    crdt::{Doc, doc::Value},
    store::DocStore,
};

use super::helpers::*;
use crate::helpers::*;

#[tokio::test]
async fn test_transaction_with_delete() {
    let ctx = TestContext::new().with_database().await;

    // Create an operation and add some data
    let op1 = ctx.database().new_transaction().await.unwrap();
    let store1 = op1.get_store::<DocStore>("data").await.unwrap();
    store1.set("key1", "value1").await.unwrap();
    store1.set("key2", "value2").await.unwrap();
    op1.commit().await.unwrap();

    // Create another operation to delete a key
    let op2 = ctx.database().new_transaction().await.unwrap();
    let store2 = op2.get_store::<DocStore>("data").await.unwrap();
    store2.delete("key1").await.unwrap();
    op2.commit().await.unwrap();

    // Verify with a third operation
    let op3 = ctx.database().new_transaction().await.unwrap();
    let store3 = op3.get_store::<DocStore>("data").await.unwrap();

    // key1 should be deleted
    assert_key_not_found(store3.get("key1").await);

    // key2 should still exist
    assert_dict_value(&store3, "key2", "value2").await;

    // Check the full state with tombstone using helpers
    let all_data = get_dict_data(&store3).await;
    assert_has_tombstone(&all_data, "key1");
    assert_tombstone_hidden(&all_data, "key1");
    assert_map_data(&all_data, &[("key2", "value2")]);
}

#[tokio::test]
async fn test_transaction_nested_values() {
    const TEST_KEY: &str = "test_key";
    let (_instance, tree) = setup_db_and_tree_with_key(TEST_KEY).await;

    // Create an operation
    let op1 = tree.new_transaction().await.unwrap();
    let store1 = op1.get_store::<DocStore>("data").await.unwrap();

    // Set a regular string value
    store1.set("string_key", "string_value").await.unwrap();

    // Create and set a nested map value
    let mut nested = Doc::new();
    nested.set("inner1", "value1".to_string());
    nested.set("inner2", "value2".to_string());

    // Use the new set_value method to store a map
    store1.set_value("map_key", nested).await.unwrap();

    // Commit the operation
    op1.commit().await.unwrap();

    // Verify with a new operation
    let op2 = tree.new_transaction().await.unwrap();
    let store2 = op2.get_store::<DocStore>("data").await.unwrap();

    // Check the string value
    match store2.get("string_key").await.unwrap() {
        Value::Text(value) => assert_eq!(value, "string_value"),
        _ => panic!("Expected string value"),
    }

    // Check the nested map
    match store2.get("map_key").await.unwrap() {
        Value::Doc(map) => {
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

#[tokio::test]
async fn test_transaction_staged_data_isolation() {
    let ctx = TestContext::new().with_database().await;

    // Create initial data
    let op1 = ctx.database().new_transaction().await.unwrap();
    let store1 = op1.get_store::<DocStore>("data").await.unwrap();
    store1.set("key1", "committed_value").await.unwrap();
    let entry1_id = op1.commit().await.unwrap();

    // Create operation from entry1
    let op2 = ctx
        .database()
        .new_transaction_with_tips(std::slice::from_ref(&entry1_id))
        .await
        .unwrap();
    let store2 = op2.get_store::<DocStore>("data").await.unwrap();

    // Initially should see committed data
    assert_dict_value(&store2, "key1", "committed_value").await;

    // Stage new data (not yet committed)
    store2.set("key1", "staged_value").await.unwrap();
    store2.set("key2", "new_staged").await.unwrap();

    // Should now see staged data
    assert_dict_value(&store2, "key1", "staged_value").await;
    assert_dict_value(&store2, "key2", "new_staged").await;

    // Create another operation from same tip - should not see staged data
    let op3 = ctx
        .database()
        .new_transaction_with_tips([entry1_id])
        .await
        .unwrap();
    let store3 = op3.get_store::<DocStore>("data").await.unwrap();

    // Should see original committed data, not staged data from op2
    assert_dict_value(&store3, "key1", "committed_value").await;
    assert_key_not_found(store3.get("key2").await);

    // Commit op2
    let entry2_id = op2.commit().await.unwrap();

    // Create operation from entry2 - should see committed staged data
    let op4 = ctx
        .database()
        .new_transaction_with_tips([entry2_id])
        .await
        .unwrap();
    let store4 = op4.get_store::<DocStore>("data").await.unwrap();

    assert_dict_value(&store4, "key1", "staged_value").await;
    assert_dict_value(&store4, "key2", "new_staged").await;
}

#[tokio::test]
async fn test_metadata_for_settings_entries() {
    let (_instance, tree) = setup_tree_with_settings(&[("name", "test_tree")]).await;

    // Create a settings update
    let settings_op = tree.new_transaction().await.unwrap();
    let settings_subtree = settings_op.get_store::<DocStore>(SETTINGS).await.unwrap();
    settings_subtree.set("version", "1.0").await.unwrap();
    let settings_id = settings_op.commit().await.unwrap();

    // Now create a data entry (not touching settings)
    let data_op = tree.new_transaction().await.unwrap();
    let data_subtree = data_op.get_store::<DocStore>("data").await.unwrap();
    data_subtree.set("key1", "value1").await.unwrap();
    let data_id = data_op.commit().await.unwrap();

    // Get both entries from the backend through the tree
    let settings_entry = tree.get_entry(&settings_id).await.unwrap();
    let data_entry = tree.get_entry(&data_id).await.unwrap();

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

#[tokio::test]
async fn test_delete_operations_with_helpers() {
    let (_instance, tree) = setup_tree_with_data(&[(
        "data",
        &[
            ("keep1", "value1"),
            ("delete_me", "temp"),
            ("keep2", "value2"),
        ] as &[(&str, &str)],
    )])
    .await;

    // Test delete operation helper
    let delete_id = test_delete_operation(&tree, "data", "delete_me").await;
    assert!(!delete_id.to_string().is_empty());

    // Verify deletion with new operation
    let read_op = tree.new_transaction().await.unwrap();
    let store = read_op.get_store::<DocStore>("data").await.unwrap();
    let all_data = get_dict_data(&store).await;

    // Test tombstone helpers
    assert_has_tombstone(&all_data, "delete_me");
    assert_tombstone_hidden(&all_data, "delete_me");

    // Verify other keys still exist
    assert_map_data(&all_data, &[("keep1", "value1"), ("keep2", "value2")]);
}

#[tokio::test]
async fn test_nested_map_operations() {
    let ctx = TestContext::new().with_database().await;

    // Test nested map creation helper
    let nested_value = create_nested_map(&[("key1", "val1"), ("key2", "val2")]);

    match nested_value {
        Value::Doc(map) => {
            assert_eq!(map.get_as::<String>("key1"), Some("val1".to_string()));
            assert_eq!(map.get_as::<String>("key2"), Some("val2".to_string()));
        }
        _ => panic!("Expected Map value"),
    }

    // Test integration with Transaction
    let entry_id = create_operation_with_nested_data(ctx.database()).await;
    assert!(!entry_id.to_string().is_empty());

    // Verify using nested data helper
    let read_op = ctx.database().new_transaction().await.unwrap();
    let store = read_op.get_store::<DocStore>("data").await.unwrap();

    assert_nested_data(
        &store,
        "string_key",
        "map_key",
        &[("inner1", "value1"), ("inner2", "value2")],
    )
    .await;
}

#[tokio::test]
async fn test_nested_data_operations_with_helpers() {
    let ctx = TestContext::new().with_database().await;

    // Test nested data creation helper
    let entry_id = create_operation_with_nested_data(ctx.database()).await;
    assert!(!entry_id.to_string().is_empty());

    // Verify using nested data helper
    let read_op = ctx.database().new_transaction().await.unwrap();
    let store = read_op.get_store::<DocStore>("data").await.unwrap();

    assert_nested_data(
        &store,
        "string_key",
        "map_key",
        &[("inner1", "value1"), ("inner2", "value2")],
    )
    .await;
}
