use std::collections::HashSet;

use eidetica::backend::VerificationStatus;
use eidetica::entry::{Entry, ID};

use super::helpers::test_backend;

#[tokio::test]
async fn test_backend_subtree_operations() {
    let backend = test_backend().await;

    // Create a root entry with a subtree
    let root_entry = Entry::root_builder()
        .set_subtree_data("subtree1", "root_subtree1_data")
        .build()
        .expect("Entry should build successfully");
    let root_id = root_entry.id();
    backend.put_verified(root_entry).await.unwrap();

    // Create child entry with subtree
    let child_entry = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data("subtree1", "child_subtree1_data")
        .add_subtree_parent("subtree1", root_id.clone())
        .build()
        .expect("Entry should build successfully");
    let child_id = child_entry.id();
    backend.put_verified(child_entry).await.unwrap();

    // Test get_store_tips
    let subtree_tips_result = backend.get_store_tips(&root_id, "subtree1").await;
    assert!(subtree_tips_result.is_ok());
    let subtree_tips = subtree_tips_result.unwrap();
    assert_eq!(subtree_tips.len(), 1);
    assert_eq!(subtree_tips[0], child_id);

    // Test get_subtree
    let subtree_result = backend.get_store(&root_id, "subtree1").await;
    assert!(subtree_result.is_ok());
    let subtree = subtree_result.unwrap();
    assert_eq!(subtree.len(), 2); // root + child
}

