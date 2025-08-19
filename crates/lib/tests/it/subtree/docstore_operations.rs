//! DocStore subtree operation tests
//!
//! This module contains tests for DocStore subtree functionality including
//! basic CRUD operations, path-based access, nested values, and persistence.

use super::helpers::*;
use crate::helpers::*;
use eidetica::crdt::Doc;
use eidetica::crdt::map::Value;
use eidetica::subtree::DocStore;

#[test]
fn test_dict_set_and_get_via_op() {
    let tree = setup_tree();

    // Use helper to create initial data
    let initial_data = &[("key1", "value1"), ("key2", "value2")];
    create_dict_operation(&tree, "my_kv", initial_data);

    // Test operation-level modifications
    let op = tree.new_operation().expect("Failed to start operation");
    let dict = op
        .get_subtree::<DocStore>("my_kv")
        .expect("Failed to get Doc");

    // Verify initial values are accessible
    assert_dict_value(&dict, "key1", "value1");
    assert_dict_value(&dict, "key2", "value2");

    // Test get_string convenience method
    assert_eq!(
        dict.get_string("key1").expect("Failed get_string key1"),
        "value1"
    );
    assert_eq!(
        dict.get_string("key2").expect("Failed get_string key2"),
        "value2"
    );

    // Test overwrite
    dict.set("key1", "value1_updated")
        .expect("Failed to overwrite key1");
    assert_dict_value(&dict, "key1", "value1_updated");

    // Test non-existent key
    assert_key_not_found(dict.get("non_existent"));

    op.commit().expect("Failed to commit operation");

    // Verify final state using helper
    let expected_final = &[("key1", "value1_updated"), ("key2", "value2")];
    assert_dict_viewer_data(&tree, "my_kv", expected_final);
}

#[test]
fn test_dict_get_all_via_viewer() {
    let tree = setup_tree();

    // Test dict persistence helper
    test_dict_persistence(&tree, "my_kv");

    // Verify get_all using a viewer
    let viewer = tree
        .get_subtree_viewer::<DocStore>("my_kv")
        .expect("Failed to get viewer");
    let all_data_crdt = viewer.get_all().expect("Failed to get all data");
    let all_data_map = all_data_crdt.as_hashmap();

    assert_eq!(all_data_map.len(), 3);
    assert_eq!(
        all_data_map.get("key_a"),
        Some(&Value::Text("val_a".to_string()))
    );
    assert_eq!(
        all_data_map.get("key_b"),
        Some(&Value::Text("val_b_updated".to_string()))
    );
    assert_eq!(
        all_data_map.get("key_c"),
        Some(&Value::Text("val_c".to_string()))
    );
}

#[test]
fn test_dict_get_all_empty() {
    let tree = setup_tree();

    // Get viewer for a non-existent subtree
    let viewer = tree
        .get_subtree_viewer::<DocStore>("empty_kv")
        .expect("Failed to get viewer for empty");
    let all_data_crdt = viewer.get_all().expect("Failed to get all data from empty");
    let all_data_map = all_data_crdt.as_hashmap();

    assert!(all_data_map.is_empty());
}

#[test]
fn test_dict_delete() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let dict = op
            .get_subtree::<DocStore>("my_kv")
            .expect("Failed to get Doc");

        // Set initial values
        dict.set("key1", "value1").expect("Failed to set key1");
        dict.set("key2", "value2").expect("Failed to set key2");

        // Delete a key
        dict.delete("key1").expect("Failed to delete key1");

        // Verify key1 is deleted
        assert_key_not_found(dict.get("key1"));

        // key2 should still be accessible
        assert_dict_value(&dict, "key2", "value2");
    }

    // Commit the operation
    op.commit().expect("Failed to commit operation");

    // Verify the deletion persisted
    let viewer = tree
        .get_subtree_viewer::<DocStore>("my_kv")
        .expect("Failed to get viewer");
    assert_key_not_found(viewer.get("key1"));

    assert_dict_value(&viewer, "key2", "value2");
}

