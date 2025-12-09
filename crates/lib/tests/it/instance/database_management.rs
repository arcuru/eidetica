//! Database management tests
//!
//! This module contains tests for User-level database discovery operations including
//! finding databases by name and handling multiple databases.
//!
//! Note: Instance-level database listing is internal. Use User::find_database for discovery.

use super::helpers::*;
use crate::helpers::test_instance_with_user;

#[test]
fn test_create_and_find_databases() {
    let (_db, mut user) = test_instance_with_user("test_user");

    // Create databases with names so we can find them
    let database1 = create_database_with_settings(&mut user, "Database1", "1.0");
    let database2 = create_database_with_settings(&mut user, "Database2", "1.0");

    // Verify both can be found by name
    let found1 = user
        .find_database("Database1")
        .expect("Should find Database1");
    assert_eq!(found1.len(), 1);
    assert_eq!(found1[0].root_id(), database1.root_id());

    let found2 = user
        .find_database("Database2")
        .expect("Should find Database2");
    assert_eq!(found2.len(), 1);
    assert_eq!(found2[0].root_id(), database2.root_id());
}

#[test]
fn test_find_database() {
    let (_db, mut user) = test_instance_with_user("find_user");

    // Use helper to set up trees for find testing
    setup_trees_for_find_testing(&mut user);

    // Test: Find non-existent name
    test_tree_not_found_error(&user, "NonExistent");

    // Test: Find unique name
    let found_unique = user
        .find_database("UniqueTree")
        .expect("find_database failed");
    assert_databases_count(&found_unique, 1);
    assert_tree_name(&found_unique[0], "UniqueTree");

    // Test: Find duplicate name
    let found_duplicate = user
        .find_database("DuplicateName")
        .expect("find_database failed");
    assert_databases_count(&found_duplicate, 2);

    // Check if both found trees have the name "DuplicateName"
    assert_tree_names_in_collection(&found_duplicate, &["DuplicateName"]);
}

#[test]
fn test_find_database_edge_cases() {
    // Test: Database with trees but none matching
    let (_db, mut user) = test_instance_with_user("edge_user");
    create_database_with_settings(&mut user, "ExistingTree", "1.0");
    test_tree_not_found_error(&user, "NonMatchingName");
}

#[test]
fn test_multiple_named_trees() {
    let (_db, mut user) = test_instance_with_user("multi_user");

    // Use helper to create multiple trees with specific names
    let names = &["Alpha", "Beta", "Gamma", "Delta"];
    let trees = create_multiple_named_trees(&mut user, names);

    assert_databases_count(&trees, 4);
    assert_tree_names_in_collection(&trees, names);

    // Verify each tree can be found individually
    for name in names {
        let found = user.find_database(name).expect("Failed to find tree");
        assert_databases_count(&found, 1);
        assert_tree_name(&found[0], name);
    }
}

#[test]
fn test_tree_management_with_complex_scenarios() {
    let (_db, mut user) = test_instance_with_user("complex_user");

    // Create trees with overlapping names and different versions
    let tree1 = create_database_with_settings(&mut user, "MyApp", "1.0");
    let tree2 = create_database_with_settings(&mut user, "MyApp", "2.0");
    let tree3 = create_database_with_settings(&mut user, "MyApp", "2.1");
    let tree4 = create_database_with_settings(&mut user, "OtherApp", "1.0");

    // Test finding by name (should return multiple versions)
    let myapp_trees = user
        .find_database("MyApp")
        .expect("Failed to find MyApp trees");
    assert_databases_count(&myapp_trees, 3);

    let otherapp_trees = user
        .find_database("OtherApp")
        .expect("Failed to find OtherApp trees");
    assert_databases_count(&otherapp_trees, 1);

    // Verify each tree has correct settings
    assert_tree_settings(&tree1, &[("name", "MyApp"), ("version", "1.0")]);
    assert_tree_settings(&tree2, &[("name", "MyApp"), ("version", "2.0")]);
    assert_tree_settings(&tree3, &[("name", "MyApp"), ("version", "2.1")]);
    assert_tree_settings(&tree4, &[("name", "OtherApp"), ("version", "1.0")]);
}

#[test]
fn test_concurrent_tree_creation() {
    let (_db, mut user) = test_instance_with_user("concurrent_user");

    // Use helper to test concurrent operations
    let concurrent_trees = test_concurrent_tree_operations(&mut user);

    assert_databases_count(&concurrent_trees, 5);

    // Verify all trees were created with expected naming pattern
    let expected_names = &[
        "ConcurrentTree0",
        "ConcurrentTree1",
        "ConcurrentTree2",
        "ConcurrentTree3",
        "ConcurrentTree4",
    ];
    assert_tree_names_in_collection(&concurrent_trees, expected_names);

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
        assert_tree_data(tree, "user_data", &user_data_str);
    }
}

#[test]
fn test_tree_metadata_management() {
    let (_db, mut user) = test_instance_with_user("metadata_user");

    // Create tree and update metadata
    let key_id = user
        .get_default_key()
        .expect("User should have default key");
    let tree = user
        .create_database(eidetica::crdt::Doc::new(), &key_id)
        .expect("Failed to create tree");

    update_tree_metadata(
        &tree,
        "MetadataTree",
        "2.1.3",
        "A tree for testing metadata",
    );

    // Verify metadata was set correctly
    assert_tree_settings(
        &tree,
        &[
            ("name", "MetadataTree"),
            ("version", "2.1.3"),
            ("description", "A tree for testing metadata"),
        ],
    );

    assert_tree_name(&tree, "MetadataTree");
}

#[test]
fn test_tree_management_error_handling() {
    let (_db, user) = test_instance_with_user("error_user");

    // Test various error scenarios
    test_database_error_conditions(&user);

    // Test finding non-existent trees with different names
    let test_names = &["", "NonExistent", "Missing", "NotFound"];
    for name in test_names {
        test_tree_not_found_error(&user, name);
    }
}

#[test]
fn test_tree_listing_and_searching() {
    let (_db, mut user) = test_instance_with_user("listing_user");

    // Create diverse set of trees
    let _tree1 = create_database_with_settings(&mut user, "ProductionApp", "3.0");
    let _tree2 = create_database_with_settings(&mut user, "StagingApp", "3.0-beta");
    let _tree3 = create_database_with_settings(&mut user, "DevelopmentApp", "3.1-alpha");

    let key_id = user
        .get_default_key()
        .expect("User should have default key");
    let _tree4 = user
        .create_database(eidetica::crdt::Doc::new(), &key_id)
        .expect("Failed to create unnamed tree");

    // Test finding specific trees
    let production = user
        .find_database("ProductionApp")
        .expect("Failed to find production");
    assert_databases_count(&production, 1);
    assert_tree_settings(
        &production[0],
        &[("name", "ProductionApp"), ("version", "3.0")],
    );

    let staging = user
        .find_database("StagingApp")
        .expect("Failed to find staging");
    assert_databases_count(&staging, 1);
    assert_tree_settings(
        &staging[0],
        &[("name", "StagingApp"), ("version", "3.0-beta")],
    );

    let development = user
        .find_database("DevelopmentApp")
        .expect("Failed to find development");
    assert_databases_count(&development, 1);
    assert_tree_settings(
        &development[0],
        &[("name", "DevelopmentApp"), ("version", "3.1-alpha")],
    );
}
