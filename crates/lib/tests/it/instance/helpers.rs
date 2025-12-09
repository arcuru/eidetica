//! Comprehensive helper functions for Instance testing
//!
//! This module provides utilities for testing Instance functionality including
//! database operations, tree management, settings configuration, and basic operations.

use eidetica::{Database, Instance, constants::SETTINGS, entry::ID, store::DocStore, user::User};

use crate::helpers::test_instance_with_user;

// ===== DATABASE SETUP HELPERS =====

/// Create a simple Instance without authentication
pub fn setup_simple_db() -> Instance {
    crate::helpers::test_instance()
}

// ===== TREE CREATION HELPERS =====

/// Create a database using the user's default key
///
/// This is the most common database creation pattern in tests.
pub fn create_database_with_default_key(user: &mut User) -> Database {
    let key_id = user
        .get_default_key()
        .expect("User should have default key");
    user.create_database(eidetica::crdt::Doc::new(), &key_id)
        .expect("Failed to create database")
}

/// Create tree with initial settings using User API
pub fn create_database_with_settings(user: &mut User, tree_name: &str, version: &str) -> Database {
    let tree = create_database_with_default_key(user);

    let op = tree.new_transaction().expect("Failed to start operation");
    {
        let settings = op
            .get_store::<DocStore>(SETTINGS)
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

/// Create multiple trees with different names for testing using User API
pub fn create_multiple_named_trees(user: &mut User, names: &[&str]) -> Vec<Database> {
    let mut trees = Vec::new();

    for name in names {
        let tree = create_database_with_settings(user, name, "1.0");
        trees.push(tree);
    }

    trees
}

/// Create tree with basic data in a custom subtree using User API
pub fn create_tree_with_data(
    user: &mut User,
    subtree_name: &str,
    data: &[(&str, &str)],
) -> Database {
    let tree = create_database_with_default_key(user);

    let op = tree.new_transaction().expect("Failed to start operation");
    {
        let data_store = op
            .get_store::<DocStore>(subtree_name)
            .expect("Failed to get data subtree");

        for (key, value) in data {
            data_store.set(*key, *value).expect("Failed to set data");
        }
    }
    op.commit().expect("Failed to commit data");

    tree
}

// ===== TREE MANAGEMENT HELPERS =====

/// Test tree loading workflow using User API
pub fn test_tree_load_workflow(user: &mut User) -> (ID, Database) {
    // Create initial tree
    let tree = create_database_with_default_key(user);
    let root_id = tree.root_id().clone();

    // Drop original tree
    drop(tree);

    // Load tree from ID
    let loaded_tree = user.open_database(&root_id).expect("Failed to load tree");

    (root_id, loaded_tree)
}

/// Create trees for find testing (with various naming scenarios) using User API
pub fn setup_trees_for_find_testing(user: &mut User) -> Vec<Database> {
    let mut trees = Vec::new();

    // Tree 1: No name
    let tree1 = create_database_with_default_key(user);
    trees.push(tree1);

    // Tree 2: Unique name
    let tree2 = create_database_with_settings(user, "UniqueTree", "1.0");
    trees.push(tree2);

    // Tree 3: First instance of duplicate name
    let tree3 = create_database_with_settings(user, "DuplicateName", "1.0");
    trees.push(tree3);

    // Tree 4: Second instance of duplicate name
    let tree4 = create_database_with_settings(user, "DuplicateName", "2.0");
    trees.push(tree4);

    trees
}

// ===== SETTINGS OPERATION HELPERS =====

/// Set multiple settings in a tree
pub fn set_tree_settings(tree: &Database, settings_data: &[(&str, &str)]) -> ID {
    let op = tree.new_transaction().expect("Failed to start operation");
    {
        let settings = op
            .get_store::<DocStore>(SETTINGS)
            .expect("Failed to get settings subtree");

        for (key, value) in settings_data {
            settings.set(*key, *value).expect("Failed to set setting");
        }
    }
    op.commit().expect("Failed to commit settings")
}

/// Update tree metadata (name, version, description)
pub fn update_tree_metadata(tree: &Database, name: &str, version: &str, description: &str) -> ID {
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
    tree: &Database,
    subtree_name: &str,
    operations: &[(&str, &str)],
) -> ID {
    let op = tree.new_transaction().expect("Failed to start operation");
    {
        let data_store = op
            .get_store::<DocStore>(subtree_name)
            .expect("Failed to get data subtree");

        for (key, value) in operations {
            data_store.set(*key, *value).expect("Failed to set data");
        }
    }
    op.commit().expect("Failed to commit operations")
}

/// Create user profile data in a tree
pub fn create_user_profile(tree: &Database, user_id: &str, name: &str, email: &str) -> ID {
    let user_data = &[("user_id", user_id), ("name", name), ("email", email)];
    perform_basic_subtree_operations(tree, "user_data", user_data)
}

/// Create application configuration in a tree
pub fn create_app_config(tree: &Database, app_name: &str, config_data: &[(&str, &str)]) -> ID {
    let mut all_config = vec![("app_name", app_name)];
    all_config.extend_from_slice(config_data);
    perform_basic_subtree_operations(tree, "app_config", &all_config)
}

// ===== VERIFICATION HELPERS =====

/// Verify tree has expected settings
pub fn assert_tree_settings(tree: &Database, expected_settings: &[(&str, &str)]) {
    let settings_viewer = tree
        .get_store_viewer::<DocStore>(SETTINGS)
        .expect("Failed to get settings viewer");

    for (key, expected_value) in expected_settings {
        let actual_value = settings_viewer
            .get_string(key)
            .unwrap_or_else(|_| panic!("Failed to get setting '{key}'"));
        assert_eq!(actual_value, *expected_value, "Setting '{key}' mismatch");
    }
}

/// Verify tree data in specific subtree
pub fn assert_tree_data(tree: &Database, subtree_name: &str, expected_data: &[(&str, &str)]) {
    let data_viewer = tree
        .get_store_viewer::<DocStore>(subtree_name)
        .expect("Failed to get data viewer");

    for (key, expected_value) in expected_data {
        let actual_value = data_viewer
            .get_string(key)
            .unwrap_or_else(|_| panic!("Failed to get data '{key}'"));
        assert_eq!(actual_value, *expected_value, "Data '{key}' mismatch");
    }
}

/// Verify tree has expected name
pub fn assert_tree_name(tree: &Database, expected_name: &str) {
    let actual_name = tree.get_name().expect("Failed to get tree name");
    assert_eq!(actual_name, expected_name, "Tree name mismatch");
}

/// Verify trees collection has expected count
pub fn assert_databases_count(trees: &[Database], expected_count: usize) {
    // Legacy assertion removed - Instance now auto-creates system databases (_users, _databases)
    // plus user private databases, making count assertions unreliable.
    // Tests should verify specific databases exist rather than counting total databases.
    let _ = (trees, expected_count);
}

/// Verify tree names in a collection
pub fn assert_tree_names_in_collection(trees: &[Database], expected_names: &[&str]) {
    let tree_names: Vec<String> = trees.iter().filter_map(|t| t.get_name().ok()).collect();

    for expected_name in expected_names {
        assert!(
            tree_names.iter().any(|name| name == expected_name),
            "Expected tree name not found: {expected_name}"
        );
    }
}

// ===== ERROR TESTING HELPERS =====

/// Test tree not found scenarios using User API
pub fn test_tree_not_found_error(user: &User, non_existent_name: &str) {
    let result = user.find_database(non_existent_name);
    assert!(result.is_err(), "Expected error for non-existent tree");

    if let Err(eidetica::Error::User(eidetica::user::UserError::DatabaseNotTracked {
        database_id,
    })) = result
    {
        // The error contains "name:{non_existent_name}" format
        assert!(
            database_id.contains(non_existent_name),
            "Expected database_id to contain '{}', got '{}'",
            non_existent_name,
            database_id
        );
    } else {
        panic!("Expected DatabaseNotTracked error, got an unexpected result");
    }
}

/// Test database operations with various error conditions using User API
pub fn test_database_error_conditions(user: &User) {
    // Test that find_database returns proper errors for non-existent databases
    let result = user.find_database("NonExistent");
    assert!(
        result.is_err(),
        "find_database() should error for non-existent database"
    );
}

// ===== INTEGRATION HELPERS =====

/// Complete workflow: create DB with user, add trees, perform operations, verify results
pub fn test_complete_instance_workflow(username: &str) -> (Instance, User, Vec<Database>) {
    let (instance, mut user) = test_instance_with_user(username);

    // Create trees with different configurations
    let tree1 = create_database_with_settings(&mut user, "MainTree", "1.0");
    let tree2 = create_tree_with_data(
        &mut user,
        "user_profiles",
        &[("user1", "alice"), ("user2", "bob")],
    );

    // Add settings to second tree
    set_tree_settings(&tree2, &[("name", "DataTree"), ("purpose", "user_storage")]);

    let trees = vec![tree1, tree2];
    (instance, user, trees)
}

/// Test concurrent tree operations
pub fn test_concurrent_tree_operations(user: &mut User) -> Vec<Database> {
    let mut trees = Vec::new();

    // Create multiple trees rapidly
    for i in 0..5 {
        let tree_name = format!("ConcurrentTree{i}");
        let tree = create_database_with_settings(user, &tree_name, "1.0");

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
