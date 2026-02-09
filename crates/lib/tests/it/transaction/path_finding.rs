//! Path finding and LCA algorithm tests for Transaction
//!
//! This module contains tests for complex scenarios involving LCA (Lowest Common Ancestor)
//! computation, path finding, and deterministic ordering in diamond and merge patterns.

use eidetica::{entry::ID, store::DocStore};

use super::helpers::*;
use crate::helpers::*;

#[tokio::test]
async fn test_transaction_diamond_pattern() {
    let ctx = TestContext::new().with_database().await;

    // Create base entry
    let txn_base = ctx.database().new_transaction().await.unwrap();
    let store_base = txn_base.get_store::<DocStore>("data").await.unwrap();
    store_base.set("base", "initial").await.unwrap();
    let base_id = txn_base.commit().await.unwrap();

    // Create two branches from base
    let op_left = ctx
        .database()
        .new_transaction_with_tips(std::slice::from_ref(&base_id))
        .await
        .unwrap();
    let store_left = op_left.get_store::<DocStore>("data").await.unwrap();
    store_left.set("left", "left_value").await.unwrap();
    store_left.set("shared", "left_version").await.unwrap();
    let left_id = op_left.commit().await.unwrap();

    let op_right = ctx
        .database()
        .new_transaction_with_tips([base_id])
        .await
        .unwrap();
    let store_right = op_right.get_store::<DocStore>("data").await.unwrap();
    store_right.set("right", "right_value").await.unwrap();
    store_right.set("shared", "right_version").await.unwrap();
    let right_id = op_right.commit().await.unwrap();

    // Create merge operation with both branches as tips
    let op_merge = ctx
        .database()
        .new_transaction_with_tips([left_id.clone(), right_id.clone()])
        .await
        .unwrap();
    let store_merge = op_merge.get_store::<DocStore>("data").await.unwrap();

    // Merge operation should see data from both branches
    let merge_state = store_merge.get_all().await.unwrap();
    assert!(merge_state.get("base").is_some(), "Should see base data");
    assert!(merge_state.get("left").is_some(), "Should see left data");
    assert!(merge_state.get("right").is_some(), "Should see right data");
    assert!(
        merge_state.get("shared").is_some(),
        "Should see shared data"
    );

    // Add merge-specific data
    store_merge.set("merged", "merge_value").await.unwrap();
    let merge_id = op_merge.commit().await.unwrap();

    // Verify merge has correct parents
    let backend = ctx.database().backend().unwrap();
    let merge_entry = backend.get(&merge_id).await.unwrap();
    let merge_parents = merge_entry.parents().unwrap();

    assert_eq!(merge_parents.len(), 2, "Merge should have two parents");
    assert!(
        merge_parents.contains(&left_id),
        "Should have left as parent"
    );
    assert!(
        merge_parents.contains(&right_id),
        "Should have right as parent"
    );
}