#[test]
fn test_dict_set_value() {
    let tree = setup_tree();

    // Use helper to create nested map operation
    create_dict_with_nested_map(&tree, "my_kv");

    // Get viewer to verify persistence
    let viewer = tree
        .get_subtree_viewer::<DocStore>("my_kv")
        .expect("Failed to get viewer");

    // Check string value persisted
    assert_dict_value(&viewer, "key1", "value1");

    // Check nested map structure
    assert_dict_nested_map(&viewer, "key2", &[("inner", "nested_value")]);
}

#[test]
fn test_dict_list_basic_operations() {
    let tree = setup_tree();

    // Use helper to create Doc with list
    let list_items = &["apple", "banana", "orange"];
    create_dict_with_list(&tree, "my_kv", list_items);

    // Verify with viewer
    assert_dict_list_data(&tree, "my_kv", "fruits", list_items);
}

#[test]
fn test_dict_list_nonexistent_key() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let dict = op
            .get_subtree::<DocStore>("my_kv")
            .expect("Failed to get Doc");

        // Test getting non-existent list should return NotFound error
        assert_key_not_found(dict.get("nonexistent"));

        // Test getting non-existent list with get_list should also return NotFound
        let list_result = dict.get_list("nonexistent");
        assert!(list_result.is_err());

        // Create a new list
        let mut new_list = eidetica::crdt::map::List::new();
        new_list.push(Value::Text("first_item".to_string()));

        dict.set_list("new_list", new_list)
            .expect("Failed to set new list");

        // Verify the new list was created
        let retrieved_list = dict.get_list("new_list").expect("Failed to get new list");
        assert_eq!(retrieved_list.len(), 1);
        assert_eq!(
            retrieved_list.get(0),
            Some(&Value::Text("first_item".to_string()))
        );
    }
}

#[test]
fn test_dict_list_persistence() {
    let tree = setup_tree();

    // Create list in first operation
    let op1 = tree.new_operation().expect("Failed to start op1");
    {
        let dict = op1
            .get_subtree::<DocStore>("my_kv")
            .expect("Failed to get Doc");

        let mut colors = eidetica::crdt::map::List::new();
        colors.push(Value::Text("red".to_string()));
        colors.push(Value::Text("green".to_string()));

        dict.set_list("colors", colors)
            .expect("Failed to set colors list");
    }
    op1.commit().expect("Failed to commit op1");

    // Modify list in second operation
    let op2 = tree.new_operation().expect("Failed to start op2");
    {
        let dict = op2
            .get_subtree::<DocStore>("my_kv")
            .expect("Failed to get Doc");

        // List should persist from previous operation
        let colors = dict.get_list("colors").expect("Failed to get colors list");
        assert_eq!(colors.len(), 2);
        assert_eq!(colors.get(0), Some(&Value::Text("red".to_string())));
        assert_eq!(colors.get(1), Some(&Value::Text("green".to_string())));

        // Modify the list - remove first element and add blue
        let mut updated_colors = colors.clone();
        updated_colors.remove(0); // Remove red
        updated_colors.push(Value::Text("blue".to_string())); // Add blue

        dict.set_list("colors", updated_colors)
            .expect("Failed to update colors list");
    }
    op2.commit().expect("Failed to commit op2");

    // Verify final state
    assert_dict_list_data(&tree, "my_kv", "colors", &["green", "blue"]);
}