#[tokio::test]
async fn test_backend_get_store_from_tips() {
    let backend = test_backend().await;
    let subtree_name = "my_subtree";

    // Create entries: root -> e1 -> e2a, e2b
    // root: has subtree (subtree height 0)
    // e1: no subtree
    // e2a: has subtree (subtree height 1)
    // e2b: has subtree (subtree height 1)

    let entry_root = Entry::root_builder()
        .set_subtree_data(subtree_name, "root_sub_data")
        .set_subtree_height(subtree_name, Some(0)) // Subtree root
        .build()
        .expect("Entry should build successfully");
    let root_entry_id = entry_root.id();
    backend.put_verified(entry_root).await.unwrap();

    let e1 = Entry::builder(root_entry_id.clone())
        .add_parent(root_entry_id.clone())
        .set_height(1) // Tree height
        .build()
        .expect("Entry should build successfully");
    let e1_id = e1.id();
    backend.put_verified(e1).await.unwrap();

    let e2a = Entry::builder(root_entry_id.clone())
        .add_parent(e1_id.clone())
        .set_height(2) // Tree height
        .set_subtree_data(subtree_name, "e2a_sub_data")
        .add_subtree_parent(subtree_name, root_entry_id.clone())
        .set_subtree_height(subtree_name, Some(1)) // Subtree height (child of subtree root)
        .build()
        .expect("Entry should build successfully");
    let e2a_id = e2a.id();
    backend.put_verified(e2a).await.unwrap();

    let e2b = Entry::builder(root_entry_id.clone())
        .add_parent(e1_id.clone())
        .set_height(2) // Tree height
        .set_subtree_data(subtree_name, "e2b_sub_data")
        .add_subtree_parent(subtree_name, root_entry_id.clone())
        .set_subtree_height(subtree_name, Some(1)) // Subtree height (child of subtree root)
        .build()
        .expect("Entry should build successfully");
    let e2b_id = e2b.id();
    backend.put_verified(e2b).await.unwrap();

    // --- Test with single tip e2a ---
    let subtree_e2a = backend
        .get_store_from_tips(&root_entry_id, subtree_name, std::slice::from_ref(&e2a_id))
        .await
        .expect("Failed to get subtree from tip e2a");
    // Should contain root and e2a (which have the subtree), but not e1 (no subtree) or e2b (not in history of tip e2a)
    assert_eq!(
        subtree_e2a.len(),
        2,
        "Subtree from e2a should have root, e2a"
    );
    let ids_e2a: Vec<_> = subtree_e2a.iter().map(|e| e.id()).collect();
    assert!(ids_e2a.contains(&root_entry_id));
    assert!(!ids_e2a.contains(&e1_id)); // e1 doesn't have the subtree
    assert!(ids_e2a.contains(&e2a_id));
    assert!(!ids_e2a.contains(&e2b_id)); // e2b is not an ancestor of e2a

    // Verify topological order (root -> e2a)
    assert_eq!(subtree_e2a[0].id(), root_entry_id);
    assert_eq!(subtree_e2a[1].id(), e2a_id);

    // --- Test with both tips e2a and e2b ---
    let subtree_both = backend
        .get_store_from_tips(
            &root_entry_id,
            subtree_name,
            &[e2a_id.clone(), e2b_id.clone()],
        )
        .await
        .expect("Failed to get subtree from tips e2a, e2b");
    // Should contain root, e2a, e2b (all have the subtree)
    assert_eq!(
        subtree_both.len(),
        3,
        "Subtree from both tips should have root, e2a, e2b"
    );
    let ids_both: Vec<_> = subtree_both.iter().map(|e| e.id()).collect();
    assert!(ids_both.contains(&root_entry_id));
    assert!(!ids_both.contains(&e1_id));
    assert!(ids_both.contains(&e2a_id));
    assert!(ids_both.contains(&e2b_id));

    // Verify topological order (root -> {e2a, e2b})
    assert_eq!(subtree_both[0].id(), root_entry_id);
    let last_two: Vec<_> = vec![subtree_both[1].id(), subtree_both[2].id()];
    assert!(last_two.contains(&e2a_id));
    assert!(last_two.contains(&e2b_id));

    // --- Test with non-existent subtree name ---
    // When given a tip that exists but doesn't have the specified store,
    // the result should be empty.
    let subtree_bad_name = backend
        .get_store_from_tips(&root_entry_id, "bad_name", std::slice::from_ref(&e2a_id))
        .await
        .expect("Getting subtree with bad name should succeed");
    assert!(
        subtree_bad_name.is_empty(),
        "Getting subtree with non-existent store name should return empty vector"
    );

    // --- Test with non-existent tip ---
    let subtree_bad_tip = backend
        .get_store_from_tips(&root_entry_id, subtree_name, &["bad_tip_id".into()])
        .await
        .expect("Failed to get subtree with non-existent tip");
    assert!(
        subtree_bad_tip.is_empty(),
        "Getting subtree from non-existent tip should return empty list"
    );

    // --- Test with non-existent tree root ---
    // When given a valid tip but an invalid root (tree_id doesn't match),
    // the result should be empty because the tip doesn't belong to the specified tree.
    let bad_root_id_2: ID = "bad_root".into();
    let subtree_bad_root = backend
        .get_store_from_tips(&bad_root_id_2, subtree_name, std::slice::from_ref(&e1_id))
        .await
        .expect("Failed to get subtree with non-existent root");
    assert!(
        subtree_bad_root.is_empty(),
        "Getting subtree from tip with mismatched root should return empty vector"
    );

    // --- Test get_subtree() convenience function ---
    // This function should get the full subtree from current tips
    let full_subtree = backend
        .get_store(&root_entry_id, subtree_name)
        .await
        .expect("Failed to get full subtree");
    assert_eq!(
        full_subtree.len(),
        3,
        "Full subtree should have root, e2a, e2b"
    );
    let full_subtree_ids: Vec<_> = full_subtree.iter().map(|e| e.id()).collect();
    assert!(full_subtree_ids.contains(&root_entry_id));
    assert!(!full_subtree_ids.contains(&e1_id)); // e1 doesn't have the subtree
    assert!(full_subtree_ids.contains(&e2a_id));
    assert!(full_subtree_ids.contains(&e2b_id));
}

