//! Tests for height calculation internals.
//!
//! These tests verify the internal `calculate_heights` and `sort_entries_by_height`
//! methods which are implementation details of the InMemory backend.
//! Height ordering is tested indirectly through `get_tree`/`get_store` in the
//! backend-agnostic tests.

use std::collections::HashMap;

use eidetica::{
    Entry,
    backend::{BackendImpl, database::InMemory},
    entry::ID,
};

/// Helper to create and store an entry with subtree data
fn create_subtree_entry(
    backend: &InMemory,
    tree_id: &ID,
    parent_id: &ID,
    subtree_name: &str,
    data: &str,
) -> ID {
    let entry = Entry::builder(tree_id.clone())
        .add_parent(parent_id.clone())
        .set_subtree_data(subtree_name, data)
        .build()
        .expect("Entry should build successfully");
    let id = entry.id();
    backend.put_verified(entry).unwrap();
    id
}

/// Helper to verify entry heights
fn assert_entry_heights(heights: &HashMap<ID, usize>, expected_heights: &[(&ID, usize)]) {
    for (entry_id, expected_height) in expected_heights {
        let actual_height = heights.get(entry_id).unwrap_or(&9999);
        assert_eq!(
            *actual_height, *expected_height,
            "Entry {entry_id} has height {actual_height}, expected {expected_height}"
        );
    }
}

#[test]
fn test_calculate_entry_height() {
    let backend = InMemory::new();

    // Create root entry
    let root = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let root_id = root.id();
    backend.put_verified(root).unwrap();

    // Create a complex tree structure:
    // root -> A -> B -> C\
    //    \                -> D
    //     \-> E -> F --->/

    // Create main branch: A -> B -> C
    let id_a = create_subtree_entry(&backend, &root_id, &root_id, "branch", "a");
    let id_b = create_subtree_entry(&backend, &root_id, &id_a, "branch", "b");
    let id_c = create_subtree_entry(&backend, &root_id, &id_b, "branch", "c");

    // Create side branch: E -> F
    let id_e = create_subtree_entry(&backend, &root_id, &root_id, "branch", "e");
    let id_f = create_subtree_entry(&backend, &root_id, &id_e, "branch", "f");

    // Create merge entry D with both C and F as parents
    let entry_d = Entry::builder(root_id.clone())
        .add_parent(id_c.clone())
        .add_parent(id_f.clone())
        .set_subtree_data("branch", "d")
        .build()
        .expect("Entry should build successfully");
    let id_d = entry_d.id();
    backend.put_verified(entry_d).unwrap();

    // Check that the tree was created correctly
    // by verifying the tip is entry D
    let tips = backend.get_tips(&root_id).unwrap();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0], id_d);

    // Check the full tree contains all 7 entries
    let tree = backend
        .get_tree_from_tips(&root_id, std::slice::from_ref(&id_d))
        .unwrap();
    assert_eq!(tree.len(), 7, "Tree should contain all 7 entries");

    // Calculate heights map and verify correct heights
    let heights = backend.calculate_heights(&root_id, None).unwrap();

    // Verify all heights using helper function
    assert_entry_heights(
        &heights,
        &[
            (&root_id, 0), // Root has height 0
            (&id_a, 1),    // First level
            (&id_e, 1),    // First level
            (&id_b, 2),    // Second level
            (&id_f, 2),    // Second level
            (&id_c, 3),    // Third level
            (&id_d, 4),    // Fourth level (takes longer path)
        ],
    );
}

#[test]
fn test_calculate_subtree_height() {
    let backend = InMemory::new();

    // Create root entry
    let root = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let root_id = root.id();
    backend.put_verified(root).unwrap();

    // A
    let entry_a = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data("sub1", "A_sub1")
        .build()
        .expect("Entry should build successfully");
    let id_a = entry_a.id();
    backend
        .put(
            eidetica::backend::VerificationStatus::Verified,
            entry_a.clone(),
        )
        .unwrap();

    // B (after A in main tree)
    let entry_b = Entry::builder(root_id.clone())
        .add_parent(id_a.clone())
        .set_subtree_data("sub1", "B_sub1")
        .build()
        .expect("Entry should build successfully");
    // B is directly under root in subtree (not under A)
    // So we don't set subtree parents
    let id_b = entry_b.id();
    backend
        .put(
            eidetica::backend::VerificationStatus::Verified,
            entry_b.clone(),
        )
        .unwrap();

    // C (after B in main tree)
    let entry_c = Entry::builder(root_id.clone())
        .add_parent(id_b.clone())
        .set_subtree_data("sub1", "C_sub1")
        .add_subtree_parent("sub1", id_a.clone())
        .add_subtree_parent("sub1", id_b.clone())
        .build()
        .expect("Entry should build successfully");
    let id_c = entry_c.id();
    backend
        .put(
            eidetica::backend::VerificationStatus::Verified,
            entry_c.clone(),
        )
        .unwrap();

    // Calculate heights for main tree
    let main_heights = backend.calculate_heights(&root_id, None).unwrap();

    // Main tree: root -> A -> B -> C
    assert_eq!(main_heights.get(&root_id).unwrap_or(&9999), &0);
    assert_eq!(main_heights.get(&id_a).unwrap_or(&9999), &1);
    assert_eq!(main_heights.get(&id_b).unwrap_or(&9999), &2);
    assert_eq!(main_heights.get(&id_c).unwrap_or(&9999), &3);

    // Calculate heights for subtree
    let sub_heights = backend.calculate_heights(&root_id, Some("sub1")).unwrap();

    // Subtree structure:
    // A   B
    //  \ /
    //   C
    assert_eq!(sub_heights.get(&id_a).unwrap(), &0);
    assert_eq!(sub_heights.get(&id_b).unwrap(), &0);
    assert_eq!(sub_heights.get(&id_c).unwrap(), &1);
}

#[test]
fn test_sort_entries() {
    let backend = InMemory::new();

    // Create a simple tree with mixed order
    let root = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let root_id = root.id();

    let entry_a = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .build()
        .expect("Entry should build successfully");
    let id_a = entry_a.id();

    let entry_b = Entry::builder(root_id.clone())
        .add_parent(id_a.clone())
        .build()
        .expect("Entry should build successfully");
    let id_b = entry_b.id();

    let entry_c = Entry::builder(root_id.clone())
        .add_parent(id_b.clone())
        .build()
        .expect("Entry should build successfully");

    // Store all entries in backend
    backend.put_verified(root.clone()).unwrap();
    backend.put_verified(entry_a.clone()).unwrap();
    backend.put_verified(entry_b.clone()).unwrap();
    backend.put_verified(entry_c.clone()).unwrap();

    // Create a vector with entries in random order
    let mut entries = vec![
        entry_c.clone(),
        root.clone(),
        entry_b.clone(),
        entry_a.clone(),
    ];

    // Sort the entries
    backend
        .sort_entries_by_height(&root_id, &mut entries)
        .unwrap();

    // Check the sorted order: root, A, B, C (by height)
    assert_eq!(entries[0].id(), root_id);
    assert_eq!(entries[1].id(), id_a);
    assert_eq!(entries[2].id(), id_b);
    assert_eq!(entries[3].id(), entry_c.id());

    // Test with an empty vector (should not panic)
    let mut empty_entries = Vec::new();
    backend
        .sort_entries_by_height(&root_id, &mut empty_entries)
        .unwrap();
    assert!(empty_entries.is_empty());
}
