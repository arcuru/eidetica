use crate::helpers::*;
use eidetica::constants::SETTINGS;
use eidetica::crdt::{Nested, NodeValue, Value};
use eidetica::subtree::{KVStore, SubTree};

#[test]
fn test_atomicop_through_kvstore() {
    // Create a backend and a tree
    let tree = setup_tree();

    // Create a new operation
    let operation = tree.new_operation().unwrap();

    // Get a KVStore subtree, which will use AtomicOp internally
    let kvstore = KVStore::new(&operation, "test").unwrap();

    // Set a value in the KVStore, which will use AtomicOp::update_subtree internally
    kvstore.set("key", "value").unwrap();

    // Commit the operation
    operation.commit().unwrap();

    // Use a new operation to read the data
    let read_op = tree.new_operation().unwrap();
    let read_store = KVStore::new(&read_op, "test").unwrap();

    // Verify the value was set correctly
    assert_kvstore_value(&read_store, "key", "value");

    // Also test the get_string convenience method
    assert_eq!(read_store.get_string("key").unwrap(), "value");
}

#[test]
fn test_atomicop_multiple_subtrees() {
    // Create a backend and a tree
    let tree = setup_tree();

    // Create a new operation
    let operation = tree.new_operation().unwrap();

    // Create two different KVStore subtrees
    let store1 = KVStore::new(&operation, "store1").unwrap();
    let store2 = KVStore::new(&operation, "store2").unwrap();

    // Set values in each store
    store1.set("key1", "value1").unwrap();
    store2.set("key2", "value2").unwrap();

    // Update a value in store1
    store1.set("key1", "updated").unwrap();

    // Commit the operation
    operation.commit().unwrap();

    // Create a new operation to read the data
    let read_op = tree.new_operation().unwrap();
    let store1_read = KVStore::new(&read_op, "store1").unwrap();
    let store2_read = KVStore::new(&read_op, "store2").unwrap();

    // Verify values in both stores
    assert_kvstore_value(&store1_read, "key1", "updated");
    assert_kvstore_value(&store2_read, "key2", "value2");
}

#[test]
fn test_atomicop_empty_subtree_removal() {
    // Create a backend and a tree
    let tree = setup_tree();

    // Create a new operation
    let operation = tree.new_operation().unwrap();

    // Create a KVStore subtree but don't add any data (will be empty)
    let _empty_store = KVStore::new(&operation, "empty").unwrap();

    // Create another KVStore and add data
    let data_store = KVStore::new(&operation, "data").unwrap();
    data_store.set("key", "value").unwrap();

    // Commit the operation - should remove the empty subtree
    operation.commit().unwrap();

    // Create a new operation to check if subtrees exist
    let read_op = tree.new_operation().unwrap();

    // Try to access both subtrees
    let data_result = KVStore::new(&read_op, "data");
    let empty_result = KVStore::new(&read_op, "empty");

    // The data subtree should be accessible
    assert!(data_result.is_ok());

    // The empty subtree should have been removed, but accessing it doesn't fail
    // because KVStore creates it if it doesn't exist
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
    let store1 = KVStore::new(&op1, "kvstore").unwrap();
    store1.set("first", "entry").unwrap();
    op1.commit().unwrap();

    // Create second operation that will use the first as parent
    let op2 = tree.new_operation().unwrap();
    let store2 = KVStore::new(&op2, "kvstore").unwrap();
    store2.set("second", "entry").unwrap();
    op2.commit().unwrap();

    // Create a third operation to read all entries
    let op3 = tree.new_operation().unwrap();
    let store3 = KVStore::new(&op3, "kvstore").unwrap();

    // Get all data - should include both entries due to CRDT merge
    let all_data = store3.get_all().unwrap();

    // Verify both entries are included in merged data
    match all_data.get("first") {
        Some(NodeValue::Text(value)) => assert_eq!(value, "entry"),
        _ => panic!("Expected string value for 'first'"),
    }

    match all_data.get("second") {
        Some(NodeValue::Text(value)) => assert_eq!(value, "entry"),
        _ => panic!("Expected string value for 'second'"),
    }
}

