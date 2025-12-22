//! DocStore subtree operation tests
//!
//! This module contains tests for DocStore subtree functionality including
//! basic CRUD operations, path-based access, nested values, and persistence.

use eidetica::{
    crdt::{
        Doc,
        doc::{List, Value, path},
    },
    store::DocStore,
};

use super::helpers::*;
use crate::helpers::*;

#[tokio::test]
async fn test_dict_set_and_get_via_op() {
    let ctx = TestContext::new().with_database().await;

    // Use helper to create initial data
    let initial_data = &[("key1", "value1"), ("key2", "value2")];
    create_dict_operation(ctx.database(), "my_kv", initial_data).await;

    // Test operation-level modifications
    let op = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start operation");
    let dict = op
        .get_store::<DocStore>("my_kv")
        .await
        .expect("Failed to get Doc");

    // Verify initial values are accessible
    assert_dict_value(&dict, "key1", "value1").await;
    assert_dict_value(&dict, "key2", "value2").await;

    // Test get_string convenience method
    assert_eq!(
        dict.get_string("key1")
            .await
            .expect("Failed get_string key1"),
        "value1"
    );
    assert_eq!(
        dict.get_string("key2")
            .await
            .expect("Failed get_string key2"),
        "value2"
    );

    // Test overwrite
    dict.set("key1", "value1_updated")
        .await
        .expect("Failed to overwrite key1");
    assert_dict_value(&dict, "key1", "value1_updated").await;

    // Test non-existent key
    assert_key_not_found(dict.get("non_existent").await);

    op.commit().await.expect("Failed to commit operation");

    // Verify final state using helper
    let expected_final = &[("key1", "value1_updated"), ("key2", "value2")];
    assert_dict_viewer_data(ctx.database(), "my_kv", expected_final).await;
}

#[tokio::test]
async fn test_dict_get_all_via_viewer() {
    let ctx = TestContext::new().with_database().await;

    // Test dict persistence helper
    test_dict_persistence(ctx.database(), "my_kv").await;

    // Verify get_all using a viewer
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("my_kv")
        .await
        .expect("Failed to get viewer");
    let all_data_crdt = viewer.get_all().await.expect("Failed to get all data");

    assert_eq!(all_data_crdt.len(), 3);
    assert_eq!(
        all_data_crdt.get("key_a"),
        Some(&Value::Text("val_a".to_string()))
    );
    assert_eq!(
        all_data_crdt.get("key_b"),
        Some(&Value::Text("val_b_updated".to_string()))
    );
    assert_eq!(
        all_data_crdt.get("key_c"),
        Some(&Value::Text("val_c".to_string()))
    );
}

#[tokio::test]
async fn test_dict_get_all_empty() {
    let ctx = TestContext::new().with_database().await;

    // Get viewer for a non-existent subtree
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("empty_kv")
        .await
        .expect("Failed to get viewer for empty");
    let all_data_crdt = viewer
        .get_all()
        .await
        .expect("Failed to get all data from empty");

    assert!(all_data_crdt.is_empty());
}

#[tokio::test]
async fn test_dict_delete() {
    let ctx = TestContext::new().with_database().await;
    let op = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start operation");

    {
        let dict = op
            .get_store::<DocStore>("my_kv")
            .await
            .expect("Failed to get Doc");

        // Set initial values
        dict.set("key1", "value1")
            .await
            .expect("Failed to set key1");
        dict.set("key2", "value2")
            .await
            .expect("Failed to set key2");

        // Delete a key
        dict.delete("key1").await.expect("Failed to delete key1");

        // Verify key1 is deleted
        assert_key_not_found(dict.get("key1").await);

        // key2 should still be accessible
        assert_dict_value(&dict, "key2", "value2").await;
    }

    // Commit the operation
    op.commit().await.expect("Failed to commit operation");

    // Verify the deletion persisted
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("my_kv")
        .await
        .expect("Failed to get viewer");
    assert_key_not_found(viewer.get("key1").await);

    assert_dict_value(&viewer, "key2", "value2").await;
}

#[tokio::test]
async fn test_dict_set_value() {
    let ctx = TestContext::new().with_database().await;

    // Use helper to create nested map operation
    create_dict_with_nested_map(ctx.database(), "my_kv").await;

    // Get viewer to verify persistence
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("my_kv")
        .await
        .expect("Failed to get viewer");

    // Check string value persisted
    assert_dict_value(&viewer, "key1", "value1").await;

    // Check nested map structure
    assert_dict_nested_map(&viewer, "key2", &[("inner", "nested_value")]).await;
}

#[tokio::test]
async fn test_dict_list_basic_operations() {
    let ctx = TestContext::new().with_database().await;

    // Use helper to create Doc with list
    let list_items = &["apple", "banana", "orange"];
    create_dict_with_list(ctx.database(), "my_kv", list_items).await;

    // Verify with viewer
    assert_dict_list_data(ctx.database(), "my_kv", "fruits", list_items).await;
}

#[tokio::test]
async fn test_dict_list_nonexistent_key() {
    let ctx = TestContext::new().with_database().await;
    let op = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start operation");

    {
        let dict = op
            .get_store::<DocStore>("my_kv")
            .await
            .expect("Failed to get Doc");

        // Test getting non-existent list should return NotFound error
        assert_key_not_found(dict.get("nonexistent").await);

        // Test getting non-existent list with get_list should also return NotFound
        let list_result = dict.get_as::<List>("nonexistent").await;
        assert!(list_result.is_err());

        // Create a new list
        let mut new_list = eidetica::crdt::doc::List::new();
        new_list.push(Value::Text("first_item".to_string()));

        dict.set_list("new_list", new_list)
            .await
            .expect("Failed to set new list");

        // Verify the new list was created
        let retrieved_list = dict
            .get_as::<List>("new_list")
            .await
            .expect("Failed to get new list");
        assert_eq!(retrieved_list.len(), 1);
        assert_eq!(
            retrieved_list.get(0),
            Some(&Value::Text("first_item".to_string()))
        );
    }
}