#[tokio::test]
async fn test_get_store_tips() {
    let backend = test_backend().await;

    // Create a tree with subtrees
    let root = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let root_id = root.id();
    backend
        .put(VerificationStatus::Verified, root.clone())
        .await
        .unwrap();

    // Add entry A with subtree "sub1"
    let entry_a = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data("sub1", "A sub1 data")
        .build()
        .expect("Entry should build successfully");
    let id_a = entry_a.id();
    backend.put_verified(entry_a).await.unwrap();

    // Initially, A is the only tip in subtree "sub1"
    let sub1_tips = backend.get_store_tips(&root_id, "sub1").await.unwrap();
    assert_eq!(sub1_tips.len(), 1);
    assert_eq!(sub1_tips[0], id_a);

    // Add entry B with subtree "sub1" as child of A
    let entry_b = Entry::builder(root_id.clone())
        .add_parent(id_a.clone())
        .set_subtree_data("sub1", "B sub1 data")
        .add_subtree_parent("sub1", id_a.clone())
        .build()
        .expect("Entry should build successfully");
    let id_b = entry_b.id();
    backend.put_verified(entry_b).await.unwrap();

    // Now B is the only tip in subtree "sub1"
    let sub1_tips = backend.get_store_tips(&root_id, "sub1").await.unwrap();
    assert_eq!(sub1_tips.len(), 1);
    assert_eq!(sub1_tips[0], id_b);

    // Add entry C with subtree "sub2" (different subtree)
    let entry_c = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data("sub2", "C sub2 data")
        .build()
        .expect("Entry should build successfully");
    let id_c = entry_c.id();
    backend.put_verified(entry_c).await.unwrap();

    // Check tips for subtree "sub1" (should still be just B)
    let sub1_tips = backend.get_store_tips(&root_id, "sub1").await.unwrap();
    assert_eq!(sub1_tips.len(), 1);
    assert_eq!(sub1_tips[0], id_b);

    // Check tips for subtree "sub2" (should be just C)
    let sub2_tips = backend.get_store_tips(&root_id, "sub2").await.unwrap();
    assert_eq!(sub2_tips.len(), 1);
    assert_eq!(sub2_tips[0], id_c);

    // Add entry D with both subtrees "sub1" and "sub2"
    let entry_d = Entry::builder(root_id.clone())
        .add_parent(id_b.clone())
        .add_parent(id_c.clone())
        .set_subtree_data("sub1", "D sub1 data")
        .add_subtree_parent("sub1", id_b.clone())
        .set_subtree_data("sub2", "D sub2 data")
        .add_subtree_parent("sub2", id_c.clone())
        .build()
        .expect("Entry should build successfully");
    let id_d = entry_d.id();
    backend.put_verified(entry_d).await.unwrap();

    // Now D should be the tip for both subtrees
    let sub1_tips = backend.get_store_tips(&root_id, "sub1").await.unwrap();
    assert_eq!(sub1_tips.len(), 1);
    assert_eq!(sub1_tips[0], id_d);

    let sub2_tips = backend.get_store_tips(&root_id, "sub2").await.unwrap();
    assert_eq!(sub2_tips.len(), 1);
    assert_eq!(sub2_tips[0], id_d);
}

// ============================================================================
// Slow Path Tests for get_store_tips_up_to_entries
// ============================================================================
//
// These tests specifically exercise the "slow path" of get_store_tips_up_to_entries,
// which is triggered when `main_entries` doesn't match the current tree tips.
// This happens when querying historical tips at specific points in history.

