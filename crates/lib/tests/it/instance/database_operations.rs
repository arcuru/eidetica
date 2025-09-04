//! Database operation tests
//!
//! This module contains tests for basic Instance operations including
//! database creation, tree creation, tree loading, and backend management.

use super::helpers::*;
use crate::helpers::setup_db_with_key;

const TEST_KEY: &str = "test_key";

#[test]
fn test_load_tree() {
    let db = setup_db_with_key(TEST_KEY);
    let (root_id, loaded_tree) = test_tree_load_workflow(&db, TEST_KEY);

    assert_eq!(loaded_tree.root_id(), &root_id);
}

#[test]
fn test_database_authentication_scenarios() {
    // Test authenticated database operations
    let auth_db = setup_db_with_key(TEST_KEY);

    // Test tree creation with authentication
    let tree1 = auth_db
        .new_tree_default(TEST_KEY)
        .expect("Failed to create tree with auth key");
    let tree2 = auth_db
        .new_tree_default(TEST_KEY)
        .expect("Failed to create second tree with auth key");

    // Verify both trees are different
    assert_ne!(tree1.root_id(), tree2.root_id());

    // Test non-authenticated database operations
    let simple_db = setup_simple_db();

    // Test basic backend operations work
    let backend = simple_db.backend();
    assert!(backend.all_roots().is_ok());

    // Test error conditions with non-authenticated database
    test_database_error_conditions(&simple_db);
}

#[test]
fn test_multiple_database_creation() {
    // Create multiple independent databases
    let db1 = setup_db_with_key("key1");
    let db2 = setup_db_with_key("key2");
    let db3 = setup_simple_db();

    // Create trees in each database
    let tree1 = db1
        .new_tree_default("key1")
        .expect("Failed to create tree in db1");
    let tree2 = db2
        .new_tree_default("key2")
        .expect("Failed to create tree in db2");

    // Verify they have different root IDs
    assert_ne!(tree1.root_id(), tree2.root_id());

    // Verify the simple db works independently
    let backend3 = db3.backend();
    assert!(backend3.all_roots().is_ok());
}

#[test]
fn test_tree_creation_workflow_with_helpers() {
    let db = setup_db_with_key(TEST_KEY);

    // Use helper to create tree with settings
    let tree = create_tree_with_settings(&db, TEST_KEY, "TestTree", "1.0");

    // Verify the tree was created correctly
    assert_tree_name(&tree, "TestTree");
    assert_tree_settings(&tree, &[("name", "TestTree"), ("version", "1.0")]);
}

#[test]
fn test_tree_creation_with_data() {
    let db = setup_db_with_key(TEST_KEY);

    // Use helper to create tree with initial data
    let user_data = &[("user_id", "alice"), ("email", "alice@example.com")];
    let tree = create_tree_with_data(&db, TEST_KEY, "user_data", user_data);

    // Verify the data was set correctly
    assert_tree_data(&tree, "user_data", user_data);
}

#[test]
fn test_database_operations_using_helpers() {
    let (_db, trees) = test_complete_instance_workflow(TEST_KEY);

    // Verify we created the expected trees
    assert_trees_count(&trees, 2);

    // Verify the first tree
    assert_tree_name(&trees[0], "MainTree");
    assert_tree_settings(&trees[0], &[("name", "MainTree"), ("version", "1.0")]);

    // Verify the second tree has data
    assert_tree_data(
        &trees[1],
        "user_profiles",
        &[("user1", "alice"), ("user2", "bob")],
    );
    assert_tree_settings(
        &trees[1],
        &[("name", "DataTree"), ("purpose", "user_storage")],
    );
}
