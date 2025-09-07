use eidetica::{
    backend::{BackendDB, database::InMemory},
    entry::{Entry, ID},
};

use super::helpers::*;

#[test]
fn test_in_memory_backend_tree_operations() {
    let (backend, root_id) = create_test_backend_with_root();

    // Create a linear chain: root -> child1 -> child2
    let chain_ids = create_linear_chain(&backend, &root_id, &root_id, 2);
    let child1_id = &chain_ids[0];
    let child2_id = &chain_ids[1];

    // Test that the tip is the last entry in the chain
    assert_single_tip(&backend, &root_id, child2_id);

    // Test that the tree contains all expected entries
    assert_tree_contains_ids(&backend, &root_id, &[&root_id, child1_id, child2_id]);
}

#[test]
fn test_in_memory_backend_complex_tree_structure() {
    let (backend, root_id) = create_test_backend_with_root();

    // Create a diamond pattern: root -> A, B -> C
    // Add subtree data to distinguish the branches
    let diamond = {
        let a_id = create_and_store_subtree_entry(&backend, &root_id, &root_id, "branch", "a");
        let b_id = create_and_store_subtree_entry(&backend, &root_id, &root_id, "branch", "b");

        // Create merge entry with both parents
        let c_entry = Entry::builder(root_id.clone())
            .add_parent(a_id.clone())
            .add_parent(b_id.clone())
            .set_subtree_data("branch", "c")
            .build();
        let c_id = c_entry.id();
        backend.put_verified(c_entry).unwrap();

        DiamondStructure {
            root_id: root_id.clone(),
            left_id: a_id,
            right_id: b_id,
            merge_id: c_id,
        }
    };

    // Test that C is the only tip
    assert_single_tip(&backend, &root_id, &diamond.merge_id);

    // Test that the tree contains all expected entries
    assert_tree_contains_ids(
        &backend,
        &root_id,
        &[
            &diamond.root_id,
            &diamond.left_id,
            &diamond.right_id,
            &diamond.merge_id,
        ],
    );

    // Extend the diamond by adding D which has C as a parent
    let d_id = create_and_store_child(&backend, &root_id, &diamond.merge_id);

    // Tips should now be D (the latest entry)
    assert_single_tip(&backend, &root_id, &d_id);
}