#[tokio::test]
async fn test_get_path_from_to_diamond_pattern() {
    let ctx = TestContext::new().with_database().await;

    // Create a diamond pattern: A -> B,C -> D
    // A is the base
    let op_a = ctx.database().new_transaction().await.unwrap();
    let store_a = op_a.get_store::<DocStore>("data").await.unwrap();
    store_a.set("base", "A").await.unwrap();
    let entry_a_id = op_a.commit().await.unwrap();

    // B branches from A
    let op_b = ctx
        .database()
        .new_transaction_with_tips(std::slice::from_ref(&entry_a_id))
        .await
        .unwrap();
    let store_b = op_b.get_store::<DocStore>("data").await.unwrap();
    store_b.set("left", "B").await.unwrap();
    let entry_b_id = op_b.commit().await.unwrap();

    // C also branches from A (parallel to B)
    let op_c = ctx
        .database()
        .new_transaction_with_tips([entry_a_id])
        .await
        .unwrap();
    let store_c = op_c.get_store::<DocStore>("data").await.unwrap();
    store_c.set("right", "C").await.unwrap();
    let entry_c_id = op_c.commit().await.unwrap();

    // D merges B and C
    let op_d = ctx
        .database()
        .new_transaction_with_tips([entry_b_id.clone(), entry_c_id.clone()])
        .await
        .unwrap();
    let store_d = op_d.get_store::<DocStore>("data").await.unwrap();
    store_d.set("merged", "D").await.unwrap();
    let entry_d_id = op_d.commit().await.unwrap();

    // Now test path finding in this diamond pattern
    // The get_path_from_to function should be able to find a valid path from A to D
    // This should work through the LCA-based algorithm when computing CRDT state

    // Create an operation that uses D as tip and access the CRDT state
    // This will internally call get_path_from_to when computing merged state
    let op_final = ctx
        .database()
        .new_transaction_with_tips([entry_d_id])
        .await
        .unwrap();
    let store_final = op_final.get_store::<DocStore>("data").await.unwrap();

    // Should be able to access all data from the diamond pattern
    let final_state = store_final.get_all().await.unwrap();

    // Verify all data is present (this will fail if path finding is broken)
    assert!(final_state.get("base").is_some(), "Should have base from A");
    assert!(final_state.get("left").is_some(), "Should have left from B");
    assert!(
        final_state.get("right").is_some(),
        "Should have right from C"
    );
    assert!(
        final_state.get("merged").is_some(),
        "Should have merged from D"
    );
}

#[tokio::test]
async fn test_get_path_from_to_diamond_between_lca_and_tip() {
    let ctx = TestContext::new().with_database().await;

    // Create the exact scenario you described:
    // LCA -> A -> C (tip)
    // LCA -> B -> C (tip)
    // Where get_path_from_to(LCA, C) only follows one path (A) and misses modifications in B

    // Step 1: Create LCA
    let op_lca = ctx.database().new_transaction().await.unwrap();
    let store_lca = op_lca.get_store::<DocStore>("data").await.unwrap();
    store_lca.set("base", "LCA").await.unwrap();
    let lca_id = op_lca.commit().await.unwrap();

    // Step 2: Create two parallel branches from LCA
    // Branch A
    let op_a = ctx
        .database()
        .new_transaction_with_tips(std::slice::from_ref(&lca_id))
        .await
        .unwrap();
    let store_a = op_a.get_store::<DocStore>("data").await.unwrap();
    store_a.set("branch_a", "modification_A").await.unwrap();
    let a_id = op_a.commit().await.unwrap();

    // Branch B (parallel to A)
    let op_b = ctx
        .database()
        .new_transaction_with_tips(std::slice::from_ref(&lca_id))
        .await
        .unwrap();
    let store_b = op_b.get_store::<DocStore>("data").await.unwrap();
    store_b.set("branch_b", "modification_B").await.unwrap(); // Critical: this modification will be missed!
    let b_id = op_b.commit().await.unwrap();

    // Step 3: Create tip C that merges both A and B
    let op_c = ctx
        .database()
        .new_transaction_with_tips([a_id.clone(), b_id.clone()])
        .await
        .unwrap();
    let store_c = op_c.get_store::<DocStore>("data").await.unwrap();
    store_c.set("tip", "merged_C").await.unwrap();
    let c_id = op_c.commit().await.unwrap();

    // Step 4: Create another tip D independently
    let op_d = ctx
        .database()
        .new_transaction_with_tips([lca_id])
        .await
        .unwrap();
    let store_d = op_d.get_store::<DocStore>("data").await.unwrap();
    store_d.set("independent", "tip_D").await.unwrap();
    let d_id = op_d.commit().await.unwrap();

    // Step 5: Now create an operation with tips [C, D]
    // The LCA of [C, D] will be LCA
    // When computing path from LCA to C, get_path_from_to will only follow one path:
    // Either LCA -> A -> C (missing branch B modifications)
    // Or LCA -> B -> C (missing branch A modifications)
    let op_final = ctx
        .database()
        .new_transaction_with_tips([c_id.clone(), d_id.clone()])
        .await
        .unwrap();
    let store_final = op_final.get_store::<DocStore>("data").await.unwrap();

    let final_state = store_final.get_all().await.unwrap();

    // With the CORRECT path finding, we should see ALL modifications:
    assert!(
        final_state.get("base").is_some(),
        "Should have base from LCA"
    );
    assert!(
        final_state.get("branch_a").is_some(),
        "Should have modification from branch A"
    );
    assert!(
        final_state.get("branch_b").is_some(),
        "Should have modification from branch B - this will fail with buggy path finding!"
    );
    assert!(final_state.get("tip").is_some(), "Should have tip C data");
    assert!(
        final_state.get("independent").is_some(),
        "Should have tip D data"
    );
}

