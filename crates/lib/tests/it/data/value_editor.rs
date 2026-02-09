//! Tests for value editor functionality
//!
//! This module tests the value editor API for manipulating nested data structures
//! through a fluent interface.

use eidetica::Result;
use eidetica::crdt::Doc;
use eidetica::crdt::doc::Value;

use super::helpers::*;

#[tokio::test]
async fn test_value_editor_set_and_get_string_at_root() -> Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("editor_test_store").await?;

    test_editor_basic_set_get(&dict, "user", Value::Text("alice".to_string())).await?;

    // Verify directly from store as well
    assert_eq!(dict.get_string("user").await?, "alice");

    Ok(())
}

#[tokio::test]
async fn test_value_editor_set_and_get_nested_string() -> Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("editor_test_store").await?;

    // Test nested editor operations
    test_nested_editor_operations(
        &dict,
        &["user", "profile", "name"],
        Value::Text("bob".to_string()),
    )
    .await?;

    // Verify the nested structure was created correctly
    let user_editor = dict.get_value_mut("user");
    let profile_editor = user_editor.get_value_mut("profile");
    let retrieved_name = profile_editor.get_value("name").await?;
    assert_text_value(&retrieved_name, "bob");

    // Get user.profile (should be a map)
    let profile_map_value = user_editor.get_value("profile").await?;
    assert_map_contains(&profile_map_value, &["name"]);

    // Get the whole user object
    let user_data = dict.get("user").await?;
    match user_data {
        Value::Doc(user_map) => match user_map.get("profile") {
            Some(Value::Doc(profile_map)) => {
                assert_text_value(profile_map.get("name").unwrap(), "bob");
            }
            _ => panic!("Expected user.profile (nested) to be a map"),
        },
        _ => panic!("Expected user to be a map"),
    }

    Ok(())
}

#[tokio::test]
async fn test_value_editor_overwrite_non_map_with_map() -> Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("editor_test_store").await?;

    // Set user = "string_value"
    dict.set("user", "string_value").await?;

    // Now try to set user.profile.name = "charlie" through editor
    test_nested_editor_operations(
        &dict,
        &["user", "profile", "name"],
        Value::Text("charlie".to_string()),
    )
    .await?;

    // Verify user.profile.name was set correctly
    let user_editor = dict.get_value_mut("user");
    let profile_editor = user_editor.get_value_mut("profile");
    let retrieved_name = profile_editor.get_value("name").await?;
    assert_text_value(&retrieved_name, "charlie");

    // Verify that 'user' is now a map
    let user_data = dict.get("user").await?;
    match user_data {
        Value::Doc(user_map) => match user_map.get("profile") {
            Some(Value::Doc(profile_map)) => {
                assert_text_value(profile_map.get("name").unwrap(), "charlie");
            }
            _ => panic!("Expected user.profile to be a map after overwrite"),
        },
        _ => panic!("Expected user to be a map after overwrite"),
    }

    Ok(())
}

#[tokio::test]
async fn test_value_editor_get_non_existent_path() -> Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("editor_test_store").await?;

    let editor = dict.get_value_mut("nonexistent");
    assert_not_found_error(editor.get().await);

    let nested_editor = editor.get_value_mut("child");
    assert_not_found_error(nested_editor.get().await);

    assert_not_found_error(nested_editor.get_value("grandchild").await);

    Ok(())
}

#[tokio::test]
async fn test_value_editor_set_deeply_nested_creates_path() -> Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("editor_test_store").await?;

    // Test deep nesting in one transaction
    test_nested_editor_operations(
        &dict,
        &["a", "b", "c"],
        Value::Text("deep_value".to_string()),
    )
    .await?;

    // Verify the entire nested structure was created
    let a_val = dict.get("a").await?;
    match a_val {
        Value::Doc(a_map) => match a_map.get("b") {
            Some(Value::Doc(b_map)) => {
                assert_text_value(b_map.get("c").unwrap(), "deep_value");
            }
            _ => panic!("Expected a.b to be a map"),
        },
        _ => panic!("Expected a to be a map"),
    }

    Ok(())
}

