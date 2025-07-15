//! Tree management tests
//!
//! This module contains tests for tree management operations including
//! tree listing, finding trees by name, and handling multiple trees.

use super::helpers::*;
use crate::helpers::setup_db_with_key;
use eidetica::backend::database::InMemory;
use eidetica::basedb::BaseDB;

const TEST_KEY: &str = "test_key";

#[test]
fn test_all_trees() {
    let db = setup_db_with_key(TEST_KEY);

    let tree1 = db
        .new_tree_default(TEST_KEY)
        .expect("Failed to create tree 1");
    let root_id1 = tree1.root_id().clone();

    let tree2 = create_tree_with_settings(&db, TEST_KEY, "Tree2", "1.0");
    let root_id2 = tree2.root_id().clone();

    let trees: Vec<eidetica::Tree> = db.all_trees().expect("Failed to get all trees");
    assert_trees_count(&trees, 2);

    let expected_ids = vec![root_id1, root_id2];
    assert_trees_contain_ids(&trees, &expected_ids);
}

#[test]
fn test_find_tree() {
    let db = setup_db_with_key(TEST_KEY);

    // Use helper to set up trees for find testing
    setup_trees_for_find_testing(&db, TEST_KEY);

    // Test: Find non-existent name
    test_tree_not_found_error(&db, "NonExistent");

    // Test: Find unique name
    let found_unique = db.find_tree("UniqueTree").expect("find_tree failed");
    assert_trees_count(&found_unique, 1);
    assert_tree_name(&found_unique[0], "UniqueTree");

    // Test: Find duplicate name
    let found_duplicate = db.find_tree("DuplicateName").expect("find_tree failed");
    assert_trees_count(&found_duplicate, 2);

    // Check if both found trees have the name "DuplicateName"
    assert_tree_names_in_collection(&found_duplicate, &["DuplicateName"]);
}

#[test]
fn test_find_tree_edge_cases() {
    // Test: Find when no trees exist
    let empty_backend = Box::new(InMemory::new());
    let empty_db = BaseDB::new(empty_backend);
    test_tree_not_found_error(&empty_db, "AnyName");

    // Test: Database with trees but none matching
    let db = setup_db_with_key(TEST_KEY);
    create_tree_with_settings(&db, TEST_KEY, "ExistingTree", "1.0");
    test_tree_not_found_error(&db, "NonMatchingName");
}

#[test]
fn test_multiple_named_trees() {
    let db = setup_db_with_key(TEST_KEY);

    // Use helper to create multiple trees with specific names
    let names = &["Alpha", "Beta", "Gamma", "Delta"];
    let trees = create_multiple_named_trees(&db, TEST_KEY, names);

    assert_trees_count(&trees, 4);
    assert_tree_names_in_collection(&trees, names);

    // Verify each tree can be found individually
    for name in names {
        let found = db.find_tree(name).expect("Failed to find tree");
        assert_trees_count(&found, 1);
        assert_tree_name(&found[0], name);
    }
}

#[test]
fn test_tree_management_with_complex_scenarios() {
    let db = setup_db_with_key(TEST_KEY);

    // Create trees with overlapping names and different versions
    let tree1 = create_tree_with_settings(&db, TEST_KEY, "MyApp", "1.0");
    let tree2 = create_tree_with_settings(&db, TEST_KEY, "MyApp", "2.0");
    let tree3 = create_tree_with_settings(&db, TEST_KEY, "MyApp", "2.1");
    let tree4 = create_tree_with_settings(&db, TEST_KEY, "OtherApp", "1.0");

    // Test finding by name (should return multiple versions)
    let myapp_trees = db.find_tree("MyApp").expect("Failed to find MyApp trees");
    assert_trees_count(&myapp_trees, 3);

    let otherapp_trees = db
        .find_tree("OtherApp")
        .expect("Failed to find OtherApp trees");
    assert_trees_count(&otherapp_trees, 1);

    // Test all_trees includes everything
    let all_trees = db.all_trees().expect("Failed to get all trees");
    assert_trees_count(&all_trees, 4);

    // Verify each tree has correct settings
    assert_tree_settings(&tree1, &[("name", "MyApp"), ("version", "1.0")]);
    assert_tree_settings(&tree2, &[("name", "MyApp"), ("version", "2.0")]);
    assert_tree_settings(&tree3, &[("name", "MyApp"), ("version", "2.1")]);
    assert_tree_settings(&tree4, &[("name", "OtherApp"), ("version", "1.0")]);
}

#[test]
fn test_concurrent_tree_creation() {
    let db = setup_db_with_key(TEST_KEY);

    // Use helper to test concurrent operations
    let concurrent_trees = test_concurrent_tree_operations(&db, TEST_KEY);

    assert_trees_count(&concurrent_trees, 5);

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
    let db = setup_db_with_key(TEST_KEY);

    // Create tree and update metadata
    let tree = db
        .new_tree_default(TEST_KEY)
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
    let db = setup_db_with_key(TEST_KEY);

    // Test various error scenarios
    test_database_error_conditions(&db);

    // Test finding non-existent trees with different names
    let test_names = &["", "NonExistent", "Missing", "NotFound"];
    for name in test_names {
        test_tree_not_found_error(&db, name);
    }
}

#[test]
fn test_tree_listing_and_searching() {
    let db = setup_db_with_key(TEST_KEY);

    // Create diverse set of trees
    let _tree1 = create_tree_with_settings(&db, TEST_KEY, "ProductionApp", "3.0");
    let _tree2 = create_tree_with_settings(&db, TEST_KEY, "StagingApp", "3.0-beta");
    let _tree3 = create_tree_with_settings(&db, TEST_KEY, "DevelopmentApp", "3.1-alpha");
    let _tree4 = db
        .new_tree_default(TEST_KEY)
        .expect("Failed to create unnamed tree");

    // Test all_trees functionality
    let all_trees = db.all_trees().expect("Failed to get all trees");
    assert_trees_count(&all_trees, 4);

    // Test finding specific trees
    let production = db
        .find_tree("ProductionApp")
        .expect("Failed to find production");
    assert_trees_count(&production, 1);
    assert_tree_settings(
        &production[0],
        &[("name", "ProductionApp"), ("version", "3.0")],
    );

    let staging = db.find_tree("StagingApp").expect("Failed to find staging");
    assert_trees_count(&staging, 1);
    assert_tree_settings(
        &staging[0],
        &[("name", "StagingApp"), ("version", "3.0-beta")],
    );

    let development = db
        .find_tree("DevelopmentApp")
        .expect("Failed to find development");
    assert_trees_count(&development, 1);
    assert_tree_settings(
        &development[0],
        &[("name", "DevelopmentApp"), ("version", "3.1-alpha")],
    );
}