#[tokio::test]
async fn test_correct_lca_and_path_sorting() {
    let ctx = TestContext::new().with_database().await;

    // Create a proper LCA scenario where sorting matters:
    // ROOT (LCA)
    //   ├─ A ─┐
    //   └─ B ─┴─ MERGE_TIP
    //   └─ C ──── OTHER_TIP
    //
    // LCA([MERGE_TIP, OTHER_TIP]) = ROOT
    // get_path_from_to(ROOT, MERGE_TIP) should return [A, B, MERGE_TIP] in height order

    // Step 1: ROOT (will be the LCA)
    let op_root = ctx.database().new_transaction().await.unwrap();
    let store_root = op_root.get_store::<DocStore>("data").await.unwrap();
    store_root.set("step", "0").await.unwrap();
    store_root.set("root", "true").await.unwrap();
    let root_id = op_root.commit().await.unwrap();

    // Step 2: Create three branches from ROOT
    // Branch A (height 1)
    let op_a = ctx
        .database()
        .new_transaction_with_tips(std::slice::from_ref(&root_id))
        .await
        .unwrap();
    let store_a = op_a.get_store::<DocStore>("data").await.unwrap();
    store_a.set("step", "1").await.unwrap();
    store_a.set("branch", "A").await.unwrap();
    let a_id = op_a.commit().await.unwrap();

    // Branch B (height 1)
    let op_b = ctx
        .database()
        .new_transaction_with_tips(std::slice::from_ref(&root_id))
        .await
        .unwrap();
    let store_b = op_b.get_store::<DocStore>("data").await.unwrap();
    store_b.set("step", "1").await.unwrap();
    store_b.set("branch", "B").await.unwrap();
    let b_id = op_b.commit().await.unwrap();

    // Branch C (height 1)
    let op_c = ctx
        .database()
        .new_transaction_with_tips([root_id])
        .await
        .unwrap();
    let store_c = op_c.get_store::<DocStore>("data").await.unwrap();
    store_c.set("step", "1").await.unwrap();
    store_c.set("branch", "C").await.unwrap();
    let c_id = op_c.commit().await.unwrap();

    // Step 3: Create merge tip from A and B (height 2)
    let op_merge = ctx
        .database()
        .new_transaction_with_tips([a_id.clone(), b_id.clone()])
        .await
        .unwrap();
    let store_merge = op_merge.get_store::<DocStore>("data").await.unwrap();
    store_merge.set("step", "2").await.unwrap();
    store_merge.set("merged", "AB").await.unwrap();
    let merge_id = op_merge.commit().await.unwrap();

    // Step 4: Create another tip from C (height 2)
    let op_other = ctx
        .database()
        .new_transaction_with_tips([c_id])
        .await
        .unwrap();
    let store_other = op_other.get_store::<DocStore>("data").await.unwrap();
    store_other.set("step", "2").await.unwrap();
    store_other.set("other", "C_extended").await.unwrap();
    let other_id = op_other.commit().await.unwrap();

    // Step 5: Now create an operation with tips [merge_id, other_id]
    // LCA should be root_id
    // Path from root to merge should include both A and B modifications
    // Sorting order is critical for deterministic CRDT merge
    let op_final = ctx
        .database()
        .new_transaction_with_tips([merge_id.clone(), other_id.clone()])
        .await
        .unwrap();
    let store_final = op_final.get_store::<DocStore>("data").await.unwrap();

    let final_state = store_final.get_all().await.unwrap();

    // Should include data from all paths with correct ordering
    assert!(final_state.get("root").is_some(), "Should have root data");
    assert!(
        final_state.get("branch").is_some(),
        "Should have branch data"
    ); // This will be last-writer-wins between A, B, C
    assert!(
        final_state.get("merged").is_some(),
        "Should have merged data"
    );
    assert!(final_state.get("other").is_some(), "Should have other data");

    // The critical test: verify that the sorting ensures deterministic results
    // Run the same operation multiple times and verify consistent results
    for _i in 0..5 {
        let txn_test = ctx
            .database()
            .new_transaction_with_tips([merge_id.clone(), other_id.clone()])
            .await
            .unwrap();
        let store_test = txn_test.get_store::<DocStore>("data").await.unwrap();
        let test_state = store_test.get_all().await.unwrap();

        // Results should be identical due to deterministic sorting
        assert_eq!(test_state.get("root"), final_state.get("root"));
        assert_eq!(test_state.get("branch"), final_state.get("branch"));
        assert_eq!(test_state.get("merged"), final_state.get("merged"));
        assert_eq!(test_state.get("other"), final_state.get("other"));
    }
}

