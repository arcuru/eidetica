//! Core tree operation tests
//!
//! This module contains tests for fundamental Tree operations including
//! entry creation, subtree operations, atomic operations, and tip management.

use super::helpers::*;
use crate::helpers::*;
use eidetica::constants::SETTINGS;
use eidetica::store::DocStore;

#[test]
fn test_insert_into_tree() {
    let tree = setup_tree();

    // Create and commit first entry using an atomic operation
    let op1 = tree.new_transaction().expect("Failed to create operation");
    let id1 = op1.commit().expect("Failed to commit operation");

    // Create and commit second entry
    let op2 = tree.new_transaction().expect("Failed to create operation");
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
    let op1 = tree.new_transaction().expect("Failed to create operation");
    {
        let users_store = op1
            .get_store::<DocStore>("users")
            .expect("Failed to get users store");

        let posts_store = op1
            .get_store::<DocStore>("posts")
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
        .get_store_viewer::<DocStore>("users")
        .expect("Failed to get users viewer (1)");
    assert_eq!(
        users_viewer1
            .get_string("user1.name")
            .expect("Failed to get user1.name (1)"),
        "Alice"
    );
    let posts_viewer1 = tree
        .get_store_viewer::<DocStore>("posts")
        .expect("Failed to get posts viewer (1)");
    assert_eq!(
        posts_viewer1
            .get_string("post1.title")
            .expect("Failed to get post1.title (1)"),
        "First Post"
    );

    // --- Create another operation modifying only the users subtree ---
    let op2 = tree
        .new_transaction()
        .expect("Failed to create operation 2");
    {
        let users_store2 = op2
            .get_store::<DocStore>("users")
            .expect("Failed to get users store (2)");
        users_store2
            .set("user2.name", "Bob")
            .expect("Failed to set second user data");
    }

    // Commit the second operation
    op2.commit().expect("Failed to commit second operation");

    // --- Test Store viewers for reading final data ---
    let users_viewer2 = tree
        .get_store_viewer::<DocStore>("users")
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
        .get_store_viewer::<DocStore>("posts")
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
    let op = tree.new_transaction().expect("Failed to create operation");
    {
        let settings_store = op
            .get_store::<DocStore>(SETTINGS)
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
    let op1 = tree.new_transaction().expect("Op1: Failed to start");
    let initial_tip = tree.get_tips().unwrap()[0].clone();
    {
        let store_a = op1
            .get_store::<DocStore>("sub_a")
            .expect("Op1: Failed get A");
        store_a.set("key_a", "val_a1").expect("Op1: Failed set A");

        let store_b = op1
            .get_store::<DocStore>("sub_b")
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
        .get_store_viewer::<DocStore>("sub_a")
        .expect("Viewer A1");
    assert_eq!(
        viewer_a1.get_string("key_a").expect("Viewer A1 get"),
        "val_a1"
    );
    let viewer_b1 = tree
        .get_store_viewer::<DocStore>("sub_b")
        .expect("Viewer B1");
    assert_eq!(
        viewer_b1.get_string("key_b").expect("Viewer B1 get"),
        "val_b1"
    );

    // --- 2. Commit an empty operation ---
    let op_empty = tree.new_transaction().expect("OpEmpty: Failed to start");
    let commit_empty_result = op_empty.commit();
    // If it's not an error, check the tip is still changed to the empty commit
    assert!(commit_empty_result.is_ok());
    assert_eq!(
        tree.get_tips().unwrap()[0],
        commit_empty_result.unwrap(),
        "Empty commit should still be a tip"
    );

    // --- 3. Attempt to commit the same op twice ---
    let op3 = tree.new_transaction().expect("Op3: Failed to start");
    {
        let store_a = op3
            .get_store::<DocStore>("sub_a")
            .expect("Op3: Failed get A");
        store_a.set("key_a", "val_a3").expect("Op3: Failed set A");
    }
    let _commit3_id = op3.commit().expect("Op3: First commit failed");

    // Commiting again won't even compile
    // let commit3_again = op3.commit();
}

#[test]
fn test_get_store_viewer() {
    let tree = setup_tree();

    // --- Initial state ---
    let op1 = tree.new_transaction().expect("Op1: Failed start");
    {
        let store = op1
            .get_store::<DocStore>("my_data")
            .expect("Op1: Failed get");
        store.set("key1", "value1").expect("Op1: Failed set");
    }
    op1.commit().expect("Op1: Failed commit");

    // --- Get viewer 1 (sees initial state) ---
    let viewer1 = tree
        .get_store_viewer::<DocStore>("my_data")
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
    let op2 = tree.new_transaction().expect("Op2: Failed start");
    {
        let store = op2
            .get_store::<DocStore>("my_data")
            .expect("Op2: Failed get");
        store
            .set("key1", "value1_updated")
            .expect("Op2: Failed update key1"); // Update existing
        store.set("key2", "value2").expect("Op2: Failed set key2"); // Add new
    }
    op2.commit().expect("Op2: Failed commit");

    // --- Get viewer 2 (sees updated state) ---
    let viewer2 = tree
        .get_store_viewer::<DocStore>("my_data")
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
    let non_existent_viewer_result = tree.get_store_viewer::<DocStore>("non_existent_subtree");
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
fn test_get_tips() {
    let tree = setup_tree();

    // Initially, the tree should have one tip (the root entry)
    let initial_tips = tree.get_tips().expect("Failed to get initial tips");
    assert_eq!(
        initial_tips.len(),
        1,
        "Tree should have exactly one initial tip"
    );

    // Create and commit first entry using helper
    let entry1_id = add_data_to_subtree(&tree, "data", &[("key1", "value1")]);

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

    // Create and commit second entry using helper
    let entry2_id = add_data_to_subtree(&tree, "data", &[("key2", "value2")]);

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
fn test_new_transaction_with_tips() {
    let tree = setup_tree();

    // Create first entry using helper
    let entry1_id = add_data_to_subtree(&tree, "data", &[("key1", "value1")]);

    // Create second entry using helper
    let entry2_id = add_data_to_subtree(&tree, "data", &[("key2", "value2")]);

    // Verify that normal operations use current tips (should see both keys)
    let normal_op = tree
        .new_transaction()
        .expect("Failed to create normal operation");
    let normal_store = normal_op
        .get_store::<DocStore>("data")
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
        .new_transaction_with_tips([entry1_id.clone()])
        .expect("Failed to create custom operation");
    let custom_store = custom_op
        .get_store::<DocStore>("data")
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

    // Commit the custom operation to create a branch using helper
    let custom_entry_id =
        create_branch_from_entry(&tree, &entry1_id, "data", &[("custom_key", "custom_value")]);

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
        .new_transaction()
        .expect("Failed to create merge operation");
    let merge_store = merge_op
        .get_store::<DocStore>("data")
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
fn test_new_transaction_with_specific_tips() {
    let tree = setup_tree();

    // Create a chain of entries: A -> B -> C using helpers
    let entry_a_id = add_data_to_subtree(&tree, "data", &[("from_a", "value_a")]);
    let entry_b_id = add_data_to_subtree(&tree, "data", &[("from_b", "value_b")]);
    let entry_c_id = add_data_to_subtree(&tree, "data", &[("from_c", "value_c")]);

    // Create operation starting from entry A (should see only A)
    let op_from_a = tree
        .new_transaction_with_tips(std::slice::from_ref(&entry_a_id))
        .expect("Failed to create op from A");
    let store_from_a = op_from_a
        .get_store::<DocStore>("data")
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
        .new_transaction_with_tips([entry_b_id])
        .expect("Failed to create op from B");
    let store_from_b = op_from_b
        .get_store::<DocStore>("data")
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
        .new_transaction_with_tips([entry_c_id])
        .expect("Failed to create op from C");
    let store_from_c = op_from_c
        .get_store::<DocStore>("data")
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

    // Test branching from an earlier point using helper
    let branch_id = create_branch_from_entry(
        &tree,
        &entry_a_id,
        "data",
        &[("branch_data", "branch_value")],
    );

    // Verify the branch only sees data from A
    let op_verify_branch = tree
        .new_transaction_with_tips([branch_id])
        .expect("Failed to create verify op");
    let store_verify_branch = op_verify_branch
        .get_store::<DocStore>("data")
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
fn test_new_transaction_with_multiple_tips() {
    let tree = setup_tree();

    // Create initial entry using helper
    let base_id = add_data_to_subtree(&tree, "data", &[("base", "value")]);

    // Create two parallel branches from base using helpers
    let branch1_id = create_branch_from_entry(&tree, &base_id, "data", &[("branch1", "value1")]);

    let branch2_id = create_branch_from_entry(&tree, &base_id, "data", &[("branch2", "value2")]);

    // Create operation with multiple tips (merge operation)
    let merge_tips = vec![branch1_id.clone(), branch2_id.clone()];
    let op_merge = tree
        .new_transaction_with_tips(&merge_tips)
        .expect("Failed to create merge operation");
    let store_merge = op_merge
        .get_store::<DocStore>("data")
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
