use eideticadb::entry::{Entry, ID};

#[test]
fn test_entry_creation() {
    let root = "test_root".to_string();
    let data = "test_data".to_string();
    let entry = Entry::new(root.clone(), data.clone());

    assert_eq!(entry.root(), &root);
    assert!(!entry.is_root()); // Regular entries are not root entries
    assert!(!entry.is_toplevel_root()); // Should be false as it's not a top-level entry
    assert_eq!(entry.get_settings().unwrap(), data); // Use get_settings() to access the main data
    assert!(entry.parents().unwrap().is_empty()); // New entry has no parents
}

#[test]
fn test_entry_toplevel_creation() {
    let data = "test_data".to_string();
    let entry = Entry::new_top_level(data.clone());

    assert!(entry.is_toplevel_root());
    assert!(entry.is_root());
    assert_eq!(entry.get_settings().unwrap(), data); // Use get_settings() to access the main data
    assert!(entry.in_subtree("root")); // Top-level entries have a "root" subtree
}

#[test]
fn test_entry_add_subtree() {
    let root = "test_root".to_string();
    let data = "test_data".to_string();
    let mut entry = Entry::new(root, data);

    let subtree_name = "subtree1";
    let subtree_data = "subtree_data".to_string();
    let result = entry.set_subtree_data(subtree_name.to_string(), subtree_data.clone());
    assert!(result.is_ok());

    // Verify subtree was added
    let subtrees = entry.subtrees();
    assert_eq!(subtrees.len(), 1);
    assert_eq!(subtrees[0], subtree_name);

    // Verify subtree data
    let fetched_data = entry.data(subtree_name).unwrap();
    assert_eq!(fetched_data, &subtree_data);

    // Check subtree parents
    let subtree_parents = entry.subtree_parents(subtree_name).unwrap();
    assert!(subtree_parents.is_empty()); // New subtree has no parents initially

    // ---- Test Overwrite ----
    let new_subtree_data = "new_subtree_data".to_string();
    let overwrite_result =
        entry.set_subtree_data(subtree_name.to_string(), new_subtree_data.clone());
    assert!(overwrite_result.is_ok());

    // Verify count is still 1
    assert_eq!(entry.subtrees().len(), 1);

    // Verify data was overwritten
    let fetched_new_data = entry.data(subtree_name).unwrap();
    assert_eq!(fetched_new_data, &new_subtree_data);
}

#[test]
fn test_entry_parents() {
    let root = "test_root".to_string();
    let data = "test_data".to_string();
    let mut entry = Entry::new(root, data);

    // Set parents for the main tree
    let parent1 = "parent1".to_string();
    let parent2 = "parent2".to_string();
    let parents = vec![parent1.clone(), parent2.clone()];
    entry.set_parents(parents.clone());

    // Verify parents were set
    let fetched_parents = entry.parents().unwrap();
    assert_eq!(fetched_parents, parents);

    // Test subtree parents
    let subtree_name = "subtree1";
    let subtree_data = "subtree_data".to_string();
    entry
        .set_subtree_data(subtree_name.to_string(), subtree_data)
        .unwrap();

    let subtree_parent = "subtree_parent".to_string();
    entry.set_subtree_parents(subtree_name, vec![subtree_parent.clone()]);

    let fetched_subtree_parents = entry.subtree_parents(subtree_name).unwrap();
    assert_eq!(fetched_subtree_parents, vec![subtree_parent]);
}

#[test]
fn test_entry_id() {
    let root = "test_root".to_string();
    let data = "test_data".to_string();
    let entry = Entry::new(root.clone(), data.clone());

    let id = entry.id();
    assert!(!id.is_empty());

    // Create identical entry - should have same ID (content-addressable)
    let identical_entry = Entry::new(root, data);
    assert_eq!(identical_entry.id(), id);

    // Create different entry - should have different ID
    let different_entry = Entry::new("different_root".to_string(), "different_data".to_string());
    assert_ne!(different_entry.id(), id);
}

#[test]
fn test_in_tree_and_subtree() {
    let root = "test_root".to_string();
    let data = "test_data".to_string();
    let mut entry = Entry::new(root.clone(), data);

    assert!(entry.in_tree(&root));
    assert!(!entry.in_tree("other_tree"));

    let subtree_name = "subtree1";
    entry
        .set_subtree_data(subtree_name.to_string(), "subtree_data".to_string())
        .unwrap();

    assert!(entry.in_subtree(subtree_name));
    assert!(!entry.in_subtree("non_existent_subtree"));
}