#[tokio::test]
async fn test_lca_path_finding_with_helpers() {
    let ctx = TestContext::new().with_database().await;

    // Test LCA scenario creation helper
    let lca_scenario = create_lca_test_scenario(ctx.database()).await;

    // Test that LCA path completeness helper works
    let tips = vec![lca_scenario.merge_tip, lca_scenario.independent_tip];
    let expected_keys = &["base", "branch_a", "branch_b", "tip", "independent"];

    assert_lca_path_completeness(ctx.database(), &tips, expected_keys).await;
}

#[tokio::test]
async fn test_deterministic_operations_with_helper() {
    let ctx = TestContext::new().with_database().await;

    // Create some initial structure
    let diamond = create_diamond_pattern(ctx.database()).await;
    let merge_id = create_merge_from_diamond(ctx.database(), &diamond).await;

    // Create independent branch
    let other_op = ctx
        .database()
        .new_transaction_with_tips([diamond.base.clone()])
        .await
        .unwrap();
    let other_store = other_op.get_store::<DocStore>("data").await.unwrap();
    other_store.set("other", "data").await.unwrap();
    let other_id = other_op.commit().await.unwrap();

    // Test deterministic operations helper
    let tips = vec![merge_id, other_id];
    test_deterministic_operations(ctx.database(), &tips, 10).await;
}

#[tokio::test]
async fn test_complex_path_finding_scenario() {
    let ctx = TestContext::new().with_database().await;

    // Create a more complex scenario with multiple merges
    // ROOT -> A -> A1 -> MERGE1
    //      -> B -> B1 -> MERGE1
    //      -> C -> C1 -> MERGE2
    //      -> D -> D1 -> MERGE2
    // Final operation uses MERGE1 and MERGE2 as tips

    let root_id = create_simple_operation(ctx.database(), "data", "root", "value").await;

    // Create four parallel branches
    let branches = &[
        ("A", "branch_a"),
        ("B", "branch_b"),
        ("C", "branch_c"),
        ("D", "branch_d"),
    ];

    let mut branch_ids = Vec::new();
    for (step, data) in branches {
        let txn = ctx
            .database()
            .new_transaction_with_tips([root_id.clone()])
            .await
            .unwrap();
        let store = txn.get_store::<DocStore>("data").await.unwrap();
        store.set("branch", *step).await.unwrap();
        store.set("unique", *data).await.unwrap();
        branch_ids.push(txn.commit().await.unwrap());
    }

    // Extend each branch
    let mut extended_ids = Vec::new();
    for (i, branch_id) in branch_ids.iter().enumerate() {
        let txn = ctx
            .database()
            .new_transaction_with_tips([branch_id.clone()])
            .await
            .unwrap();
        let store = txn.get_store::<DocStore>("data").await.unwrap();
        store.set("extended", format!("ext_{i}")).await.unwrap();
        extended_ids.push(txn.commit().await.unwrap());
    }

    // Create two merges
    let merge1_op = ctx
        .database()
        .new_transaction_with_tips([extended_ids[0].clone(), extended_ids[1].clone()])
        .await
        .unwrap();
    let merge1_store = merge1_op.get_store::<DocStore>("data").await.unwrap();
    merge1_store.set("merge", "merge1").await.unwrap();
    let merge1_id = merge1_op.commit().await.unwrap();

    let merge2_op = ctx
        .database()
        .new_transaction_with_tips([extended_ids[2].clone(), extended_ids[3].clone()])
        .await
        .unwrap();
    let merge2_store = merge2_op.get_store::<DocStore>("data").await.unwrap();
    merge2_store.set("merge", "merge2").await.unwrap();
    let merge2_id = merge2_op.commit().await.unwrap();

    // Final operation using both merges
    let final_tips = vec![merge1_id, merge2_id];
    assert_lca_path_completeness(
        ctx.database(),
        &final_tips,
        &["root", "branch", "unique", "extended", "merge"],
    )
    .await;
}

