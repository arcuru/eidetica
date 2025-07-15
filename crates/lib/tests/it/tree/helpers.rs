//! Comprehensive helper functions for Tree testing
//!
//! This module provides utilities for testing Tree functionality including
//! core operations, API methods, merging algorithms, and settings management.

use crate::helpers::*;
use eidetica::Tree;
use eidetica::auth::types::{AuthKey, KeyStatus, Permission};
use eidetica::basedb::BaseDB;
use eidetica::crdt::Map;
use eidetica::crdt::map::Value;
use eidetica::entry::ID;
use eidetica::subtree::Dict;

// ===== OPERATION HELPERS =====

/// Create operation and add data to specific subtree
pub fn add_data_to_subtree(tree: &Tree, subtree_name: &str, data: &[(&str, &str)]) -> ID {
    let op = tree.new_operation().expect("Failed to create operation");
    {
        let store = op
            .get_subtree::<Dict>(subtree_name)
            .expect("Failed to get subtree");
        for (key, value) in data {
            store.set(*key, *value).expect("Failed to set data");
        }
    }
    op.commit().expect("Failed to commit")
}

/// Create authenticated operation and add data
pub fn add_authenticated_data(
    tree: &Tree,
    key_name: &str,
    subtree_name: &str,
    data: &[(&str, &str)],
) -> ID {
    let op = tree
        .new_authenticated_operation(key_name)
        .expect("Failed to create authenticated operation");
    {
        let store = op
            .get_subtree::<Dict>(subtree_name)
            .expect("Failed to get subtree");
        for (key, value) in data {
            store.set(*key, *value).expect("Failed to set data");
        }
    }
    op.commit().expect("Failed to commit")
}

/// Create branch from specific entry
pub fn create_branch_from_entry(
    tree: &Tree,
    entry_id: &ID,
    subtree_name: &str,
    data: &[(&str, &str)],
) -> ID {
    let op = tree
        .new_operation_with_tips(std::slice::from_ref(entry_id))
        .expect("Failed to create branch operation");
    {
        let store = op
            .get_subtree::<Dict>(subtree_name)
            .expect("Failed to get subtree");
        for (key, value) in data {
            store.set(*key, *value).expect("Failed to set data");
        }
    }
    op.commit().expect("Failed to commit branch")
}

// ===== VERIFICATION HELPERS =====

/// Verify tree contains expected data in subtree
pub fn assert_subtree_data(tree: &Tree, subtree_name: &str, expected: &[(&str, &str)]) {
    let viewer = tree
        .get_subtree_viewer::<Dict>(subtree_name)
        .expect("Failed to get subtree viewer");

    for (key, expected_value) in expected {
        let actual = viewer
            .get_string(key)
            .unwrap_or_else(|_| panic!("Failed to get {key} from {subtree_name}"));
        assert_eq!(actual, *expected_value, "Mismatch in {subtree_name}.{key}");
    }
}

/// Verify entry has expected authentication properties
pub fn assert_entry_authentication(tree: &Tree, entry_id: &ID, expected_key: &str) {
    let entry = tree.get_entry(entry_id).expect("Failed to get entry");
    let sig_info = &entry.sig;

    assert!(
        sig_info.is_signed_by(expected_key),
        "Entry not signed by {expected_key}"
    );
    assert!(sig_info.sig.is_some(), "Entry should have signature");

    let is_valid = tree
        .verify_entry_signature(entry_id)
        .expect("Failed to verify signature");
    assert!(is_valid, "Entry signature should be valid");
}

/// Verify entry parent relationships
pub fn assert_entry_parents(tree: &Tree, entry_id: &ID, expected_parents: &[ID]) {
    let backend = tree.backend();
    let entry = backend.get(entry_id).expect("Failed to get entry");
    let actual_parents = entry.parents().expect("Failed to get parents");

    assert_eq!(
        actual_parents.len(),
        expected_parents.len(),
        "Parent count mismatch for {entry_id}"
    );

    for expected_parent in expected_parents {
        assert!(
            actual_parents.contains(expected_parent),
            "Expected parent {expected_parent} not found for {entry_id}"
        );
    }
}

/// Verify entry exists and belongs to tree
pub fn assert_entry_belongs_to_tree(tree: &Tree, entry_id: &ID) {
    let result = tree.get_entry(entry_id);
    assert!(result.is_ok(), "Entry {entry_id} should exist in tree");
}

// ===== COMPLEX SCENARIO HELPERS =====

