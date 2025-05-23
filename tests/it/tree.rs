use crate::helpers::*;
use eidetica::constants::SETTINGS;
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

    // Verify retrieval through backend directly
    let backend = tree.backend();
    let backend_guard = backend.lock().unwrap();

    let retrieved_entry1 = backend_guard.get(&id1).expect("Failed to get entry 1");
    assert_eq!(retrieved_entry1.id(), id1);

    let retrieved_entry2 = backend_guard.get(&id2).expect("Failed to get entry 2");
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
