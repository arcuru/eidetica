use eidetica::{Entry, constants::ROOT};

use super::helpers::*;

#[test]
fn test_entry_creation() {
    let root = "test_root";
    let entry = create_entry_with_parents(root, &["parent1"]);

    assert_eq!(entry.root(), root);
    assert!(!entry.is_root()); // Regular entries are not root entries

    assert_has_parents(&entry, &["parent1"]); // Entry now has parents as required
}

#[test]
fn test_entry_toplevel_creation() {
    let entry = create_root_entry();

    assert!(entry.root().is_empty());
    assert!(entry.is_root());
    assert!(entry.in_subtree(ROOT)); // Top-level entries have a "root" subtree
}

#[test]
fn test_in_tree_and_subtree() {
    let root = "test_root_subtrees";
    let entry = create_entry_with_subtree(root, "subtree1", "subtree_data");

    // Root entries created with Entry::root_builder() have empty string as root
    assert!(entry.in_tree(""));
    assert!(entry.in_tree(entry.id())); // Also check by entry ID
    assert!(!entry.in_tree("other_tree"));
    assert!(entry.in_subtree("subtree1"));
    assert!(!entry.in_subtree("non_existent_subtree"));
}

#[test]
fn test_entry_parents() {
    let root = "test_root_parents";

    // Create entry with main tree parents
    let entry_with_parents = create_entry_with_parents(root, &["parent1", "parent2"]);
    assert_has_parents(&entry_with_parents, &["parent1", "parent2"]);

    // Create entry with subtree and subtree parents
    let subtree_name = "subtree1";
    let subtree_data = "subtree_data";
    let entry_with_subtree_parents =
        create_entry_with_subtree_parents(root, subtree_name, subtree_data, &["subtree_parent"]);

    // Verify subtree parents
    assert_subtree_has_parents(
        &entry_with_subtree_parents,
        subtree_name,
        &["subtree_parent"],
    );

    // Test entry with both main and subtree parents
    let mut builder = Entry::builder(root);
    builder.set_parents_mut(vec!["parent1".into(), "parent2".into()]);
    builder.set_subtree_data_mut(subtree_name, subtree_data);
    builder.set_subtree_parents_mut(subtree_name, vec!["subtree_parent".into()]);
    let complex_entry = builder.build();

    assert_has_parents(&complex_entry, &["parent1", "parent2"]);
    assert_subtree_has_parents(&complex_entry, subtree_name, &["subtree_parent"]);
}
