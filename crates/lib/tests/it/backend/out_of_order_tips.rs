//! Tests for correct tips tracking when entries arrive out of order.
//!
//! These tests verify that the backend correctly tracks tips even when
//! entries are stored in non-topological order (child before parent),
//! which can happen during sync operations.
//!
//! Key insight: Tips are "entries with no children among the stored entries".
//! This means tips can be correct even in a partially-synced state where
//! some entries in the DAG are missing.

use eidetica::entry::Entry;

use super::helpers::test_backend;

/// Test that tree-level tips are correct when entries arrive out of order.
///
/// Scenario: A -> B -> C, but we store in order: A, C, B
/// Expected: Only C should be a tip (B has a child)
#[tokio::test]
async fn test_tree_tips_out_of_order_arrival() {
    let backend = test_backend().await;

    // Create and store root A
    let entry_a = Entry::root_builder()
        .build()
        .expect("Root entry should build");
    let id_a = entry_a.id();
    backend.put_verified(entry_a).await.unwrap();

    // Verify A is the only tip
    let tips = backend.get_tips(&id_a).await.unwrap();
    assert_eq!(tips.len(), 1, "Initially A should be the only tip");
    assert_eq!(tips[0], id_a);

    // Build entry B (parent: A) but DON'T store it yet
    let entry_b = Entry::builder(id_a.clone())
        .add_parent(id_a.clone())
        .build()
        .expect("Entry B should build");
    let id_b = entry_b.id();

    // Build and store entry C (parent: B) BEFORE storing B
    let entry_c = Entry::builder(id_a.clone())
        .add_parent(id_b.clone())
        .build()
        .expect("Entry C should build");
    let id_c = entry_c.id();
    backend.put_verified(entry_c).await.unwrap();

    // At this point, C is stored but its parent B is not
    // C should be a tip
    let tips = backend.get_tips(&id_a).await.unwrap();
    assert!(tips.contains(&id_c), "C should be a tip");

    // Now store B (the parent of C)
    backend.put_verified(entry_b).await.unwrap();

    // After storing B, only C should be a tip
    // B should NOT be a tip because C already references it as a parent
    let tips = backend.get_tips(&id_a).await.unwrap();
    assert_eq!(tips.len(), 1, "Only C should be a tip after storing B");
    assert_eq!(tips[0], id_c, "The single tip should be C, not B");
}

/// Test store-level tips with out-of-order arrival.
///
/// Same scenario but with store/subtree parents.
#[tokio::test]
async fn test_store_tips_out_of_order_arrival() {
    let backend = test_backend().await;
    let store_name = "test_store";

    // Create and store root A with store data
    let entry_a = Entry::root_builder()
        .set_subtree_data(store_name, "data_a")
        .build()
        .expect("Root entry should build");
    let id_a = entry_a.id();
    backend.put_verified(entry_a).await.unwrap();

    // Verify A is the only store tip
    let tips = backend.get_store_tips(&id_a, store_name).await.unwrap();
    assert_eq!(tips.len(), 1, "Initially A should be the only store tip");
    assert_eq!(tips[0], id_a);

    // Build entry B (store parent: A) but DON'T store it yet
    let entry_b = Entry::builder(id_a.clone())
        .add_parent(id_a.clone())
        .set_subtree_data(store_name, "data_b")
        .add_subtree_parent(store_name, id_a.clone())
        .build()
        .expect("Entry B should build");
    let id_b = entry_b.id();

    // Build and store entry C (store parent: B) BEFORE storing B
    let entry_c = Entry::builder(id_a.clone())
        .add_parent(id_b.clone())
        .set_subtree_data(store_name, "data_c")
        .add_subtree_parent(store_name, id_b.clone())
        .build()
        .expect("Entry C should build");
    let id_c = entry_c.id();
    backend.put_verified(entry_c).await.unwrap();

    // C should be a store tip
    let tips = backend.get_store_tips(&id_a, store_name).await.unwrap();
    assert!(tips.contains(&id_c), "C should be a store tip");

    // Now store B
    backend.put_verified(entry_b).await.unwrap();

    // After storing B, only C should be a store tip
    let tips = backend.get_store_tips(&id_a, store_name).await.unwrap();
    assert_eq!(
        tips.len(),
        1,
        "Only C should be a store tip after storing B"
    );
    assert_eq!(tips[0], id_c, "The single store tip should be C, not B");
}

