//! Path finding and LCA algorithm tests for Transaction
//!
//! This module contains tests for complex scenarios involving LCA (Lowest Common Ancestor)
//! computation, path finding, and deterministic ordering in diamond and merge patterns.

use eidetica::store::DocStore;

use super::helpers::*;
use crate::helpers::*;

#[test]
fn test_transaction_diamond_pattern() {
    let (_instance, tree) = setup_tree();

    // Create base entry
    let op_base = tree.new_transaction().unwrap();
    let store_base = op_base.get_store::<DocStore>("data").unwrap();
    store_base.set("base", "initial").unwrap();
    let base_id = op_base.commit().unwrap();

    // Create two branches from base
    let op_left = tree
        .new_transaction_with_tips(std::slice::from_ref(&base_id))
        .unwrap();
    let store_left = op_left.get_store::<DocStore>("data").unwrap();
    store_left.set("left", "left_value").unwrap();
    store_left.set("shared", "left_version").unwrap();
    let left_id = op_left.commit().unwrap();

    let op_right = tree.new_transaction_with_tips([base_id]).unwrap();
    let store_right = op_right.get_store::<DocStore>("data").unwrap();
    store_right.set("right", "right_value").unwrap();
    store_right.set("shared", "right_version").unwrap();
    let right_id = op_right.commit().unwrap();

    // Create merge operation with both branches as tips
    let op_merge = tree
        .new_transaction_with_tips([left_id.clone(), right_id.clone()])
        .unwrap();
    let store_merge = op_merge.get_store::<DocStore>("data").unwrap();

    // Merge operation should see data from both branches
    let merge_state = store_merge.get_all().unwrap();
    assert!(merge_state.get("base").is_some(), "Should see base data");
    assert!(merge_state.get("left").is_some(), "Should see left data");
    assert!(merge_state.get("right").is_some(), "Should see right data");
    assert!(
        merge_state.get("shared").is_some(),
        "Should see shared data"
    );

    // Add merge-specific data
    store_merge.set("merged", "merge_value").unwrap();
    let merge_id = op_merge.commit().unwrap();

    // Verify merge has correct parents
    let backend = tree.backend().unwrap();
    let merge_entry = backend.get(&merge_id).unwrap();
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

#[test]
fn test_get_path_from_to_diamond_pattern() {
    let (_instance, tree) = setup_tree();

    // Create a diamond pattern: A -> B,C -> D
    // A is the base
    let op_a = tree.new_transaction().unwrap();
    let store_a = op_a.get_store::<DocStore>("data").unwrap();
    store_a.set("base", "A").unwrap();
    let entry_a_id = op_a.commit().unwrap();

    // B branches from A
    let op_b = tree
        .new_transaction_with_tips(std::slice::from_ref(&entry_a_id))
        .unwrap();
    let store_b = op_b.get_store::<DocStore>("data").unwrap();
    store_b.set("left", "B").unwrap();
    let entry_b_id = op_b.commit().unwrap();

    // C also branches from A (parallel to B)
    let op_c = tree.new_transaction_with_tips([entry_a_id]).unwrap();
    let store_c = op_c.get_store::<DocStore>("data").unwrap();
    store_c.set("right", "C").unwrap();
    let entry_c_id = op_c.commit().unwrap();

    // D merges B and C
    let op_d = tree
        .new_transaction_with_tips([entry_b_id.clone(), entry_c_id.clone()])
        .unwrap();
    let store_d = op_d.get_store::<DocStore>("data").unwrap();
    store_d.set("merged", "D").unwrap();
    let entry_d_id = op_d.commit().unwrap();

    // Now test path finding in this diamond pattern
    // The get_path_from_to function should be able to find a valid path from A to D
    // This should work through the LCA-based algorithm when computing CRDT state

    // Create an operation that uses D as tip and access the CRDT state
    // This will internally call get_path_from_to when computing merged state
    let op_final = tree.new_transaction_with_tips([entry_d_id]).unwrap();
    let store_final = op_final.get_store::<DocStore>("data").unwrap();

    // Should be able to access all data from the diamond pattern
    let final_state = store_final.get_all().unwrap();

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

#[test]
fn test_get_path_from_to_diamond_between_lca_and_tip() {
    let (_instance, tree) = setup_tree();

    // Create the exact scenario you described:
    // LCA -> A -> C (tip)
    // LCA -> B -> C (tip)
    // Where get_path_from_to(LCA, C) only follows one path (A) and misses modifications in B

    // Step 1: Create LCA
    let op_lca = tree.new_transaction().unwrap();
    let store_lca = op_lca.get_store::<DocStore>("data").unwrap();
    store_lca.set("base", "LCA").unwrap();
    let lca_id = op_lca.commit().unwrap();

    // Step 2: Create two parallel branches from LCA
    // Branch A
    let op_a = tree
        .new_transaction_with_tips(std::slice::from_ref(&lca_id))
        .unwrap();
    let store_a = op_a.get_store::<DocStore>("data").unwrap();
    store_a.set("branch_a", "modification_A").unwrap();
    let a_id = op_a.commit().unwrap();

    // Branch B (parallel to A)
    let op_b = tree
        .new_transaction_with_tips(std::slice::from_ref(&lca_id))
        .unwrap();
    let store_b = op_b.get_store::<DocStore>("data").unwrap();
    store_b.set("branch_b", "modification_B").unwrap(); // Critical: this modification will be missed!
    let b_id = op_b.commit().unwrap();

    // Step 3: Create tip C that merges both A and B
    let op_c = tree
        .new_transaction_with_tips([a_id.clone(), b_id.clone()])
        .unwrap();
    let store_c = op_c.get_store::<DocStore>("data").unwrap();
    store_c.set("tip", "merged_C").unwrap();
    let c_id = op_c.commit().unwrap();

    // Step 4: Create another tip D independently
    let op_d = tree.new_transaction_with_tips([lca_id]).unwrap();
    let store_d = op_d.get_store::<DocStore>("data").unwrap();
    store_d.set("independent", "tip_D").unwrap();
    let d_id = op_d.commit().unwrap();

    // Step 5: Now create an operation with tips [C, D]
    // The LCA of [C, D] will be LCA
    // When computing path from LCA to C, get_path_from_to will only follow one path:
    // Either LCA -> A -> C (missing branch B modifications)
    // Or LCA -> B -> C (missing branch A modifications)
    let op_final = tree
        .new_transaction_with_tips([c_id.clone(), d_id.clone()])
        .unwrap();
    let store_final = op_final.get_store::<DocStore>("data").unwrap();

    let final_state = store_final.get_all().unwrap();

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

#[test]
fn test_correct_lca_and_path_sorting() {
    let (_instance, tree) = setup_tree();

    // Create a proper LCA scenario where sorting matters:
    // ROOT (LCA)
    //   ├─ A ─┐
    //   └─ B ─┴─ MERGE_TIP
    //   └─ C ──── OTHER_TIP
    //
    // LCA([MERGE_TIP, OTHER_TIP]) = ROOT
    // get_path_from_to(ROOT, MERGE_TIP) should return [A, B, MERGE_TIP] in height order

    // Step 1: ROOT (will be the LCA)
    let op_root = tree.new_transaction().unwrap();
    let store_root = op_root.get_store::<DocStore>("data").unwrap();
    store_root.set("step", "0").unwrap();
    store_root.set("root", "true").unwrap();
    let root_id = op_root.commit().unwrap();

    // Step 2: Create three branches from ROOT
    // Branch A (height 1)
    let op_a = tree
        .new_transaction_with_tips(std::slice::from_ref(&root_id))
        .unwrap();
    let store_a = op_a.get_store::<DocStore>("data").unwrap();
    store_a.set("step", "1").unwrap();
    store_a.set("branch", "A").unwrap();
    let a_id = op_a.commit().unwrap();

    // Branch B (height 1)
    let op_b = tree
        .new_transaction_with_tips(std::slice::from_ref(&root_id))
        .unwrap();
    let store_b = op_b.get_store::<DocStore>("data").unwrap();
    store_b.set("step", "1").unwrap();
    store_b.set("branch", "B").unwrap();
    let b_id = op_b.commit().unwrap();

    // Branch C (height 1)
    let op_c = tree.new_transaction_with_tips([root_id]).unwrap();
    let store_c = op_c.get_store::<DocStore>("data").unwrap();
    store_c.set("step", "1").unwrap();
    store_c.set("branch", "C").unwrap();
    let c_id = op_c.commit().unwrap();

    // Step 3: Create merge tip from A and B (height 2)
    let op_merge = tree
        .new_transaction_with_tips([a_id.clone(), b_id.clone()])
        .unwrap();
    let store_merge = op_merge.get_store::<DocStore>("data").unwrap();
    store_merge.set("step", "2").unwrap();
    store_merge.set("merged", "AB").unwrap();
    let merge_id = op_merge.commit().unwrap();

    // Step 4: Create another tip from C (height 2)
    let op_other = tree.new_transaction_with_tips([c_id]).unwrap();
    let store_other = op_other.get_store::<DocStore>("data").unwrap();
    store_other.set("step", "2").unwrap();
    store_other.set("other", "C_extended").unwrap();
    let other_id = op_other.commit().unwrap();

    // Step 5: Now create an operation with tips [merge_id, other_id]
    // LCA should be root_id
    // Path from root to merge should include both A and B modifications
    // Sorting order is critical for deterministic CRDT merge
    let op_final = tree
        .new_transaction_with_tips([merge_id.clone(), other_id.clone()])
        .unwrap();
    let store_final = op_final.get_store::<DocStore>("data").unwrap();

    let final_state = store_final.get_all().unwrap();

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
        let op_test = tree
            .new_transaction_with_tips([merge_id.clone(), other_id.clone()])
            .unwrap();
        let store_test = op_test.get_store::<DocStore>("data").unwrap();
        let test_state = store_test.get_all().unwrap();

        // Results should be identical due to deterministic sorting
        assert_eq!(test_state.get("root"), final_state.get("root"));
        assert_eq!(test_state.get("branch"), final_state.get("branch"));
        assert_eq!(test_state.get("merged"), final_state.get("merged"));
        assert_eq!(test_state.get("other"), final_state.get("other"));
    }
}

#[test]
fn test_lca_path_finding_with_helpers() {
    let (_instance, tree) = setup_tree();

    // Test LCA scenario creation helper
    let lca_scenario = create_lca_test_scenario(&tree);

    // Test that LCA path completeness helper works
    let tips = vec![lca_scenario.merge_tip, lca_scenario.independent_tip];
    let expected_keys = &["base", "branch_a", "branch_b", "tip", "independent"];

    assert_lca_path_completeness(&tree, &tips, expected_keys);
}

#[test]
fn test_deterministic_operations_with_helper() {
    let (_instance, tree) = setup_tree();

    // Create some initial structure
    let diamond = create_diamond_pattern(&tree);
    let merge_id = create_merge_from_diamond(&tree, &diamond);

    // Create independent branch
    let other_op = tree
        .new_transaction_with_tips([diamond.base.clone()])
        .unwrap();
    let other_store = other_op.get_store::<DocStore>("data").unwrap();
    other_store.set("other", "data").unwrap();
    let other_id = other_op.commit().unwrap();

    // Test deterministic operations helper
    let tips = vec![merge_id, other_id];
    test_deterministic_operations(&tree, &tips, 10);
}

#[test]
fn test_complex_path_finding_scenario() {
    let (_instance, tree) = setup_tree();

    // Create a more complex scenario with multiple merges
    // ROOT -> A -> A1 -> MERGE1
    //      -> B -> B1 -> MERGE1
    //      -> C -> C1 -> MERGE2
    //      -> D -> D1 -> MERGE2
    // Final operation uses MERGE1 and MERGE2 as tips

    let root_id = create_simple_operation(&tree, "data", "root", "value");

    // Create four parallel branches
    let branches = &[
        ("A", "branch_a"),
        ("B", "branch_b"),
        ("C", "branch_c"),
        ("D", "branch_d"),
    ];

    let mut branch_ids = Vec::new();
    for (step, data) in branches {
        let op = tree.new_transaction_with_tips([root_id.clone()]).unwrap();
        let store = op.get_store::<DocStore>("data").unwrap();
        store.set("branch", *step).unwrap();
        store.set("unique", *data).unwrap();
        branch_ids.push(op.commit().unwrap());
    }

    // Extend each branch
    let mut extended_ids = Vec::new();
    for (i, branch_id) in branch_ids.iter().enumerate() {
        let op = tree.new_transaction_with_tips([branch_id.clone()]).unwrap();
        let store = op.get_store::<DocStore>("data").unwrap();
        store.set("extended", format!("ext_{i}")).unwrap();
        extended_ids.push(op.commit().unwrap());
    }

    // Create two merges
    let merge1_op = tree
        .new_transaction_with_tips([extended_ids[0].clone(), extended_ids[1].clone()])
        .unwrap();
    let merge1_store = merge1_op.get_store::<DocStore>("data").unwrap();
    merge1_store.set("merge", "merge1").unwrap();
    let merge1_id = merge1_op.commit().unwrap();

    let merge2_op = tree
        .new_transaction_with_tips([extended_ids[2].clone(), extended_ids[3].clone()])
        .unwrap();
    let merge2_store = merge2_op.get_store::<DocStore>("data").unwrap();
    merge2_store.set("merge", "merge2").unwrap();
    let merge2_id = merge2_op.commit().unwrap();

    // Final operation using both merges
    let final_tips = vec![merge1_id, merge2_id];
    assert_lca_path_completeness(
        &tree,
        &final_tips,
        &["root", "branch", "unique", "extended", "merge"],
    );
}