#[tokio::test]
async fn test_dict_list_persistence() {
    let ctx = TestContext::new().with_database().await;

    // Create list in first operation
    let op1 = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start op1");
    {
        let dict = op1
            .get_store::<DocStore>("my_kv")
            .await
            .expect("Failed to get Doc");

        let mut colors = eidetica::crdt::doc::List::new();
        colors.push(Value::Text("red".to_string()));
        colors.push(Value::Text("green".to_string()));

        dict.set_list("colors", colors)
            .await
            .expect("Failed to set colors list");
    }
    op1.commit().await.expect("Failed to commit op1");

    // Modify list in second operation
    let op2 = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start op2");
    {
        let dict = op2
            .get_store::<DocStore>("my_kv")
            .await
            .expect("Failed to get Doc");

        // List should persist from previous operation
        let colors = dict
            .get_as::<List>("colors")
            .await
            .expect("Failed to get colors list");
        assert_eq!(colors.len(), 2);
        assert_eq!(colors.get(0), Some(&Value::Text("red".to_string())));
        assert_eq!(colors.get(1), Some(&Value::Text("green".to_string())));

        // Modify the list - remove first element and add blue
        let mut updated_colors = colors.clone();
        updated_colors.remove(0); // Remove red
        updated_colors.push(Value::Text("blue".to_string())); // Add blue

        dict.set_list("colors", updated_colors)
            .await
            .expect("Failed to update colors list");
    }
    op2.commit().await.expect("Failed to commit op2");

    // Verify final state
    assert_dict_list_data(ctx.database(), "my_kv", "colors", &["green", "blue"]).await;
}

#[tokio::test]
async fn test_dict_update_nested_value() {
    let ctx = TestContext::new().with_database().await;

    // First operation: Create initial nested structure
    let op1 = ctx
        .database()
        .new_transaction()
        .await
        .expect("Op1: Failed to start");
    {
        let dict = op1
            .get_store::<DocStore>("nested_test")
            .await
            .expect("Op1: Failed to get Doc");

        // Create level1 -> level2_str structure
        let mut l1_map = Doc::new();
        l1_map.set("level2_str", "initial_value");
        dict.set_value("level1", l1_map)
            .await
            .expect("Op1: Failed to set level1");
    }
    op1.commit().await.expect("Op1: Failed to commit");

    // Second operation: Update with another structure
    let op2 = ctx
        .database()
        .new_transaction()
        .await
        .expect("Op2: Failed to start");
    {
        let dict = op2
            .get_store::<DocStore>("nested_test")
            .await
            .expect("Op2: Failed to get Doc");

        // Create an entirely new map structure that will replace the old one
        let mut l2_map = Doc::new();
        l2_map.set("deep_key", "deep_value");

        let mut new_l1_map = Doc::new();
        new_l1_map.set("level2_map", l2_map);

        // Completely replace the previous value at level1
        dict.set_value("level1", new_l1_map.clone())
            .await
            .expect("Op2: Failed to overwrite level1");

        // Verify the update within the same operation
        match dict.get("level1").await.expect("Failed to get level1") {
            Value::Doc(retrieved_l1_map) => {
                // Check if level2_map exists with the expected content
                match retrieved_l1_map.get("level2_map") {
                    Some(Value::Doc(retrieved_l2_map)) => match retrieved_l2_map.get("deep_key") {
                        Some(Value::Text(val)) => assert_eq!(val, "deep_value"),
                        _ => panic!("Expected string 'deep_value' at deep_key"),
                    },
                    _ => panic!("Expected 'level2_map' to be a map"),
                }
            }
            _ => panic!("Expected 'level1' to be a map"),
        }
    }
    op2.commit().await.expect("Op2: Failed to commit");

    // Verify the update persists after commit
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("nested_test")
        .await
        .expect("Failed to get viewer");

    // Verify the structure after commit
    match viewer
        .get("level1")
        .await
        .expect("Viewer: Failed to get level1")
    {
        Value::Doc(retrieved_l1_map) => {
            // Check if level2_map exists with expected content
            match retrieved_l1_map.get("level2_map") {
                Some(Value::Doc(retrieved_l2_map)) => match retrieved_l2_map.get("deep_key") {
                    Some(Value::Text(val)) => assert_eq!(val, "deep_value"),
                    _ => panic!("Viewer: Expected string 'deep_value' at deep_key"),
                },
                _ => panic!("Viewer: Expected 'level2_map' to be a map"),
            }
        }
        _ => panic!("Viewer: Expected 'level1' to be a map"),
    }
}

#[tokio::test]
async fn test_dict_comprehensive_operations() {
    let ctx = TestContext::new().with_database().await;
    let op = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start operation");

    {
        let dict = op
            .get_store::<DocStore>("test_store")
            .await
            .expect("Failed to get Doc");

        // Set basic string values
        dict.set("key1", "value1")
            .await
            .expect("Failed to set key1");
        dict.set("key2", "value2")
            .await
            .expect("Failed to set key2");

        // Set a nested map value
        let mut nested = Doc::new();
        nested.set("nested_key1", "nested_value1");
        nested.set("nested_key2", "nested_value2");
        dict.set_value("nested", Value::Doc(nested.clone()))
            .await
            .expect("Failed to set nested map");
    }

    // Commit the operation
    op.commit().await.expect("Failed to commit operation");

    // Get a viewer to check the subtree
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("test_store")
        .await
        .expect("Failed to get viewer");

    // Check string values
    assert_dict_value(&viewer, "key1", "value1").await;
    assert_dict_value(&viewer, "key2", "value2").await;

    // Check nested map
    assert_dict_nested_map(
        &viewer,
        "nested",
        &[
            ("nested_key1", "nested_value1"),
            ("nested_key2", "nested_value2"),
        ],
    )
    .await;

    // Check non-existent key
    assert_key_not_found(viewer.get("non_existent").await);
}