/// Test that find_merge_base correctly finds the merge base when there are bypass paths.
///
/// This test creates a DAG where a traditional LCA (lowest common ancestor) exists,
/// but there's a parallel path that bypasses it. The correct merge base should be
/// the ancestor where ALL paths converge - not just a common ancestor.
///
/// DAG structure:
/// ```text
///         R (root)
///        / \
///       A   X
///      /|   |
///     B |   |
///     |  \ /
///     D   C
///     |   |
///     E   F
/// ```
///
/// Where:
/// - E's only path to R: E → D → B → A → R
/// - F's paths to R: F → C → A → R  OR  F → C → X → R (bypass!)
///
/// Traditional LCA of [E, F] = A (both can reach A)
/// Correct merge base = R (the only point where ALL paths must converge)
#[tokio::test]
async fn test_find_merge_base_with_bypass_path() {
    let ctx = TestContext::new().with_database().await;

    // Step 1: Create R (the true merge base / root)
    let op_r = ctx.database().new_transaction().await.unwrap();
    let store_r = op_r.get_store::<DocStore>("data").await.unwrap();
    store_r.set("root", "R_data").await.unwrap();
    let r_id = op_r.commit().await.unwrap();

    // Step 2: Create A (branches from R)
    let op_a = ctx
        .database()
        .new_transaction_with_tips(std::slice::from_ref(&r_id))
        .await
        .unwrap();
    let store_a = op_a.get_store::<DocStore>("data").await.unwrap();
    store_a.set("branch_a", "A_data").await.unwrap();
    let a_id = op_a.commit().await.unwrap();

    // Step 3: Create X (also branches from R, parallel to A)
    let op_x = ctx
        .database()
        .new_transaction_with_tips(std::slice::from_ref(&r_id))
        .await
        .unwrap();
    let store_x = op_x.get_store::<DocStore>("data").await.unwrap();
    store_x.set("bypass_x", "X_data").await.unwrap(); // This data will be MISSED with buggy LCA!
    let x_id = op_x.commit().await.unwrap();

    // Step 4: Create B (child of A only)
    let op_b = ctx
        .database()
        .new_transaction_with_tips(std::slice::from_ref(&a_id))
        .await
        .unwrap();
    let store_b = op_b.get_store::<DocStore>("data").await.unwrap();
    store_b.set("branch_b", "B_data").await.unwrap();
    let b_id = op_b.commit().await.unwrap();

    // Step 5: Create C (child of BOTH A and X - this creates the bypass!)
    let op_c = ctx
        .database()
        .new_transaction_with_tips([a_id.clone(), x_id.clone()])
        .await
        .unwrap();
    let store_c = op_c.get_store::<DocStore>("data").await.unwrap();
    store_c.set("merge_c", "C_data").await.unwrap();
    let c_id = op_c.commit().await.unwrap();

    // Step 6: Create D (child of B only)
    let op_d = ctx
        .database()
        .new_transaction_with_tips(std::slice::from_ref(&b_id))
        .await
        .unwrap();
    let store_d = op_d.get_store::<DocStore>("data").await.unwrap();
    store_d.set("branch_d", "D_data").await.unwrap();
    let d_id = op_d.commit().await.unwrap();

    // Step 7: Create E (tip, child of D)
    let op_e = ctx
        .database()
        .new_transaction_with_tips(std::slice::from_ref(&d_id))
        .await
        .unwrap();
    let store_e = op_e.get_store::<DocStore>("data").await.unwrap();
    store_e.set("tip_e", "E_data").await.unwrap();
    let e_id = op_e.commit().await.unwrap();

    // Step 8: Create F (tip, child of C)
    let op_f = ctx
        .database()
        .new_transaction_with_tips(std::slice::from_ref(&c_id))
        .await
        .unwrap();
    let store_f = op_f.get_store::<DocStore>("data").await.unwrap();
    store_f.set("tip_f", "F_data").await.unwrap();
    let f_id = op_f.commit().await.unwrap();

    // Now query state from tips [E, F]
    // With buggy LCA: finds A as LCA, misses X's data
    // With correct merge base: finds R, includes all data including X
    let op_final = ctx
        .database()
        .new_transaction_with_tips([e_id.clone(), f_id.clone()])
        .await
        .unwrap();
    let store_final = op_final.get_store::<DocStore>("data").await.unwrap();
    let final_state = store_final.get_all().await.unwrap();

    // These should all pass regardless of LCA implementation
    assert!(final_state.get("root").is_some(), "Should have root R data");
    assert!(
        final_state.get("branch_a").is_some(),
        "Should have branch A data"
    );
    assert!(
        final_state.get("branch_b").is_some(),
        "Should have branch B data"
    );
    assert!(
        final_state.get("merge_c").is_some(),
        "Should have merge C data"
    );
    assert!(
        final_state.get("branch_d").is_some(),
        "Should have branch D data"
    );
    assert!(final_state.get("tip_e").is_some(), "Should have tip E data");
    assert!(final_state.get("tip_f").is_some(), "Should have tip F data");

    // Directly test find_merge_base
    let backend = ctx.database().backend().unwrap();
    let merge_base = backend
        .find_merge_base(ctx.database().root_id(), "data", &[e_id, f_id])
        .await
        .unwrap();

    // Traditional LCA would return A (both E and F can reach A)
    // Correct merge base should return R (the only point where ALL paths converge)
    assert_eq!(
        merge_base, r_id,
        "find_merge_base should return R, not the traditional LCA A. \
         E's path: E→D→B→A→R. F's paths: F→C→A→R OR F→C→X→R. \
         Since F can bypass A via X, the merge base is R, not A."
    );

    // With the corrected find_merge_base algorithm:
    // - find_merge_base correctly returns R (the merge base, not traditional LCA)
    // - State is computed from R, which includes all branches
    // - X's data is correctly included because R is the proper merge base
    assert!(
        final_state.get("bypass_x").is_some(),
        "Should have bypass X data - find_merge_base correctly returns R as the merge base"
    );
}

