//! Comprehensive helper functions for BaseDB testing
//!
//! This module provides utilities for testing BaseDB functionality including
//! database operations, tree management, settings configuration, and basic operations.

use crate::helpers::setup_db_with_key;
use eidetica::Tree;
use eidetica::backend::database::InMemory;
use eidetica::basedb::BaseDB;
use eidetica::constants::SETTINGS;
use eidetica::entry::ID;
use eidetica::subtree::DocStore;

// ===== DATABASE SETUP HELPERS =====

/// Create a simple BaseDB without authentication
pub fn setup_simple_db() -> BaseDB {
    let backend = Box::new(InMemory::new());
    BaseDB::new(backend)
}

// ===== TREE CREATION HELPERS =====

/// Create tree with initial settings
pub fn create_tree_with_settings(
    db: &BaseDB,
    key_name: &str,
    tree_name: &str,
    version: &str,
) -> Tree {
    let tree = db
        .new_tree_default(key_name)
        .expect("Failed to create tree");

    let op = tree.new_operation().expect("Failed to start operation");
    {
        let settings = op
            .get_subtree::<DocStore>(SETTINGS)
            .expect("Failed to get settings subtree");

        settings
            .set("name", tree_name)
            .expect("Failed to set tree name");
        settings
            .set("version", version)
            .expect("Failed to set tree version");
    }
    op.commit().expect("Failed to commit settings");

    tree
}

/// Create multiple trees with different names for testing
pub fn create_multiple_named_trees(db: &BaseDB, key_name: &str, names: &[&str]) -> Vec<Tree> {
    let mut trees = Vec::new();

    for name in names {
        let tree = create_tree_with_settings(db, key_name, name, "1.0");
        trees.push(tree);
    }

    trees
}

/// Create tree with basic data in a custom subtree
pub fn create_tree_with_data(
    db: &BaseDB,
    key_name: &str,
    subtree_name: &str,
    data: &[(&str, &str)],
) -> Tree {
    let tree = db
        .new_tree_default(key_name)
        .expect("Failed to create tree");

    let op = tree.new_operation().expect("Failed to start operation");
    {
        let data_store = op
            .get_subtree::<DocStore>(subtree_name)
            .expect("Failed to get data subtree");

        for (key, value) in data {
            data_store.set(*key, *value).expect("Failed to set data");
        }
    }
    op.commit().expect("Failed to commit data");

    tree
}

// ===== TREE MANAGEMENT HELPERS =====

/// Test tree loading workflow
pub fn test_tree_load_workflow(db: &BaseDB, key_name: &str) -> (ID, Tree) {
    // Create initial tree
    let tree = db
        .new_tree_default(key_name)
        .expect("Failed to create tree");
    let root_id = tree.root_id().clone();

    // Drop original tree
    drop(tree);

    // Load tree from ID
    let loaded_tree = db.load_tree(&root_id).expect("Failed to load tree");

    (root_id, loaded_tree)
}

/// Create trees for find testing (with various naming scenarios)
pub fn setup_trees_for_find_testing(db: &BaseDB, key_name: &str) -> Vec<Tree> {
    let mut trees = Vec::new();

    // Tree 1: No name
    let tree1 = db
        .new_tree_default(key_name)
        .expect("Failed to create tree 1");
    trees.push(tree1);

    // Tree 2: Unique name
    let tree2 = create_tree_with_settings(db, key_name, "UniqueTree", "1.0");
    trees.push(tree2);

    // Tree 3: First instance of duplicate name
    let tree3 = create_tree_with_settings(db, key_name, "DuplicateName", "1.0");
    trees.push(tree3);

    // Tree 4: Second instance of duplicate name
    let tree4 = create_tree_with_settings(db, key_name, "DuplicateName", "2.0");
    trees.push(tree4);

    trees
}

// ===== SETTINGS OPERATION HELPERS =====

/// Set multiple settings in a tree
pub fn set_tree_settings(tree: &Tree, settings_data: &[(&str, &str)]) -> ID {
    let op = tree.new_operation().expect("Failed to start operation");
    {
        let settings = op
            .get_subtree::<DocStore>(SETTINGS)
            .expect("Failed to get settings subtree");

        for (key, value) in settings_data {
            settings.set(*key, *value).expect("Failed to set setting");
        }
    }
    op.commit().expect("Failed to commit settings")
}

/// Update tree metadata (name, version, description)
pub fn update_tree_metadata(tree: &Tree, name: &str, version: &str, description: &str) -> ID {
    let metadata = &[
        ("name", name),
        ("version", version),
        ("description", description),
    ];
    set_tree_settings(tree, metadata)
}

// ===== DATA OPERATION HELPERS =====

/// Perform basic subtree operations
pub fn perform_basic_subtree_operations(
    tree: &Tree,
    subtree_name: &str,
    operations: &[(&str, &str)],
) -> ID {
    let op = tree.new_operation().expect("Failed to start operation");
    {
        let data_store = op
            .get_subtree::<DocStore>(subtree_name)
            .expect("Failed to get data subtree");

        for (key, value) in operations {
            data_store.set(*key, *value).expect("Failed to set data");
        }
    }
    op.commit().expect("Failed to commit operations")
}

