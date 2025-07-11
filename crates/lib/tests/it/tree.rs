use crate::helpers::*;
use eidetica::constants::SETTINGS;
use eidetica::crdt::Map;
use eidetica::subtree::KVStore;

#[test]
fn test_insert_into_tree() {
    let tree = setup_tree();

    // Create and commit first entry using an atomic operation
    let op1 = tree.new_operation().expect("Failed to create operation");
    let id1 = op1.commit().expect("Failed to commit operation");

    // Create and commit second entry
    let op2 = tree.new_operation().expect("Failed to create operation");
    let id2 = op2.commit().expect("Failed to commit operation");

    // Verify tips include id2
    let tips = tree.get_tips().expect("Failed to get tips");
    assert!(tips.contains(&id2));
    assert!(!tips.contains(&id1)); // id1 should no longer be a tip

    // Verify retrieval through Tree API
    let retrieved_entry1 = tree.get_entry(&id1).expect("Failed to get entry 1");
    assert_eq!(retrieved_entry1.id(), id1);

    let retrieved_entry2 = tree.get_entry(&id2).expect("Failed to get entry 2");
    assert_eq!(retrieved_entry2.id(), id2);
    assert_eq!(retrieved_entry2.parents().unwrap(), vec![id1]);
}

#[test]
fn test_get_settings() {
    // Set up the tree with initial settings
    let settings = [("setting_key", "setting_value")];
    let tree = setup_tree_with_settings(&settings);
    let retrieved_settings = tree.get_settings().expect("Failed to get settings");

    assert_eq!(
        retrieved_settings
            .get_string("setting_key")
            .expect("Failed to get setting"),
        "setting_value"
    );
}

#[test]
fn test_subtree_operations() {
    // Create a fresh tree
    let tree = setup_tree();

    // Create and commit the initial data with operation
    let op1 = tree.new_operation().expect("Failed to create operation");
    {
        let users_store = op1
            .get_subtree::<KVStore>("users")
            .expect("Failed to get users store");

        let posts_store = op1
            .get_subtree::<KVStore>("posts")
            .expect("Failed to get posts store");

        users_store
            .set("user1.name", "Alice")
            .expect("Failed to set user data");

        posts_store
            .set("post1.title", "First Post")
            .expect("Failed to set post data");
    }
    op1.commit().expect("Failed to commit operation");

    // --- Verify initial data with viewers ---
    let users_viewer1 = tree
        .get_subtree_viewer::<KVStore>("users")
        .expect("Failed to get users viewer (1)");
    assert_eq!(
        users_viewer1
            .get_string("user1.name")
            .expect("Failed to get user1.name (1)"),
        "Alice"
    );
    let posts_viewer1 = tree
        .get_subtree_viewer::<KVStore>("posts")
        .expect("Failed to get posts viewer (1)");
    assert_eq!(
        posts_viewer1
            .get_string("post1.title")
            .expect("Failed to get post1.title (1)"),
        "First Post"
    );

    // --- Create another operation modifying only the users subtree ---
    let op2 = tree.new_operation().expect("Failed to create operation 2");
    {
        let users_store2 = op2
            .get_subtree::<KVStore>("users")
            .expect("Failed to get users store (2)");
        users_store2
            .set("user2.name", "Bob")
            .expect("Failed to set second user data");
    }

    // Commit the second operation
    op2.commit().expect("Failed to commit second operation");

    // --- Test SubTree viewers for reading final data ---
    let users_viewer2 = tree
        .get_subtree_viewer::<KVStore>("users")
        .expect("Failed to get users viewer (2)");
    assert_eq!(
        users_viewer2
            .get_string("user1.name")
            .expect("Failed to get user1.name (2)"),
        "Alice"
    ); // Should still exist
    assert_eq!(
        users_viewer2
            .get_string("user2.name")
            .expect("Failed to get user2.name (2)"),
        "Bob"
    ); // New user should exist

    let posts_viewer2 = tree
        .get_subtree_viewer::<KVStore>("posts")
        .expect("Failed to get posts viewer (2)");
    assert_eq!(
        posts_viewer2
            .get_string("post1.title")
            .expect("Failed to get post1.title (2)"),
        "First Post"
    ); // Post should be unchanged
}

