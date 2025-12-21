//! Database management tests
//!
//! This module contains tests for User-level database discovery operations including
//! finding databases by name and handling multiple databases.
//!
//! Note: Instance-level database listing is internal. Use User::find_database for discovery.

use super::helpers::*;
use crate::helpers::test_instance_with_user;

#[tokio::test]
async fn test_create_and_find_databases() {
    let (_db, mut user) = test_instance_with_user("test_user").await;

    // Create databases with names so we can find them
    let database1 = create_database_with_settings(&mut user, "Database1", "1.0").await;
    let database2 = create_database_with_settings(&mut user, "Database2", "1.0").await;

    // Verify both can be found by name
    let found1 = user
        .find_database("Database1")
        .await
        .expect("Should find Database1");
    assert_eq!(found1.len(), 1);
    assert_eq!(found1[0].root_id(), database1.root_id());

    let found2 = user
        .find_database("Database2")
        .await
        .expect("Should find Database2");
    assert_eq!(found2.len(), 1);
    assert_eq!(found2[0].root_id(), database2.root_id());
}

#[tokio::test]
async fn test_find_database() {
    let (_db, mut user) = test_instance_with_user("find_user").await;

    // Use helper to set up trees for find testing
    setup_trees_for_find_testing(&mut user).await;

    // Test: Find non-existent name
    test_tree_not_found_error(&user, "NonExistent").await;

    // Test: Find unique name
    let found_unique = user
        .find_database("UniqueTree")
        .await
        .expect("find_database failed");
    assert_databases_count(&found_unique, 1);
    assert_tree_name(&found_unique[0], "UniqueTree").await;

    // Test: Find duplicate name
    let found_duplicate = user
        .find_database("DuplicateName")
        .await
        .expect("find_database failed");
    assert_databases_count(&found_duplicate, 2);

    // Check if both found trees have the name "DuplicateName"
    assert_tree_names_in_collection(&found_duplicate, &["DuplicateName"]).await;
}

#[tokio::test]
async fn test_find_database_edge_cases() {
    // Test: Database with trees but none matching
    let (_db, mut user) = test_instance_with_user("edge_user").await;
    create_database_with_settings(&mut user, "ExistingTree", "1.0").await;
    test_tree_not_found_error(&user, "NonMatchingName").await;
}

#[tokio::test]
async fn test_multiple_named_trees() {
    let (_db, mut user) = test_instance_with_user("multi_user").await;

    // Use helper to create multiple trees with specific names
    let names = &["Alpha", "Beta", "Gamma", "Delta"];
    let trees = create_multiple_named_trees(&mut user, names).await;

    assert_databases_count(&trees, 4);
    assert_tree_names_in_collection(&trees, names).await;

    // Verify each tree can be found individually
    for name in names {
        let found = user.find_database(name).await.expect("Failed to find tree");
        assert_databases_count(&found, 1);
        assert_tree_name(&found[0], name).await;
    }
}

#[tokio::test]
async fn test_tree_management_with_complex_scenarios() {
    let (_db, mut user) = test_instance_with_user("complex_user").await;

    // Create trees with overlapping names and different versions
    let tree1 = create_database_with_settings(&mut user, "MyApp", "1.0").await;
    let tree2 = create_database_with_settings(&mut user, "MyApp", "2.0").await;
    let tree3 = create_database_with_settings(&mut user, "MyApp", "2.1").await;
    let tree4 = create_database_with_settings(&mut user, "OtherApp", "1.0").await;

    // Test finding by name (should return multiple versions)
    let myapp_trees = user
        .find_database("MyApp")
        .await
        .expect("Failed to find MyApp trees");
    assert_databases_count(&myapp_trees, 3);

    let otherapp_trees = user
        .find_database("OtherApp")
        .await
        .expect("Failed to find OtherApp trees");
    assert_databases_count(&otherapp_trees, 1);

    // Verify each tree has correct settings
    assert_tree_settings(&tree1, &[("name", "MyApp"), ("version", "1.0")]).await;
    assert_tree_settings(&tree2, &[("name", "MyApp"), ("version", "2.0")]).await;
    assert_tree_settings(&tree3, &[("name", "MyApp"), ("version", "2.1")]).await;
    assert_tree_settings(&tree4, &[("name", "OtherApp"), ("version", "1.0")]).await;
}

