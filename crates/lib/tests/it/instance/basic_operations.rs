//! Basic operation tests
//!
//! This module contains tests for basic Instance operations through trees including
//! subtree modifications, data operations, and integration with tree functionality.

use super::helpers::*;
use crate::helpers::test_instance_with_user;

#[tokio::test]
async fn test_basic_subtree_modification() {
    let (_instance, mut user) = test_instance_with_user("test_user").await;
    let tree = create_database_with_default_key(&mut user).await;

    // Use helper to perform basic subtree operations
    let user_data = &[("user_id", "alice"), ("email", "alice@example.com")];
    perform_basic_subtree_operations(&tree, "user_data", user_data).await;

    // Verify the data was set correctly using helper
    assert_tree_data(&tree, "user_data", user_data).await;
}

#[tokio::test]
async fn test_subtree_operations_using_helpers() {
    let (_instance, mut user) = test_instance_with_user("helper_user").await;
    let tree = create_database_with_default_key(&mut user).await;

    // Use helper to perform basic operations
    let user_operations = &[
        ("user_id", "helper_user"),
        ("name", "Helper User"),
        ("email", "helper@example.com"),
        ("role", "tester"),
    ];

    perform_basic_subtree_operations(&tree, "user_data", user_operations).await;

    // Verify operations were applied
    assert_tree_data(&tree, "user_data", user_operations).await;
}

#[tokio::test]
async fn test_user_profile_creation() {
    let (_instance, mut user) = test_instance_with_user("profile_user").await;
    let tree = create_database_with_default_key(&mut user).await;

    // Use helper to create user profile
    create_user_profile(&tree, "john123", "John Smith", "john@company.com").await;

    // Verify user profile was created
    assert_tree_data(
        &tree,
        "user_data",
        &[
            ("user_id", "john123"),
            ("name", "John Smith"),
            ("email", "john@company.com"),
        ],
    )
    .await;
}

#[tokio::test]
async fn test_application_configuration() {
    let (_instance, mut user) = test_instance_with_user("config_user").await;
    let tree = create_database_with_default_key(&mut user).await;

    // Use helper to create app config
    let config_data = &[
        ("debug_mode", "true"),
        ("max_connections", "100"),
        ("timeout", "30"),
        ("log_level", "info"),
    ];

    create_app_config(&tree, "TestApplication", config_data).await;

    // Verify app config was created
    let mut expected_config = vec![("app_name", "TestApplication")];
    expected_config.extend_from_slice(config_data);
    assert_tree_data(&tree, "app_config", &expected_config).await;
}

#[tokio::test]
async fn test_multiple_subtree_operations() {
    let (_instance, mut user) = test_instance_with_user("multi_user").await;
    let tree = create_database_with_default_key(&mut user).await;

    // Perform operations on multiple subtrees
    create_user_profile(&tree, "multi_user", "Multi User", "multi@test.com").await;
    create_app_config(&tree, "MultiApp", &[("version", "1.0"), ("env", "test")]).await;

    // Add custom data to another subtree
    perform_basic_subtree_operations(
        &tree,
        "custom_data",
        &[("setting1", "value1"), ("setting2", "value2")],
    )
    .await;

    // Verify all subtrees have correct data
    assert_tree_data(
        &tree,
        "user_data",
        &[
            ("user_id", "multi_user"),
            ("name", "Multi User"),
            ("email", "multi@test.com"),
        ],
    )
    .await;

    assert_tree_data(
        &tree,
        "app_config",
        &[
            ("app_name", "MultiApp"),
            ("version", "1.0"),
            ("env", "test"),
        ],
    )
    .await;

    assert_tree_data(
        &tree,
        "custom_data",
        &[("setting1", "value1"), ("setting2", "value2")],
    )
    .await;
}

#[tokio::test]
async fn test_data_persistence_across_operations() {
    let (_instance, mut user) = test_instance_with_user("persist_user").await;
    let tree = create_database_with_default_key(&mut user).await;

    // First operation: create user
    create_user_profile(
        &tree,
        "persistent_user",
        "Persistent User",
        "persist@test.com",
    )
    .await;

    // Second operation: add more user data
    perform_basic_subtree_operations(
        &tree,
        "user_data",
        &[("last_login", "2023-01-01"), ("preferences", "dark_mode")],
    )
    .await;

    // Third operation: create separate config
    create_app_config(&tree, "PersistentApp", &[("theme", "dark")]).await;

    // Verify all data persists and coexists
    assert_tree_data(
        &tree,
        "user_data",
        &[
            ("user_id", "persistent_user"),
            ("name", "Persistent User"),
            ("email", "persist@test.com"),
            ("last_login", "2023-01-01"),
            ("preferences", "dark_mode"),
        ],
    )
    .await;

    assert_tree_data(
        &tree,
        "app_config",
        &[("app_name", "PersistentApp"), ("theme", "dark")],
    )
    .await;
}