/// Create user profile data in a tree
pub fn create_user_profile(tree: &Tree, user_id: &str, name: &str, email: &str) -> ID {
    let user_data = &[("user_id", user_id), ("name", name), ("email", email)];
    perform_basic_subtree_operations(tree, "user_data", user_data)
}

/// Create application configuration in a tree
pub fn create_app_config(tree: &Tree, app_name: &str, config_data: &[(&str, &str)]) -> ID {
    let mut all_config = vec![("app_name", app_name)];
    all_config.extend_from_slice(config_data);
    perform_basic_subtree_operations(tree, "app_config", &all_config)
}

// ===== VERIFICATION HELPERS =====

/// Verify tree has expected settings
pub fn assert_tree_settings(tree: &Tree, expected_settings: &[(&str, &str)]) {
    let settings_viewer = tree
        .get_subtree_viewer::<DocStore>(SETTINGS)
        .expect("Failed to get settings viewer");

    for (key, expected_value) in expected_settings {
        let actual_value = settings_viewer
            .get_string(key)
            .unwrap_or_else(|_| panic!("Failed to get setting '{key}'"));
        assert_eq!(actual_value, *expected_value, "Setting '{key}' mismatch");
    }
}

/// Verify tree data in specific subtree
pub fn assert_tree_data(tree: &Tree, subtree_name: &str, expected_data: &[(&str, &str)]) {
    let data_viewer = tree
        .get_subtree_viewer::<DocStore>(subtree_name)
        .expect("Failed to get data viewer");

    for (key, expected_value) in expected_data {
        let actual_value = data_viewer
            .get_string(key)
            .unwrap_or_else(|_| panic!("Failed to get data '{key}'"));
        assert_eq!(actual_value, *expected_value, "Data '{key}' mismatch");
    }
}

/// Verify tree has expected name
pub fn assert_tree_name(tree: &Tree, expected_name: &str) {
    let actual_name = tree.get_name().expect("Failed to get tree name");
    assert_eq!(actual_name, expected_name, "Tree name mismatch");
}

/// Verify trees collection contains expected IDs
pub fn assert_trees_contain_ids(trees: &[Tree], expected_ids: &[ID]) {
    let found_ids: Vec<ID> = trees.iter().map(|t| t.root_id().clone()).collect();
    for expected_id in expected_ids {
        assert!(
            found_ids.contains(expected_id),
            "Expected tree ID not found: {expected_id}"
        );
    }
}

/// Verify trees collection has expected count
pub fn assert_trees_count(trees: &[Tree], expected_count: usize) {
    assert_eq!(trees.len(), expected_count, "Trees count mismatch");
}

/// Verify tree names in a collection
pub fn assert_tree_names_in_collection(trees: &[Tree], expected_names: &[&str]) {
    let tree_names: Vec<String> = trees.iter().filter_map(|t| t.get_name().ok()).collect();

    for expected_name in expected_names {
        assert!(
            tree_names.iter().any(|name| name == expected_name),
            "Expected tree name not found: {expected_name}"
        );
    }
}

// ===== ERROR TESTING HELPERS =====

/// Test tree not found scenarios
pub fn test_tree_not_found_error(db: &BaseDB, non_existent_name: &str) {
    let result = db.find_tree(non_existent_name);
    assert!(result.is_err(), "Expected error for non-existent tree");

    if let Err(eidetica::Error::Base(eidetica::basedb::BaseError::TreeNotFound { name })) = result {
        assert_eq!(name, non_existent_name);
    } else {
        panic!("Expected TreeNotFound error, got an unexpected result");
    }
}

/// Test database operations with various error conditions
pub fn test_database_error_conditions(db: &BaseDB) {
    // Test with empty database
    let all_trees_result = db.all_trees();
    assert!(
        all_trees_result.is_ok(),
        "all_trees should work with empty database"
    );

    let empty_trees = all_trees_result.unwrap();
    assert_eq!(empty_trees.len(), 0, "Empty database should have no trees");
}

// ===== INTEGRATION HELPERS =====

/// Complete workflow: create DB, add trees, perform operations, verify results
pub fn test_complete_basedb_workflow(key_name: &str) -> (BaseDB, Vec<Tree>) {
    let db = setup_db_with_key(key_name);

    // Create trees with different configurations
    let tree1 = create_tree_with_settings(&db, key_name, "MainTree", "1.0");
    let tree2 = create_tree_with_data(
        &db,
        key_name,
        "user_profiles",
        &[("user1", "alice"), ("user2", "bob")],
    );

    // Add settings to second tree
    set_tree_settings(&tree2, &[("name", "DataTree"), ("purpose", "user_storage")]);

    let trees = vec![tree1, tree2];
    (db, trees)
}

/// Test concurrent tree operations
pub fn test_concurrent_tree_operations(db: &BaseDB, key_name: &str) -> Vec<Tree> {
    let mut trees = Vec::new();

    // Create multiple trees rapidly
    for i in 0..5 {
        let tree_name = format!("ConcurrentTree{i}");
        let tree = create_tree_with_settings(db, key_name, &tree_name, "1.0");

        // Add some data to each tree
        create_user_profile(
            &tree,
            &format!("user{i}"),
            &format!("User {i}"),
            &format!("user{i}@test.com"),
        );

        trees.push(tree);
    }

    trees
}
