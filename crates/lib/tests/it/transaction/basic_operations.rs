//! Basic Transaction operation tests
//!
//! This module contains tests for fundamental Transaction functionality including
//! Doc operations, multiple subtrees, empty subtree handling, and parent relationships.

use eidetica::store::DocStore;

use super::helpers::*;
use crate::helpers::*;

#[tokio::test]
async fn test_transaction_through_dict() {
    // Create a backend and a tree
    let ctx = TestContext::new().with_database().await;

    // Create a new operation
    let operation = ctx.database().new_transaction().await.unwrap();

    // Get a Doc subtree, which will use Transaction internally
    let dict = operation.get_store::<DocStore>("test").await.unwrap();

    // Set a value in the Doc, which will use Transaction::update_subtree internally
    dict.set("key", "value").await.unwrap();

    // Commit the operation
    operation.commit().await.unwrap();

    // Use a new operation to read the data
    let read_op = ctx.database().new_transaction().await.unwrap();
    let read_store = read_op.get_store::<DocStore>("test").await.unwrap();

    // Verify the value was set correctly
    assert_dict_value(&read_store, "key", "value").await;

    // Also test the get_string convenience method
    assert_eq!(read_store.get_string("key").await.unwrap(), "value");
}

#[tokio::test]
async fn test_transaction_multiple_subtrees() {
    // Create a backend and a tree
    let ctx = TestContext::new().with_database().await;

    // Create a new operation
    let operation = ctx.database().new_transaction().await.unwrap();

    // Create two different Doc subtrees
    let store1 = operation.get_store::<DocStore>("store1").await.unwrap();
    let store2 = operation.get_store::<DocStore>("store2").await.unwrap();

    // Set values in each store
    store1.set("key1", "value1").await.unwrap();
    store2.set("key2", "value2").await.unwrap();

    // Update a value in store1
    store1.set("key1", "updated").await.unwrap();

    // Commit the operation
    operation.commit().await.unwrap();

    // Create a new transaction to read the data
    let read_op = ctx.database().new_transaction().await.unwrap();
    let store1_read = read_op.get_store::<DocStore>("store1").await.unwrap();
    let store2_read = read_op.get_store::<DocStore>("store2").await.unwrap();

    // Verify values in both stores using helpers
    assert_dict_contains(&store1_read, &[("key1", "updated")]).await;
    assert_dict_contains(&store2_read, &[("key2", "value2")]).await;
}

#[tokio::test]
async fn test_transaction_empty_subtree_removal() {
    // Create a backend and a tree
    let ctx = TestContext::new().with_database().await;

    // Create a new operation
    let operation = ctx.database().new_transaction().await.unwrap();

    // Create a Doc subtree but don't add any data (will be empty)
    let _empty_store = operation.get_store::<DocStore>("empty").await.unwrap();

    // Create another Doc and add data
    let data_store = operation.get_store::<DocStore>("data").await.unwrap();
    data_store.set("key", "value").await.unwrap();

    // Commit the operation - should remove the empty subtree
    operation.commit().await.unwrap();

    // Create a new transaction to check if subtrees exist
    let read_op = ctx.database().new_transaction().await.unwrap();

    // Try to access both subtrees
    let data_result = read_op.get_store::<DocStore>("data").await;
    let empty_result = read_op.get_store::<DocStore>("empty").await;

    // The data subtree should be accessible
    assert!(data_result.is_ok());

    // The empty subtree should have been removed, but accessing it doesn't fail
    // because Doc creates it if it doesn't exist
    assert!(empty_result.is_ok());

    // However, the empty subtree should not have any data
    let empty_store = empty_result.unwrap();
    // If we try to get any key from the empty store, it should return NotFound
    assert_key_not_found(empty_store.get("any_key").await);
}

#[tokio::test]
async fn test_transaction_parent_relationships() {
    // Create a backend and a tree
    let ctx = TestContext::new().with_database().await;

    // Create first transaction and set data
    let txn1 = ctx.database().new_transaction().await.unwrap();
    let store1 = txn1.get_store::<DocStore>("data").await.unwrap();
    store1.set("first", "entry").await.unwrap();
    txn1.commit().await.unwrap();

    // Create second transaction that will use the first as parent
    let txn2 = ctx.database().new_transaction().await.unwrap();
    let store2 = txn2.get_store::<DocStore>("data").await.unwrap();
    store2.set("second", "entry").await.unwrap();
    txn2.commit().await.unwrap();

    // Create a third transaction to read all entries
    let txn3 = ctx.database().new_transaction().await.unwrap();
    let store3 = txn3.get_store::<DocStore>("data").await.unwrap();

    // Get all data - should include both entries due to CRDT merge
    let all_data = get_dict_data(&store3).await;

    // Verify both entries are included in merged data using helpers
    assert_map_data(&all_data, &[("first", "entry"), ("second", "entry")]);
}