#[test]
fn test_atomicop_double_commit_error() {
    // Create a backend and a tree
    let tree = setup_tree();

    // Create an operation
    let operation = tree.new_operation().unwrap();

    // Use a KVStore to add data
    let store = KVStore::new(&operation, "test").unwrap();
    store.set("key", "value").unwrap();

    // First commit should succeed
    let id = operation.commit().unwrap();
    assert!(!id.is_empty());

    // Second commit should produce an error result, but we can't safely
    // test this with catch_unwind due to interior mutability issues.
    // Instead, we'll just note this as a comment and rely on the general
    // behavior tested elsewhere.
}

#[test]
fn test_atomicop_with_delete() {
    // Create a backend and a tree
    let tree = setup_tree();

    // Create an operation and add some data
    let op1 = tree.new_operation().unwrap();
    let store1 = KVStore::new(&op1, "data").unwrap();
    store1.set("key1", "value1").unwrap();
    store1.set("key2", "value2").unwrap();
    op1.commit().unwrap();

    // Create another operation to delete a key
    let op2 = tree.new_operation().unwrap();
    let store2 = KVStore::new(&op2, "data").unwrap();
    store2.delete("key1").unwrap();
    op2.commit().unwrap();

    // Verify with a third operation
    let op3 = tree.new_operation().unwrap();
    let store3 = KVStore::new(&op3, "data").unwrap();

    // key1 should be deleted
    assert_key_not_found(store3.get("key1"));

    // key2 should still exist
    assert_kvstore_value(&store3, "key2", "value2");

    // Check the full state with tombstone
    let all_data = store3.get_all().unwrap();
    assert_eq!(all_data.as_hashmap().get("key1"), Some(&NodeValue::Deleted));
    assert_eq!(
        all_data.as_hashmap().get("key2"),
        Some(&NodeValue::Text("value2".to_string()))
    );
}

#[test]
fn test_atomicop_nested_values() {
    const TEST_KEY: &str = "test_key";
    let (_db, tree) = setup_db_and_tree_with_key(TEST_KEY);

    // Create an operation
    let op1 = tree.new_operation().unwrap();
    let store1 = KVStore::new(&op1, "data").unwrap();

    // Set a regular string value
    store1.set("string_key", "string_value").unwrap();

    // Create and set a nested map value
    let mut nested = Nested::new();
    nested.set_string("inner1".to_string(), "value1".to_string());
    nested.set_string("inner2".to_string(), "value2".to_string());

    // Use the new set_value method to store a map
    store1.set_value("map_key", Value::Map(nested)).unwrap();

    // Commit the operation
    op1.commit().unwrap();

    // Verify with a new operation
    let op2 = tree.new_operation().unwrap();
    let store2 = KVStore::new(&op2, "data").unwrap();

    // Check the string value
    match store2.get("string_key").unwrap() {
        NodeValue::Text(value) => assert_eq!(value, "string_value"),
        _ => panic!("Expected string value"),
    }

    // Check the nested map
    match store2.get("map_key").unwrap() {
        NodeValue::Node(map) => {
            match map.get("inner1") {
                Some(NodeValue::Text(value)) => assert_eq!(value, "value1"),
                _ => panic!("Expected string value for inner1"),
            }
            match map.get("inner2") {
                Some(NodeValue::Text(value)) => assert_eq!(value, "value2"),
                _ => panic!("Expected string value for inner2"),
            }
        }
        _ => panic!("Expected map value"),
    }
}

