use super::helpers::*;
use eidetica::entry::Entry;

#[test]
fn test_entry_add_subtree() {
    let root = "test_root_parents";

    // Part 1: Create entry using the builder pattern directly
    let subtree_name = "subtree1";
    let subtree_data = "subtree_data";

    // Use the builder pattern with direct chaining (no variable)
    let entry = Entry::builder(root)
        .set_subtree_data(subtree_name, subtree_data)
        .build();

    // Verify subtree was added
    let subtrees = entry.subtrees();
    assert_eq!(subtrees.len(), 1);
    assert_eq!(subtrees[0], subtree_name);

    // Verify subtree data
    let fetched_data = entry.data(subtree_name).unwrap();
    assert_eq!(fetched_data, subtree_data);

    // Check subtree parents
    assert_subtree_no_parents(&entry, subtree_name); // New subtree has no parents initially

    // Part 2: Test overwrite using the mutable reference pattern
    let mut builder = Entry::builder(root);
    builder.set_subtree_data_mut(subtree_name, subtree_data);
    let new_subtree_data = "new_subtree_data";
    builder.set_subtree_data_mut(subtree_name, new_subtree_data);

    // Build the entry
    let new_entry = builder.build();

    // Verify count is still 1
    assert_eq!(new_entry.subtrees().len(), 1);

    // Verify data was overwritten
    let fetched_new_data = new_entry.data(subtree_name).unwrap();
    assert_eq!(fetched_new_data, new_subtree_data);
}

#[test]
fn test_entry_with_multiple_subtrees() {
    let root = "test_root_order";

    // Create a builder
    let mut builder = Entry::builder(root);

    // Add several subtrees
    let subtrees = [
        ("users", "user_data"),
        ("posts", "post_data"),
        ("comments", "comment_data"),
        ("ratings", "rating_data"),
    ];

    for (name, data) in subtrees.iter() {
        builder.set_subtree_data_mut(*name, *data);
    }

    // Add parents to each subtree
    for (name, _) in subtrees.iter() {
        let parent_id = format!("parent_for_{name}").into();
        builder.set_subtree_parents_mut(*name, vec![parent_id]);
    }

    // Build the entry
    let entry = builder.build();

    // Verify all subtrees were added
    assert_has_subtrees(&entry, &["users", "posts", "comments", "ratings"]);

    // Verify each subtree has the right data
    for (name, data) in subtrees.iter() {
        assert!(entry.in_subtree(name));
        assert_eq!(entry.data(name).unwrap(), data);
    }

    // Try to access a non-existent subtree
    let non_existent = entry.data("non_existent");
    assert!(non_existent.is_err());

    // Verify parents were set correctly
    for (name, _) in subtrees.iter() {
        let parent_id = format!("parent_for_{name}");
        let parents = entry.subtree_parents(name).unwrap();
        assert_eq!(parents.len(), 1);
        assert_eq!(parents[0], parent_id);
    }
}

#[test]
fn test_entry_remove_empty_subtrees() {
    let root = "test_root_build";
    // Apply remove_empty_subtrees via builder reconstruction
    let mut builder = Entry::builder(root);
    builder.set_subtree_data_mut("sub1", "data1");
    builder.set_subtree_data_mut("sub2_empty", "");
    builder.set_subtree_data_mut("sub3", "data3");
    builder.remove_empty_subtrees_mut();
    let entry = builder.build();

    // Verify empty subtree was removed and remaining data is intact
    assert_subtrees_with_data(&entry, &[("sub1", "data1"), ("sub3", "data3")]);
    assert!(!entry.in_subtree("sub2_empty"));
}

#[test]
fn test_subtrees_are_sorted() {
    // Create entry with subtrees in reverse order
    let entry = create_entry_with_subtrees("root_id", &[("c", "{}"), ("a", "{}"), ("b", "{}")]);

    // Verify subtrees are sorted alphabetically
    let subtrees = entry.subtrees();
    assert_eq!(
        subtrees,
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
}

#[test]
fn test_entrybuilder_subtree_parent_methods() {
    // Test the add_subtree_parent and add_subtree_parent_mut methods

    // Create a builder with a subtree
    let mut builder = Entry::builder("test_root").set_subtree_data("subtree1", "data1");

    // Add first subtree parent with mutable method
    builder.add_subtree_parent_mut("subtree1", "sp1");

    // Add second subtree parent with ownership method
    let builder = builder.add_subtree_parent("subtree1", "sp2");

    // Build the entry
    let entry = builder.build();

    // Check that both subtree parents were added
    assert_subtree_has_parents(&entry, "subtree1", &["sp1", "sp2"]);

    // Also test adding to an existing list of subtree parents
    let entry2 = Entry::builder("test_root")
        .set_subtree_data("subtree1", "data1")
        .set_subtree_parents("subtree1", vec!["sp1".into(), "sp2".into()])
        .add_subtree_parent("subtree1", "sp3")
        .build();

    assert_subtree_has_parents(&entry2, "subtree1", &["sp1", "sp2", "sp3"]);

    // Test adding a parent to a non-existent subtree (should create the subtree)
    let entry3 = Entry::builder("test_root")
        .add_subtree_parent("new_subtree", "sp1")
        .build();

    assert!(entry3.in_subtree("new_subtree"));
    assert_subtree_has_parents(&entry3, "new_subtree", &["sp1"]);
}

#[test]
fn test_subtree_empty_name() {
    // Builder with empty subtree names
    let entry_with_empty_subtree = Entry::builder("test_root")
        .set_subtree_data("", "empty_subtree_data")
        .build();

    // Verify the empty-named subtree exists
    assert!(entry_with_empty_subtree.in_subtree(""));
    assert_eq!(
        entry_with_empty_subtree.data("").unwrap(),
        "empty_subtree_data"
    );
}
