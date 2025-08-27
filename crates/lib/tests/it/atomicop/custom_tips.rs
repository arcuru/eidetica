//! Custom tips operation tests for AtomicOp
//!
//! This module contains tests for operations using custom tips including
//! branching, parallel operations, and tip-based state management.

use super::helpers::*;
use crate::helpers::*;
use eidetica::crdt::doc::Value;
use eidetica::subtree::DocStore;

#[test]
fn test_atomicop_with_custom_tips() {
    let tree = setup_tree();

    // Create a chain of operations: A -> B -> C
    let op_a = tree.new_operation().unwrap();
    let store_a = op_a.get_subtree::<DocStore>("data").unwrap();
    store_a.set("step", "A").unwrap();
    store_a.set("a_data", "value_a").unwrap();
    let entry_a_id = op_a.commit().unwrap();

    let op_b = tree.new_operation().unwrap();
    let store_b = op_b.get_subtree::<DocStore>("data").unwrap();
    store_b.set("step", "B").unwrap();
    store_b.set("b_data", "value_b").unwrap();
    let _entry_b_id = op_b.commit().unwrap();

    let op_c = tree.new_operation().unwrap();
    let store_c = op_c.get_subtree::<DocStore>("data").unwrap();
    store_c.set("step", "C").unwrap();
    store_c.set("c_data", "value_c").unwrap();
    let _entry_c_id = op_c.commit().unwrap();

    // Create operation from entry A using new_operation_with_tips
    let op_from_a = tree
        .new_operation_with_tips(std::slice::from_ref(&entry_a_id))
        .unwrap();
    let store_from_a = op_from_a.get_subtree::<DocStore>("data").unwrap();

    // This operation should only see data from A
    let state_from_a = get_dict_data(&store_from_a);
    assert_map_data(&state_from_a, &[("step", "A"), ("a_data", "value_a")]);

    // Add new data to this operation
    store_from_a.set("branch_data", "branch_value").unwrap();
    let branch_id = op_from_a.commit().unwrap();

    // Verify the branch entry has correct parent relationship
    let backend = tree.backend();
    let branch_entry = backend.get(&branch_id).unwrap();
    let branch_parents = branch_entry.parents().unwrap();

    assert_eq!(branch_parents.len(), 1, "Branch should have one parent");
    assert_eq!(
        branch_parents[0], entry_a_id,
        "Branch should have entry A as parent"
    );
}

#[test]
fn test_atomicop_multiple_subtrees_with_custom_tips() {
    let tree = setup_tree();

    // Create base entry with multiple subtrees
    let op_base = tree.new_operation().unwrap();
    let users_base = op_base.get_subtree::<DocStore>("users").unwrap();
    let posts_base = op_base.get_subtree::<DocStore>("posts").unwrap();

    users_base.set("user1", "alice").unwrap();
    posts_base.set("post1", "hello").unwrap();
    let base_id = op_base.commit().unwrap();

    // Create branch that only modifies users
    let op_users = tree
        .new_operation_with_tips(std::slice::from_ref(&base_id))
        .unwrap();
    let users_branch = op_users.get_subtree::<DocStore>("users").unwrap();
    users_branch.set("user2", "bob").unwrap();
    let users_id = op_users.commit().unwrap();

    // Create branch that only modifies posts
    let op_posts = tree.new_operation_with_tips([base_id]).unwrap();
    let posts_branch = op_posts.get_subtree::<DocStore>("posts").unwrap();
    posts_branch.set("post2", "world").unwrap();
    let posts_id = op_posts.commit().unwrap();

    // Create merge operation
    let op_merge = tree
        .new_operation_with_tips([users_id.clone(), posts_id.clone()])
        .unwrap();
    let users_merge = op_merge.get_subtree::<DocStore>("users").unwrap();
    let posts_merge = op_merge.get_subtree::<DocStore>("posts").unwrap();

    // Should see data from both branches in both subtrees
    let users_state = get_dict_data(&users_merge);
    assert_map_data(&users_state, &[("user1", "alice"), ("user2", "bob")]);

    let posts_state = get_dict_data(&posts_merge);
    assert_map_data(&posts_state, &[("post1", "hello"), ("post2", "world")]);

    // Add new data in merge
    users_merge.set("user3", "charlie").unwrap();
    posts_merge.set("post3", "merged").unwrap();
    let merge_id = op_merge.commit().unwrap();

    // Verify final state has all data
    let op_final = tree.new_operation_with_tips([merge_id]).unwrap();
    let users_final = op_final.get_subtree::<DocStore>("users").unwrap();
    let posts_final = op_final.get_subtree::<DocStore>("posts").unwrap();

    let final_users = users_final.get_all().unwrap();
    assert!(final_users.get("user1").is_some());
    assert!(final_users.get("user2").is_some());
    assert!(final_users.get("user3").is_some());

    let final_posts = posts_final.get_all().unwrap();
    assert!(final_posts.get("post1").is_some());
    assert!(final_posts.get("post2").is_some());
    assert!(final_posts.get("post3").is_some());
}