#[tokio::test]
async fn test_empty_dict_behavior() {
    let ctx = TestContext::new().with_database().await;

    // Test empty Doc behavior
    assert_dict_viewer_count(ctx.database(), "empty_dict", 0).await;

    let dict_viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("empty_dict")
        .await
        .expect("Failed to get empty Doc viewer");
    assert_key_not_found(dict_viewer.get("any_key").await);
}

#[tokio::test]
async fn test_docstore_path_based_access() {
    let ctx = TestContext::new().with_database().await;

    // Create operation and set up nested data structure
    let op = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start operation");
    let dict = op
        .get_store::<DocStore>("path_test")
        .await
        .expect("Failed to get DocStore");

    // Set up mixed structure - some direct, some that would be path-accessible
    dict.set("top_level", "root_value")
        .await
        .expect("Failed to set top_level");
    dict.set("counter", 42)
        .await
        .expect("Failed to set counter");

    // Create nested structure by setting a Doc with nested data
    let mut user_doc = Doc::new();
    user_doc.set("name", "Alice");
    user_doc.set("age", 30);

    let mut profile_doc = Doc::new();
    profile_doc.set("email", "alice@example.com");
    profile_doc.set("verified", true);
    user_doc.set("profile", Value::Doc(profile_doc));

    dict.set("user", Value::Doc(user_doc))
        .await
        .expect("Failed to set user");

    // Test get_path() for various path levels

    // Top-level path access (equivalent to direct access)
    let top_value = dict
        .get_path(path!("top_level"))
        .await
        .expect("Failed to get top_level path");
    assert_eq!(top_value, Value::Text("root_value".to_string()));

    let counter_value = dict
        .get_path(path!("counter"))
        .await
        .expect("Failed to get counter path");
    assert_eq!(counter_value, Value::Int(42));

    // Nested path access
    let user_name = dict
        .get_path(path!("user.name"))
        .await
        .expect("Failed to get user.name path");
    assert_eq!(user_name, Value::Text("Alice".to_string()));

    let user_age = dict
        .get_path(path!("user.age"))
        .await
        .expect("Failed to get user.age path");
    assert_eq!(user_age, Value::Int(30));

    // Deep nested path access
    let user_email = dict
        .get_path(path!("user.profile.email"))
        .await
        .expect("Failed to get user.profile.email path");
    assert_eq!(user_email, Value::Text("alice@example.com".to_string()));

    let user_verified = dict
        .get_path(path!("user.profile.verified"))
        .await
        .expect("Failed to get user.profile.verified path");
    assert_eq!(user_verified, Value::Bool(true));

    // Test get_path_as() with type conversion

    // Direct type conversion
    let top_typed: String = dict
        .get_path_as(path!("top_level"))
        .await
        .expect("Failed to get typed top_level");
    assert_eq!(top_typed, "root_value");

    let counter_typed: i64 = dict
        .get_path_as(path!("counter"))
        .await
        .expect("Failed to get typed counter");
    assert_eq!(counter_typed, 42);

    // Nested type conversion
    let name_typed: String = dict
        .get_path_as(path!("user.name"))
        .await
        .expect("Failed to get typed user.name");
    assert_eq!(name_typed, "Alice");

    let age_typed: i64 = dict
        .get_path_as(path!("user.age"))
        .await
        .expect("Failed to get typed user.age");
    assert_eq!(age_typed, 30);

    // Deep nested type conversion
    let email_typed: String = dict
        .get_path_as(path!("user.profile.email"))
        .await
        .expect("Failed to get typed user.profile.email");
    assert_eq!(email_typed, "alice@example.com");

    let verified_typed: bool = dict
        .get_path_as(path!("user.profile.verified"))
        .await
        .expect("Failed to get typed user.profile.verified");
    assert!(verified_typed);

    // Test error cases

    // Non-existent top-level path
    let missing_result = dict.get_path(path!("missing_key")).await;
    assert!(missing_result.is_err());

    // Non-existent nested path
    let missing_nested = dict.get_path(path!("user.missing")).await;
    assert!(missing_nested.is_err());

    // Non-existent deep path
    let missing_deep = dict.get_path(path!("user.profile.missing")).await;
    assert!(missing_deep.is_err());

    // Type mismatch with get_path_as
    let type_mismatch: Result<i64, _> = dict.get_path_as(path!("user.name")).await; // String as i64
    assert!(type_mismatch.is_err());

    // Commit and verify persistence
    op.commit().await.expect("Failed to commit operation");

    // Test via viewer (read-only access)
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("path_test")
        .await
        .expect("Failed to get viewer");

    // Verify all path access still works after commit
    assert_eq!(
        viewer.get_path(path!("top_level")).await.unwrap(),
        Value::Text("root_value".to_string())
    );
    assert_eq!(
        viewer.get_path(path!("user.name")).await.unwrap(),
        Value::Text("Alice".to_string())
    );
    assert_eq!(
        viewer.get_path(path!("user.profile.email")).await.unwrap(),
        Value::Text("alice@example.com".to_string())
    );

    // Verify typed access still works
    let persisted_name: String = viewer
        .get_path_as(path!("user.name"))
        .await
        .expect("Failed to get persisted name");
    assert_eq!(persisted_name, "Alice");
}