/// Test the slow path with a linear chain, querying tips at historical points.
///
/// Structure:
/// ```
/// root -> A -> B -> C -> D
/// ```
/// Each entry is in subtree "sub1".
/// Test queries tips at: {A}, {B}, {C}, and {A, B} to verify slow path correctness.
#[tokio::test]
async fn test_get_store_tips_up_to_entries_linear_chain() {
    let backend = test_backend().await;
    let subtree = "sub1";

    // Build linear chain: root -> A -> B -> C -> D
    let root = Entry::root_builder()
        .set_subtree_data(subtree, "root_data")
        .build()
        .unwrap();
    let root_id = root.id();
    backend.put_verified(root).await.unwrap();

    let entry_a = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data(subtree, "a_data")
        .add_subtree_parent(subtree, root_id.clone())
        .build()
        .unwrap();
    let id_a = entry_a.id();
    backend.put_verified(entry_a).await.unwrap();

    let entry_b = Entry::builder(root_id.clone())
        .add_parent(id_a.clone())
        .set_subtree_data(subtree, "b_data")
        .add_subtree_parent(subtree, id_a.clone())
        .build()
        .unwrap();
    let id_b = entry_b.id();
    backend.put_verified(entry_b).await.unwrap();

    let entry_c = Entry::builder(root_id.clone())
        .add_parent(id_b.clone())
        .set_subtree_data(subtree, "c_data")
        .add_subtree_parent(subtree, id_b.clone())
        .build()
        .unwrap();
    let id_c = entry_c.id();
    backend.put_verified(entry_c).await.unwrap();

    let entry_d = Entry::builder(root_id.clone())
        .add_parent(id_c.clone())
        .set_subtree_data(subtree, "d_data")
        .add_subtree_parent(subtree, id_c.clone())
        .build()
        .unwrap();
    let id_d = entry_d.id();
    backend.put_verified(entry_d).await.unwrap();

    // Verify current tips (fast path) - should be D
    let current_tips = backend.get_store_tips(&root_id, subtree).await.unwrap();
    assert_eq!(current_tips.len(), 1);
    assert_eq!(current_tips[0], id_d);

    // --- Slow path tests: query historical tips ---

    // Query tips up to {A} - should return A as the tip
    let tips_at_a = backend
        .get_store_tips_up_to_entries(&root_id, subtree, std::slice::from_ref(&id_a))
        .await
        .unwrap();
    assert_eq!(tips_at_a.len(), 1, "Tips at A should have 1 entry");
    assert_eq!(tips_at_a[0], id_a, "Tip at A should be A");

    // Query tips up to {B} - should return B as the tip
    let tips_at_b = backend
        .get_store_tips_up_to_entries(&root_id, subtree, std::slice::from_ref(&id_b))
        .await
        .unwrap();
    assert_eq!(tips_at_b.len(), 1, "Tips at B should have 1 entry");
    assert_eq!(tips_at_b[0], id_b, "Tip at B should be B");

    // Query tips up to {C} - should return C as the tip
    let tips_at_c = backend
        .get_store_tips_up_to_entries(&root_id, subtree, std::slice::from_ref(&id_c))
        .await
        .unwrap();
    assert_eq!(tips_at_c.len(), 1, "Tips at C should have 1 entry");
    assert_eq!(tips_at_c[0], id_c, "Tip at C should be C");

    // Query tips up to {root} - should return root as the tip
    let tips_at_root = backend
        .get_store_tips_up_to_entries(&root_id, subtree, std::slice::from_ref(&root_id))
        .await
        .unwrap();
    assert_eq!(tips_at_root.len(), 1, "Tips at root should have 1 entry");
    assert_eq!(tips_at_root[0], root_id, "Tip at root should be root");

    // Query tips up to {A, B} - should return B (A is ancestor of B)
    let tips_at_ab = backend
        .get_store_tips_up_to_entries(&root_id, subtree, &[id_a.clone(), id_b.clone()])
        .await
        .unwrap();
    assert_eq!(tips_at_ab.len(), 1, "Tips at {{A, B}} should have 1 entry");
    assert_eq!(tips_at_ab[0], id_b, "Tip at {{A, B}} should be B");
}

/// Test the slow path with a diamond pattern (fork and merge).
///
/// Structure:
/// ```
///        root
///       /    \
///      A      B
///       \    /
///         C
/// ```
/// All entries are in subtree "sub1".
/// Test queries at different stages of the diamond.
#[tokio::test]
async fn test_get_store_tips_up_to_entries_diamond_pattern() {
    let backend = test_backend().await;
    let subtree = "sub1";

    // Build diamond: root -> A, B -> C
    let root = Entry::root_builder()
        .set_subtree_data(subtree, "root_data")
        .build()
        .unwrap();
    let root_id = root.id();
    backend.put_verified(root).await.unwrap();

    let entry_a = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data(subtree, "a_data")
        .add_subtree_parent(subtree, root_id.clone())
        .build()
        .unwrap();
    let id_a = entry_a.id();
    backend.put_verified(entry_a).await.unwrap();

    let entry_b = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data(subtree, "b_data")
        .add_subtree_parent(subtree, root_id.clone())
        .build()
        .unwrap();
    let id_b = entry_b.id();
    backend.put_verified(entry_b).await.unwrap();

    let entry_c = Entry::builder(root_id.clone())
        .add_parent(id_a.clone())
        .add_parent(id_b.clone())
        .set_subtree_data(subtree, "c_data")
        .add_subtree_parent(subtree, id_a.clone())
        .add_subtree_parent(subtree, id_b.clone())
        .build()
        .unwrap();
    let id_c = entry_c.id();
    backend.put_verified(entry_c).await.unwrap();

    // Verify current tips (fast path) - should be C
    let current_tips = backend.get_store_tips(&root_id, subtree).await.unwrap();
    assert_eq!(current_tips.len(), 1);
    assert_eq!(current_tips[0], id_c);

    // --- Slow path tests ---

    // Query tips up to {A} - should return A
    let tips_at_a = backend
        .get_store_tips_up_to_entries(&root_id, subtree, std::slice::from_ref(&id_a))
        .await
        .unwrap();
    assert_eq!(tips_at_a.len(), 1);
    assert_eq!(tips_at_a[0], id_a);

    // Query tips up to {B} - should return B
    let tips_at_b = backend
        .get_store_tips_up_to_entries(&root_id, subtree, std::slice::from_ref(&id_b))
        .await
        .unwrap();
    assert_eq!(tips_at_b.len(), 1);
    assert_eq!(tips_at_b[0], id_b);

    // Query tips up to {A, B} - should return BOTH A and B (neither is ancestor of the other)
    let tips_at_ab = backend
        .get_store_tips_up_to_entries(&root_id, subtree, &[id_a.clone(), id_b.clone()])
        .await
        .unwrap();
    let tips_set: HashSet<_> = tips_at_ab.iter().collect();
    assert_eq!(
        tips_at_ab.len(),
        2,
        "Tips at {{A, B}} should have 2 entries"
    );
    assert!(
        tips_set.contains(&id_a),
        "Tips at {{A, B}} should contain A"
    );
    assert!(
        tips_set.contains(&id_b),
        "Tips at {{A, B}} should contain B"
    );

    // Query tips up to {root} - should return root
    let tips_at_root = backend
        .get_store_tips_up_to_entries(&root_id, subtree, std::slice::from_ref(&root_id))
        .await
        .unwrap();
    assert_eq!(tips_at_root.len(), 1);
    assert_eq!(tips_at_root[0], root_id);
}