#[test]
fn test_atomicop_custom_tips_subtree_in_ancestors_not_tips() {
    let tree = setup_tree();

    // Create base entry with subtree data
    let op1 = tree.new_operation().unwrap();
    let store1 = op1.get_subtree::<DocStore>("data").unwrap();
    store1.set("key1", "value1").unwrap();
    let entry1_id = op1.commit().unwrap();

    // Create a parallel branch that also has subtree data
    let op2 = tree
        .new_operation_with_tips(std::slice::from_ref(&entry1_id))
        .unwrap();
    let store2 = op2.get_subtree::<DocStore>("data").unwrap();
    store2.set("key2", "value2").unwrap();
    let entry2_id = op2.commit().unwrap();

    // Create another branch that does NOT touch the "data" subtree at all
    let op3 = tree.new_operation_with_tips([entry1_id]).unwrap();
    // Only touch a different subtree
    let settings3 = op3.get_subtree::<DocStore>("settings").unwrap();
    settings3.set("config", "value").unwrap();
    let entry3_id = op3.commit().unwrap();

    // Create a merge operation using both branches as tips
    // entry2_id has subtree data, entry3_id does NOT have subtree data
    let op4 = tree
        .new_operation_with_tips([entry2_id.clone(), entry3_id.clone()])
        .unwrap();
    let store4 = op4.get_subtree::<DocStore>("data").unwrap();

    // Should be able to access all the data from both branches
    // This tests the case where one tip has the subtree (entry2) and one doesn't (entry3)
    let state = store4.get_all().unwrap();
    match state.get("key1") {
        Some(Value::Text(value)) => assert_eq!(value, "value1"),
        _ => panic!("Expected key1 to have value 'value1' from entry1"),
    }
    match state.get("key2") {
        Some(Value::Text(value)) => assert_eq!(value, "value2"),
        _ => panic!("Expected key2 to have value 'value2' from entry2"),
    }

    // Should also be able to access settings from entry3
    let settings4 = op4.get_subtree::<DocStore>("settings").unwrap();
    let settings_state = settings4.get_all().unwrap();
    match settings_state.get("config") {
        Some(Value::Text(value)) => assert_eq!(value, "value"),
        _ => panic!("Expected config to have value 'value'"),
    }
}

#[test]
fn test_atomicop_custom_tips_no_subtree_data_in_tips() {
    let tree = setup_tree();

    // Create entry with subtree data
    let op1 = tree.new_operation().unwrap();
    let store1 = op1.get_subtree::<DocStore>("data").unwrap();
    store1.set("original", "value").unwrap();
    let _entry1_id = op1.commit().unwrap();

    // Create entry that does NOT modify the "data" subtree
    // This simulates the case where we have tree evolution but no subtree changes
    let op2 = tree.new_operation().unwrap();
    let settings2 = op2.get_subtree::<DocStore>("settings").unwrap();
    settings2.set("config1", "value1").unwrap();
    let entry2_id = op2.commit().unwrap();

    // Create another entry that also doesn't modify "data" subtree
    let op3 = tree.new_operation().unwrap();
    let metadata3 = op3.get_subtree::<DocStore>("metadata").unwrap();
    metadata3.set("info", "some info").unwrap();
    let entry3_id = op3.commit().unwrap();

    // Now use ONLY the entries that don't have "data" subtree as custom tips
    // The "data" subtree should still be accessible from their common ancestor (entry1)
    let op4 = tree
        .new_operation_with_tips([entry2_id.clone(), entry3_id.clone()])
        .unwrap();
    let store4 = op4.get_subtree::<DocStore>("data").unwrap();

    // This should work: accessing subtree data that exists in ancestors
    // but not in the tip entries themselves
    let state = store4.get_all().unwrap();
    match state.get("original") {
        Some(Value::Text(value)) => assert_eq!(value, "value"),
        _ => panic!("Expected 'original' to have value 'value' from ancestor entry1"),
    }

    // Verify we can also access the data from the tip entries
    let settings4 = op4.get_subtree::<DocStore>("settings").unwrap();
    let settings_state = settings4.get_all().unwrap();
    assert!(
        settings_state.get("config1").is_some(),
        "Should have config1 from entry2"
    );

    let metadata4 = op4.get_subtree::<DocStore>("metadata").unwrap();
    let metadata_state = metadata4.get_all().unwrap();
    assert!(
        metadata_state.get("info").is_some(),
        "Should have info from entry3"
    );
}