#[tokio::test]
async fn test_docstore_path_mixed_with_staging() {
    let ctx = TestContext::new().with_database().await;

    // Create initial committed data
    {
        let op = ctx
            .database()
            .new_transaction()
            .await
            .expect("Failed to start operation");
        let dict = op
            .get_store::<DocStore>("staging_test")
            .await
            .expect("Failed to get DocStore");

        // Set some initial data
        let mut config_doc = Doc::new();
        config_doc.set("version", "1.0");
        config_doc.set("debug", false);
        dict.set("config", Value::Doc(config_doc))
            .await
            .expect("Failed to set config");

        op.commit().await.expect("Failed to commit initial data");
    }

    // Now test staging behavior with paths
    let op = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start operation");
    let dict = op
        .get_store::<DocStore>("staging_test")
        .await
        .expect("Failed to get DocStore");

    // Verify we can access committed data via path
    let initial_version: String = dict
        .get_path_as(path!("config.version"))
        .await
        .expect("Failed to get initial version");
    assert_eq!(initial_version, "1.0");

    let initial_debug: bool = dict
        .get_path_as(path!("config.debug"))
        .await
        .expect("Failed to get initial debug");
    assert!(!initial_debug);

    // Stage some changes (update existing and add new) by updating the nested structure
    let mut updated_config = Doc::new();
    updated_config.set("version", "2.0"); // Update version
    updated_config.set("debug", false); // Keep debug same
    updated_config.set("environment", "production"); // Add new field
    dict.set("config", Value::Doc(updated_config))
        .await
        .expect("Failed to stage config update");
    dict.set("new_key", "new_value")
        .await
        .expect("Failed to stage new key");

    // Test that staged changes are visible via path access
    let staged_version: String = dict
        .get_path_as(path!("config.version"))
        .await
        .expect("Failed to get staged version");
    assert_eq!(staged_version, "2.0"); // Should see staged version

    let staged_env: String = dict
        .get_path_as(path!("config.environment"))
        .await
        .expect("Failed to get staged environment");
    assert_eq!(staged_env, "production");

    let new_value: String = dict
        .get_path_as(path!("new_key"))
        .await
        .expect("Failed to get new key");
    assert_eq!(new_value, "new_value");

    // Verify that committed data that wasn't changed is still accessible
    let unchanged_debug: bool = dict
        .get_path_as(path!("config.debug"))
        .await
        .expect("Failed to get unchanged debug");
    assert!(!unchanged_debug); // Should still be false

    // Test that viewer still sees old committed data
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("staging_test")
        .await
        .expect("Failed to get viewer");

    let viewer_version: String = viewer
        .get_path_as(path!("config.version"))
        .await
        .expect("Failed to get viewer version");
    assert_eq!(viewer_version, "1.0"); // Should see committed version

    // New staged data shouldn't be visible to viewer
    let viewer_env = viewer.get_path(path!("config.environment")).await;
    assert!(viewer_env.is_err()); // Should not exist in committed data

    let viewer_new = viewer.get_path(path!("new_key")).await;
    assert!(viewer_new.is_err()); // Should not exist in committed data
}

#[tokio::test]
async fn test_docstore_set_path() {
    let ctx = TestContext::new().with_database().await;

    let op = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start operation");
    let dict = op
        .get_store::<DocStore>("set_path_test")
        .await
        .expect("Failed to get DocStore");

    // Test setting simple path (single level)
    dict.set_path(path!("simple"), "value")
        .await
        .expect("Failed to set simple path");
    assert_eq!(
        dict.get_path(path!("simple")).await.unwrap(),
        Value::Text("value".to_string())
    );

    // Test setting nested paths (creates intermediate structure)
    dict.set_path(path!("user.name"), "Alice")
        .await
        .expect("Failed to set user.name");
    dict.set_path(path!("user.age"), 30)
        .await
        .expect("Failed to set user.age");
    dict.set_path(path!("user.profile.email"), "alice@example.com")
        .await
        .expect("Failed to set user.profile.email");
    dict.set_path(path!("user.profile.verified"), true)
        .await
        .expect("Failed to set user.profile.verified");

    // Test deep nesting
    dict.set_path(path!("config.database.host"), "localhost")
        .await
        .expect("Failed to set deep path");
    dict.set_path(path!("config.database.port"), 5432)
        .await
        .expect("Failed to set deep path port");

    // Verify all values are accessible
    assert_eq!(
        dict.get_path(path!("user.name")).await.unwrap(),
        Value::Text("Alice".to_string())
    );
    assert_eq!(
        dict.get_path(path!("user.age")).await.unwrap(),
        Value::Int(30)
    );
    assert_eq!(
        dict.get_path(path!("user.profile.email")).await.unwrap(),
        Value::Text("alice@example.com".to_string())
    );
    assert_eq!(
        dict.get_path(path!("user.profile.verified")).await.unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        dict.get_path(path!("config.database.host")).await.unwrap(),
        Value::Text("localhost".to_string())
    );
    assert_eq!(
        dict.get_path(path!("config.database.port")).await.unwrap(),
        Value::Int(5432)
    );

    // Test overwriting existing values
    dict.set_path(path!("user.age"), 31)
        .await
        .expect("Failed to overwrite user.age");
    assert_eq!(
        dict.get_path(path!("user.age")).await.unwrap(),
        Value::Int(31)
    );

    // Test overwriting path segments (should work as expected)
    dict.set_path(path!("user.profile"), "simple_string")
        .await
        .expect("Failed to overwrite user.profile");
    assert_eq!(
        dict.get_path(path!("user.profile")).await.unwrap(),
        Value::Text("simple_string".to_string())
    );

    // Verify that previous nested values under user.profile are now inaccessible
    let email_result = dict.get_path(path!("user.profile.email")).await;
    assert!(email_result.is_err());

    // Commit and verify persistence
    op.commit().await.expect("Failed to commit operation");

    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("set_path_test")
        .await
        .expect("Failed to get viewer");

    assert_eq!(
        viewer.get_path(path!("user.name")).await.unwrap(),
        Value::Text("Alice".to_string())
    );
    assert_eq!(
        viewer.get_path(path!("user.age")).await.unwrap(),
        Value::Int(31)
    );
    assert_eq!(
        viewer
            .get_path(path!("config.database.host"))
            .await
            .unwrap(),
        Value::Text("localhost".to_string())
    );
}

