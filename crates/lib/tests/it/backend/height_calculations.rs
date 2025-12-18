//! Tests for Entry height storage and serialization.
//!
//! These tests verify that heights are correctly stored in Entry structures
//! and survive serialization roundtrips. Height *computation* via Transaction
//! is tested in the transaction/height_strategy.rs tests.

use eidetica::Entry;

#[test]
fn test_height_stored_in_entry() {
    // Verify that height is correctly stored and retrieved from Entry
    let root = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    assert_eq!(root.height(), 0, "Root height should be 0");

    let root_id = root.id();

    // Create entry with explicit height
    let entry_with_height = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_height(5)
        .build()
        .expect("Entry should build successfully");

    assert_eq!(entry_with_height.height(), 5, "Entry height should be 5");

    // Create entry with subtree height
    let entry_with_subtree = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data("test_store", "data")
        .set_subtree_height("test_store", Some(3))
        .build()
        .expect("Entry should build successfully");

    assert_eq!(
        entry_with_subtree.subtree_height("test_store").unwrap(),
        3,
        "Subtree height should be 3"
    );
}

#[test]
fn test_height_serialization() {
    // Create a proper root entry first to get a valid ID
    let root = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let root_id = root.id();

    let entry = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_height(42)
        .set_subtree_data("store1", "data")
        .set_subtree_height("store1", Some(7))
        .build()
        .expect("Entry should build successfully");

    // Serialize and deserialize
    let json = serde_json::to_string(&entry).expect("Should serialize");
    let deserialized: Entry = serde_json::from_str(&json).expect("Should deserialize");

    assert_eq!(
        deserialized.height(),
        42,
        "Tree height should survive roundtrip"
    );
    assert_eq!(
        deserialized.subtree_height("store1").unwrap(),
        7,
        "Subtree height should survive roundtrip"
    );
}

#[test]
fn test_zero_height_not_serialized() {
    // Verify that height 0 is not serialized (skip_serializing_if = "is_zero")
    let root = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");

    let json = serde_json::to_string(&root).expect("Should serialize");

    // Height 0 should be skipped in serialization
    assert!(
        !json.contains("\"h\":0"),
        "Zero height should not be serialized: {json}"
    );

    // But deserializing without height should give height 0
    let deserialized: Entry = serde_json::from_str(&json).expect("Should deserialize");
    assert_eq!(
        deserialized.height(),
        0,
        "Missing height should default to 0"
    );
}

#[test]
fn test_subtree_height_inheritance() {
    // Test that Entry.subtree_height() returns tree height when subtree height is None
    let root = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let root_id = root.id();

    // Create entry with explicit tree height and subtree without explicit height
    let entry = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_height(42) // Tree height
        .set_subtree_data("test_store", "data")
        // Note: not setting subtree height, so it defaults to None (inherit)
        .build()
        .expect("Entry should build successfully");

    // Subtree height should inherit from tree (42)
    assert_eq!(
        entry.subtree_height("test_store").unwrap(),
        42,
        "Subtree with no explicit height should inherit tree height"
    );

    assert_eq!(entry.height(), 42, "Tree height should be 42");
}

#[test]
fn test_subtree_independent_height_vs_inherited() {
    let root = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let root_id = root.id();

    // Create entry with both inherited and independent subtree heights
    let entry = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_height(100) // Tree height
        .set_subtree_data("inherited_store", "data1")
        // inherited_store height not set, defaults to None (inherit)
        .set_subtree_data("independent_store", "data2")
        .set_subtree_height("independent_store", Some(5)) // Independent height
        .build()
        .expect("Entry should build successfully");

    // Inherited store should return tree height
    assert_eq!(
        entry.subtree_height("inherited_store").unwrap(),
        100,
        "Subtree with no explicit height should return tree height"
    );

    // Independent store should return its own height
    assert_eq!(
        entry.subtree_height("independent_store").unwrap(),
        5,
        "Subtree with explicit height should return that height"
    );
}