#[tokio::test]
async fn test_value_editor_set_string_on_editor_path() -> Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("editor_test_store").await?;

    let user_editor = dict.get_value_mut("user");
    // At this point, user_editor points to ["user"].
    // To make the value at ["user"] be Map({"name": "dave"}), we get an editor for "name" field and set it.
    let name_within_user_editor = user_editor.get_value_mut("name");
    name_within_user_editor
        .set(Value::Text("dave".to_string()))
        .await?;

    let user_data = dict.get("user").await?;
    assert_map_contains(&user_data, &["name"]);
    match user_data {
        Value::Doc(user_map) => {
            assert_text_value(user_map.get("name").unwrap(), "dave");
        }
        _ => panic!("Expected user to be a map with name field"),
    }

    // Further nesting: user_editor still points to ["user"].
    let profile_editor = user_editor.get_value_mut("profile");
    // profile_editor points to ["user", "profile"].
    // To make value at ["user", "profile"] be Map({"email": ...}), get editor for "email" and set it.
    let email_within_profile_editor = profile_editor.get_value_mut("email");
    email_within_profile_editor
        .set(Value::Text("dave@example.com".to_string()))
        .await?;

    let user_data_updated = dict.get("user").await?;
    match user_data_updated {
        Value::Doc(user_map_updated) => {
            match user_map_updated.get("profile") {
                Some(Value::Doc(profile_map_updated)) => {
                    assert_text_value(
                        profile_map_updated.get("email").unwrap(),
                        "dave@example.com",
                    );
                }
                _ => panic!("Expected user.profile to be a map with email field"),
            }
            // Check that "user.name" is still there
            assert_text_value(user_map_updated.get("name").unwrap(), "dave");
        }
        _ => panic!("Expected user to be a map after profile update"),
    }

    Ok(())
}

#[tokio::test]
async fn test_value_editor_root_operations() -> Result<()> {
    let (_db, tree, txn, dict) = setup_complete_test_env("editor_test_store").await?;

    // Set some values at the top level
    dict.set("key1", "value1").await?;
    dict.set("key2", "value2").await?;

    // Get a root editor
    let root_editor = dict.get_root_mut();

    // We should be able to get values via the root editor
    match root_editor.get().await? {
        Value::Doc(map) => {
            assert!(map.contains_key("key1"));
            assert!(map.contains_key("key2"));
        }
        _ => panic!("Expected root editor to get a map"),
    }

    // Get values directly from the top level
    assert_text_value(&root_editor.get_value("key1").await?, "value1");

    // Create a new nested map at root level
    let mut nested = Doc::new();
    nested.set("nested_key", "nested_value");
    root_editor
        .get_value_mut("nested")
        .set(nested.into())
        .await?;

    // Verify the nested structure
    assert_map_contains(&root_editor.get_value("nested").await?, &["nested_key"]);

    // Delete a value at root level
    root_editor.delete_child("key1").await?;

    // Verify deletion
    assert_not_found_error(root_editor.get_value("key1").await);

    txn.commit().await?;

    // Verify after commit
    let txn_viewer = tree.new_transaction().await?;
    let viewer_dict = setup_dict_subtree(&txn_viewer, "editor_test_store").await?;
    assert_not_found_error(viewer_dict.get("key1").await);
    assert_eq!(viewer_dict.get_string("key2").await?, "value2");

    Ok(())
}

#[tokio::test]
async fn test_value_editor_delete_methods() -> Result<()> {
    let (_db, tree, txn, dict) = setup_complete_test_env("editor_test_store").await?;

    // Set up a nested structure
    let mut user_profile = Doc::new();
    user_profile.set("name", "Alice");
    user_profile.set("email", "alice@example.com");

    let mut user_data = Doc::new();
    user_data.set("profile", user_profile);
    user_data.set("role", "admin");

    dict.set_value("user", user_data).await?;

    // Get an editor for the user object
    let user_editor = dict.get_value_mut("user");

    // Test delete_child method
    user_editor.delete_child("role").await?;

    // Verify the role is deleted
    assert_not_found_error(user_editor.get_value("role").await);

    // The profile should still exist
    assert_map_contains(&user_editor.get_value("profile").await?, &["name", "email"]);

    // Get editor for profile
    let profile_editor = user_editor.get_value_mut("profile");

    // Test delete_self method
    profile_editor.delete_self().await?;

    // Verify the profile is deleted
    assert_not_found_error(user_editor.get_value("profile").await);

    // But the parent object (user) should still exist
    match dict.get("user").await? {
        Value::Doc(_) => (),
        other => panic!("Expected user map to still exist, got {other:?}"),
    }

    txn.commit().await?;

    // Verify after commit
    let txn_viewer = tree.new_transaction().await?;
    let viewer_dict = setup_dict_subtree(&txn_viewer, "editor_test_store").await?;

    // User exists but has no role or profile
    match viewer_dict.get("user").await? {
        Value::Doc(map) => {
            // Check that the entries are properly marked as deleted (tombstones)
            assert_path_deleted(&map, &["role"]);
            assert_path_deleted(&map, &["profile"]);
        }
        _ => panic!("Expected user to be a map after commit"),
    }

    Ok(())
}

#[tokio::test]
async fn test_value_editor_set_non_map_to_root() -> Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("editor_test_store").await?;

    // Get a root editor
    let root_editor = dict.get_root_mut();

    // Attempting to set a non-map value at root should fail
    assert_type_error(
        root_editor
            .set(Value::Text("test string".to_string()))
            .await,
    );

    // Setting a map value should succeed
    let mut map = Doc::new();
    map.set("key", "value");
    assert!(root_editor.set(map.into()).await.is_ok());

    Ok(())
}