/// Test the slow path with multiple subtrees having different histories.
///
/// Structure (tree):
/// ```
///      root
///     /    \
///    A      B
///    |      |
///    C      D
/// ```
///
/// Subtree memberships:
/// - "sub1": root -> A -> C
/// - "sub2": root -> B -> D
///
/// Test that historical tips for each subtree are computed correctly.
#[tokio::test]
async fn test_get_store_tips_up_to_entries_multiple_subtrees() {
    let backend = test_backend().await;

    // Build tree with two parallel branches
    let root = Entry::root_builder()
        .set_subtree_data("sub1", "root_sub1")
        .set_subtree_data("sub2", "root_sub2")
        .build()
        .unwrap();
    let root_id = root.id();
    backend.put_verified(root).await.unwrap();

    // Branch 1: root -> A -> C (sub1 only)
    let entry_a = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data("sub1", "a_sub1")
        .add_subtree_parent("sub1", root_id.clone())
        .build()
        .unwrap();
    let id_a = entry_a.id();
    backend.put_verified(entry_a).await.unwrap();

    let entry_c = Entry::builder(root_id.clone())
        .add_parent(id_a.clone())
        .set_subtree_data("sub1", "c_sub1")
        .add_subtree_parent("sub1", id_a.clone())
        .build()
        .unwrap();
    let id_c = entry_c.id();
    backend.put_verified(entry_c).await.unwrap();

    // Branch 2: root -> B -> D (sub2 only)
    let entry_b = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data("sub2", "b_sub2")
        .add_subtree_parent("sub2", root_id.clone())
        .build()
        .unwrap();
    let id_b = entry_b.id();
    backend.put_verified(entry_b).await.unwrap();

    let entry_d = Entry::builder(root_id.clone())
        .add_parent(id_b.clone())
        .set_subtree_data("sub2", "d_sub2")
        .add_subtree_parent("sub2", id_b.clone())
        .build()
        .unwrap();
    let id_d = entry_d.id();
    backend.put_verified(entry_d).await.unwrap();

    // Current tree tips are C and D (parallel branches)
    let tree_tips = backend.get_tips(&root_id).await.unwrap();
    let tree_tips_set: HashSet<_> = tree_tips.iter().collect();
    assert_eq!(tree_tips.len(), 2);
    assert!(tree_tips_set.contains(&id_c));
    assert!(tree_tips_set.contains(&id_d));

    // --- Query historical tips for sub1 at different tree tips ---

    // Query sub1 tips up to {A} - should return A
    let sub1_tips_at_a = backend
        .get_store_tips_up_to_entries(&root_id, "sub1", std::slice::from_ref(&id_a))
        .await
        .unwrap();
    assert_eq!(sub1_tips_at_a.len(), 1);
    assert_eq!(sub1_tips_at_a[0], id_a);

    // Query sub1 tips up to {C} - should return C
    let sub1_tips_at_c = backend
        .get_store_tips_up_to_entries(&root_id, "sub1", std::slice::from_ref(&id_c))
        .await
        .unwrap();
    assert_eq!(sub1_tips_at_c.len(), 1);
    assert_eq!(sub1_tips_at_c[0], id_c);

    // Query sub1 tips up to {B} - B is not in sub1, so only root is reachable
    let sub1_tips_at_b = backend
        .get_store_tips_up_to_entries(&root_id, "sub1", std::slice::from_ref(&id_b))
        .await
        .unwrap();
    assert_eq!(
        sub1_tips_at_b.len(),
        1,
        "sub1 tips at B should have 1 entry (root)"
    );
    assert_eq!(sub1_tips_at_b[0], root_id, "sub1 tip at B should be root");

    // --- Query historical tips for sub2 at different tree tips ---

    // Query sub2 tips up to {B} - should return B
    let sub2_tips_at_b = backend
        .get_store_tips_up_to_entries(&root_id, "sub2", std::slice::from_ref(&id_b))
        .await
        .unwrap();
    assert_eq!(sub2_tips_at_b.len(), 1);
    assert_eq!(sub2_tips_at_b[0], id_b);

    // Query sub2 tips up to {D} - should return D
    let sub2_tips_at_d = backend
        .get_store_tips_up_to_entries(&root_id, "sub2", std::slice::from_ref(&id_d))
        .await
        .unwrap();
    assert_eq!(sub2_tips_at_d.len(), 1);
    assert_eq!(sub2_tips_at_d[0], id_d);

    // Query sub2 tips up to {A} - A is not in sub2, so only root is reachable
    let sub2_tips_at_a = backend
        .get_store_tips_up_to_entries(&root_id, "sub2", std::slice::from_ref(&id_a))
        .await
        .unwrap();
    assert_eq!(
        sub2_tips_at_a.len(),
        1,
        "sub2 tips at A should have 1 entry (root)"
    );
    assert_eq!(sub2_tips_at_a[0], root_id, "sub2 tip at A should be root");

    // Query sub1 tips up to {C, D} (both current tree tips)
    // Only C is in sub1, D is not, so tip should be C
    let sub1_tips_at_cd = backend
        .get_store_tips_up_to_entries(&root_id, "sub1", &[id_c.clone(), id_d.clone()])
        .await
        .unwrap();
    assert_eq!(sub1_tips_at_cd.len(), 1);
    assert_eq!(sub1_tips_at_cd[0], id_c);
}