#[test]
fn test_get_name_from_settings() {
    // Create tree with settings
    let settings = [("name", "TestTree")];
    let tree = setup_tree_with_settings(&settings);

    // Test that get_name works
    let name = tree.get_name().expect("Failed to get tree name");
    assert_eq!(name, "TestTree");

    // Update the name using an operation
    let op = tree.new_operation().expect("Failed to create operation");
    {
        let settings_store = op
            .get_subtree::<KVStore>(SETTINGS)
            .expect("Failed to get settings store in op");
        settings_store
            .set("name", "UpdatedTreeName")
            .expect("Failed to update name in op");
    }
    op.commit().expect("Failed to commit name update operation");

    // Get updated name
    let updated_name = tree.get_name().expect("Failed to get updated tree name");
    assert_eq!(updated_name, "UpdatedTreeName");
}

#[test]
fn test_atomic_op_scenarios() {
    let tree = setup_tree();

    // --- 1. Modify multiple subtrees in one op and read staged data ---
    let op1 = tree.new_operation().expect("Op1: Failed to start");
    let initial_tip = tree.get_tips().unwrap()[0].clone();
    {
        let store_a = op1
            .get_subtree::<KVStore>("sub_a")
            .expect("Op1: Failed get A");
        store_a.set("key_a", "val_a1").expect("Op1: Failed set A");

        let store_b = op1
            .get_subtree::<KVStore>("sub_b")
            .expect("Op1: Failed get B");
        store_b.set("key_b", "val_b1").expect("Op1: Failed set B");

        // Read staged data within the op
        assert_eq!(
            store_a
                .get_string("key_a")
                .expect("Op1: Failed read staged A"),
            "val_a1"
        );
        assert_eq!(
            store_b
                .get_string("key_b")
                .expect("Op1: Failed read staged B"),
            "val_b1"
        );

        // Try reading non-staged key (should be NotFound)
        assert!(store_a.get("non_existent").is_err());
        assert_key_not_found(store_a.get("non_existent"));
    }
    let commit1_id = op1.commit().expect("Op1: Failed to commit");
    assert_ne!(commit1_id, initial_tip, "Op1: Commit should create new tip");

    // Verify commit with viewers
    let viewer_a1 = tree
        .get_subtree_viewer::<KVStore>("sub_a")
        .expect("Viewer A1");
    assert_eq!(
        viewer_a1.get_string("key_a").expect("Viewer A1 get"),
        "val_a1"
    );
    let viewer_b1 = tree
        .get_subtree_viewer::<KVStore>("sub_b")
        .expect("Viewer B1");
    assert_eq!(
        viewer_b1.get_string("key_b").expect("Viewer B1 get"),
        "val_b1"
    );

    // --- 2. Commit an empty operation ---
    let op_empty = tree.new_operation().expect("OpEmpty: Failed to start");
    let commit_empty_result = op_empty.commit();
    // If it's not an error, check the tip is still changed to the empty commit
    assert!(commit_empty_result.is_ok());
    assert_eq!(
        tree.get_tips().unwrap()[0],
        commit_empty_result.unwrap(),
        "Empty commit should still be a tip"
    );

    // --- 3. Attempt to commit the same op twice ---
    let op3 = tree.new_operation().expect("Op3: Failed to start");
    {
        let store_a = op3
            .get_subtree::<KVStore>("sub_a")
            .expect("Op3: Failed get A");
        store_a.set("key_a", "val_a3").expect("Op3: Failed set A");
    }
    let _commit3_id = op3.commit().expect("Op3: First commit failed");

    // Commiting again won't even compile
    // let commit3_again = op3.commit();
}

