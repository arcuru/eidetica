//! Database operation tests
//!
//! This module contains tests for basic Instance operations including
//! database creation, tree creation, tree loading, and backend management.

use super::helpers::*;
use crate::helpers::test_instance_with_user;
use eidetica::crdt::Doc;

#[tokio::test]
async fn test_load_tree() {
    let (_instance, mut user) = test_instance_with_user("test_user").await;
    let (root_id, loaded_tree) = test_tree_load_workflow(&mut user).await;

    assert_eq!(loaded_tree.root_id(), &root_id);
}

#[tokio::test]
async fn test_database_authentication_scenarios() {
    // Test authenticated database operations
    let (_instance, mut user) = test_instance_with_user("auth_user").await;
    let key_id = user
        .get_default_key()
        .expect("User should have default key");

    // Test tree creation with authentication
    let tree1 = user
        .create_database(Doc::new(), &key_id)
        .await
        .expect("Failed to create tree with auth key");
    let tree2 = user
        .create_database(Doc::new(), &key_id)
        .await
        .expect("Failed to create second tree with auth key");

    // Verify both trees are different
    assert_ne!(tree1.root_id(), tree2.root_id());

    // Test non-authenticated database operations
    let simple_db = setup_simple_db().await;

    // Test basic backend operations work
    let backend = simple_db.backend();
    assert!(backend.all_roots().await.is_ok());
}

#[tokio::test]
async fn test_multiple_database_creation() {
    // Create multiple independent instance+user combinations
    let (_instance1, mut user1) = test_instance_with_user("user1").await;
    let (_instance2, mut user2) = test_instance_with_user("user2").await;
    let db3 = setup_simple_db().await;

    // Create trees in each user's context
    let key_id1 = user1
        .get_default_key()
        .expect("User1 should have default key");
    let tree1 = user1
        .create_database(Doc::new(), &key_id1)
        .await
        .expect("Failed to create tree for user1");

    let key_id2 = user2
        .get_default_key()
        .expect("User2 should have default key");
    let tree2 = user2
        .create_database(Doc::new(), &key_id2)
        .await
        .expect("Failed to create tree for user2");

    // Verify they have different root IDs
    assert_ne!(tree1.root_id(), tree2.root_id());

    // Verify the simple db works independently
    let backend3 = db3.backend();
    assert!(backend3.all_roots().await.is_ok());
}

#[tokio::test]
async fn test_tree_creation_workflow_with_helpers() {
    let (_instance, mut user) = test_instance_with_user("helper_user").await;

    // Use helper to create tree with settings
    let tree = create_database_with_settings(&mut user, "TestTree", "1.0").await;

    // Verify the tree was created correctly
    assert_tree_name(&tree, "TestTree").await;
    assert_tree_settings(&tree, &[("name", "TestTree"), ("version", "1.0")]).await;
}

#[tokio::test]
async fn test_tree_creation_with_data() {
    let (_instance, mut user) = test_instance_with_user("data_user").await;

    // Use helper to create tree with initial data
    let user_data = &[("user_id", "alice"), ("email", "alice@example.com")];
    let tree = create_tree_with_data(&mut user, "user_data", user_data).await;

    // Verify the data was set correctly
    assert_tree_data(&tree, "user_data", user_data).await;
}

#[tokio::test]
async fn test_database_operations_using_helpers() {
    let (_instance, _user, trees) = test_complete_instance_workflow("workflow_user").await;

    // Verify we created the expected trees
    assert_databases_count(&trees, 2);

    // Verify the first tree
    assert_tree_name(&trees[0], "MainTree").await;
    assert_tree_settings(&trees[0], &[("name", "MainTree"), ("version", "1.0")]).await;

    // Verify the second tree has data
    assert_tree_data(
        &trees[1],
        "user_profiles",
        &[("user1", "alice"), ("user2", "bob")],
    ).await;
    assert_tree_settings(
        &trees[1],
        &[("name", "DataTree"), ("purpose", "user_storage")],
    ).await;
}