/// Test edge cases for get_store_tips_up_to_entries slow path.
#[tokio::test]
async fn test_get_store_tips_up_to_entries_edge_cases() {
    let backend = test_backend().await;
    let subtree = "sub1";

    // Build simple chain: root -> A -> B
    let root = Entry::root_builder()
        .set_subtree_data(subtree, "root_data")
        .build()
        .unwrap();
    let root_id = root.id();
    backend.put_verified(root).await.unwrap();

    let entry_a = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data(subtree, "a_data")
        .add_subtree_parent(subtree, root_id.clone())
        .build()
        .unwrap();
    let id_a = entry_a.id();
    backend.put_verified(entry_a).await.unwrap();

    let entry_b = Entry::builder(root_id.clone())
        .add_parent(id_a.clone())
        .set_subtree_data(subtree, "b_data")
        .add_subtree_parent(subtree, id_a.clone())
        .build()
        .unwrap();
    backend.put_verified(entry_b).await.unwrap();

    // --- Edge case: empty main_entries ---
    let tips_empty = backend
        .get_store_tips_up_to_entries(&root_id, subtree, &[])
        .await
        .unwrap();
    assert!(
        tips_empty.is_empty(),
        "Tips with empty main_entries should be empty"
    );

    // --- Edge case: non-existent entry ID ---
    // Backends may either return an error or an empty result for non-existent entries
    let fake_id: ID = "nonexistent_entry_12345".into();
    let result = backend
        .get_store_tips_up_to_entries(&root_id, subtree, &[fake_id])
        .await;
    match result {
        Err(_) => {} // InMemory backend returns error
        Ok(tips) => assert!(
            tips.is_empty(),
            "Tips with non-existent entry should be empty"
        ),
    }

    // --- Edge case: non-existent subtree name returns empty ---
    let tips_bad_subtree = backend
        .get_store_tips_up_to_entries(&root_id, "nonexistent_subtree", std::slice::from_ref(&id_a))
        .await
        .unwrap();
    assert!(
        tips_bad_subtree.is_empty(),
        "Tips for non-existent subtree should be empty"
    );
}