#[test]
fn test_dict_update_nested_value() {
    let tree = setup_tree();

    // First operation: Create initial nested structure
    let op1 = tree.new_operation().expect("Op1: Failed to start");
    {
        let dict = op1
            .get_subtree::<DocStore>("nested_test")
            .expect("Op1: Failed to get Doc");

        // Create level1 -> level2_str structure
        let mut l1_map = Doc::new();
        l1_map.set_string("level2_str", "initial_value");
        dict.set_value("level1", l1_map)
            .expect("Op1: Failed to set level1");
    }
    op1.commit().expect("Op1: Failed to commit");

    // Second operation: Update with another structure
    let op2 = tree.new_operation().expect("Op2: Failed to start");
    {
        let dict = op2
            .get_subtree::<DocStore>("nested_test")
            .expect("Op2: Failed to get Doc");

        // Create an entirely new map structure that will replace the old one
        let mut l2_map = Doc::new();
        l2_map.set_string("deep_key", "deep_value");

        let mut new_l1_map = Doc::new();
        new_l1_map.set_map("level2_map", l2_map);

        // Completely replace the previous value at level1
        dict.set_value("level1", new_l1_map.clone())
            .expect("Op2: Failed to overwrite level1");

        // Verify the update within the same operation
        match dict.get("level1").expect("Failed to get level1") {
            Value::Node(retrieved_l1_map) => {
                // Check if level2_map exists with the expected content
                match retrieved_l1_map.get("level2_map") {
                    Some(Value::Node(retrieved_l2_map)) => match retrieved_l2_map.get("deep_key") {
                        Some(Value::Text(val)) => assert_eq!(val, "deep_value"),
                        _ => panic!("Expected string 'deep_value' at deep_key"),
                    },
                    _ => panic!("Expected 'level2_map' to be a map"),
                }
            }
            _ => panic!("Expected 'level1' to be a map"),
        }
    }
    op2.commit().expect("Op2: Failed to commit");

    // Verify the update persists after commit
    let viewer = tree
        .get_subtree_viewer::<DocStore>("nested_test")
        .expect("Failed to get viewer");

    // Verify the structure after commit
    match viewer.get("level1").expect("Viewer: Failed to get level1") {
        Value::Node(retrieved_l1_map) => {
            // Check if level2_map exists with expected content
            match retrieved_l1_map.get("level2_map") {
                Some(Value::Node(retrieved_l2_map)) => match retrieved_l2_map.get("deep_key") {
                    Some(Value::Text(val)) => assert_eq!(val, "deep_value"),
                    _ => panic!("Viewer: Expected string 'deep_value' at deep_key"),
                },
                _ => panic!("Viewer: Expected 'level2_map' to be a map"),
            }
        }
        _ => panic!("Viewer: Expected 'level1' to be a map"),
    }
}

#[test]
fn test_dict_comprehensive_operations() {
    let tree = setup_tree();
    let op = tree.new_operation().expect("Failed to start operation");

    {
        let dict = op
            .get_subtree::<DocStore>("test_store")
            .expect("Failed to get Doc");

        // Set basic string values
        dict.set("key1", "value1").expect("Failed to set key1");
        dict.set("key2", "value2").expect("Failed to set key2");

        // Set a nested map value
        let mut nested = Doc::new();
        nested.set_string("nested_key1", "nested_value1");
        nested.set_string("nested_key2", "nested_value2");
        dict.set_value("nested", Value::Node(nested.clone().into()))
            .expect("Failed to set nested map");
    }

    // Commit the operation
    op.commit().expect("Failed to commit operation");

    // Get a viewer to check the subtree
    let viewer = tree
        .get_subtree_viewer::<DocStore>("test_store")
        .expect("Failed to get viewer");

    // Check string values
    assert_dict_value(&viewer, "key1", "value1");
    assert_dict_value(&viewer, "key2", "value2");

    // Check nested map
    assert_dict_nested_map(
        &viewer,
        "nested",
        &[
            ("nested_key1", "nested_value1"),
            ("nested_key2", "nested_value2"),
        ],
    );

    // Check non-existent key
    assert_key_not_found(viewer.get("non_existent"));
}

