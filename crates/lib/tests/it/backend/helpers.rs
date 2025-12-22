use eidetica::{
    backend::BackendImpl,
    entry::{Entry, ID},
};

// Re-export test_backend from parent helpers for backend selection
pub use crate::helpers::test_backend;

/// Create a test backend with a root entry already stored
/// Returns (backend, root_id)
pub async fn create_test_backend_with_root() -> (Box<dyn BackendImpl>, ID) {
    let backend = test_backend().await;
    let root_id = create_and_store_root(&*backend).await;
    (backend, root_id)
}

/// Create and store a root entry in the backend
/// Returns the root entry ID
pub async fn create_and_store_root(backend: &dyn BackendImpl) -> ID {
    let root_entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let root_id = root_entry.id();
    backend.put_verified(root_entry).await.unwrap();
    root_id
}

/// Create and store a child entry with specified parent
/// Returns the child entry ID
pub async fn create_and_store_child(backend: &dyn BackendImpl, tree_id: &ID, parent_id: &ID) -> ID {
    let entry = Entry::builder(tree_id.clone())
        .add_parent(parent_id.clone())
        .build()
        .expect("Child entry should build successfully");
    let id = entry.id();
    backend.put_verified(entry).await.unwrap();
    id
}

/// Create and store an entry with subtree data and parent
/// Returns the entry ID
pub async fn create_and_store_subtree_entry(
    backend: &dyn BackendImpl,
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
    backend.put_verified(entry).await.unwrap();
    id
}

/// Create a linear chain of entries
/// Returns vector of entry IDs in order (excluding root)
pub async fn create_linear_chain(
    backend: &dyn BackendImpl,
    tree_id: &ID,
    root_id: &ID,
    chain_length: usize,
) -> Vec<ID> {
    let mut ids = Vec::new();
    let mut parent_id = root_id.clone();

    for _ in 0..chain_length {
        let child_id = create_and_store_child(backend, tree_id, &parent_id).await;
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
pub async fn assert_single_tip(backend: &dyn BackendImpl, tree_id: &ID, expected_tip: &ID) {
    let tips = backend.get_tips(tree_id).await.unwrap();
    assert_eq!(tips.len(), 1, "Expected exactly one tip");
    assert_eq!(tips[0], *expected_tip, "Tip ID doesn't match expected");
}

/// Assert that a tree contains the specified entry IDs
pub async fn assert_tree_contains_ids(
    backend: &dyn BackendImpl,
    tree_id: &ID,
    expected_ids: &[&ID],
) {
    let tree = backend.get_tree(tree_id).await.unwrap();
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