/// Test the slow path with a complex DAG structure.
///
/// Structure:
/// ```
///           root
///          / | \
///         A  B  C
///        / \   / \
///       D   E F   G
///        \ /   \ /
///         H     I
/// ```
///
/// All entries are in subtree "sub1".
/// Tests various historical tip queries on this complex structure.
#[tokio::test]
async fn test_get_store_tips_up_to_entries_complex_dag() {
    let backend = test_backend().await;
    let subtree = "sub1";

    // Build the complex DAG
    let root = Entry::root_builder()
        .set_subtree_data(subtree, "root_data")
        .build()
        .unwrap();
    let root_id = root.id();
    backend.put_verified(root).await.unwrap();

    // Level 1: A, B, C
    let entry_a = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data(subtree, "a_data")
        .add_subtree_parent(subtree, root_id.clone())
        .build()
        .unwrap();
    let id_a = entry_a.id();
    backend.put_verified(entry_a).await.unwrap();

    let entry_b = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data(subtree, "b_data")
        .add_subtree_parent(subtree, root_id.clone())
        .build()
        .unwrap();
    let id_b = entry_b.id();
    backend.put_verified(entry_b).await.unwrap();

    let entry_c = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data(subtree, "c_data")
        .add_subtree_parent(subtree, root_id.clone())
        .build()
        .unwrap();
    let id_c = entry_c.id();
    backend.put_verified(entry_c).await.unwrap();

    // Level 2: D (child of A), E (child of A, B), F (child of B, C), G (child of C)
    let entry_d = Entry::builder(root_id.clone())
        .add_parent(id_a.clone())
        .set_subtree_data(subtree, "d_data")
        .add_subtree_parent(subtree, id_a.clone())
        .build()
        .unwrap();
    let id_d = entry_d.id();
    backend.put_verified(entry_d).await.unwrap();

    let entry_e = Entry::builder(root_id.clone())
        .add_parent(id_a.clone())
        .add_parent(id_b.clone())
        .set_subtree_data(subtree, "e_data")
        .add_subtree_parent(subtree, id_a.clone())
        .add_subtree_parent(subtree, id_b.clone())
        .build()
        .unwrap();
    let id_e = entry_e.id();
    backend.put_verified(entry_e).await.unwrap();

    let entry_f = Entry::builder(root_id.clone())
        .add_parent(id_b.clone())
        .add_parent(id_c.clone())
        .set_subtree_data(subtree, "f_data")
        .add_subtree_parent(subtree, id_b.clone())
        .add_subtree_parent(subtree, id_c.clone())
        .build()
        .unwrap();
    let id_f = entry_f.id();
    backend.put_verified(entry_f).await.unwrap();

    let entry_g = Entry::builder(root_id.clone())
        .add_parent(id_c.clone())
        .set_subtree_data(subtree, "g_data")
        .add_subtree_parent(subtree, id_c.clone())
        .build()
        .unwrap();
    let id_g = entry_g.id();
    backend.put_verified(entry_g).await.unwrap();

    // Level 3: H (child of D, E), I (child of F, G)
    let entry_h = Entry::builder(root_id.clone())
        .add_parent(id_d.clone())
        .add_parent(id_e.clone())
        .set_subtree_data(subtree, "h_data")
        .add_subtree_parent(subtree, id_d.clone())
        .add_subtree_parent(subtree, id_e.clone())
        .build()
        .unwrap();
    let id_h = entry_h.id();
    backend.put_verified(entry_h).await.unwrap();

    let entry_i = Entry::builder(root_id.clone())
        .add_parent(id_f.clone())
        .add_parent(id_g.clone())
        .set_subtree_data(subtree, "i_data")
        .add_subtree_parent(subtree, id_f.clone())
        .add_subtree_parent(subtree, id_g.clone())
        .build()
        .unwrap();
    let id_i = entry_i.id();
    backend.put_verified(entry_i).await.unwrap();

    // Current tips should be H and I
    let current_tips = backend.get_store_tips(&root_id, subtree).await.unwrap();
    let current_tips_set: HashSet<_> = current_tips.iter().collect();
    assert_eq!(current_tips.len(), 2);
    assert!(current_tips_set.contains(&id_h));
    assert!(current_tips_set.contains(&id_i));

    // --- Slow path tests ---

    // Query tips up to {A, B, C} - should return A, B, C
    let tips_abc = backend
        .get_store_tips_up_to_entries(
            &root_id,
            subtree,
            &[id_a.clone(), id_b.clone(), id_c.clone()],
        )
        .await
        .unwrap();
    let tips_abc_set: HashSet<_> = tips_abc.iter().collect();
    assert_eq!(
        tips_abc.len(),
        3,
        "Tips at {{A, B, C}} should have 3 entries"
    );
    assert!(tips_abc_set.contains(&id_a));
    assert!(tips_abc_set.contains(&id_b));
    assert!(tips_abc_set.contains(&id_c));

    // Query tips up to {D, E, F, G} - all four should be tips
    let tips_defg = backend
        .get_store_tips_up_to_entries(
            &root_id,
            subtree,
            &[id_d.clone(), id_e.clone(), id_f.clone(), id_g.clone()],
        )
        .await
        .unwrap();
    let tips_defg_set: HashSet<_> = tips_defg.iter().collect();
    assert_eq!(
        tips_defg.len(),
        4,
        "Tips at {{D, E, F, G}} should have 4 entries"
    );
    assert!(tips_defg_set.contains(&id_d));
    assert!(tips_defg_set.contains(&id_e));
    assert!(tips_defg_set.contains(&id_f));
    assert!(tips_defg_set.contains(&id_g));

    // Query tips up to {A, D} - A is ancestor of D, so only D is tip
    let tips_ad = backend
        .get_store_tips_up_to_entries(&root_id, subtree, &[id_a.clone(), id_d.clone()])
        .await
        .unwrap();
    assert_eq!(tips_ad.len(), 1, "Tips at {{A, D}} should have 1 entry");
    assert_eq!(tips_ad[0], id_d, "Tip at {{A, D}} should be D");

    // Query tips up to {E} - should return E only
    let tips_e = backend
        .get_store_tips_up_to_entries(&root_id, subtree, std::slice::from_ref(&id_e))
        .await
        .unwrap();
    assert_eq!(tips_e.len(), 1);
    assert_eq!(tips_e[0], id_e);

    // Query tips up to {D, E} - both D and E are tips (neither is ancestor of the other)
    let tips_de = backend
        .get_store_tips_up_to_entries(&root_id, subtree, &[id_d.clone(), id_e.clone()])
        .await
        .unwrap();
    let tips_de_set: HashSet<_> = tips_de.iter().collect();
    assert_eq!(tips_de.len(), 2, "Tips at {{D, E}} should have 2 entries");
    assert!(tips_de_set.contains(&id_d));
    assert!(tips_de_set.contains(&id_e));

    // Query tips up to {H} - should return H only
    let tips_h = backend
        .get_store_tips_up_to_entries(&root_id, subtree, std::slice::from_ref(&id_h))
        .await
        .unwrap();
    assert_eq!(tips_h.len(), 1);
    assert_eq!(tips_h[0], id_h);

    // Query tips up to {D, E, H} - D and E are ancestors of H, so only H is tip
    let tips_deh = backend
        .get_store_tips_up_to_entries(
            &root_id,
            subtree,
            &[id_d.clone(), id_e.clone(), id_h.clone()],
        )
        .await
        .unwrap();
    assert_eq!(tips_deh.len(), 1, "Tips at {{D, E, H}} should have 1 entry");
    assert_eq!(tips_deh[0], id_h, "Tip at {{D, E, H}} should be H");

    // Query tips up to {H, I} (current tips)
    let tips_hi = backend
        .get_store_tips_up_to_entries(&root_id, subtree, &[id_h.clone(), id_i.clone()])
        .await
        .unwrap();
    let tips_hi_set: HashSet<_> = tips_hi.iter().collect();
    assert_eq!(tips_hi.len(), 2, "Tips at {{H, I}} should have 2 entries");
    assert!(tips_hi_set.contains(&id_h));
    assert!(tips_hi_set.contains(&id_i));
}