#[test]
fn test_backend_get_tree_from_tips() {
    let backend = InMemory::new();
    let root_id: ID = "tree_root".into();

    // Create entries: root -> e1 -> e2a, e2b
    let root_entry = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .build();
    let root_entry_id = root_entry.id();
    backend.put_verified(root_entry).unwrap();

    let e1_entry = Entry::builder(root_id.clone())
        .add_parent(root_entry_id.clone())
        .build();
    let e1_id = e1_entry.id();
    backend.put_verified(e1_entry).unwrap();

    let e2a_entry = Entry::builder(root_id.clone())
        .add_parent(e1_id.clone())
        .set_subtree_data("branch", "a")
        .build();
    let e2a_id = e2a_entry.id();
    backend.put_verified(e2a_entry).unwrap();

    let e2b_entry = Entry::builder(root_id.clone())
        .add_parent(e1_id.clone())
        .set_subtree_data("branch", "b")
        .build();
    let e2b_id = e2b_entry.id();
    backend.put_verified(e2b_entry).unwrap();

    // --- Test with single tip e2a ---
    let tree_e2a = backend
        .get_tree_from_tips(&root_id, std::slice::from_ref(&e2a_id))
        .expect("Failed to get tree from tip e2a");
    assert_eq!(tree_e2a.len(), 3, "Tree from e2a should have root, e1, e2a");
    let ids_e2a: Vec<_> = tree_e2a.iter().map(|e| e.id()).collect();
    assert!(ids_e2a.contains(&root_entry_id));
    assert!(ids_e2a.contains(&e1_id));
    assert!(ids_e2a.contains(&e2a_id));
    assert!(!ids_e2a.contains(&e2b_id)); // Should not contain e2b

    // Verify topological order (root -> e1 -> e2a)
    assert_eq!(tree_e2a[0].id(), root_entry_id);
    assert_eq!(tree_e2a[1].id(), e1_id);
    assert_eq!(tree_e2a[2].id(), e2a_id);

    // --- Test with both tips e2a and e2b ---
    let tree_both = backend
        .get_tree_from_tips(&root_id, &[e2a_id.clone(), e2b_id.clone()])
        .expect("Failed to get tree from tips e2a, e2b");
    assert_eq!(
        tree_both.len(),
        4,
        "Tree from both tips should have all 4 entries"
    );
    let ids_both: Vec<_> = tree_both.iter().map(|e| e.id()).collect();
    assert!(ids_both.contains(&root_entry_id));
    assert!(ids_both.contains(&e1_id));
    assert!(ids_both.contains(&e2a_id));
    assert!(ids_both.contains(&e2b_id));

    // Verify topological order (root -> e1 -> {e2a, e2b})
    assert_eq!(tree_both[0].id(), root_entry_id);
    assert_eq!(tree_both[1].id(), e1_id);
    // Order of e2a and e2b might vary, check they are last two
    let last_two: Vec<_> = vec![tree_both[2].id(), tree_both[3].id()];
    assert!(last_two.contains(&e2a_id));
    assert!(last_two.contains(&e2b_id));

    // --- Test with non-existent tip ---
    let tree_bad_tip = backend
        .get_tree_from_tips(&root_id, &["bad_tip_id".into()])
        .expect("Failed to get tree with non-existent tip");
    assert!(
        tree_bad_tip.is_empty(),
        "Getting tree from non-existent tip should return empty vector"
    );

    // --- Test with non-existent tree root ---
    let bad_root_id: ID = "bad_root".into();
    let tree_bad_root = backend
        .get_tree_from_tips(&bad_root_id, std::slice::from_ref(&e1_id))
        .expect("Failed to get tree with non-existent root");
    assert!(
        tree_bad_root.is_empty(),
        "Getting tree from non-existent root should return empty vector"
    );

    // --- Test get_tree() convenience function ---
    // This function should get the full tree from current tips
    let full_tree = backend.get_tree(&root_id).expect("Failed to get full tree");
    assert_eq!(full_tree.len(), 4, "Full tree should have all 4 entries");
    let full_tree_ids: Vec<_> = full_tree.iter().map(|e| e.id()).collect();
    assert!(full_tree_ids.contains(&root_entry_id));
    assert!(full_tree_ids.contains(&e1_id));
    assert!(full_tree_ids.contains(&e2a_id));
    assert!(full_tree_ids.contains(&e2b_id));
}

#[test]
fn test_get_tips() {
    let backend = InMemory::new();

    // Create a simple tree structure:
    // Root -> A -> B
    //    \-> C

    let root = Entry::root_builder().build();
    let root_id = root.id();
    backend
        .put(
            eidetica::backend::VerificationStatus::Verified,
            root.clone(),
        )
        .unwrap();

    // Initially, root is the only tip
    let tips = backend.get_tips(&root_id).unwrap();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0], root_id);

    // Add child A
    let entry_a = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .build();
    let id_a = entry_a.id();
    backend
        .put(
            eidetica::backend::VerificationStatus::Verified,
            entry_a.clone(),
        )
        .unwrap();

    // Now A should be the only tip
    let tips = backend.get_tips(&root_id).unwrap();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0], id_a);

    // Add child B from A
    let entry_b = Entry::builder(root_id.clone())
        .add_parent(id_a.clone())
        .build();
    let id_b = entry_b.id();
    backend
        .put(
            eidetica::backend::VerificationStatus::Verified,
            entry_b.clone(),
        )
        .unwrap();

    // Now B should be the only tip from that branch
    let tips = backend.get_tips(&root_id).unwrap();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0], id_b);

    // Add child C directly from Root (creates a branch)
    let entry_c = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .build();
    let id_c = entry_c.id();
    backend
        .put(
            eidetica::backend::VerificationStatus::Verified,
            entry_c.clone(),
        )
        .unwrap();

    // Now should have 2 tips: B and C
    let tips = backend.get_tips(&root_id).unwrap();
    assert_eq!(tips.len(), 2);
    assert!(tips.contains(&id_b));
    assert!(tips.contains(&id_c));
}
