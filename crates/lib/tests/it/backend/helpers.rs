use std::collections::HashMap;

use eidetica::{
    backend::{BackendDB, database::InMemory},
    entry::{Entry, ID},
};

/// Create a new test backend
pub fn create_test_backend() -> InMemory {
    InMemory::new()
}

/// Create a test backend with a root entry already stored
/// Returns (backend, root_id)
pub fn create_test_backend_with_root() -> (InMemory, ID) {
    let backend = create_test_backend();
    let root_id = create_and_store_root(&backend);
    (backend, root_id)
}

/// Create and store a root entry in the backend
/// Returns the root entry ID
pub fn create_and_store_root(backend: &InMemory) -> ID {
    let root_entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let root_id = root_entry.id();
    backend.put_verified(root_entry).unwrap();
    root_id
}

/// Create and store a child entry with specified parent
/// Returns the child entry ID
pub fn create_and_store_child(backend: &InMemory, tree_id: &ID, parent_id: &ID) -> ID {
    let entry = Entry::builder(tree_id.clone())
        .add_parent(parent_id.clone())
        .build()
        .expect("Child entry should build successfully");
    let id = entry.id();
    backend.put_verified(entry).unwrap();
    id
}

/// Create and store an entry with subtree data and parent
/// Returns the entry ID
pub fn create_and_store_subtree_entry(
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
        .expect("Subtree entry should build successfully");
    let id = entry.id();
    backend.put_verified(entry).unwrap();
    id
}

/// Create a linear chain of entries
/// Returns vector of entry IDs in order (excluding root)
pub fn create_linear_chain(
    backend: &InMemory,
    tree_id: &ID,
    root_id: &ID,
    chain_length: usize,
) -> Vec<ID> {
    let mut ids = Vec::new();
    let mut parent_id = root_id.clone();

    for _ in 0..chain_length {
        let child_id = create_and_store_child(backend, tree_id, &parent_id);
        ids.push(child_id.clone());
        parent_id = child_id;
    }

    ids
}

/// Structure representing a diamond DAG pattern
#[derive(Debug)]
pub struct DiamondStructure {
    pub root_id: ID,
    pub left_id: ID,
    pub right_id: ID,
    pub merge_id: ID,
}

/// Assert that a tree has a single tip with the specified ID
pub fn assert_single_tip(backend: &InMemory, tree_id: &ID, expected_tip: &ID) {
    let tips = backend.get_tips(tree_id).unwrap();
    assert_eq!(tips.len(), 1, "Expected exactly one tip");
    assert_eq!(tips[0], *expected_tip, "Tip ID doesn't match expected");
}

/// Assert that a tree contains the specified entry IDs
pub fn assert_tree_contains_ids(backend: &InMemory, tree_id: &ID, expected_ids: &[&ID]) {
    let tree = backend.get_tree(tree_id).unwrap();
    let tree_ids: Vec<ID> = tree.iter().map(|e| e.id()).collect();

    assert_eq!(
        tree.len(),
        expected_ids.len(),
        "Tree size doesn't match expected"
    );

    for expected_id in expected_ids {
        assert!(
            tree_ids.contains(expected_id),
            "Tree doesn't contain expected ID: {expected_id}"
        );
    }
}

/// Assert that an entry has the expected height
pub fn assert_entry_height(heights: &HashMap<ID, usize>, entry_id: &ID, expected_height: usize) {
    let actual_height = heights.get(entry_id).unwrap_or(&9999);
    assert_eq!(
        *actual_height, expected_height,
        "Entry {entry_id} has height {actual_height}, expected {expected_height}"
    );
}

/// Assert that multiple entries have the expected heights
pub fn assert_entry_heights(heights: &HashMap<ID, usize>, expected_heights: &[(&ID, usize)]) {
    for (entry_id, expected_height) in expected_heights {
        assert_entry_height(heights, entry_id, *expected_height);
    }
}