#[test]
fn test_entry_with_multiple_subtrees() {
    let root = "test_root".to_string();
    let main_data = "main_data".to_string();

    // Create an entry with multiple subtrees
    let mut entry = Entry::new(root, main_data);

    // Add several subtrees
    let subtrees = [
        ("users", "user_data"),
        ("posts", "post_data"),
        ("comments", "comment_data"),
        ("ratings", "rating_data"),
    ];

    for (name, data) in subtrees.iter() {
        entry
            .set_subtree_data(name.to_string(), data.to_string())
            .unwrap_or_else(|_| panic!("Failed to add subtree {}", name));
    }

    // Verify all subtrees were added
    let subtree_names = entry.subtrees();
    assert_eq!(subtree_names.len(), 4);

    // Verify each subtree has the right data
    for (name, data) in subtrees.iter() {
        assert!(entry.in_subtree(name));
        assert_eq!(entry.data(name).unwrap(), &data.to_string());
    }

    // Try to access a non-existent subtree
    let non_existent = entry.data("non_existent");
    assert!(non_existent.is_err());

    // Add parents to each subtree
    for (name, _) in subtrees.iter() {
        let parent_id = format!("parent_for_{}", name);
        entry.set_subtree_parents(name, vec![parent_id.clone()]);

        // Verify parents were set correctly
        let parents = entry.subtree_parents(name).unwrap();
        assert_eq!(parents.len(), 1);
        assert_eq!(parents[0], parent_id);
    }
}

#[test]
fn test_entry_id_determinism() {
    // Test that entries with the same data but created differently have the same ID

    // First entry
    let mut entry1 = Entry::new("test_root".to_string(), "main_data".to_string());
    // Parents order should not matter
    entry1.set_parents(vec!["parent1".to_string(), "parent2".to_string()]);
    entry1
        .set_subtree_data("subtree1".to_string(), "data1".to_string())
        .unwrap();
    entry1
        .set_subtree_data("subtree2".to_string(), "data2".to_string())
        .unwrap();
    entry1.set_subtree_parents("subtree1", vec!["sub_parent1".to_string()]);

    // Second entry with same content but adding subtrees and parents in different order
    let mut entry2 = Entry::new("test_root".to_string(), "main_data".to_string());
    // Order of adding subtrees should not matter
    entry2
        .set_subtree_data("subtree2".to_string(), "data2".to_string())
        .unwrap();
    entry2
        .set_subtree_data("subtree1".to_string(), "data1".to_string())
        .unwrap();
    // Order of parents should not matter
    // Now using different order to test that the order of parents does not matter
    entry2.set_parents(vec!["parent2".to_string(), "parent1".to_string()]);
    entry2.set_subtree_parents("subtree1", vec!["sub_parent1".to_string()]);

    // IDs should be the same
    assert_eq!(entry1.id(), entry2.id());

    // Now modify entry2 in a subtle way
    let mut entry3 = entry2.clone();
    entry3.set_subtree_parents("subtree1", vec!["different_parent".to_string()]);

    // IDs should now be different
    assert_ne!(entry1.id(), entry3.id());
}

#[test]
fn test_entry_remove_empty_subtrees() {
    let root = "test_root".to_string();
    let data = "test_data".to_string();
    let mut entry = Entry::new(root, data);

    // Add some subtrees, some with data, some without
    entry
        .set_subtree_data("sub1".to_string(), "data1".to_string())
        .unwrap();
    entry
        .set_subtree_data("sub2_empty".to_string(), "".to_string())
        .unwrap(); // Empty data
    entry
        .set_subtree_data("sub3".to_string(), "data3".to_string())
        .unwrap();

    assert_eq!(entry.subtrees().len(), 3);

    // Call the cleanup method
    assert!(entry.remove_empty_subtrees().is_ok());

    // Verify empty subtree was removed
    let remaining_subtrees = entry.subtrees();
    assert_eq!(remaining_subtrees.len(), 2);
    assert!(remaining_subtrees.contains(&"sub1".to_string()));
    assert!(remaining_subtrees.contains(&"sub3".to_string()));
    assert!(!remaining_subtrees.contains(&"sub2_empty".to_string()));

    // Verify data of remaining subtrees is intact
    assert_eq!(entry.data("sub1").unwrap(), "data1");
    assert_eq!(entry.data("sub3").unwrap(), "data3");

    // Test removing when all are non-empty
    let mut entry_all_full = Entry::new("root2".to_string(), "data".to_string());
    entry_all_full
        .set_subtree_data("full1".to_string(), "data1".to_string())
        .unwrap();
    entry_all_full
        .set_subtree_data("full2".to_string(), "data2".to_string())
        .unwrap();
    assert!(entry_all_full.remove_empty_subtrees().is_ok());
    assert_eq!(entry_all_full.subtrees().len(), 2);

    // Test removing when all are empty
    let mut entry_all_empty = Entry::new("root3".to_string(), "data".to_string());
    entry_all_empty
        .set_subtree_data("empty1".to_string(), "".to_string())
        .unwrap();
    entry_all_empty
        .set_subtree_data("empty2".to_string(), "".to_string())
        .unwrap();
    assert!(entry_all_empty.remove_empty_subtrees().is_ok());
    assert!(entry_all_empty.subtrees().is_empty());
}