#[tokio::test]
async fn test_data_operations_with_special_characters() {
    let (_instance, mut user) = test_instance_with_user("special_user").await;
    let tree = create_database_with_default_key(&mut user).await;

    // Test data with special characters
    let special_data = &[
        ("json_data", r#"{"key": "value", "number": 42}"#),
        (
            "html_content",
            "<h1>Title</h1><p>Content with &amp; entities</p>",
        ),
        ("unicode_text", "测试 Unicode 🚀 emoji support"),
        (
            "escaped_quotes",
            r#"String with "quotes" and 'apostrophes'"#,
        ),
        ("multiline", "Line 1\nLine 2\nLine 3"),
        ("special_chars", "!@#$%^&*()_+-=[]{}|;:,.<>?"),
    ];

    perform_basic_subtree_operations(&tree, "special_data", special_data).await;

    // Verify special character data was stored correctly
    assert_tree_data(&tree, "special_data", special_data).await;
}

#[tokio::test]
async fn test_data_overwrite_operations() {
    let (_instance, mut user) = test_instance_with_user("overwrite_user").await;
    let tree = create_database_with_default_key(&mut user).await;

    // Initial data
    perform_basic_subtree_operations(
        &tree,
        "overwrite_test",
        &[
            ("key1", "initial_value1"),
            ("key2", "initial_value2"),
            ("key3", "initial_value3"),
        ],
    )
    .await;

    // Verify initial data
    assert_tree_data(
        &tree,
        "overwrite_test",
        &[
            ("key1", "initial_value1"),
            ("key2", "initial_value2"),
            ("key3", "initial_value3"),
        ],
    )
    .await;

    // Overwrite some values and add new ones
    perform_basic_subtree_operations(
        &tree,
        "overwrite_test",
        &[
            ("key1", "updated_value1"),
            ("key3", "updated_value3"),
            ("key4", "new_value4"),
        ],
    )
    .await;

    // Verify updated data (key2 should remain, others updated/added)
    assert_tree_data(
        &tree,
        "overwrite_test",
        &[
            ("key1", "updated_value1"),
            ("key2", "initial_value2"),
            ("key3", "updated_value3"),
            ("key4", "new_value4"),
        ],
    )
    .await;
}

#[tokio::test]
async fn test_large_data_operations() {
    let (_instance, mut user) = test_instance_with_user("large_user").await;
    let tree = create_database_with_default_key(&mut user).await;

    // Create large dataset
    let mut large_data = Vec::new();
    for i in 0..100 {
        large_data.push((format!("key_{i:03}"), format!("value_{i:03}")));
    }

    // Convert to string references for the helper function
    let large_data_refs: Vec<(&str, &str)> = large_data
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    perform_basic_subtree_operations(&tree, "large_dataset", &large_data_refs).await;

    // Verify a subset of the large data
    let sample_data = &[
        ("key_000", "value_000"),
        ("key_025", "value_025"),
        ("key_050", "value_050"),
        ("key_075", "value_075"),
        ("key_099", "value_099"),
    ];

    assert_tree_data(&tree, "large_dataset", sample_data).await;
}

#[tokio::test]
async fn test_empty_and_edge_case_operations() {
    let (_instance, mut user) = test_instance_with_user("edge_user").await;
    let tree = create_database_with_default_key(&mut user).await;

    // Test with edge case values
    let edge_cases = &[
        ("empty_value", ""),
        ("whitespace_only", "   "),
        ("single_space", " "),
        ("tab_character", "\t"),
        ("newline_character", "\n"),
        ("zero", "0"),
        ("negative", "-1"),
        ("decimal", "3.14159"),
    ];

    perform_basic_subtree_operations(&tree, "edge_cases", edge_cases).await;

    // Verify edge case data
    assert_tree_data(&tree, "edge_cases", edge_cases).await;
}

#[tokio::test]
async fn test_basic_operations_integration() {
    let (_instance, mut user) = test_instance_with_user("integration_user").await;

    // Create tree with initial settings
    let tree = create_database_with_settings(&mut user, "IntegrationTree", "1.0").await;

    // Add user profile
    create_user_profile(
        &tree,
        "integration_user",
        "Integration User",
        "integration@test.com",
    )
    .await;

    // Add app configuration
    create_app_config(
        &tree,
        "IntegrationApp",
        &[("environment", "testing"), ("features", "all_enabled")],
    )
    .await;

    // Add custom business data
    perform_basic_subtree_operations(
        &tree,
        "business_data",
        &[
            ("department", "engineering"),
            ("project", "integration_tests"),
            ("status", "active"),
        ],
    )
    .await;

    // Verify the complete integration
    assert_tree_name(&tree, "IntegrationTree").await;
    assert_tree_settings(&tree, &[("name", "IntegrationTree"), ("version", "1.0")]).await;

    assert_tree_data(
        &tree,
        "user_data",
        &[
            ("user_id", "integration_user"),
            ("name", "Integration User"),
            ("email", "integration@test.com"),
        ],
    )
    .await;

    assert_tree_data(
        &tree,
        "app_config",
        &[
            ("app_name", "IntegrationApp"),
            ("environment", "testing"),
            ("features", "all_enabled"),
        ],
    )
    .await;

    assert_tree_data(
        &tree,
        "business_data",
        &[
            ("department", "engineering"),
            ("project", "integration_tests"),
            ("status", "active"),
        ],
    )
    .await;
}