#[test]
fn test_get_subtree_viewer() {
    let tree = setup_tree();

    // --- Initial state ---
    let op1 = tree.new_operation().expect("Op1: Failed start");
    {
        let store = op1
            .get_subtree::<KVStore>("my_data")
            .expect("Op1: Failed get");
        store.set("key1", "value1").expect("Op1: Failed set");
    }
    op1.commit().expect("Op1: Failed commit");

    // --- Get viewer 1 (sees initial state) ---
    let viewer1 = tree
        .get_subtree_viewer::<KVStore>("my_data")
        .expect("Viewer1: Failed get");
    assert_eq!(
        viewer1
            .get_string("key1")
            .expect("Viewer1: Failed read key1"),
        "value1"
    );
    assert!(
        viewer1.get("key2").is_err(),
        "Viewer1: key2 should not exist yet"
    );

    // --- Second operation ---
    let op2 = tree.new_operation().expect("Op2: Failed start");
    {
        let store = op2
            .get_subtree::<KVStore>("my_data")
            .expect("Op2: Failed get");
        store
            .set("key1", "value1_updated")
            .expect("Op2: Failed update key1"); // Update existing
        store.set("key2", "value2").expect("Op2: Failed set key2"); // Add new
    }
    op2.commit().expect("Op2: Failed commit");

    // --- Get viewer 2 (sees updated state) ---
    let viewer2 = tree
        .get_subtree_viewer::<KVStore>("my_data")
        .expect("Viewer2: Failed get");
    assert_eq!(
        viewer2
            .get_string("key1")
            .expect("Viewer2: Failed read key1"),
        "value1_updated"
    );
    assert_eq!(
        viewer2
            .get_string("key2")
            .expect("Viewer2: Failed read key2"),
        "value2"
    );

    // --- Verify viewer 1 still sees the old state ---
    assert_eq!(
        viewer1
            .get_string("key1")
            .expect("Viewer1 (post-commit): Failed read key1"),
        "value1"
    );
    assert!(
        viewer1.get("key2").is_err(),
        "Viewer1 (post-commit): key2 should still not exist"
    );

    // --- Test viewer for non-existent subtree ---
    let non_existent_viewer_result = tree.get_subtree_viewer::<KVStore>("non_existent_subtree");
    // Depending on implementation, this might create an empty viewer or return an error.
    // Let's assume it successfully returns an empty viewer for now.
    assert!(
        non_existent_viewer_result.is_ok(),
        "Getting viewer for non-existent subtree should be OK"
    );
    let empty_viewer = non_existent_viewer_result.unwrap();
    assert!(
        empty_viewer.get("any_key").is_err(),
        "Viewer for non-existent subtree should be empty"
    );
    assert_key_not_found(empty_viewer.get("any_key"));
}

#[test]
fn test_setup_tree_with_multiple_kvstores() {
    // Prepare test data
    let users = [("user1", "Alice"), ("user2", "Bob")];
    let posts = [("post1", "First Post")];
    let comments = [("comment1", "Great post!")];

    let subtrees = [
        ("users", &users[..]),
        ("posts", &posts[..]),
        ("comments", &comments[..]),
    ];

    // Create the tree with the helper
    let tree = setup_tree_with_multiple_kvstores(&subtrees);

    // Verify the data was correctly set
    let users_viewer = tree
        .get_subtree_viewer::<KVStore>("users")
        .expect("Failed to get users viewer");
    assert_eq!(
        users_viewer
            .get_string("user1")
            .expect("Failed to get user1"),
        "Alice"
    );
    assert_eq!(
        users_viewer
            .get_string("user2")
            .expect("Failed to get user2"),
        "Bob"
    );

    let posts_viewer = tree
        .get_subtree_viewer::<KVStore>("posts")
        .expect("Failed to get posts viewer");
    assert_eq!(
        posts_viewer
            .get_string("post1")
            .expect("Failed to get post1"),
        "First Post"
    );

    let comments_viewer = tree
        .get_subtree_viewer::<KVStore>("comments")
        .expect("Failed to get comments viewer");
    assert_eq!(
        comments_viewer
            .get_string("comment1")
            .expect("Failed to get comment1"),
        "Great post!"
    );
}