/// Test that multi-tip merge state is cached and reused.
///
/// This verifies that when reading from multiple tips, the computed merge state
/// is cached using a synthetic ID based on sorted tip IDs, and subsequent reads
/// hit the cache instead of recomputing.
#[tokio::test]
async fn test_multi_tip_merge_state_caching() {
    let ctx = TestContext::new().with_database().await;

    // Create diamond pattern: base -> left, right (two tips)
    let diamond = create_diamond_pattern(ctx.database()).await;

    // Clear any existing cache to ensure clean state
    ctx.database()
        .backend()
        .unwrap()
        .clear_crdt_cache()
        .await
        .unwrap();

    // Create the expected cache key (sorted tip IDs)
    let mut sorted_tips = [diamond.left.as_str(), diamond.right.as_str()];
    sorted_tips.sort();
    let cache_key = format!("merge:{}", sorted_tips.join(":"));
    let cache_id = ID::new(cache_key);

    // Verify cache is empty before read
    let cached_before = ctx
        .database()
        .backend()
        .unwrap()
        .get_cached_crdt_state(&cache_id, "data")
        .await
        .unwrap();
    assert!(
        cached_before.is_none(),
        "Cache should be empty before first read"
    );

    // Read state with multiple tips - this should populate the cache
    let tx = ctx
        .database()
        .new_transaction_with_tips([diamond.left.clone(), diamond.right.clone()])
        .await
        .unwrap();
    let store = tx.get_store::<DocStore>("data").await.unwrap();
    let state = store.get_all().await.unwrap();

    // Verify we got the merged state
    assert!(state.get("base").is_some(), "Should have base data");
    assert!(state.get("left").is_some(), "Should have left branch data");
    assert!(
        state.get("right").is_some(),
        "Should have right branch data"
    );

    // Verify cache is now populated
    let cached_after = ctx
        .database()
        .backend()
        .unwrap()
        .get_cached_crdt_state(&cache_id, "data")
        .await
        .unwrap();
    assert!(
        cached_after.is_some(),
        "Cache should be populated after first read"
    );

    // Read again - should hit cache and return same result
    let tx2 = ctx
        .database()
        .new_transaction_with_tips([diamond.left.clone(), diamond.right.clone()])
        .await
        .unwrap();
    let store2 = tx2.get_store::<DocStore>("data").await.unwrap();
    let state2 = store2.get_all().await.unwrap();

    // Verify same data is returned from cache
    assert!(
        state2.get("base").is_some(),
        "Cached read should have base data"
    );
    assert!(
        state2.get("left").is_some(),
        "Cached read should have left data"
    );
    assert!(
        state2.get("right").is_some(),
        "Cached read should have right data"
    );
}