#[test]
fn test_add_subtree_success() {
    let mut entry = Entry::new("root_id".to_string(), "{}".to_string());
    let result = entry.set_subtree_data("my_subtree".to_string(), "{}".to_string());
    assert!(result.is_ok());
    assert!(entry.in_subtree("my_subtree"));
    assert_eq!(entry.subtrees().len(), 1);
}

#[test]
fn test_add_subtree_duplicate() {
    let mut entry = Entry::new("root_id".to_string(), "{}".to_string());
    entry
        .set_subtree_data(
            "my_subtree".to_string(),
            "{\"initial\":\"data\"}".to_string(),
        )
        .expect("First add should succeed");
    let parents: Vec<ID> = vec!["parent1".to_string(), "parent2".to_string()];
    entry.set_subtree_parents("my_subtree", parents.clone());

    assert_eq!(entry.subtrees().len(), 1);
    assert_eq!(
        entry.data("my_subtree").unwrap(),
        &"{\"initial\":\"data\"}".to_string()
    );
    assert_eq!(entry.subtree_parents("my_subtree").unwrap(), parents);

    let result = entry.set_subtree_data(
        "my_subtree".to_string(),
        "{\"updated\":\"data\"}".to_string(),
    );
    assert!(
        result.is_ok(),
        "Adding duplicate subtree should succeed with overwrite behavior"
    );
    assert_eq!(entry.subtrees().len(), 1);
    assert_eq!(
        entry.data("my_subtree").unwrap(),
        &"{\"updated\":\"data\"}".to_string()
    );
    assert_eq!(entry.subtree_parents("my_subtree").unwrap(), parents);
}

#[test]
fn test_subtrees_are_sorted() {
    let mut entry = Entry::new("root_id".to_string(), "{}".to_string());
    entry
        .set_subtree_data("z_subtree".to_string(), "{}".to_string())
        .unwrap();
    entry
        .set_subtree_data("a_subtree".to_string(), "{}".to_string())
        .unwrap();
    entry
        .set_subtree_data("m_subtree".to_string(), "{}".to_string())
        .unwrap();

    let sorted_subtree_names = entry.subtrees();
    assert_eq!(sorted_subtree_names.len(), 3);
    assert_eq!(
        sorted_subtree_names,
        vec!["a_subtree", "m_subtree", "z_subtree"]
    );
}

#[test]
fn test_parents_are_sorted() {
    let mut entry = Entry::new("root_id".to_string(), "{}".to_string());
    entry.set_parents(vec![
        "z_parent".to_string(),
        "a_parent".to_string(),
        "m_parent".to_string(),
    ]);

    let parents = entry.parents().unwrap();
    assert_eq!(parents.len(), 3);
    assert_eq!(parents, vec!["a_parent", "m_parent", "z_parent"]);

    entry
        .set_subtree_data("test_subtree".to_string(), "{}".to_string())
        .unwrap();
    entry.set_subtree_parents(
        "test_subtree",
        vec![
            "z_subparent".to_string(),
            "a_subparent".to_string(),
            "m_subparent".to_string(),
        ],
    );

    let subtree_parents = entry.subtree_parents("test_subtree").unwrap();
    assert_eq!(subtree_parents.len(), 3);
    assert_eq!(
        subtree_parents,
        vec!["a_subparent", "m_subparent", "z_subparent"]
    );
}
