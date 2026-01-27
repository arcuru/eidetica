//! Settings operation tests
//!
//! This module contains tests for tree settings operations including
//! initial settings creation, settings modification, and metadata management.

use eidetica::crdt::Doc;

use super::helpers::*;
use crate::helpers::test_instance_with_user;

#[tokio::test]
async fn test_create_tree_with_initial_settings() {
    let (_instance, mut user) = test_instance_with_user("settings_user").await;

    // Use helper to create tree with settings
    let tree = create_database_with_settings(&mut user, "My Settings Tree", "1.0").await;

    // Verify settings using helper
    assert_tree_name(&tree, "My Settings Tree").await;
    assert_tree_settings(&tree, &[("name", "My Settings Tree"), ("version", "1.0")]).await;
}

#[tokio::test]
async fn test_settings_using_helpers() {
    let (_instance, mut user) = test_instance_with_user("helper_user").await;

    // Use helper to create tree with settings
    let tree = create_database_with_settings(&mut user, "HelperTree", "2.0").await;

    // Verify settings were applied correctly
    assert_tree_name(&tree, "HelperTree").await;
    assert_tree_settings(&tree, &[("name", "HelperTree"), ("version", "2.0")]).await;
}

#[tokio::test]
async fn test_multiple_settings_updates() {
    let (_instance, mut user) = test_instance_with_user("multi_user").await;

    // Create tree and perform multiple settings updates
    let key_id = user
        .get_default_key()
        .expect("User should have default key");
    let tree = user
        .create_database(Doc::new(), &key_id)
        .await
        .expect("Failed to create tree");

    // First update: basic info
    set_tree_settings(
        &tree,
        &[
            ("name", "EvolvingTree"),
            ("version", "1.0"),
            ("author", "TestSuite"),
        ],
    )
    .await;

    // Verify first update
    assert_tree_settings(
        &tree,
        &[
            ("name", "EvolvingTree"),
            ("version", "1.0"),
            ("author", "TestSuite"),
        ],
    )
    .await;

    // Second update: add more metadata
    set_tree_settings(
        &tree,
        &[
            ("description", "A tree that evolves over time"),
            ("category", "testing"),
            ("environment", "development"),
        ],
    )
    .await;

    // Verify all settings are present
    assert_tree_settings(
        &tree,
        &[
            ("name", "EvolvingTree"),
            ("version", "1.0"),
            ("author", "TestSuite"),
            ("description", "A tree that evolves over time"),
            ("category", "testing"),
            ("environment", "development"),
        ],
    )
    .await;
}

#[tokio::test]
async fn test_settings_overwrite() {
    let (_instance, mut user) = test_instance_with_user("overwrite_user").await;

    let tree = create_database_with_settings(&mut user, "OverwriteTest", "1.0").await;

    // Verify initial settings
    assert_tree_settings(&tree, &[("name", "OverwriteTest"), ("version", "1.0")]).await;

    // Overwrite settings
    set_tree_settings(
        &tree,
        &[
            ("name", "UpdatedTree"),
            ("version", "2.0"),
            ("updated", "true"),
        ],
    )
    .await;

    // Verify settings were overwritten and new ones added
    assert_tree_settings(
        &tree,
        &[
            ("name", "UpdatedTree"),
            ("version", "2.0"),
            ("updated", "true"),
        ],
    )
    .await;

    // Verify tree name reflects the change
    assert_tree_name(&tree, "UpdatedTree").await;
}

#[tokio::test]
async fn test_metadata_helper_functions() {
    let (_instance, mut user) = test_instance_with_user("metadata_user").await;

    let key_id = user
        .get_default_key()
        .expect("User should have default key");
    let tree = user
        .create_database(Doc::new(), &key_id)
        .await
        .expect("Failed to create tree");

    // Use metadata helper
    update_tree_metadata(
        &tree,
        "MetadataTestTree",
        "3.1.4",
        "Comprehensive metadata test",
    )
    .await;

    // Verify all metadata was set
    assert_tree_settings(
        &tree,
        &[
            ("name", "MetadataTestTree"),
            ("version", "3.1.4"),
            ("description", "Comprehensive metadata test"),
        ],
    )
    .await;

    assert_tree_name(&tree, "MetadataTestTree").await;
}