/// Test that merge cache key is order-independent (tips are sorted).
#[tokio::test]
async fn test_multi_tip_cache_key_is_order_independent() {
    let ctx = TestContext::new().with_database().await;
    let diamond = create_diamond_pattern(ctx.database()).await;

    // Clear cache
    ctx.database()
        .backend()
        .unwrap()
        .clear_crdt_cache()
        .await
        .unwrap();

    // Read with tips in order [left, right]
    let tx1 = ctx
        .database()
        .new_transaction_with_tips([diamond.left.clone(), diamond.right.clone()])
        .await
        .unwrap();
    let store1 = tx1.get_store::<DocStore>("data").await.unwrap();
    let _ = store1.get_all().await.unwrap();

    // Get the cache key (sorted, so order doesn't matter)
    let mut sorted_tips = [diamond.left.as_str(), diamond.right.as_str()];
    sorted_tips.sort();
    let cache_key = format!("merge:{}", sorted_tips.join(":"));
    let cache_id = ID::new(cache_key);

    // Verify cache was populated
    let cached = ctx
        .database()
        .backend()
        .unwrap()
        .get_cached_crdt_state(&cache_id, "data")
        .await
        .unwrap();
    assert!(cached.is_some(), "Cache should be populated");

    // Clear cache again
    ctx.database()
        .backend()
        .unwrap()
        .clear_crdt_cache()
        .await
        .unwrap();

    // Read with tips in REVERSE order [right, left]
    let tx2 = ctx
        .database()
        .new_transaction_with_tips([diamond.right.clone(), diamond.left.clone()])
        .await
        .unwrap();
    let store2 = tx2.get_store::<DocStore>("data").await.unwrap();
    let _ = store2.get_all().await.unwrap();

    // Should use the SAME cache key (tips are sorted internally)
    let cached_reverse = ctx
        .database()
        .backend()
        .unwrap()
        .get_cached_crdt_state(&cache_id, "data")
        .await
        .unwrap();
    assert!(
        cached_reverse.is_some(),
        "Same cache key should be used regardless of tip order"
    );
}
