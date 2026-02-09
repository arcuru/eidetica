//! Tests for path-based operations on Doc structures
//!
//! This module tests the path-based API for accessing and modifying nested data
//! structures using string paths.

use eidetica::Result;
use eidetica::crdt::Doc;
use eidetica::crdt::doc::Value;

use super::helpers::*;

#[tokio::test]
async fn test_dict_set_at_path_and_get_at_path_simple() -> Result<()> {
    let (_db, tree, txn, dict) = setup_complete_test_env("path_test_store").await?;

    let path = ["simple_key"];
    let value = Value::Text("simple_value".to_string());

    test_path_operations(&dict, &path, value.clone()).await?;

    // Verify with regular get as well
    assert_text_value(&dict.get("simple_key").await?, "simple_value");

    txn.commit().await?;

    // Verify after commit
    let txn_viewer = tree.new_transaction().await?;
    let viewer_dict = setup_path_test_dict(&txn_viewer).await?;
    assert_eq!(viewer_dict.get_at_path(path).await?, value);
    assert_text_value(&viewer_dict.get("simple_key").await?, "simple_value");

    Ok(())
}

#[tokio::test]
async fn test_dict_set_at_path_and_get_at_path_nested() -> Result<()> {
    let (_db, tree, txn, dict) = setup_complete_test_env("path_test_store").await?;

    let path = ["user", "profile", "email"];
    let value = Value::Text("test@example.com".to_string());

    test_path_operations(&dict, &path, value.clone()).await?;

    // Verify intermediate map structure
    let profile_path = ["user", "profile"];
    match dict.get_at_path(profile_path).await? {
        Value::Doc(profile_map) => {
            assert_text_value(profile_map.get("email").unwrap(), "test@example.com");
        }
        _ => panic!("Expected user.profile to be a map"),
    }

    txn.commit().await?;

    // Verify after commit
    let txn_viewer = tree.new_transaction().await?;
    let viewer_dict = setup_path_test_dict(&txn_viewer).await?;
    assert_eq!(viewer_dict.get_at_path(path).await?, value);

    Ok(())
}

#[tokio::test]
async fn test_dict_set_at_path_creates_intermediate_maps() -> Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("path_test_store").await?;

    let path = ["a", "b", "c"];
    let value = Value::Text("deep_value".to_string());

    test_path_operations(&dict, &path, value.clone()).await?;

    // Verify intermediate maps were created
    match dict.get_at_path(["a", "b"]).await? {
        Value::Doc(_) => (),
        other => panic!("Expected a.b to be a map, got {other:?}"),
    }
    match dict.get_at_path(["a"]).await? {
        Value::Doc(_) => (),
        other => panic!("Expected a to be a map, got {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn test_dict_set_at_path_overwrites_non_map() -> Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("path_test_store").await?;

    // Set user.profile = "string_value"
    dict.set_at_path(["user", "profile"], Value::Text("string_value".to_string()))
        .await?;

    // Now try to set user.profile.name = "charlie"
    let new_path = ["user", "profile", "name"];
    let new_value = Value::Text("charlie".to_string());
    dict.set_at_path(new_path, new_value.clone()).await?;

    assert_eq!(dict.get_at_path(new_path).await?, new_value);

    // Verify that 'user.profile' is now a map
    match dict.get_at_path(["user", "profile"]).await? {
        Value::Doc(profile_map) => {
            assert_text_value(profile_map.get("name").unwrap(), "charlie");
        }
        _ => panic!("Expected user.profile to be a map after overwrite"),
    }

    Ok(())
}

#[tokio::test]
async fn test_dict_get_at_path_not_found() -> Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("path_test_store").await?;

    let path = ["non", "existent", "key"];
    assert_not_found_error(dict.get_at_path(path).await);

    // Test path where an intermediate key segment does not exist within a valid map.
    // Set up: existing_root -> some_child_map (empty map)
    let child_map = Doc::new();
    dict.set_at_path(["existing_root_map"], child_map.into())
        .await?;

    let path_intermediate_missing = ["existing_root_map", "non_existent_child_in_map", "key"];
    assert_not_found_error(dict.get_at_path(path_intermediate_missing).await);

    // Test path leading to a tombstone
    let tombstone_path = ["deleted", "item"];
    dict.set_at_path(tombstone_path, Value::Text("temp".to_string()))
        .await?;
    dict.set_at_path(tombstone_path, Value::Deleted).await?;
    assert_not_found_error(dict.get_at_path(tombstone_path).await);

    Ok(())
}