#[test]
fn test_get_tips() {
    let tree = setup_tree();

    // Initially, the tree should have one tip (the root entry)
    let initial_tips = tree.get_tips().expect("Failed to get initial tips");
    assert_eq!(
        initial_tips.len(),
        1,
        "Tree should have exactly one initial tip"
    );

    // Create and commit first entry
    let op1 = tree.new_operation().expect("Failed to create operation 1");
    let store1 = op1
        .get_subtree::<KVStore>("data")
        .expect("Failed to get store 1");
    store1.set("key1", "value1").expect("Failed to set key1");
    let entry1_id = op1.commit().expect("Failed to commit operation 1");

    // Tips should now include entry1_id
    let tips_after_op1 = tree.get_tips().expect("Failed to get tips after op1");
    assert_eq!(
        tips_after_op1.len(),
        1,
        "Should have exactly one tip after op1"
    );
    assert!(
        tips_after_op1.contains(&entry1_id),
        "Tips should contain entry1_id"
    );
    assert!(
        !tips_after_op1.contains(&initial_tips[0]),
        "Initial tip should no longer be a tip"
    );

    // Create and commit second entry
    let op2 = tree.new_operation().expect("Failed to create operation 2");
    let store2 = op2
        .get_subtree::<KVStore>("data")
        .expect("Failed to get store 2");
    store2.set("key2", "value2").expect("Failed to set key2");
    let entry2_id = op2.commit().expect("Failed to commit operation 2");

    // Tips should now include entry2_id
    let tips_after_op2 = tree.get_tips().expect("Failed to get tips after op2");
    assert_eq!(
        tips_after_op2.len(),
        1,
        "Should have exactly one tip after op2"
    );
    assert!(
        tips_after_op2.contains(&entry2_id),
        "Tips should contain entry2_id"
    );
    assert!(
        !tips_after_op2.contains(&entry1_id),
        "Entry1 should no longer be a tip"
    );
}

#[test]
fn test_new_operation_with_tips() {
    let tree = setup_tree();

    // Create first entry
    let op1 = tree.new_operation().expect("Failed to create operation 1");
    let store1 = op1
        .get_subtree::<KVStore>("data")
        .expect("Failed to get store 1");
    store1.set("key1", "value1").expect("Failed to set key1");
    let entry1_id = op1.commit().expect("Failed to commit operation 1");

    // Create second entry
    let op2 = tree.new_operation().expect("Failed to create operation 2");
    let store2 = op2
        .get_subtree::<KVStore>("data")
        .expect("Failed to get store 2");
    store2.set("key2", "value2").expect("Failed to set key2");
    let entry2_id = op2.commit().expect("Failed to commit operation 2");

    // Verify that normal operations use current tips (should see both keys)
    let normal_op = tree
        .new_operation()
        .expect("Failed to create normal operation");
    let normal_store = normal_op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get normal store");
    let normal_state = normal_store.get_all().expect("Failed to get normal state");
    assert!(
        normal_state.get("key1").is_some(),
        "Normal operation should see key1"
    );
    assert!(
        normal_state.get("key2").is_some(),
        "Normal operation should see key2"
    );

    // Create operation with custom tips (using entry1 instead of current tip)
    let custom_op = tree
        .new_operation_with_tips([entry1_id])
        .expect("Failed to create custom operation");
    let custom_store = custom_op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get custom store");
    let custom_state = custom_store.get_all().expect("Failed to get custom state");
    assert!(
        custom_state.get("key1").is_some(),
        "Custom operation should see key1"
    );
    assert!(
        custom_state.get("key2").is_none(),
        "Custom operation should not see key2"
    );

    // Commit the custom operation to create a branch
    custom_store
        .set("custom_key", "custom_value")
        .expect("Failed to set custom_key");
    let custom_entry_id = custom_op
        .commit()
        .expect("Failed to commit custom operation");

    // Now we should have two tips: entry2_id and custom_entry_id
    let tips_after_branch = tree.get_tips().expect("Failed to get tips after branch");
    assert_eq!(
        tips_after_branch.len(),
        2,
        "Should have exactly two tips after branching"
    );
    assert!(
        tips_after_branch.contains(&entry2_id),
        "Tips should contain entry2_id"
    );
    assert!(
        tips_after_branch.contains(&custom_entry_id),
        "Tips should contain custom_entry_id"
    );

    // Create a merge operation that should see both branches
    let merge_op = tree
        .new_operation()
        .expect("Failed to create merge operation");
    let merge_store = merge_op
        .get_subtree::<KVStore>("data")
        .expect("Failed to get merge store");
    let merge_state = merge_store.get_all().expect("Failed to get merge state");

    // Merge operation should see data from all paths
    assert!(merge_state.get("key1").is_some(), "Merge should see key1");
    assert!(merge_state.get("key2").is_some(), "Merge should see key2");
    assert!(
        merge_state.get("custom_key").is_some(),
        "Merge should see custom_key"
    );
}

