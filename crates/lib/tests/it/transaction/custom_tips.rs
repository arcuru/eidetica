//! Custom tips operation tests for Transaction
//!
//! This module contains tests for operations using custom tips including
//! branching, parallel operations, and tip-based state management.

use eidetica::{crdt::doc::Value, store::DocStore};

use super::helpers::*;
use crate::helpers::*;

#[tokio::test]
async fn test_transaction_with_custom_tips() {
    let ctx = TestContext::new().with_database().await;

    // Create a chain of operations: A -> B -> C
    let op_a = ctx.database().new_transaction().await.unwrap();
    let store_a = op_a.get_store::<DocStore>("data").await.unwrap();
    store_a.set("step", "A").await.unwrap();
    store_a.set("a_data", "value_a").await.unwrap();
    let entry_a_id = op_a.commit().await.unwrap();

    let op_b = ctx.database().new_transaction().await.unwrap();
    let store_b = op_b.get_store::<DocStore>("data").await.unwrap();
    store_b.set("step", "B").await.unwrap();
    store_b.set("b_data", "value_b").await.unwrap();
    let _entry_b_id = op_b.commit().await.unwrap();

    let op_c = ctx.database().new_transaction().await.unwrap();
    let store_c = op_c.get_store::<DocStore>("data").await.unwrap();
    store_c.set("step", "C").await.unwrap();
    store_c.set("c_data", "value_c").await.unwrap();
    let _entry_c_id = op_c.commit().await.unwrap();

    // Create operation from entry A using new_transaction_with_tips
    let op_from_a = ctx
        .database()
        .new_transaction_with_tips(std::slice::from_ref(&entry_a_id))
        .await
        .unwrap();
    let store_from_a = op_from_a.get_store::<DocStore>("data").await.unwrap();

    // This operation should only see data from A
    let state_from_a = get_dict_data(&store_from_a).await;
    assert_map_data(&state_from_a, &[("step", "A"), ("a_data", "value_a")]);

    // Add new data to this operation
    store_from_a
        .set("branch_data", "branch_value")
        .await
        .unwrap();
    let branch_id = op_from_a.commit().await.unwrap();

    // Verify the branch entry has correct parent relationship
    let backend = ctx.database().backend().unwrap();
    let branch_entry = backend.get(&branch_id).await.unwrap();
    let branch_parents = branch_entry.parents().unwrap();

    assert_eq!(branch_parents.len(), 1, "Branch should have one parent");
    assert_eq!(
        branch_parents[0], entry_a_id,
        "Branch should have entry A as parent"
    );
}

#[tokio::test]
async fn test_transaction_multiple_subtrees_with_custom_tips() {
    let ctx = TestContext::new().with_database().await;

    // Create base entry with multiple subtrees
    let op_base = ctx.database().new_transaction().await.unwrap();
    let users_base = op_base.get_store::<DocStore>("users").await.unwrap();
    let posts_base = op_base.get_store::<DocStore>("posts").await.unwrap();

    users_base.set("user1", "alice").await.unwrap();
    posts_base.set("post1", "hello").await.unwrap();
    let base_id = op_base.commit().await.unwrap();

    // Create branch that only modifies users
    let op_users = ctx
        .database()
        .new_transaction_with_tips(std::slice::from_ref(&base_id))
        .await
        .unwrap();
    let users_branch = op_users.get_store::<DocStore>("users").await.unwrap();
    users_branch.set("user2", "bob").await.unwrap();
    let users_id = op_users.commit().await.unwrap();

    // Create branch that only modifies posts
    let op_posts = ctx
        .database()
        .new_transaction_with_tips([base_id])
        .await
        .unwrap();
    let posts_branch = op_posts.get_store::<DocStore>("posts").await.unwrap();
    posts_branch.set("post2", "world").await.unwrap();
    let posts_id = op_posts.commit().await.unwrap();

    // Create merge operation
    let op_merge = ctx
        .database()
        .new_transaction_with_tips([users_id.clone(), posts_id.clone()])
        .await
        .unwrap();
    let users_merge = op_merge.get_store::<DocStore>("users").await.unwrap();
    let posts_merge = op_merge.get_store::<DocStore>("posts").await.unwrap();

    // Should see data from both branches in both subtrees
    let users_state = get_dict_data(&users_merge).await;
    assert_map_data(&users_state, &[("user1", "alice"), ("user2", "bob")]);

    let posts_state = get_dict_data(&posts_merge).await;
    assert_map_data(&posts_state, &[("post1", "hello"), ("post2", "world")]);

    // Add new data in merge
    users_merge.set("user3", "charlie").await.unwrap();
    posts_merge.set("post3", "merged").await.unwrap();
    let merge_id = op_merge.commit().await.unwrap();

    // Verify final state has all data
    let op_final = ctx
        .database()
        .new_transaction_with_tips([merge_id])
        .await
        .unwrap();
    let users_final = op_final.get_store::<DocStore>("users").await.unwrap();
    let posts_final = op_final.get_store::<DocStore>("posts").await.unwrap();

    let final_users = users_final.get_all().await.unwrap();
    assert!(final_users.get("user1").is_some());
    assert!(final_users.get("user2").is_some());
    assert!(final_users.get("user3").is_some());

    let final_posts = posts_final.get_all().await.unwrap();
    assert!(final_posts.get("post1").is_some());
    assert!(final_posts.get("post2").is_some());
    assert!(final_posts.get("post3").is_some());
}