/// Test a more complex out-of-order scenario with a diamond pattern.
///
/// DAG structure: A -> B -> D
///                A -> C -> D
///
/// Store in order: A, D, B, C
/// Expected: Only D should be a tip
#[tokio::test]
async fn test_diamond_tips_out_of_order_arrival() {
    let backend = test_backend().await;

    // Create and store root A
    let entry_a = Entry::root_builder()
        .build()
        .expect("Root entry should build");
    let id_a = entry_a.id();
    backend.put_verified(entry_a).await.unwrap();

    // Build B and C (but don't store yet)
    let entry_b = Entry::builder(id_a.clone())
        .add_parent(id_a.clone())
        .build()
        .expect("Entry B should build");
    let id_b = entry_b.id();

    let entry_c = Entry::builder(id_a.clone())
        .add_parent(id_a.clone())
        .build()
        .expect("Entry C should build");
    let id_c = entry_c.id();

    // Build and store D (merge commit with parents B and C) first
    let entry_d = Entry::builder(id_a.clone())
        .add_parent(id_b.clone())
        .add_parent(id_c.clone())
        .build()
        .expect("Entry D should build");
    let id_d = entry_d.id();
    backend.put_verified(entry_d).await.unwrap();

    // D should be a tip (its parents don't exist yet)
    let tips = backend.get_tips(&id_a).await.unwrap();
    assert!(tips.contains(&id_d), "D should be a tip");

    // Store B
    backend.put_verified(entry_b).await.unwrap();

    // B should NOT be a tip (D references it)
    let tips = backend.get_tips(&id_a).await.unwrap();
    assert!(
        !tips.contains(&id_b),
        "B should NOT be a tip (D is its child)"
    );
    assert!(tips.contains(&id_d), "D should still be a tip");

    // Store C
    backend.put_verified(entry_c).await.unwrap();

    // C should NOT be a tip (D references it)
    // Only D should be a tip
    let tips = backend.get_tips(&id_a).await.unwrap();
    assert_eq!(tips.len(), 1, "Only D should be a tip");
    assert_eq!(tips[0], id_d, "The single tip should be D");
}

/// Test partial sync state with gaps in the chain.
///
/// Full DAG structure: A -> B -> C -> D -> E
///
/// Store in order: B, E, D (A and C never arrive)
///
/// Expected tips: B and E
///
/// This represents a partially-synced state where:
/// - B has no children among stored entries (C is missing)
/// - E has no children
/// - D is not a tip because E references it
///
/// This is correct behavior! Tips represent "leaf nodes among stored entries",
/// not "leaf nodes in the complete DAG".
#[tokio::test]
async fn test_partial_sync_with_gaps() {
    let backend = test_backend().await;

    // Create a fake root to establish tree_id (we won't store A)
    let entry_a = Entry::root_builder()
        .build()
        .expect("Root entry should build");
    let id_a = entry_a.id();
    // Note: We intentionally do NOT store A

    // Build the chain A -> B -> C -> D -> E (but only store B, E, D)
    let entry_b = Entry::builder(id_a.clone())
        .add_parent(id_a.clone())
        .build()
        .expect("Entry B should build");
    let id_b = entry_b.id();

    let entry_c = Entry::builder(id_a.clone())
        .add_parent(id_b.clone())
        .build()
        .expect("Entry C should build");
    let id_c = entry_c.id();
    // Note: We intentionally do NOT store C

    let entry_d = Entry::builder(id_a.clone())
        .add_parent(id_c.clone())
        .build()
        .expect("Entry D should build");
    let id_d = entry_d.id();

    let entry_e = Entry::builder(id_a.clone())
        .add_parent(id_d.clone())
        .build()
        .expect("Entry E should build");
    let id_e = entry_e.id();

    // Store in order: B, E, D
    backend.put_verified(entry_b).await.unwrap();

    // After storing B: B is the only entry, so B is a tip
    let tips = backend.get_tips(&id_a).await.unwrap();
    assert_eq!(tips.len(), 1, "After storing B: only B should be a tip");
    assert!(tips.contains(&id_b));

    backend.put_verified(entry_e).await.unwrap();

    // After storing E: B and E are both tips
    // B has no children (C is not stored)
    // E has no children
    let tips = backend.get_tips(&id_a).await.unwrap();
    assert_eq!(tips.len(), 2, "After storing E: B and E should be tips");
    assert!(tips.contains(&id_b), "B should be a tip (C is missing)");
    assert!(tips.contains(&id_e), "E should be a tip (no children)");

    backend.put_verified(entry_d).await.unwrap();

    // After storing D: B and E are still the tips
    // D is NOT a tip because E references it as parent
    // B is still a tip because C is missing
    let tips = backend.get_tips(&id_a).await.unwrap();
    assert_eq!(
        tips.len(),
        2,
        "After storing D: B and E should still be tips"
    );
    assert!(
        tips.contains(&id_b),
        "B should still be a tip (C is missing)"
    );
    assert!(tips.contains(&id_e), "E should still be a tip");
    assert!(
        !tips.contains(&id_d),
        "D should NOT be a tip (E is its child)"
    );
}