#[test]
fn test_new_operation_with_specific_tips() {
    let tree = setup_tree();

    // Create a chain of entries: A -> B -> C
    let op_a = tree.new_operation().expect("Failed to create operation A");
    let store_a = op_a
        .get_subtree::<KVStore>("data")
        .expect("Failed to get store A");
    store_a
        .set("from_a", "value_a")
        .expect("Failed to set from_a");
    let entry_a_id = op_a.commit().expect("Failed to commit operation A");

    let op_b = tree.new_operation().expect("Failed to create operation B");
    let store_b = op_b
        .get_subtree::<KVStore>("data")
        .expect("Failed to get store B");
    store_b
        .set("from_b", "value_b")
        .expect("Failed to set from_b");
    let entry_b_id = op_b.commit().expect("Failed to commit operation B");

    let op_c = tree.new_operation().expect("Failed to create operation C");
    let store_c = op_c
        .get_subtree::<KVStore>("data")
        .expect("Failed to get store C");
    store_c
        .set("from_c", "value_c")
        .expect("Failed to set from_c");
    let entry_c_id = op_c.commit().expect("Failed to commit operation C");

    // Create operation starting from entry A (should see only A)
    let op_from_a = tree
        .new_operation_with_tips(std::slice::from_ref(&entry_a_id))
        .expect("Failed to create op from A");
    let store_from_a = op_from_a
        .get_subtree::<KVStore>("data")
        .expect("Failed to get store from A");
    let state_from_a = store_from_a.get_all().expect("Failed to get state from A");

    assert!(
        state_from_a.get("from_a").is_some(),
        "Should see data from A"
    );
    assert!(
        state_from_a.get("from_b").is_none(),
        "Should not see data from B"
    );
    assert!(
        state_from_a.get("from_c").is_none(),
        "Should not see data from C"
    );

    // Create operation starting from entry B (should see A and B but not C)
    let op_from_b = tree
        .new_operation_with_tips([entry_b_id])
        .expect("Failed to create op from B");
    let store_from_b = op_from_b
        .get_subtree::<KVStore>("data")
        .expect("Failed to get store from B");
    let state_from_b = store_from_b.get_all().expect("Failed to get state from B");

    assert!(
        state_from_b.get("from_a").is_some(),
        "Should see data from A"
    );
    assert!(
        state_from_b.get("from_b").is_some(),
        "Should see data from B"
    );
    assert!(
        state_from_b.get("from_c").is_none(),
        "Should not see data from C"
    );

    // Create operation starting from entry C (should see all)
    let op_from_c = tree
        .new_operation_with_tips([entry_c_id])
        .expect("Failed to create op from C");
    let store_from_c = op_from_c
        .get_subtree::<KVStore>("data")
        .expect("Failed to get store from C");
    let state_from_c = store_from_c.get_all().expect("Failed to get state from C");

    assert!(
        state_from_c.get("from_a").is_some(),
        "Should see data from A"
    );
    assert!(
        state_from_c.get("from_b").is_some(),
        "Should see data from B"
    );
    assert!(
        state_from_c.get("from_c").is_some(),
        "Should see data from C"
    );

    // Test branching from an earlier point
    let op_branch = tree
        .new_operation_with_tips([entry_a_id])
        .expect("Failed to create branch from A");
    let store_branch = op_branch
        .get_subtree::<KVStore>("data")
        .expect("Failed to get store branch");
    store_branch
        .set("branch_data", "branch_value")
        .expect("Failed to set branch_data");
    let branch_id = op_branch.commit().expect("Failed to commit branch");

    // Verify the branch only sees data from A
    let op_verify_branch = tree
        .new_operation_with_tips([branch_id])
        .expect("Failed to create verify op");
    let store_verify_branch = op_verify_branch
        .get_subtree::<KVStore>("data")
        .expect("Failed to get verify store");
    let state_verify_branch = store_verify_branch
        .get_all()
        .expect("Failed to get verify state");

    assert!(
        state_verify_branch.get("from_a").is_some(),
        "Branch should see data from A"
    );
    assert!(
        state_verify_branch.get("branch_data").is_some(),
        "Branch should see its own data"
    );
    assert!(
        state_verify_branch.get("from_b").is_none(),
        "Branch should not see data from B"
    );
    assert!(
        state_verify_branch.get("from_c").is_none(),
        "Branch should not see data from C"
    );
}