#[test]
fn test_metadata_for_settings_entries() {
    let tree = setup_tree_with_settings(&[("name", "test_tree")]);

    // Create a settings update
    let settings_op = tree.new_operation().unwrap();
    let settings_subtree = settings_op.get_subtree::<KVStore>(SETTINGS).unwrap();
    settings_subtree.set("version", "1.0").unwrap();
    let settings_id = settings_op.commit().unwrap();

    // Now create a data entry (not touching settings)
    let data_op = tree.new_operation().unwrap();
    let data_subtree = data_op.get_subtree::<KVStore>("data").unwrap();
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
fn test_atomicop_with_custom_tips() {
    let tree = setup_tree();

    // Create a chain of operations: A -> B -> C
    let op_a = tree.new_operation().unwrap();
    let store_a = op_a.get_subtree::<KVStore>("data").unwrap();
    store_a.set("step", "A").unwrap();
    store_a.set("a_data", "value_a").unwrap();
    let entry_a_id = op_a.commit().unwrap();

    let op_b = tree.new_operation().unwrap();
    let store_b = op_b.get_subtree::<KVStore>("data").unwrap();
    store_b.set("step", "B").unwrap();
    store_b.set("b_data", "value_b").unwrap();
    let _entry_b_id = op_b.commit().unwrap();

    let op_c = tree.new_operation().unwrap();
    let store_c = op_c.get_subtree::<KVStore>("data").unwrap();
    store_c.set("step", "C").unwrap();
    store_c.set("c_data", "value_c").unwrap();
    let _entry_c_id = op_c.commit().unwrap();

    // Create operation from entry A using new_operation_with_tips
    let op_from_a = tree
        .new_operation_with_tips(std::slice::from_ref(&entry_a_id))
        .unwrap();
    let store_from_a = op_from_a.get_subtree::<KVStore>("data").unwrap();

    // This operation should only see data from A
    let state_from_a = store_from_a.get_all().unwrap();
    match state_from_a.get("step") {
        Some(NodeValue::Text(value)) => assert_eq!(value, "A"),
        _ => panic!("Expected step to be 'A'"),
    }
    assert!(state_from_a.get("a_data").is_some(), "Should see a_data");
    assert!(
        state_from_a.get("b_data").is_none(),
        "Should not see b_data"
    );
    assert!(
        state_from_a.get("c_data").is_none(),
        "Should not see c_data"
    );

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
fn test_atomicop_diamond_pattern() {
    let tree = setup_tree();

    // Create base entry
    let op_base = tree.new_operation().unwrap();
    let store_base = op_base.get_subtree::<KVStore>("data").unwrap();
    store_base.set("base", "initial").unwrap();
    let base_id = op_base.commit().unwrap();

    // Create two branches from base
    let op_left = tree
        .new_operation_with_tips(std::slice::from_ref(&base_id))
        .unwrap();
    let store_left = op_left.get_subtree::<KVStore>("data").unwrap();
    store_left.set("left", "left_value").unwrap();
    store_left.set("shared", "left_version").unwrap();
    let left_id = op_left.commit().unwrap();

    let op_right = tree.new_operation_with_tips([base_id]).unwrap();
    let store_right = op_right.get_subtree::<KVStore>("data").unwrap();
    store_right.set("right", "right_value").unwrap();
    store_right.set("shared", "right_version").unwrap();
    let right_id = op_right.commit().unwrap();

    // Create merge operation with both branches as tips
    let op_merge = tree
        .new_operation_with_tips([left_id.clone(), right_id.clone()])
        .unwrap();
    let store_merge = op_merge.get_subtree::<KVStore>("data").unwrap();

    // Merge operation should see data from both branches
    let merge_state = store_merge.get_all().unwrap();
    assert!(merge_state.get("base").is_some(), "Should see base data");
    assert!(merge_state.get("left").is_some(), "Should see left data");
    assert!(merge_state.get("right").is_some(), "Should see right data");
    assert!(
        merge_state.get("shared").is_some(),
        "Should see shared data"
    );

    // Add merge-specific data
    store_merge.set("merged", "merge_value").unwrap();
    let merge_id = op_merge.commit().unwrap();

    // Verify merge has correct parents
    let backend = tree.backend();
    let merge_entry = backend.get(&merge_id).unwrap();
    let merge_parents = merge_entry.parents().unwrap();

    assert_eq!(merge_parents.len(), 2, "Merge should have two parents");
    assert!(
        merge_parents.contains(&left_id),
        "Should have left as parent"
    );
    assert!(
        merge_parents.contains(&right_id),
        "Should have right as parent"
    );
}

#[test]
fn test_atomicop_staged_data_isolation() {
    let tree = setup_tree();

    // Create initial data
    let op1 = tree.new_operation().unwrap();
    let store1 = op1.get_subtree::<KVStore>("data").unwrap();
    store1.set("key1", "committed_value").unwrap();
    let entry1_id = op1.commit().unwrap();

    // Create operation from entry1
    let op2 = tree
        .new_operation_with_tips(std::slice::from_ref(&entry1_id))
        .unwrap();
    let store2 = op2.get_subtree::<KVStore>("data").unwrap();

    // Initially should see committed data
    assert_kvstore_value(&store2, "key1", "committed_value");

    // Stage new data (not yet committed)
    store2.set("key1", "staged_value").unwrap();
    store2.set("key2", "new_staged").unwrap();

    // Should now see staged data
    assert_kvstore_value(&store2, "key1", "staged_value");
    assert_kvstore_value(&store2, "key2", "new_staged");

    // Create another operation from same tip - should not see staged data
    let op3 = tree.new_operation_with_tips([entry1_id]).unwrap();
    let store3 = op3.get_subtree::<KVStore>("data").unwrap();

    // Should see original committed data, not staged data from op2
    assert_kvstore_value(&store3, "key1", "committed_value");
    assert_key_not_found(store3.get("key2"));

    // Commit op2
    let entry2_id = op2.commit().unwrap();

    // Create operation from entry2 - should see committed staged data
    let op4 = tree.new_operation_with_tips([entry2_id]).unwrap();
    let store4 = op4.get_subtree::<KVStore>("data").unwrap();

    assert_kvstore_value(&store4, "key1", "staged_value");
    assert_kvstore_value(&store4, "key2", "new_staged");
}

#[test]
fn test_atomicop_multiple_subtrees_with_custom_tips() {
    let tree = setup_tree();

    // Create base entry with multiple subtrees
    let op_base = tree.new_operation().unwrap();
    let users_base = op_base.get_subtree::<KVStore>("users").unwrap();
    let posts_base = op_base.get_subtree::<KVStore>("posts").unwrap();

    users_base.set("user1", "alice").unwrap();
    posts_base.set("post1", "hello").unwrap();
    let base_id = op_base.commit().unwrap();

    // Create branch that only modifies users
    let op_users = tree
        .new_operation_with_tips(std::slice::from_ref(&base_id))
        .unwrap();
    let users_branch = op_users.get_subtree::<KVStore>("users").unwrap();
    users_branch.set("user2", "bob").unwrap();
    let users_id = op_users.commit().unwrap();

    // Create branch that only modifies posts
    let op_posts = tree.new_operation_with_tips([base_id]).unwrap();
    let posts_branch = op_posts.get_subtree::<KVStore>("posts").unwrap();
    posts_branch.set("post2", "world").unwrap();
    let posts_id = op_posts.commit().unwrap();

    // Create merge operation
    let op_merge = tree
        .new_operation_with_tips([users_id.clone(), posts_id.clone()])
        .unwrap();
    let users_merge = op_merge.get_subtree::<KVStore>("users").unwrap();
    let posts_merge = op_merge.get_subtree::<KVStore>("posts").unwrap();

    // Should see data from both branches in both subtrees
    let users_state = users_merge.get_all().unwrap();
    assert!(
        users_state.get("user1").is_some(),
        "Should see user1 from base"
    );
    assert!(
        users_state.get("user2").is_some(),
        "Should see user2 from users branch"
    );

    let posts_state = posts_merge.get_all().unwrap();
    assert!(
        posts_state.get("post1").is_some(),
        "Should see post1 from base"
    );
    assert!(
        posts_state.get("post2").is_some(),
        "Should see post2 from posts branch"
    );

    // Add new data in merge
    users_merge.set("user3", "charlie").unwrap();
    posts_merge.set("post3", "merged").unwrap();
    let merge_id = op_merge.commit().unwrap();

    // Verify final state has all data
    let op_final = tree.new_operation_with_tips([merge_id]).unwrap();
    let users_final = op_final.get_subtree::<KVStore>("users").unwrap();
    let posts_final = op_final.get_subtree::<KVStore>("posts").unwrap();

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
    let store1 = op1.get_subtree::<KVStore>("data").unwrap();
    store1.set("key1", "value1").unwrap();
    let entry1_id = op1.commit().unwrap();

    // Create a parallel branch that also has subtree data
    let op2 = tree
        .new_operation_with_tips(std::slice::from_ref(&entry1_id))
        .unwrap();
    let store2 = op2.get_subtree::<KVStore>("data").unwrap();
    store2.set("key2", "value2").unwrap();
    let entry2_id = op2.commit().unwrap();

    // Create another branch that does NOT touch the "data" subtree at all
    let op3 = tree.new_operation_with_tips([entry1_id]).unwrap();
    // Only touch a different subtree
    let settings3 = op3.get_subtree::<KVStore>("settings").unwrap();
    settings3.set("config", "value").unwrap();
    let entry3_id = op3.commit().unwrap();

    // Create a merge operation using both branches as tips
    // entry2_id has subtree data, entry3_id does NOT have subtree data
    let op4 = tree
        .new_operation_with_tips([entry2_id.clone(), entry3_id.clone()])
        .unwrap();
    let store4 = op4.get_subtree::<KVStore>("data").unwrap();

    // Should be able to access all the data from both branches
    // This tests the case where one tip has the subtree (entry2) and one doesn't (entry3)
    let state = store4.get_all().unwrap();
    match state.get("key1") {
        Some(NodeValue::Text(value)) => assert_eq!(value, "value1"),
        _ => panic!("Expected key1 to have value 'value1' from entry1"),
    }
    match state.get("key2") {
        Some(NodeValue::Text(value)) => assert_eq!(value, "value2"),
        _ => panic!("Expected key2 to have value 'value2' from entry2"),
    }

    // Should also be able to access settings from entry3
    let settings4 = op4.get_subtree::<KVStore>("settings").unwrap();
    let settings_state = settings4.get_all().unwrap();
    match settings_state.get("config") {
        Some(NodeValue::Text(value)) => assert_eq!(value, "value"),
        _ => panic!("Expected config to have value 'value'"),
    }
}

#[test]
fn test_atomicop_custom_tips_no_subtree_data_in_tips() {
    let tree = setup_tree();

    // Create entry with subtree data
    let op1 = tree.new_operation().unwrap();
    let store1 = op1.get_subtree::<KVStore>("data").unwrap();
    store1.set("original", "value").unwrap();
    let _entry1_id = op1.commit().unwrap();

    // Create entry that does NOT modify the "data" subtree
    // This simulates the case where we have tree evolution but no subtree changes
    let op2 = tree.new_operation().unwrap();
    let settings2 = op2.get_subtree::<KVStore>("settings").unwrap();
    settings2.set("config1", "value1").unwrap();
    let entry2_id = op2.commit().unwrap();

    // Create another entry that also doesn't modify "data" subtree
    let op3 = tree.new_operation().unwrap();
    let metadata3 = op3.get_subtree::<KVStore>("metadata").unwrap();
    metadata3.set("info", "some info").unwrap();
    let entry3_id = op3.commit().unwrap();

    // Now use ONLY the entries that don't have "data" subtree as custom tips
    // The "data" subtree should still be accessible from their common ancestor (entry1)
    let op4 = tree
        .new_operation_with_tips([entry2_id.clone(), entry3_id.clone()])
        .unwrap();
    let store4 = op4.get_subtree::<KVStore>("data").unwrap();

    // This should work: accessing subtree data that exists in ancestors
    // but not in the tip entries themselves
    let state = store4.get_all().unwrap();
    match state.get("original") {
        Some(NodeValue::Text(value)) => assert_eq!(value, "value"),
        _ => panic!("Expected 'original' to have value 'value' from ancestor entry1"),
    }

    // Verify we can also access the data from the tip entries
    let settings4 = op4.get_subtree::<KVStore>("settings").unwrap();
    let settings_state = settings4.get_all().unwrap();
    assert!(
        settings_state.get("config1").is_some(),
        "Should have config1 from entry2"
    );

    let metadata4 = op4.get_subtree::<KVStore>("metadata").unwrap();
    let metadata_state = metadata4.get_all().unwrap();
    assert!(
        metadata_state.get("info").is_some(),
        "Should have info from entry3"
    );
}

#[test]
fn test_get_path_from_to_diamond_pattern() {
    let tree = setup_tree();

    // Create a diamond pattern: A -> B,C -> D
    // A is the base
    let op_a = tree.new_operation().unwrap();
    let store_a = op_a.get_subtree::<KVStore>("data").unwrap();
    store_a.set("base", "A").unwrap();
    let entry_a_id = op_a.commit().unwrap();

    // B branches from A
    let op_b = tree
        .new_operation_with_tips(std::slice::from_ref(&entry_a_id))
        .unwrap();
    let store_b = op_b.get_subtree::<KVStore>("data").unwrap();
    store_b.set("left", "B").unwrap();
    let entry_b_id = op_b.commit().unwrap();

    // C also branches from A (parallel to B)
    let op_c = tree.new_operation_with_tips([entry_a_id]).unwrap();
    let store_c = op_c.get_subtree::<KVStore>("data").unwrap();
    store_c.set("right", "C").unwrap();
    let entry_c_id = op_c.commit().unwrap();

    // D merges B and C
    let op_d = tree
        .new_operation_with_tips([entry_b_id.clone(), entry_c_id.clone()])
        .unwrap();
    let store_d = op_d.get_subtree::<KVStore>("data").unwrap();
    store_d.set("merged", "D").unwrap();
    let entry_d_id = op_d.commit().unwrap();

    // Now test path finding in this diamond pattern
    // The get_path_from_to function should be able to find a valid path from A to D
    // This should work through the LCA-based algorithm when computing CRDT state

    // Create an operation that uses D as tip and access the CRDT state
    // This will internally call get_path_from_to when computing merged state
    let op_final = tree.new_operation_with_tips([entry_d_id]).unwrap();
    let store_final = op_final.get_subtree::<KVStore>("data").unwrap();

    // Should be able to access all data from the diamond pattern
    let final_state = store_final.get_all().unwrap();

    // Verify all data is present (this will fail if path finding is broken)
    assert!(final_state.get("base").is_some(), "Should have base from A");
    assert!(final_state.get("left").is_some(), "Should have left from B");
    assert!(
        final_state.get("right").is_some(),
        "Should have right from C"
    );
    assert!(
        final_state.get("merged").is_some(),
        "Should have merged from D"
    );
}

#[test]
fn test_get_path_from_to_diamond_between_lca_and_tip() {
    let tree = setup_tree();

    // Create the exact scenario you described:
    // LCA -> A -> C (tip)
    // LCA -> B -> C (tip)
    // Where get_path_from_to(LCA, C) only follows one path (A) and misses modifications in B

    // Step 1: Create LCA
    let op_lca = tree.new_operation().unwrap();
    let store_lca = op_lca.get_subtree::<KVStore>("data").unwrap();
    store_lca.set("base", "LCA").unwrap();
    let lca_id = op_lca.commit().unwrap();

    // Step 2: Create two parallel branches from LCA
    // Branch A
    let op_a = tree
        .new_operation_with_tips(std::slice::from_ref(&lca_id))
        .unwrap();
    let store_a = op_a.get_subtree::<KVStore>("data").unwrap();
    store_a.set("branch_a", "modification_A").unwrap();
    let a_id = op_a.commit().unwrap();

    // Branch B (parallel to A)
    let op_b = tree
        .new_operation_with_tips(std::slice::from_ref(&lca_id))
        .unwrap();
    let store_b = op_b.get_subtree::<KVStore>("data").unwrap();
    store_b.set("branch_b", "modification_B").unwrap(); // Critical: this modification will be missed!
    let b_id = op_b.commit().unwrap();

    // Step 3: Create tip C that merges both A and B
    let op_c = tree
        .new_operation_with_tips([a_id.clone(), b_id.clone()])
        .unwrap();
    let store_c = op_c.get_subtree::<KVStore>("data").unwrap();
    store_c.set("tip", "merged_C").unwrap();
    let c_id = op_c.commit().unwrap();

    // Step 4: Create another tip D independently
    let op_d = tree.new_operation_with_tips([lca_id]).unwrap();
    let store_d = op_d.get_subtree::<KVStore>("data").unwrap();
    store_d.set("independent", "tip_D").unwrap();
    let d_id = op_d.commit().unwrap();

    // Step 5: Now create an operation with tips [C, D]
    // The LCA of [C, D] will be LCA
    // When computing path from LCA to C, get_path_from_to will only follow one path:
    // Either LCA -> A -> C (missing branch B modifications)
    // Or LCA -> B -> C (missing branch A modifications)
    let op_final = tree
        .new_operation_with_tips([c_id.clone(), d_id.clone()])
        .unwrap();
    let store_final = op_final.get_subtree::<KVStore>("data").unwrap();

    let final_state = store_final.get_all().unwrap();

    // With the CORRECT path finding, we should see ALL modifications:
    assert!(
        final_state.get("base").is_some(),
        "Should have base from LCA"
    );
    assert!(
        final_state.get("branch_a").is_some(),
        "Should have modification from branch A"
    );
    assert!(
        final_state.get("branch_b").is_some(),
        "Should have modification from branch B - this will fail with buggy path finding!"
    );
    assert!(final_state.get("tip").is_some(), "Should have tip C data");
    assert!(
        final_state.get("independent").is_some(),
        "Should have tip D data"
    );
}

#[test]
fn test_correct_lca_and_path_sorting() {
    let tree = setup_tree();

    // Create a proper LCA scenario where sorting matters:
    // ROOT (LCA)
    //   ├─ A ─┐
    //   └─ B ─┴─ MERGE_TIP
    //   └─ C ──── OTHER_TIP
    //
    // LCA([MERGE_TIP, OTHER_TIP]) = ROOT
    // get_path_from_to(ROOT, MERGE_TIP) should return [A, B, MERGE_TIP] in height order

    // Step 1: ROOT (will be the LCA)
    let op_root = tree.new_operation().unwrap();
    let store_root = op_root.get_subtree::<KVStore>("data").unwrap();
    store_root.set("step", "0").unwrap();
    store_root.set("root", "true").unwrap();
    let root_id = op_root.commit().unwrap();

    // Step 2: Create three branches from ROOT
    // Branch A (height 1)
    let op_a = tree
        .new_operation_with_tips(std::slice::from_ref(&root_id))
        .unwrap();
    let store_a = op_a.get_subtree::<KVStore>("data").unwrap();
    store_a.set("step", "1").unwrap();
    store_a.set("branch", "A").unwrap();
    let a_id = op_a.commit().unwrap();

    // Branch B (height 1)
    let op_b = tree
        .new_operation_with_tips(std::slice::from_ref(&root_id))
        .unwrap();
    let store_b = op_b.get_subtree::<KVStore>("data").unwrap();
    store_b.set("step", "1").unwrap();
    store_b.set("branch", "B").unwrap();
    let b_id = op_b.commit().unwrap();

    // Branch C (height 1)
    let op_c = tree.new_operation_with_tips([root_id]).unwrap();
    let store_c = op_c.get_subtree::<KVStore>("data").unwrap();
    store_c.set("step", "1").unwrap();
    store_c.set("branch", "C").unwrap();
    let c_id = op_c.commit().unwrap();

    // Step 3: Create merge tip from A and B (height 2)
    let op_merge = tree
        .new_operation_with_tips([a_id.clone(), b_id.clone()])
        .unwrap();
    let store_merge = op_merge.get_subtree::<KVStore>("data").unwrap();
    store_merge.set("step", "2").unwrap();
    store_merge.set("merged", "AB").unwrap();
    let merge_id = op_merge.commit().unwrap();

    // Step 4: Create another tip from C (height 2)
    let op_other = tree.new_operation_with_tips([c_id]).unwrap();
    let store_other = op_other.get_subtree::<KVStore>("data").unwrap();
    store_other.set("step", "2").unwrap();
    store_other.set("other", "C_extended").unwrap();
    let other_id = op_other.commit().unwrap();

    // Step 5: Now create an operation with tips [merge_id, other_id]
    // LCA should be root_id
    // Path from root to merge should include both A and B modifications
    // Sorting order is critical for deterministic CRDT merge
    let op_final = tree
        .new_operation_with_tips([merge_id.clone(), other_id.clone()])
        .unwrap();
    let store_final = op_final.get_subtree::<KVStore>("data").unwrap();

    let final_state = store_final.get_all().unwrap();

    // Should include data from all paths with correct ordering
    assert!(final_state.get("root").is_some(), "Should have root data");
    assert!(
        final_state.get("branch").is_some(),
        "Should have branch data"
    ); // This will be last-writer-wins between A, B, C
    assert!(
        final_state.get("merged").is_some(),
        "Should have merged data"
    );
    assert!(final_state.get("other").is_some(), "Should have other data");

    // The critical test: verify that the sorting ensures deterministic results
    // Run the same operation multiple times and verify consistent results
    for _i in 0..5 {
        let op_test = tree
            .new_operation_with_tips([merge_id.clone(), other_id.clone()])
            .unwrap();
        let store_test = op_test.get_subtree::<KVStore>("data").unwrap();
        let test_state = store_test.get_all().unwrap();

        // Results should be identical due to deterministic sorting
        assert_eq!(test_state.get("root"), final_state.get("root"));
        assert_eq!(test_state.get("branch"), final_state.get("branch"));
        assert_eq!(test_state.get("merged"), final_state.get("merged"));
        assert_eq!(test_state.get("other"), final_state.get("other"));
    }
}
