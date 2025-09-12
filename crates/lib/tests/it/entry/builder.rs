use eidetica::Entry;

use super::helpers::*;

#[test]
fn test_dual_api_patterns() {
    // Test 1: Builder pattern with ownership
    // This pattern takes self and returns Self, allowing method chaining
    let entry = Entry::builder("root_id")
        .set_parents(vec!["parent1".into(), "parent2".into()])
        .set_subtree_data("subtree1", "subtree_data1")
        .set_subtree_parents("subtree1", vec!["subtree_parent1".into()])
        .add_subtree_parent("subtree1", "subtree_parent2")
        .build();

    // Verify the entry was built correctly
    assert_eq!(entry.root(), "root_id");
    assert!(entry.in_subtree("subtree1"));
    assert_eq!(entry.data("subtree1").unwrap(), "subtree_data1");
    assert_has_parents(&entry, &["parent1", "parent2"]);
    assert_subtree_has_parents(&entry, "subtree1", &["subtree_parent1", "subtree_parent2"]);

    // Test 2: Mutable reference pattern
    // This pattern takes &mut self and returns &mut Self
    // Useful when you need to keep the builder in a variable
    let mut builder = Entry::builder("root_id2");

    // Use the _mut methods for modifications
    builder
        .set_parents_mut(vec!["parent3".into(), "parent4".into()])
        .set_subtree_data_mut("subtree2", "subtree_data2")
        .set_subtree_parents_mut("subtree2", vec!["subtree_parent3".into()])
        .add_subtree_parent_mut("subtree2", "subtree_parent4");

    // Build the entry
    let entry2 = builder.build();

    // Verify the entry was built correctly
    assert_eq!(entry2.root(), "root_id2");
    assert!(entry2.in_subtree("subtree2"));
    assert_eq!(entry2.data("subtree2").unwrap(), "subtree_data2");
    assert_has_parents(&entry2, &["parent3", "parent4"]);
    assert_subtree_has_parents(&entry2, "subtree2", &["subtree_parent3", "subtree_parent4"]);
}

#[test]
fn test_entrybuilder_api_consistency() {
    // Test that both ownership and mutable reference APIs produce identical results

    // First entry using ownership chaining API
    let entry1 = Entry::builder("root")
        .set_parents(vec!["parent1".into(), "parent2".into()])
        .set_subtree_data("subtree1", "data1")
        .set_subtree_parents("subtree1", vec!["sp1".into()])
        .add_parent("parent3")
        .add_subtree_parent("subtree1", "sp2")
        .remove_empty_subtrees()
        .build();

    // Second entry using mutable reference API
    let mut builder2 = Entry::builder("root");
    builder2
        .set_parents_mut(vec!["parent1".into(), "parent2".into()])
        .set_subtree_data_mut("subtree1", "data1")
        .set_subtree_parents_mut("subtree1", vec!["sp1".into()])
        .add_parent_mut("parent3")
        .add_subtree_parent_mut("subtree1", "sp2")
        .remove_empty_subtrees_mut();
    let entry2 = builder2.build();

    // IDs should be identical, showing that both APIs produce equivalent results
    assert_same_id(&entry1, &entry2);
}

#[test]
fn test_entrybuilder_empty_subtree_removal() {
    // Test the behavior of removing empty subtrees

    // Create a builder with one subtree with data and one with empty data
    let builder = Entry::builder("root")
        .set_subtree_data("subtree1", "data1")
        .set_subtree_data("empty", "");

    // Create two copies to test each API
    let entry1 = builder.clone().remove_empty_subtrees().build();

    let mut builder2 = builder.clone();
    builder2.remove_empty_subtrees_mut();
    let entry2 = builder2.build();

    // Both entries should have only one subtree (the empty one should be removed)
    assert_eq!(entry1.subtrees().len(), 1);
    assert_eq!(entry2.subtrees().len(), 1);

    // Both should have the same ID
    assert_same_id(&entry1, &entry2);

    // Both should have the non-empty subtree
    assert!(entry1.in_subtree("subtree1"));
    assert!(!entry1.in_subtree("empty"));
}

#[test]
fn test_entrybuilder_add_parent_methods() {
    // Test the add_parent and add_parent_mut methods

    // Start with no parents
    let mut builder = Entry::builder("test_root");

    // Add first parent with mutable method
    builder.add_parent_mut("parent1");

    // Add second parent with ownership method
    let builder = builder.add_parent("parent2");

    // Build the entry
    let entry = builder.build();

    // Check that both parents were added
    assert_has_parents(&entry, &["parent1", "parent2"]);

    // Also test adding to an existing list of parents
    let entry2 = Entry::builder("test_root")
        .set_parents(vec!["parent1".into(), "parent2".into()])
        .add_parent("parent3")
        .build();

    assert_has_parents(&entry2, &["parent1", "parent2", "parent3"]);
}

#[test]
fn test_entrybuilder_parent_deduplication() {
    // Test that duplicate parent IDs are handled correctly

    // Create an entry with duplicate parents
    let entry =
        create_entry_with_duplicate_parents("test_root", &["parent1", "parent2", "parent1"]);

    // Also add subtree with duplicate parents
    let builder = Entry::builder("test_root")
        .set_parents(vec!["parent1".into(), "parent2".into(), "parent1".into()])
        .set_subtree_data("subtree1", "data1")
        .set_subtree_parents("subtree1", vec!["sp1".into(), "sp2".into(), "sp1".into()]);
    let entry_with_subtree = builder.build();

    // Check that the main tree parents have duplicates removed
    assert_has_parents(&entry, &["parent1", "parent2"]);
    assert_has_parents(&entry_with_subtree, &["parent1", "parent2"]);

    // Check that the subtree parents have duplicates removed
    assert_subtree_has_parents(&entry_with_subtree, "subtree1", &["sp1", "sp2"]);
}

#[test]
fn test_parents_are_sorted() {
    let entry = create_entry_with_unsorted_parents(
        "root_id",
        &["c", "a", "b"],
        &[("test", &["z", "x", "y"])],
    );

    // Verify both main tree and subtree parents are sorted
    assert_parents_sorted(&entry, &["a", "b", "c"], &[("test", &["x", "y", "z"])]);
}

#[test]
fn test_entrybuilder_edge_cases() {
    // Test behavior of EntryBuilder with edge cases

    // Empty root entry (created with Entry::root_builder)
    let empty_entry = create_empty_entry();
    assert_eq!(empty_entry.root(), ""); // Default root should be empty string
    assert_no_parents(&empty_entry); // Root entries have no parents
    assert!(empty_entry.is_root()); // Should be a root entry
    assert!(empty_entry.in_subtree("_root")); // Root entries have the _root subtree
}