#[test]
fn test_new_operation_with_multiple_tips() {
    let tree = setup_tree();

    // Create initial entry
    let op_base = tree
        .new_operation()
        .expect("Failed to create base operation");
    let store_base = op_base
        .get_subtree::<KVStore>("data")
        .expect("Failed to get base store");
    store_base.set("base", "value").expect("Failed to set base");
    let base_id = op_base.commit().expect("Failed to commit base operation");

    // Create two parallel branches from base
    let op_branch1 = tree
        .new_operation_with_tips(std::slice::from_ref(&base_id))
        .expect("Failed to create branch1");
    let store_branch1 = op_branch1
        .get_subtree::<KVStore>("data")
        .expect("Failed to get branch1 store");
    store_branch1
        .set("branch1", "value1")
        .expect("Failed to set branch1");
    let branch1_id = op_branch1.commit().expect("Failed to commit branch1");

    let op_branch2 = tree
        .new_operation_with_tips([base_id])
        .expect("Failed to create branch2");
    let store_branch2 = op_branch2
        .get_subtree::<KVStore>("data")
        .expect("Failed to get branch2 store");
    store_branch2
        .set("branch2", "value2")
        .expect("Failed to set branch2");
    let branch2_id = op_branch2.commit().expect("Failed to commit branch2");

    // Create operation with multiple tips (merge operation)
    let merge_tips = vec![branch1_id.clone(), branch2_id.clone()];
    let op_merge = tree
        .new_operation_with_tips(&merge_tips)
        .expect("Failed to create merge operation");
    let store_merge = op_merge
        .get_subtree::<KVStore>("data")
        .expect("Failed to get merge store");
    let state_merge = store_merge.get_all().expect("Failed to get merge state");

    // Merge operation should see data from all branches
    assert!(
        state_merge.get("base").is_some(),
        "Merge should see base data"
    );
    assert!(
        state_merge.get("branch1").is_some(),
        "Merge should see branch1 data"
    );
    assert!(
        state_merge.get("branch2").is_some(),
        "Merge should see branch2 data"
    );

    // Commit merge operation
    store_merge
        .set("merged", "final")
        .expect("Failed to set merged");
    let merge_id = op_merge.commit().expect("Failed to commit merge");

    // Verify the merge operation correctly set up parents
    let backend = tree.backend();
    let merge_entry = backend.get(&merge_id).expect("Failed to get merge entry");
    let merge_parents = merge_entry.parents().expect("Failed to get merge parents");

    assert_eq!(
        merge_parents.len(),
        2,
        "Merge entry should have two parents"
    );
    assert!(
        merge_parents.contains(&branch1_id),
        "Merge should have branch1 as parent"
    );
    assert!(
        merge_parents.contains(&branch2_id),
        "Merge should have branch2 as parent"
    );
}

#[test]
fn test_new_operation_with_empty_tips_validation() {
    let tree = setup_tree();

    // Attempting to create operation with empty tips should return an error
    let result = tree.new_operation_with_tips(&[]);
    assert!(
        result.is_err(),
        "Creating operation with empty tips should be an error"
    );
}

#[test]
fn test_new_operation_with_invalid_tree_tips() {
    const TEST_KEY1: &str = "test_key1";
    const TEST_KEY2: &str = "test_key2";
    let db = setup_db_with_keys(&[TEST_KEY1, TEST_KEY2]);

    // Create tree1
    let mut tree1_settings = Map::new();
    tree1_settings.set_string("name", "tree1");
    let tree1 = db
        .new_tree(tree1_settings, TEST_KEY1)
        .expect("Failed to create tree1");

    // Create tree2 with same backend but different root
    let mut tree2_settings = Map::new();
    tree2_settings.set_string("name", "tree2");
    let tree2 = db
        .new_tree(tree2_settings, TEST_KEY2)
        .expect("Failed to create tree2");

    // Create an entry in tree1
    let op1 = tree1.new_operation().expect("Failed to create operation");
    let store1 = op1
        .get_subtree::<KVStore>("data")
        .expect("Failed to get store");
    store1.set("key1", "value1").expect("Failed to set key1");
    let entry1_id = op1.commit().expect("Failed to commit");

    // Try to use entry from tree1 as tip for operation in tree2 - should fail
    let result = tree2.new_operation_with_tips([entry1_id]);
    assert!(
        result.is_err(),
        "Using tip from different tree should be an error"
    );
}