#[tokio::test]
async fn test_settings_with_complex_values() {
    let (_instance, mut user) = test_instance_with_user("complex_user").await;

    let key_id = user
        .get_default_key()
        .expect("User should have default key");
    let tree = user
        .create_database(Doc::new(), &key_id)
        .await
        .expect("Failed to create tree");

    // Set settings with various types of values
    set_tree_settings(
        &tree,
        &[
            ("name", "ComplexSettingsTree"),
            ("version", "1.0.0-beta.1+build.123"),
            (
                "description",
                "A tree with complex settings including special characters: !@#$%^&*()",
            ),
            ("json_config", r#"{"enabled": true, "max_items": 100}"#),
            ("url", "https://example.com/api/v1?param=value&other=data"),
            ("multiline", "Line 1\nLine 2\nLine 3"),
            ("unicode", "测试 Unicode 🚀"),
        ],
    )
    .await;

    // Verify all complex settings were stored correctly
    assert_tree_settings(
        &tree,
        &[
            ("name", "ComplexSettingsTree"),
            ("version", "1.0.0-beta.1+build.123"),
            (
                "description",
                "A tree with complex settings including special characters: !@#$%^&*()",
            ),
            ("json_config", r#"{"enabled": true, "max_items": 100}"#),
            ("url", "https://example.com/api/v1?param=value&other=data"),
            ("multiline", "Line 1\nLine 2\nLine 3"),
            ("unicode", "测试 Unicode 🚀"),
        ],
    )
    .await;
}

#[tokio::test]
async fn test_settings_persistence_across_operations() {
    let (_instance, mut user) = test_instance_with_user("persist_user").await;

    let tree = create_database_with_settings(&mut user, "PersistenceTest", "1.0").await;

    // Perform some operations that modify other subtrees
    create_user_profile(&tree, "user123", "John Doe", "john@example.com").await;
    create_app_config(&tree, "TestApp", &[("debug", "true"), ("port", "8080")]).await;

    // Verify settings are still intact after other operations
    assert_tree_settings(&tree, &[("name", "PersistenceTest"), ("version", "1.0")]).await;
    assert_tree_name(&tree, "PersistenceTest").await;

    // Add more settings
    set_tree_settings(
        &tree,
        &[("last_modified", "2023-01-01"), ("status", "active")],
    )
    .await;

    // Verify all settings coexist
    assert_tree_settings(
        &tree,
        &[
            ("name", "PersistenceTest"),
            ("version", "1.0"),
            ("last_modified", "2023-01-01"),
            ("status", "active"),
        ],
    )
    .await;
}

#[tokio::test]
async fn test_settings_in_multiple_trees() {
    let (_instance, mut user) = test_instance_with_user("multi_tree_user").await;

    // Create multiple trees with different settings
    let tree1 = create_database_with_settings(&mut user, "Tree1", "1.0").await;
    let tree2 = create_database_with_settings(&mut user, "Tree2", "2.0").await;
    let tree3 = create_database_with_settings(&mut user, "Tree3", "3.0").await;

    // Add unique settings to each
    set_tree_settings(&tree1, &[("purpose", "development"), ("team", "frontend")]).await;
    set_tree_settings(&tree2, &[("purpose", "staging"), ("team", "backend")]).await;
    set_tree_settings(&tree3, &[("purpose", "production"), ("team", "devops")]).await;

    // Verify each tree has its own settings
    assert_tree_settings(
        &tree1,
        &[
            ("name", "Tree1"),
            ("version", "1.0"),
            ("purpose", "development"),
            ("team", "frontend"),
        ],
    )
    .await;

    assert_tree_settings(
        &tree2,
        &[
            ("name", "Tree2"),
            ("version", "2.0"),
            ("purpose", "staging"),
            ("team", "backend"),
        ],
    )
    .await;

    assert_tree_settings(
        &tree3,
        &[
            ("name", "Tree3"),
            ("version", "3.0"),
            ("purpose", "production"),
            ("team", "devops"),
        ],
    )
    .await;
}

#[tokio::test]
async fn test_empty_and_edge_case_settings() {
    let (_instance, mut user) = test_instance_with_user("edge_user").await;

    let key_id = user
        .get_default_key()
        .expect("User should have default key");
    let tree = user
        .create_database(Doc::new(), &key_id)
        .await
        .expect("Failed to create tree");

    // Test edge case values
    set_tree_settings(
        &tree,
        &[
            ("empty_string", ""),
            ("whitespace", "   "),
            ("single_char", "x"),
            ("numbers", "12345"),
            ("boolean_string", "true"),
            ("null_string", "null"),
        ],
    )
    .await;

    // Verify edge case settings
    assert_tree_settings(
        &tree,
        &[
            ("empty_string", ""),
            ("whitespace", "   "),
            ("single_char", "x"),
            ("numbers", "12345"),
            ("boolean_string", "true"),
            ("null_string", "null"),
        ],
    )
    .await;
}