#[tokio::test]
async fn test_concurrent_tree_creation() {
    let (_db, mut user) = test_instance_with_user("concurrent_user").await;

    // Use helper to test concurrent operations
    let concurrent_trees = test_concurrent_tree_operations(&mut user).await;

    assert_databases_count(&concurrent_trees, 5);

    // Verify all trees were created with expected naming pattern
    let expected_names = &[
        "ConcurrentTree0",
        "ConcurrentTree1",
        "ConcurrentTree2",
        "ConcurrentTree3",
        "ConcurrentTree4",
    ];
    assert_tree_names_in_collection(&concurrent_trees, expected_names).await;

    // Verify each tree has user data
    for (i, tree) in concurrent_trees.iter().enumerate() {
        let expected_user_data = &[
            ("user_id", &format!("user{i}")),
            ("name", &format!("User {i}")),
            ("email", &format!("user{i}@test.com")),
        ];

        // Convert the expected data to the format needed by assert_tree_data
        let user_data_str: Vec<(&str, &str)> = expected_user_data
            .iter()
            .map(|(k, v)| (*k, v.as_str()))
            .collect();
        assert_tree_data(tree, "user_data", &user_data_str).await;
    }
}

#[tokio::test]
async fn test_tree_metadata_management() {
    let (_db, mut user) = test_instance_with_user("metadata_user").await;

    // Create tree and update metadata
    let key_id = user
        .get_default_key()
        .expect("User should have default key");
    let tree = user
        .create_database(eidetica::crdt::Doc::new(), &key_id)
        .await
        .expect("Failed to create tree");

    update_tree_metadata(
        &tree,
        "MetadataTree",
        "2.1.3",
        "A tree for testing metadata",
    ).await;

    // Verify metadata was set correctly
    assert_tree_settings(
        &tree,
        &[
            ("name", "MetadataTree"),
            ("version", "2.1.3"),
            ("description", "A tree for testing metadata"),
        ],
    ).await;

    assert_tree_name(&tree, "MetadataTree").await;
}

#[tokio::test]
async fn test_tree_management_error_handling() {
    let (_db, user) = test_instance_with_user("error_user").await;

    // Test various error scenarios
    test_database_error_conditions(&user).await;

    // Test finding non-existent trees with different names
    let test_names = &["", "NonExistent", "Missing", "NotFound"];
    for name in test_names {
        test_tree_not_found_error(&user, name).await;
    }
}

#[tokio::test]
async fn test_tree_listing_and_searching() {
    let (_db, mut user) = test_instance_with_user("listing_user").await;

    // Create diverse set of trees
    let _tree1 = create_database_with_settings(&mut user, "ProductionApp", "3.0").await;
    let _tree2 = create_database_with_settings(&mut user, "StagingApp", "3.0-beta").await;
    let _tree3 = create_database_with_settings(&mut user, "DevelopmentApp", "3.1-alpha").await;

    let key_id = user
        .get_default_key()
        .expect("User should have default key");
    let _tree4 = user
        .create_database(eidetica::crdt::Doc::new(), &key_id)
        .await
        .expect("Failed to create unnamed tree");

    // Test finding specific trees
    let production = user
        .find_database("ProductionApp")
        .await
        .expect("Failed to find production");
    assert_databases_count(&production, 1);
    assert_tree_settings(
        &production[0],
        &[("name", "ProductionApp"), ("version", "3.0")],
    ).await;

    let staging = user
        .find_database("StagingApp")
        .await
        .expect("Failed to find staging");
    assert_databases_count(&staging, 1);
    assert_tree_settings(
        &staging[0],
        &[("name", "StagingApp"), ("version", "3.0-beta")],
    ).await;

    let development = user
        .find_database("DevelopmentApp")
        .await
        .expect("Failed to find development");
    assert_databases_count(&development, 1);
    assert_tree_settings(
        &development[0],
        &[("name", "DevelopmentApp"), ("version", "3.1-alpha")],
    ).await;
}