#[tokio::test]
async fn test_dict_get_at_path_invalid_intermediate_type() -> Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("path_test_store").await?;

    // Set a.b = "string" (not a map)
    dict.set_at_path(["a", "b"], Value::Text("i_am_not_a_map".to_string()))
        .await?;

    // Try to get a.b.c
    let path = ["a", "b", "c"];
    assert_type_error(dict.get_at_path(path).await);

    Ok(())
}

#[tokio::test]
async fn test_dict_set_at_path_empty_path() -> Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("path_test_store").await?;

    let path: Vec<String> = vec![];

    // Setting a non-map value at the root should fail
    assert_type_error(
        dict.set_at_path(&path, Value::Text("test".to_string()))
            .await,
    );

    // Setting a map value at the root should succeed
    let nested_map = Doc::new();
    assert!(dict.set_at_path(&path, nested_map.into()).await.is_ok());

    Ok(())
}

#[tokio::test]
async fn test_dict_get_at_path_empty_path() -> Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("path_test_store").await?;

    let path: Vec<String> = vec![];

    // Getting the root should return a map (the entire Doc contents)
    match dict.get_at_path(&path).await? {
        Value::Doc(_) => (),
        other => panic!("Expected Map for root path, got {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn test_dict_cascading_delete() -> Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("path_test_store").await?;

    // Create a deeply nested structure using path operations
    dict.set_at_path(
        ["level1", "level2", "level3", "deepest"],
        Value::Text("treasure".to_string()),
    )
    .await?;

    // Verify it was created
    assert_text_value(
        &dict
            .get_at_path(["level1", "level2", "level3", "deepest"])
            .await?,
        "treasure",
    );

    // Delete the entire structure by setting level1 to tombstone
    dict.set_at_path(["level1"], Value::Deleted).await?;

    // Verify it's gone from get
    assert_not_found_error(dict.get_at_path(["level1"]).await);
    assert_not_found_error(
        dict.get_at_path(["level1", "level2", "level3", "deepest"])
            .await,
    );

    // Add a new level1 with different content and verify it works
    dict.set_at_path(
        ["level1", "new_value"],
        Value::Text("resurrected".to_string()),
    )
    .await?;

    // Verify level1 is accessible again
    assert_text_value(
        &dict.get_at_path(["level1", "new_value"]).await?,
        "resurrected",
    );

    Ok(())
}

#[tokio::test]
async fn test_path_operations_complex_scenarios() -> Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("path_test_store").await?;

    // Test multiple overlapping paths
    dict.set_at_path(
        ["user", "profile", "name"],
        Value::Text("Alice".to_string()),
    )
    .await?;
    dict.set_at_path(
        ["user", "profile", "email"],
        Value::Text("alice@example.com".to_string()),
    )
    .await?;
    dict.set_at_path(
        ["user", "settings", "theme"],
        Value::Text("dark".to_string()),
    )
    .await?;
    dict.set_at_path(
        ["user", "settings", "language"],
        Value::Text("en".to_string()),
    )
    .await?;

    // Verify all paths work
    assert_text_value(
        &dict.get_at_path(["user", "profile", "name"]).await?,
        "Alice",
    );
    assert_text_value(
        &dict.get_at_path(["user", "profile", "email"]).await?,
        "alice@example.com",
    );
    assert_text_value(
        &dict.get_at_path(["user", "settings", "theme"]).await?,
        "dark",
    );
    assert_text_value(
        &dict.get_at_path(["user", "settings", "language"]).await?,
        "en",
    );

    // Verify intermediate structures
    match dict.get_at_path(["user"]).await? {
        Value::Doc(user_map) => {
            assert!(user_map.contains_key("profile"));
            assert!(user_map.contains_key("settings"));
        }
        _ => panic!("Expected user to be a map"),
    }

    match dict.get_at_path(["user", "profile"]).await? {
        Value::Doc(_) => {
            let profile_map = dict.get_at_path(["user", "profile"]).await?;
            assert_map_contains(&profile_map, &["name", "email"]);
        }
        _ => panic!("Expected user.profile to be a map"),
    }

    // Test partial deletion
    dict.set_at_path(["user", "profile"], Value::Deleted)
        .await?;

    // Profile should be gone but settings should remain
    assert_not_found_error(dict.get_at_path(["user", "profile"]).await);
    assert_not_found_error(dict.get_at_path(["user", "profile", "name"]).await);
    assert_text_value(
        &dict.get_at_path(["user", "settings", "theme"]).await?,
        "dark",
    );

    Ok(())
}