#[tokio::test]
async fn test_docstore_modify_path() {
    let ctx = TestContext::new().with_database().await;

    let op = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start operation");
    let dict = op
        .get_store::<DocStore>("modify_path_test")
        .await
        .expect("Failed to get DocStore");

    // Set up initial data
    dict.set_path(path!("stats.score"), 100)
        .await
        .expect("Failed to set initial score");
    dict.set_path(path!("config.retries"), 3)
        .await
        .expect("Failed to set initial retries");
    dict.set_path(path!("user.name"), "Alice")
        .await
        .expect("Failed to set initial name");

    // Test modifying integer values
    dict.modify_path::<i64, _>(path!("stats.score"), |score| {
        *score += 50;
    })
    .await
    .expect("Failed to modify score");
    assert_eq!(
        dict.get_path_as::<i64>(path!("stats.score")).await.unwrap(),
        150
    );

    dict.modify_path::<i64, _>(path!("config.retries"), |retries| {
        *retries *= 2;
    })
    .await
    .expect("Failed to modify retries");
    assert_eq!(
        dict.get_path_as::<i64>(path!("config.retries"))
            .await
            .unwrap(),
        6
    );

    // Test modifying string values
    dict.modify_path::<String, _>(path!("user.name"), |name| {
        name.push_str(" Smith");
    })
    .await
    .expect("Failed to modify name");
    assert_eq!(
        dict.get_path_as::<String>(path!("user.name"))
            .await
            .unwrap(),
        "Alice Smith"
    );

    // Test error case - path doesn't exist
    let result = dict
        .modify_path::<i64, _>(path!("nonexistent.path"), |_| {})
        .await;
    assert!(result.is_err());

    // Test error case - type mismatch
    let result = dict
        .modify_path::<i64, _>(path!("user.name"), |_| {}) // name is string, not int
        .await;
    assert!(result.is_err());

    // Commit and verify persistence
    op.commit().await.expect("Failed to commit operation");

    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("modify_path_test")
        .await
        .expect("Failed to get viewer");

    assert_eq!(
        viewer
            .get_path_as::<i64>(path!("stats.score"))
            .await
            .unwrap(),
        150
    );
    assert_eq!(
        viewer
            .get_path_as::<i64>(path!("config.retries"))
            .await
            .unwrap(),
        6
    );
    assert_eq!(
        viewer
            .get_path_as::<String>(path!("user.name"))
            .await
            .unwrap(),
        "Alice Smith"
    );
}