/// Create diamond pattern for testing complex merges
pub fn create_diamond_pattern(tree: &Tree, base_data: &[(&str, &str)]) -> (ID, ID, ID, ID) {
    // Create base entry (A)
    let base_id = add_data_to_subtree(tree, "data", base_data);

    // Create two branches from base (B and C)
    let branch_b_id = create_branch_from_entry(
        tree,
        &base_id,
        "data",
        &[("branch", "B"), ("b_specific", "B_data")],
    );

    let branch_c_id = create_branch_from_entry(
        tree,
        &base_id,
        "data",
        &[("branch", "C"), ("c_specific", "C_data")],
    );

    // Create merge entry (D) from both branches
    let merge_tips = vec![branch_b_id.clone(), branch_c_id.clone()];
    let op = tree
        .new_operation_with_tips(&merge_tips)
        .expect("Failed to create merge operation");
    {
        let store = op
            .get_subtree::<Dict>("data")
            .expect("Failed to get data store");
        store.set("merge", "D").expect("Failed to set merge data");
        store
            .set("final", "merged")
            .expect("Failed to set final data");
    }
    let merge_id = op.commit().expect("Failed to commit merge");

    (base_id, branch_b_id, branch_c_id, merge_id)
}

/// Create linear chain of entries
pub fn create_linear_chain(tree: &Tree, subtree_name: &str, chain_length: usize) -> Vec<ID> {
    let mut entry_ids = Vec::new();

    for i in 0..chain_length {
        let step_str = i.to_string();
        let step_key = format!("step_{i}");
        let step_value = format!("value_{i}");
        let data = vec![
            ("step", step_str.as_str()),
            (step_key.as_str(), step_value.as_str()),
        ];
        let entry_id = add_data_to_subtree(tree, subtree_name, &data);
        entry_ids.push(entry_id);
    }

    entry_ids
}

/// Create tree with authentication setup for testing
pub fn setup_tree_with_auth_config(key_name: &str) -> (BaseDB, Tree) {
    let db = setup_db();
    let public_key = db.add_private_key(key_name).expect("Failed to add key");

    // Create auth settings
    let mut settings = Map::new();
    let mut auth_settings = Map::new();
    auth_settings
        .set_json(
            key_name.to_string(),
            AuthKey {
                pubkey: eidetica::auth::crypto::format_public_key(&public_key),
                permissions: Permission::Admin(0),
                status: KeyStatus::Active,
            },
        )
        .unwrap();
    settings.set_map("auth", auth_settings);
    settings.set_string("name", "AuthenticatedTree");

    let tree = db
        .new_tree(settings, key_name)
        .expect("Failed to create tree");
    (db, tree)
}

// ===== ERROR TESTING HELPERS =====

// ===== PERFORMANCE TESTING HELPERS =====

/// Create deep chain for performance testing
pub fn create_deep_chain_for_performance(tree: &Tree, depth: usize) -> Vec<ID> {
    let mut entry_ids = Vec::new();

    for i in 0..depth {
        let depth_str = i.to_string();
        let data_str = format!("deep_value_{i}");
        let data = vec![("depth", depth_str.as_str()), ("data", data_str.as_str())];
        let entry_id = add_data_to_subtree(tree, "deep_data", &data);
        entry_ids.push(entry_id);
    }

    entry_ids
}

/// Verify performance characteristics of deep operations
pub fn assert_deep_operations_performance(tree: &Tree, depth: usize) {
    // Create deep chain
    let _entry_ids = create_deep_chain_for_performance(tree, depth);

    // Reading should not cause stack overflow
    let viewer = tree
        .get_subtree_viewer::<Dict>("deep_data")
        .expect("Deep operations should not fail");
    let final_state = viewer.get_all().expect("Should get final state");

    // Should have accumulated all data
    assert!(
        final_state.len() >= 2,
        "Should have accumulated data from deep chain"
    );

    // Final values should be from last operation
    if let Some(Value::Text(depth_value)) = final_state.get("depth") {
        assert_eq!(
            depth_value,
            &(depth - 1).to_string(),
            "Should have final depth value"
        );
    } else {
        panic!("Should have depth value");
    }
}

// ===== CONSISTENCY TESTING HELPERS =====

/// Verify deterministic behavior across multiple reads
pub fn assert_deterministic_reads(tree: &Tree, subtree_name: &str, read_count: usize) {
    let mut results = Vec::new();

    for _ in 0..read_count {
        let viewer = tree
            .get_subtree_viewer::<Dict>(subtree_name)
            .expect("Failed to get viewer");
        let state = viewer.get_all().expect("Failed to get state");
        results.push(state);
    }

    // All results should be identical
    for i in 1..results.len() {
        assert_eq!(
            results[0], results[i],
            "Read {i} differs from read 0 - not deterministic"
        );
    }
}

/// Verify caching consistency
pub fn assert_caching_consistency(tree: &Tree, subtree_name: &str) {
    // Force cache clear
    tree.backend()
        .clear_crdt_cache()
        .expect("Failed to clear cache");

    // First read - should populate cache
    let viewer1 = tree
        .get_subtree_viewer::<Dict>(subtree_name)
        .expect("Failed to get viewer 1");
    let state1 = viewer1.get_all().expect("Failed to get state 1");

    // Second read - should use cache
    let viewer2 = tree
        .get_subtree_viewer::<Dict>(subtree_name)
        .expect("Failed to get viewer 2");
    let state2 = viewer2.get_all().expect("Failed to get state 2");

    assert_eq!(state1, state2, "Cached and non-cached reads should match");
}