#[tokio::test]
async fn test_transaction_custom_tips_subtree_in_ancestors_not_tips() {
    let ctx = TestContext::new().with_database().await;

    // Create base entry with subtree data
    let op1 = ctx.database().new_transaction().await.unwrap();
    let store1 = op1.get_store::<DocStore>("data").await.unwrap();
    store1.set("key1", "value1").await.unwrap();
    let entry1_id = op1.commit().await.unwrap();

    // Create a parallel branch that also has subtree data
    let op2 = ctx
        .database()
        .new_transaction_with_tips(std::slice::from_ref(&entry1_id))
        .await
        .unwrap();
    let store2 = op2.get_store::<DocStore>("data").await.unwrap();
    store2.set("key2", "value2").await.unwrap();
    let entry2_id = op2.commit().await.unwrap();

    // Create another branch that does NOT touch the "data" subtree at all
    let op3 = ctx
        .database()
        .new_transaction_with_tips([entry1_id])
        .await
        .unwrap();
    // Only touch a different subtree
    let settings3 = op3.get_store::<DocStore>("settings").await.unwrap();
    settings3.set("config", "value").await.unwrap();
    let entry3_id = op3.commit().await.unwrap();

    // Create a merge operation using both branches as tips
    // entry2_id has subtree data, entry3_id does NOT have subtree data
    let op4 = ctx
        .database()
        .new_transaction_with_tips([entry2_id.clone(), entry3_id.clone()])
        .await
        .unwrap();
    let store4 = op4.get_store::<DocStore>("data").await.unwrap();

    // Should be able to access all the data from both branches
    // This tests the case where one tip has the subtree (entry2) and one doesn't (entry3)
    let state = store4.get_all().await.unwrap();
    match state.get("key1") {
        Some(Value::Text(value)) => assert_eq!(value, "value1"),
        _ => panic!("Expected key1 to have value 'value1' from entry1"),
    }
    match state.get("key2") {
        Some(Value::Text(value)) => assert_eq!(value, "value2"),
        _ => panic!("Expected key2 to have value 'value2' from entry2"),
    }

    // Should also be able to access settings from entry3
    let settings4 = op4.get_store::<DocStore>("settings").await.unwrap();
    let settings_state = settings4.get_all().await.unwrap();
    match settings_state.get("config") {
        Some(Value::Text(value)) => assert_eq!(value, "value"),
        _ => panic!("Expected config to have value 'value'"),
    }
}

#[tokio::test]
async fn test_transaction_custom_tips_no_subtree_data_in_tips() {
    let ctx = TestContext::new().with_database().await;

    // Create entry with subtree data
    let op1 = ctx.database().new_transaction().await.unwrap();
    let store1 = op1.get_store::<DocStore>("data").await.unwrap();
    store1.set("original", "value").await.unwrap();
    let _entry1_id = op1.commit().await.unwrap();

    // Create entry that does NOT modify the "data" subtree
    // This simulates the case where we have tree evolution but no subtree changes
    let op2 = ctx.database().new_transaction().await.unwrap();
    let settings2 = op2.get_store::<DocStore>("settings").await.unwrap();
    settings2.set("config1", "value1").await.unwrap();
    let entry2_id = op2.commit().await.unwrap();

    // Create another entry that also doesn't modify "data" subtree
    let op3 = ctx.database().new_transaction().await.unwrap();
    let metadata3 = op3.get_store::<DocStore>("metadata").await.unwrap();
    metadata3.set("info", "some info").await.unwrap();
    let entry3_id = op3.commit().await.unwrap();

    // Now use ONLY the entries that don't have "data" subtree as custom tips
    // The "data" subtree should still be accessible from their common ancestor (entry1)
    let op4 = ctx
        .database()
        .new_transaction_with_tips([entry2_id.clone(), entry3_id.clone()])
        .await
        .unwrap();
    let store4 = op4.get_store::<DocStore>("data").await.unwrap();

    // This should work: accessing subtree data that exists in ancestors
    // but not in the tip entries themselves
    let state = store4.get_all().await.unwrap();
    match state.get("original") {
        Some(Value::Text(value)) => assert_eq!(value, "value"),
        _ => panic!("Expected 'original' to have value 'value' from ancestor entry1"),
    }

    // Verify we can also access the data from the tip entries
    let settings4 = op4.get_store::<DocStore>("settings").await.unwrap();
    let settings_state = settings4.get_all().await.unwrap();
    assert!(
        settings_state.get("config1").is_some(),
        "Should have config1 from entry2"
    );

    let metadata4 = op4.get_store::<DocStore>("metadata").await.unwrap();
    let metadata_state = metadata4.get_all().await.unwrap();
    assert!(
        metadata_state.get("info").is_some(),
        "Should have info from entry3"
    );
}