#[tokio::test]
async fn test_docstore_get_or_insert_path() {
    let ctx = TestContext::new().with_database().await;

    let op = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start operation");
    let dict = op
        .get_store::<DocStore>("get_or_insert_path_test")
        .await
        .expect("Failed to get DocStore");

    // Test inserting when path doesn't exist (creates structure)
    let score1: i64 = dict
        .get_or_insert_path(path!("player.stats.score"), 0)
        .await
        .expect("Failed to get_or_insert_path score");
    assert_eq!(score1, 0);

    // Verify structure was created
    assert_eq!(
        dict.get_path_as::<i64>(path!("player.stats.score"))
            .await
            .unwrap(),
        0
    );

    // Test returning existing value when path exists
    dict.set_path(path!("player.stats.score"), 42)
        .await
        .expect("Failed to set score");
    let score2: i64 = dict
        .get_or_insert_path(path!("player.stats.score"), 100)
        .await
        .expect("Failed to get_or_insert_path existing score");
    assert_eq!(score2, 42); // Should return existing value, not default

    // Test with different data types
    let name: String = dict
        .get_or_insert_path(path!("player.info.name"), "DefaultName".to_string())
        .await
        .expect("Failed to get_or_insert_path name");
    assert_eq!(name, "DefaultName");

    let active: bool = dict
        .get_or_insert_path(path!("player.status.active"), true)
        .await
        .expect("Failed to get_or_insert_path active");
    assert!(active);

    // Test existing values are returned
    dict.set_path(path!("player.info.name"), "Alice")
        .await
        .expect("Failed to set name");
    let existing_name: String = dict
        .get_or_insert_path(path!("player.info.name"), "ShouldNotBeUsed".to_string())
        .await
        .expect("Failed to get existing name");
    assert_eq!(existing_name, "Alice");

    // Verify all paths exist
    assert_eq!(
        dict.get_path_as::<i64>(path!("player.stats.score"))
            .await
            .unwrap(),
        42
    );
    assert_eq!(
        dict.get_path_as::<String>(path!("player.info.name"))
            .await
            .unwrap(),
        "Alice"
    );
    assert!(
        dict.get_path_as::<bool>(path!("player.status.active"))
            .await
            .unwrap()
    );

    // Commit and verify persistence
    op.commit().await.expect("Failed to commit operation");

    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("get_or_insert_path_test")
        .await
        .expect("Failed to get viewer");

    assert_eq!(
        viewer
            .get_path_as::<i64>(path!("player.stats.score"))
            .await
            .unwrap(),
        42
    );
    assert_eq!(
        viewer
            .get_path_as::<String>(path!("player.info.name"))
            .await
            .unwrap(),
        "Alice"
    );
    assert!(
        viewer
            .get_path_as::<bool>(path!("player.status.active"))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn test_docstore_modify_or_insert_path() {
    let ctx = TestContext::new().with_database().await;

    let op = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start operation");
    let dict = op
        .get_store::<DocStore>("modify_or_insert_path_test")
        .await
        .expect("Failed to get DocStore");

    // Test inserting and modifying when path doesn't exist
    dict.modify_or_insert_path::<i64, _>(path!("metrics.requests"), 0, |count| {
        *count += 10;
    })
    .await
    .expect("Failed to modify_or_insert_path requests");
    assert_eq!(
        dict.get_path_as::<i64>(path!("metrics.requests"))
            .await
            .unwrap(),
        10
    );

    // Test modifying existing value
    dict.modify_or_insert_path::<i64, _>(path!("metrics.requests"), 100, |count| {
        *count *= 2;
    })
    .await
    .expect("Failed to modify existing requests");
    assert_eq!(
        dict.get_path_as::<i64>(path!("metrics.requests"))
            .await
            .unwrap(),
        20
    ); // 10 * 2, not 100 * 2

    // Test with string values
    dict.modify_or_insert_path::<String, _>(
        path!("config.environment"),
        "dev".to_string(),
        |env| {
            env.push_str(".local");
        },
    )
    .await
    .expect("Failed to modify_or_insert_path environment");
    assert_eq!(
        dict.get_path_as::<String>(path!("config.environment"))
            .await
            .unwrap(),
        "dev.local"
    );

    // Test modifying existing string
    dict.modify_or_insert_path::<String, _>(
        path!("config.environment"),
        "prod".to_string(),
        |env| {
            *env = format!("override-{env}");
        },
    )
    .await
    .expect("Failed to modify existing environment");
    assert_eq!(
        dict.get_path_as::<String>(path!("config.environment"))
            .await
            .unwrap(),
        "override-dev.local"
    );

    // Test complex nested path creation and modification
    dict.modify_or_insert_path::<i64, _>(path!("app.features.cache.ttl"), 300, |ttl| {
        *ttl += 60; // Add 1 minute
    })
    .await
    .expect("Failed to modify_or_insert_path ttl");
    assert_eq!(
        dict.get_path_as::<i64>(path!("app.features.cache.ttl"))
            .await
            .unwrap(),
        360
    );

    // Test multiple operations on the same nested structure
    dict.modify_or_insert_path::<i64, _>(path!("app.features.cache.size"), 1024, |size| {
        *size *= 2;
    })
    .await
    .expect("Failed to modify_or_insert_path size");
    assert_eq!(
        dict.get_path_as::<i64>(path!("app.features.cache.size"))
            .await
            .unwrap(),
        2048
    );

    // Verify that existing structure is preserved
    assert_eq!(
        dict.get_path_as::<i64>(path!("app.features.cache.ttl"))
            .await
            .unwrap(),
        360
    );

    // Commit and verify persistence
    op.commit().await.expect("Failed to commit operation");

    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("modify_or_insert_path_test")
        .await
        .expect("Failed to get viewer");

    assert_eq!(
        viewer
            .get_path_as::<i64>(path!("metrics.requests"))
            .await
            .unwrap(),
        20
    );
    assert_eq!(
        viewer
            .get_path_as::<String>(path!("config.environment"))
            .await
            .unwrap(),
        "override-dev.local"
    );
    assert_eq!(
        viewer
            .get_path_as::<i64>(path!("app.features.cache.ttl"))
            .await
            .unwrap(),
        360
    );
    assert_eq!(
        viewer
            .get_path_as::<i64>(path!("app.features.cache.size"))
            .await
            .unwrap(),
        2048
    );
}

#[tokio::test]
async fn test_docstore_path_mutation_interoperability() {
    let ctx = TestContext::new().with_database().await;

    let op = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start operation");
    let dict = op
        .get_store::<DocStore>("interop_test")
        .await
        .expect("Failed to get DocStore");

    // Mix direct and path-based operations
    dict.set("level1", "direct")
        .await
        .expect("Failed to set direct");

    // Setting a nested path when level1 is not a map should replace level1 with a node
    let result = dict.set_path(path!("level1.nested"), "path_based").await;
    assert!(result.is_ok()); // Should succeed by replacing level1 with a node

    // Verify that level1 is now a node containing "nested" = "path_based"
    assert_eq!(
        dict.get_string("level1.nested")
            .await
            .expect("Failed to get level1.nested"),
        "path_based"
    );

    // However, we can set level1 to be a map structure directly
    dict.set_path(path!("level1_map.nested"), "path_based")
        .await
        .expect("Failed to set nested"); // This creates level1_map as a map

    // Verify that level1_map is now a map
    let level1_value = dict
        .get("level1_map")
        .await
        .expect("Failed to get level1_map");
    match level1_value {
        Value::Doc(_) => {} // Expected
        _ => panic!("Expected level1_map to be a Doc after path operation"),
    }

    // Verify nested value is accessible
    assert_eq!(
        dict.get_path_as::<String>(path!("level1_map.nested"))
            .await
            .unwrap(),
        "path_based"
    );

    // Mix get_as and get_path_as operations
    dict.set_path(path!("data.count"), 42)
        .await
        .expect("Failed to set count");
    let direct_count_result: Result<i64, _> = dict.get_as("data").await; // Should fail - data is a map
    assert!(direct_count_result.is_err());
    let path_count: i64 = dict.get_path_as(path!("data.count")).await.unwrap();
    assert_eq!(path_count, 42);

    // Mix modify and modify_path operations
    dict.set("simple_count", 10)
        .await
        .expect("Failed to set simple_count");
    dict.modify::<i64, _>("simple_count", |count| *count += 5)
        .await
        .expect("Failed to modify simple_count");
    assert_eq!(dict.get_as::<i64>("simple_count").await.unwrap(), 15);

    dict.modify_path::<i64, _>(path!("data.count"), |count| *count *= 2)
        .await
        .expect("Failed to modify path count");
    assert_eq!(
        dict.get_path_as::<i64>(path!("data.count")).await.unwrap(),
        84
    );

    // Mix get_or_insert and get_or_insert_path
    let simple_new: String = dict
        .get_or_insert("new_simple", "simple".to_string())
        .await
        .expect("Failed to get_or_insert simple");
    assert_eq!(simple_new, "simple");

    let path_new: String = dict
        .get_or_insert_path(path!("new_path.deep.value"), "deep".to_string())
        .await
        .expect("Failed to get_or_insert_path deep");
    assert_eq!(path_new, "deep");

    // Verify both exist with different access methods
    assert_eq!(dict.get_as::<String>("new_simple").await.unwrap(), "simple");
    assert_eq!(
        dict.get_path_as::<String>(path!("new_path.deep.value"))
            .await
            .unwrap(),
        "deep"
    );

    // Commit and verify all operations persisted correctly
    op.commit().await.expect("Failed to commit operation");

    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("interop_test")
        .await
        .expect("Failed to get viewer");

    assert_eq!(
        viewer
            .get_path_as::<String>(path!("level1_map.nested"))
            .await
            .unwrap(),
        "path_based"
    );
    assert_eq!(viewer.get_as::<i64>("simple_count").await.unwrap(), 15);
    assert_eq!(
        viewer
            .get_path_as::<i64>(path!("data.count"))
            .await
            .unwrap(),
        84
    );
    assert_eq!(
        viewer.get_as::<String>("new_simple").await.unwrap(),
        "simple"
    );
    assert_eq!(
        viewer
            .get_path_as::<String>(path!("new_path.deep.value"))
            .await
            .unwrap(),
        "deep"
    );
}

#[tokio::test]
async fn test_docstore_contains_key() {
    let ctx = TestContext::new().with_database().await;

    let op = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start operation");
    let dict = op
        .get_store::<DocStore>("contains_key_test")
        .await
        .expect("Failed to get DocStore");

    // Test empty DocStore
    assert!(!dict.contains_key("nonexistent").await);
    assert!(!dict.contains_key("missing").await);

    // Test with staged data (not yet committed)
    dict.set("name", "Alice").await.expect("Failed to set name");
    dict.set("age", 30).await.expect("Failed to set age");
    assert!(dict.contains_key("name").await);
    assert!(dict.contains_key("age").await);
    assert!(!dict.contains_key("missing").await);

    // Test with nested values
    dict.set_path(path!("user.profile.email"), "alice@example.com")
        .await
        .expect("Failed to set nested value");
    assert!(dict.contains_key("user").await); // Top-level key exists
    assert!(!dict.contains_key("profile").await); // Nested key doesn't exist at top level
    assert!(!dict.contains_key("email").await); // Deep nested key doesn't exist at top level

    // Test deletion (tombstones)
    dict.delete("name").await.expect("Failed to delete name");
    assert!(!dict.contains_key("name").await); // Deleted key should not exist
    assert!(dict.contains_key("age").await); // Other keys should still exist

    // Commit and test persistence
    op.commit().await.expect("Failed to commit operation");

    // Test with committed data
    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("contains_key_test")
        .await
        .expect("Failed to get viewer");

    assert!(!viewer.contains_key("name").await); // Deleted key not in committed data
    assert!(viewer.contains_key("age").await); // Existing key in committed data
    assert!(viewer.contains_key("user").await); // Nested structure in committed data

    // Test with new operation on committed data
    let op2 = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start second operation");
    let dict2 = op2
        .get_store::<DocStore>("contains_key_test")
        .await
        .expect("Failed to get DocStore");

    // Should see committed data
    assert!(!dict2.contains_key("name").await); // Still deleted
    assert!(dict2.contains_key("age").await); // Still exists
    assert!(dict2.contains_key("user").await); // Still exists

    // Stage new changes
    dict2
        .set("name", "Bob")
        .await
        .expect("Failed to re-set name");
    dict2.set("city", "NYC").await.expect("Failed to set city");

    // Should see both committed and staged data
    assert!(dict2.contains_key("name").await); // Now exists in staging
    assert!(dict2.contains_key("age").await); // Exists in committed data
    assert!(dict2.contains_key("user").await); // Exists in committed data
    assert!(dict2.contains_key("city").await); // Exists in staging

    // Viewer should only see committed data
    assert!(!viewer.contains_key("name").await); // Not in committed data
    assert!(!viewer.contains_key("city").await); // Not in committed data
}

#[tokio::test]
async fn test_docstore_contains_path() {
    let ctx = TestContext::new().with_database().await;

    let op = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start operation");
    let dict = op
        .get_store::<DocStore>("contains_path_test")
        .await
        .expect("Failed to get DocStore");

    // Test empty DocStore
    assert!(!dict.contains_path(path!("nonexistent")).await);
    assert!(!dict.contains_path(path!("missing.path")).await);

    // Test with simple paths
    dict.set("name", "Alice").await.expect("Failed to set name");
    assert!(dict.contains_path(path!("name")).await); // Direct key exists
    assert!(!dict.contains_path(path!("name.invalid")).await); // Can't navigate through string

    // Test with nested structure
    dict.set_path(path!("user.profile.name"), "Alice")
        .await
        .expect("Failed to set nested path");
    dict.set_path(path!("user.profile.email"), "alice@example.com")
        .await
        .expect("Failed to set nested email");
    dict.set_path(path!("user.settings.theme"), "dark")
        .await
        .expect("Failed to set settings");

    // Test intermediate paths
    assert!(dict.contains_path(path!("user")).await); // Top level
    assert!(dict.contains_path(path!("user.profile")).await); // Intermediate level
    assert!(dict.contains_path(path!("user.settings")).await); // Another intermediate level

    // Test full paths
    assert!(dict.contains_path(path!("user.profile.name")).await); // Full nested path
    assert!(dict.contains_path(path!("user.profile.email")).await); // Another full path
    assert!(dict.contains_path(path!("user.settings.theme")).await); // Different branch

    // Test non-existent paths
    assert!(!dict.contains_path(path!("user.profile.age")).await); // Missing leaf
    assert!(!dict.contains_path(path!("user.profile.missing")).await); // Missing leaf
    assert!(!dict.contains_path(path!("user.missing")).await); // Missing intermediate
    assert!(!dict.contains_path(path!("missing.user.profile")).await); // Missing root

    // Test deep nesting
    dict.set_path(path!("app.config.database.host"), "localhost")
        .await
        .expect("Failed to set deep path");
    dict.set_path(path!("app.config.database.port"), 5432)
        .await
        .expect("Failed to set deep port");

    assert!(dict.contains_path(path!("app")).await);
    assert!(dict.contains_path(path!("app.config")).await);
    assert!(dict.contains_path(path!("app.config.database")).await);
    assert!(dict.contains_path(path!("app.config.database.host")).await);
    assert!(dict.contains_path(path!("app.config.database.port")).await);
    assert!(!dict.contains_path(path!("app.config.database.name")).await);

    // Test path deletion
    dict.set_path(path!("temp.value"), "test")
        .await
        .expect("Failed to set temp value");
    assert!(dict.contains_path(path!("temp")).await);
    assert!(dict.contains_path(path!("temp.value")).await);

    // Override with simple value (should make nested path invalid)
    dict.set("temp", "simple")
        .await
        .expect("Failed to override temp");
    assert!(dict.contains_path(path!("temp")).await); // Exists as simple value
    assert!(!dict.contains_path(path!("temp.value")).await); // No longer accessible

    // Commit and test persistence
    op.commit().await.expect("Failed to commit operation");

    let viewer = ctx
        .database()
        .get_store_viewer::<DocStore>("contains_path_test")
        .await
        .expect("Failed to get viewer");

    // Test committed paths
    assert!(viewer.contains_path(path!("name")).await);
    assert!(viewer.contains_path(path!("user.profile.name")).await);
    assert!(viewer.contains_path(path!("user.profile.email")).await);
    assert!(viewer.contains_path(path!("user.settings.theme")).await);
    assert!(
        viewer
            .contains_path(path!("app.config.database.host"))
            .await
    );
    assert!(viewer.contains_path(path!("temp")).await); // Should be simple value
    assert!(!viewer.contains_path(path!("temp.value")).await); // Should not exist

    // Test with new operation adding more paths
    let op2 = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start second operation");
    let dict2 = op2
        .get_store::<DocStore>("contains_path_test")
        .await
        .expect("Failed to get DocStore");

    // Should see committed paths
    assert!(dict2.contains_path(path!("user.profile.name")).await);

    // Add staged paths
    dict2
        .set_path(path!("user.profile.age"), 30)
        .await
        .expect("Failed to set age");
    dict2
        .set_path(path!("new.staged.path"), "value")
        .await
        .expect("Failed to set staged path");

    // Should see both committed and staged paths
    assert!(dict2.contains_path(path!("user.profile.name")).await); // Committed
    assert!(dict2.contains_path(path!("user.profile.age")).await); // Staged
    assert!(dict2.contains_path(path!("new")).await); // Staged intermediate
    assert!(dict2.contains_path(path!("new.staged")).await); // Staged intermediate
    assert!(dict2.contains_path(path!("new.staged.path")).await); // Staged full

    // Viewer should only see committed paths
    assert!(viewer.contains_path(path!("user.profile.name")).await); // Committed
    assert!(!viewer.contains_path(path!("user.profile.age")).await); // Not committed
    assert!(!viewer.contains_path(path!("new")).await); // Not committed
}

#[tokio::test]
async fn test_docstore_contains_methods_consistency() {
    let ctx = TestContext::new().with_database().await;

    let op = ctx
        .database()
        .new_transaction()
        .await
        .expect("Failed to start operation");
    let dict = op
        .get_store::<DocStore>("consistency_test")
        .await
        .expect("Failed to get DocStore");

    // Test consistency between contains_key and contains_path for simple keys
    dict.set("simple", "value")
        .await
        .expect("Failed to set simple");
    dict.set("number", 42).await.expect("Failed to set number");

    assert_eq!(
        dict.contains_key("simple").await,
        dict.contains_path(path!("simple")).await
    );
    assert_eq!(
        dict.contains_key("number").await,
        dict.contains_path(path!("number")).await
    );
    assert_eq!(
        dict.contains_key("missing").await,
        dict.contains_path(path!("missing")).await
    );

    // Test nested structure
    dict.set_path(path!("nested.key"), "value")
        .await
        .expect("Failed to set nested");

    // contains_key should find top-level key
    assert!(dict.contains_key("nested").await);
    assert!(dict.contains_path(path!("nested")).await);

    // contains_path should find nested path, contains_key should not
    assert!(!dict.contains_key("key").await); // key is not at top level
    assert!(dict.contains_path(path!("nested.key")).await); // but path exists

    // Test get methods consistency with contains methods
    assert_eq!(
        dict.get("simple").await.is_ok(),
        dict.contains_key("simple").await
    );
    assert_eq!(
        dict.get("number").await.is_ok(),
        dict.contains_key("number").await
    );
    assert_eq!(
        dict.get("missing").await.is_err(),
        !dict.contains_key("missing").await
    );

    assert_eq!(
        dict.get_path(path!("simple")).await.is_ok(),
        dict.contains_path(path!("simple")).await
    );
    assert_eq!(
        dict.get_path(path!("nested.key")).await.is_ok(),
        dict.contains_path(path!("nested.key")).await
    );
    assert_eq!(
        dict.get_path(path!("missing.path")).await.is_err(),
        !dict.contains_path(path!("missing.path")).await
    );

    // Test with deletion
    dict.delete("simple")
        .await
        .expect("Failed to delete simple");
    assert_eq!(
        dict.get("simple").await.is_err(),
        !dict.contains_key("simple").await
    );
    assert_eq!(
        dict.get_path(path!("simple")).await.is_err(),
        !dict.contains_path(path!("simple")).await
    );

    // Test with type-safe getters
    let name_exists = dict.contains_key("number").await;
    let name_get_result = dict.get_as::<i64>("number").await.is_ok();
    assert_eq!(name_exists, name_get_result);

    let path_exists = dict.contains_path(path!("nested.key")).await;
    let path_get_result = dict
        .get_path_as::<String>(path!("nested.key"))
        .await
        .is_ok();
    assert_eq!(path_exists, path_get_result);
}