#[test]
fn test_empty_dict_behavior() {
    let tree = setup_tree();

    // Test empty Doc behavior
    assert_dict_viewer_count(&tree, "empty_dict", 0);

    let dict_viewer = tree
        .get_subtree_viewer::<DocStore>("empty_dict")
        .expect("Failed to get empty Doc viewer");
    assert_key_not_found(dict_viewer.get("any_key"));
}

#[test]
fn test_docstore_path_based_access() {
    let tree = setup_tree();

    // Create operation and set up nested data structure
    let op = tree.new_operation().expect("Failed to start operation");
    let dict = op
        .get_subtree::<DocStore>("path_test")
        .expect("Failed to get DocStore");

    // Set up mixed structure - some direct, some that would be path-accessible
    dict.set("top_level", "root_value")
        .expect("Failed to set top_level");
    dict.set("counter", 42).expect("Failed to set counter");

    // Create nested structure by setting a Doc with nested data
    let mut user_doc = Doc::new();
    user_doc.set("name", "Alice");
    user_doc.set("age", 30);

    let mut profile_doc = Doc::new();
    profile_doc.set("email", "alice@example.com");
    profile_doc.set("verified", true);
    user_doc.set("profile", Value::Node(profile_doc.into()));

    dict.set("user", Value::Node(user_doc.into()))
        .expect("Failed to set user");

    // Test get_path() for various path levels

    // Top-level path access (equivalent to direct access)
    let top_value = dict
        .get_path("top_level")
        .expect("Failed to get top_level path");
    assert_eq!(top_value, Value::Text("root_value".to_string()));

    let counter_value = dict
        .get_path("counter")
        .expect("Failed to get counter path");
    assert_eq!(counter_value, Value::Int(42));

    // Nested path access
    let user_name = dict
        .get_path("user.name")
        .expect("Failed to get user.name path");
    assert_eq!(user_name, Value::Text("Alice".to_string()));

    let user_age = dict
        .get_path("user.age")
        .expect("Failed to get user.age path");
    assert_eq!(user_age, Value::Int(30));

    // Deep nested path access
    let user_email = dict
        .get_path("user.profile.email")
        .expect("Failed to get user.profile.email path");
    assert_eq!(user_email, Value::Text("alice@example.com".to_string()));

    let user_verified = dict
        .get_path("user.profile.verified")
        .expect("Failed to get user.profile.verified path");
    assert_eq!(user_verified, Value::Bool(true));

    // Test get_path_as() with type conversion

    // Direct type conversion
    let top_typed: String = dict
        .get_path_as("top_level")
        .expect("Failed to get typed top_level");
    assert_eq!(top_typed, "root_value");

    let counter_typed: i64 = dict
        .get_path_as("counter")
        .expect("Failed to get typed counter");
    assert_eq!(counter_typed, 42);

    // Nested type conversion
    let name_typed: String = dict
        .get_path_as("user.name")
        .expect("Failed to get typed user.name");
    assert_eq!(name_typed, "Alice");

    let age_typed: i64 = dict
        .get_path_as("user.age")
        .expect("Failed to get typed user.age");
    assert_eq!(age_typed, 30);

    // Deep nested type conversion
    let email_typed: String = dict
        .get_path_as("user.profile.email")
        .expect("Failed to get typed user.profile.email");
    assert_eq!(email_typed, "alice@example.com");

    let verified_typed: bool = dict
        .get_path_as("user.profile.verified")
        .expect("Failed to get typed user.profile.verified");
    assert!(verified_typed);

    // Test error cases

    // Non-existent top-level path
    let missing_result = dict.get_path("missing_key");
    assert!(missing_result.is_err());

    // Non-existent nested path
    let missing_nested = dict.get_path("user.missing");
    assert!(missing_nested.is_err());

    // Non-existent deep path
    let missing_deep = dict.get_path("user.profile.missing");
    assert!(missing_deep.is_err());

    // Type mismatch with get_path_as
    let type_mismatch: Result<i64, _> = dict.get_path_as("user.name"); // String as i64
    assert!(type_mismatch.is_err());

    // Commit and verify persistence
    op.commit().expect("Failed to commit operation");

    // Test via viewer (read-only access)
    let viewer = tree
        .get_subtree_viewer::<DocStore>("path_test")
        .expect("Failed to get viewer");

    // Verify all path access still works after commit
    assert_eq!(
        viewer.get_path("top_level").unwrap(),
        Value::Text("root_value".to_string())
    );
    assert_eq!(
        viewer.get_path("user.name").unwrap(),
        Value::Text("Alice".to_string())
    );
    assert_eq!(
        viewer.get_path("user.profile.email").unwrap(),
        Value::Text("alice@example.com".to_string())
    );

    // Verify typed access still works
    let persisted_name: String = viewer
        .get_path_as("user.name")
        .expect("Failed to get persisted name");
    assert_eq!(persisted_name, "Alice");
}

#[test]
fn test_docstore_path_mixed_with_staging() {
    let tree = setup_tree();

    // Create initial committed data
    {
        let op = tree.new_operation().expect("Failed to start operation");
        let dict = op
            .get_subtree::<DocStore>("staging_test")
            .expect("Failed to get DocStore");

        // Set some initial data
        let mut config_doc = Doc::new();
        config_doc.set("version", "1.0");
        config_doc.set("debug", false);
        dict.set("config", Value::Node(config_doc.into()))
            .expect("Failed to set config");

        op.commit().expect("Failed to commit initial data");
    }

    // Now test staging behavior with paths
    let op = tree.new_operation().expect("Failed to start operation");
    let dict = op
        .get_subtree::<DocStore>("staging_test")
        .expect("Failed to get DocStore");

    // Verify we can access committed data via path
    let initial_version: String = dict
        .get_path_as("config.version")
        .expect("Failed to get initial version");
    assert_eq!(initial_version, "1.0");

    let initial_debug: bool = dict
        .get_path_as("config.debug")
        .expect("Failed to get initial debug");
    assert!(!initial_debug);

    // Stage some changes (update existing and add new) by updating the nested structure
    let mut updated_config = Doc::new();
    updated_config.set("version", "2.0"); // Update version
    updated_config.set("debug", false); // Keep debug same
    updated_config.set("environment", "production"); // Add new field
    dict.set("config", Value::Node(updated_config.into()))
        .expect("Failed to stage config update");
    dict.set("new_key", "new_value")
        .expect("Failed to stage new key");

    // Test that staged changes are visible via path access
    let staged_version: String = dict
        .get_path_as("config.version")
        .expect("Failed to get staged version");
    assert_eq!(staged_version, "2.0"); // Should see staged version

    let staged_env: String = dict
        .get_path_as("config.environment")
        .expect("Failed to get staged environment");
    assert_eq!(staged_env, "production");

    let new_value: String = dict.get_path_as("new_key").expect("Failed to get new key");
    assert_eq!(new_value, "new_value");

    // Verify that committed data that wasn't changed is still accessible
    let unchanged_debug: bool = dict
        .get_path_as("config.debug")
        .expect("Failed to get unchanged debug");
    assert!(!unchanged_debug); // Should still be false

    // Test that viewer still sees old committed data
    let viewer = tree
        .get_subtree_viewer::<DocStore>("staging_test")
        .expect("Failed to get viewer");

    let viewer_version: String = viewer
        .get_path_as("config.version")
        .expect("Failed to get viewer version");
    assert_eq!(viewer_version, "1.0"); // Should see committed version

    // New staged data shouldn't be visible to viewer
    let viewer_env = viewer.get_path("config.environment");
    assert!(viewer_env.is_err()); // Should not exist in committed data

    let viewer_new = viewer.get_path("new_key");
    assert!(viewer_new.is_err()); // Should not exist in committed data
}
