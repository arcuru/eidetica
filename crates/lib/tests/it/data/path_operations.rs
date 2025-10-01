//! Tests for path-based operations on Doc structures
//!
//! This module tests the path-based API for accessing and modifying nested data
//! structures using string paths.

use eidetica::crdt::doc::Value;

use super::helpers::*;

#[test]
fn test_dict_set_at_path_and_get_at_path_simple() -> eidetica::Result<()> {
    let (_db, tree, op, dict) = setup_complete_test_env("path_test_store")?;

    let path = ["simple_key"];
    let value = Value::Text("simple_value".to_string());

    test_path_operations(&dict, &path, value.clone())?;

    // Verify with regular get as well
    assert_text_value(&dict.get("simple_key")?, "simple_value");

    op.commit()?;

    // Verify after commit
    let viewer_op = tree.new_transaction()?;
    let viewer_dict = setup_path_test_dict(&viewer_op)?;
    assert_eq!(viewer_dict.get_at_path(path)?, value);
    assert_text_value(&viewer_dict.get("simple_key")?, "simple_value");

    Ok(())
}

#[test]
fn test_dict_set_at_path_and_get_at_path_nested() -> eidetica::Result<()> {
    let (_db, tree, op, dict) = setup_complete_test_env("path_test_store")?;

    let path = ["user", "profile", "email"];
    let value = Value::Text("test@example.com".to_string());

    test_path_operations(&dict, &path, value.clone())?;

    // Verify intermediate map structure
    let profile_path = ["user", "profile"];
    match dict.get_at_path(profile_path)? {
        Value::Doc(profile_map) => {
            assert_text_value(profile_map.get("email").unwrap(), "test@example.com");
        }
        _ => panic!("Expected user.profile to be a map"),
    }

    op.commit()?;

    // Verify after commit
    let viewer_op = tree.new_transaction()?;
    let viewer_dict = setup_path_test_dict(&viewer_op)?;
    assert_eq!(viewer_dict.get_at_path(path)?, value);

    Ok(())
}

#[test]
fn test_dict_set_at_path_creates_intermediate_maps() -> eidetica::Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("path_test_store")?;

    let path = ["a", "b", "c"];
    let value = Value::Text("deep_value".to_string());

    test_path_operations(&dict, &path, value.clone())?;

    // Verify intermediate maps were created
    match dict.get_at_path(["a", "b"])? {
        Value::Doc(_) => (),
        other => panic!("Expected a.b to be a map, got {other:?}"),
    }
    match dict.get_at_path(["a"])? {
        Value::Doc(_) => (),
        other => panic!("Expected a to be a map, got {other:?}"),
    }

    Ok(())
}

#[test]
fn test_dict_set_at_path_overwrites_non_map() -> eidetica::Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("path_test_store")?;

    // Set user.profile = "string_value"
    dict.set_at_path(["user", "profile"], Value::Text("string_value".to_string()))?;

    // Now try to set user.profile.name = "charlie"
    let new_path = ["user", "profile", "name"];
    let new_value = Value::Text("charlie".to_string());
    dict.set_at_path(new_path, new_value.clone())?;

    assert_eq!(dict.get_at_path(new_path)?, new_value);

    // Verify that 'user.profile' is now a map
    match dict.get_at_path(["user", "profile"])? {
        Value::Doc(profile_map) => {
            assert_text_value(profile_map.get("name").unwrap(), "charlie");
        }
        _ => panic!("Expected user.profile to be a map after overwrite"),
    }

    Ok(())
}

#[test]
fn test_dict_get_at_path_not_found() -> eidetica::Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("path_test_store")?;

    let path = ["non", "existent", "key"];
    assert_not_found_error(dict.get_at_path(path));

    // Test path where an intermediate key segment does not exist within a valid map.
    // Set up: existing_root -> some_child_map (empty map)
    let child_map = eidetica::crdt::Doc::new();
    dict.set_at_path(["existing_root_map"], child_map.into())?;

    let path_intermediate_missing = ["existing_root_map", "non_existent_child_in_map", "key"];
    assert_not_found_error(dict.get_at_path(path_intermediate_missing));

    // Test path leading to a tombstone
    let tombstone_path = ["deleted", "item"];
    dict.set_at_path(tombstone_path, Value::Text("temp".to_string()))?;
    dict.set_at_path(tombstone_path, Value::Deleted)?;
    assert_not_found_error(dict.get_at_path(tombstone_path));

    Ok(())
}

#[test]
fn test_dict_get_at_path_invalid_intermediate_type() -> eidetica::Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("path_test_store")?;

    // Set a.b = "string" (not a map)
    dict.set_at_path(["a", "b"], Value::Text("i_am_not_a_map".to_string()))?;

    // Try to get a.b.c
    let path = ["a", "b", "c"];
    assert_type_error(dict.get_at_path(path));

    Ok(())
}

#[test]
fn test_dict_set_at_path_empty_path() -> eidetica::Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("path_test_store")?;

    let path: Vec<String> = vec![];

    // Setting a non-map value at the root should fail
    assert_type_error(dict.set_at_path(&path, Value::Text("test".to_string())));

    // Setting a map value at the root should succeed
    let nested_map = eidetica::crdt::Doc::new();
    assert!(dict.set_at_path(&path, nested_map.into()).is_ok());

    Ok(())
}

#[test]
fn test_dict_get_at_path_empty_path() -> eidetica::Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("path_test_store")?;

    let path: Vec<String> = vec![];

    // Getting the root should return a map (the entire Doc contents)
    match dict.get_at_path(&path)? {
        Value::Doc(_) => (),
        other => panic!("Expected Map for root path, got {other:?}"),
    }

    Ok(())
}

#[test]
fn test_dict_cascading_delete() -> eidetica::Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("path_test_store")?;

    // Create a deeply nested structure using path operations
    dict.set_at_path(
        ["level1", "level2", "level3", "deepest"],
        Value::Text("treasure".to_string()),
    )?;

    // Verify it was created
    assert_text_value(
        &dict.get_at_path(["level1", "level2", "level3", "deepest"])?,
        "treasure",
    );

    // Delete the entire structure by setting level1 to tombstone
    dict.set_at_path(["level1"], Value::Deleted)?;

    // Verify it's gone from get
    assert_not_found_error(dict.get_at_path(["level1"]));
    assert_not_found_error(dict.get_at_path(["level1", "level2", "level3", "deepest"]));

    // Add a new level1 with different content and verify it works
    dict.set_at_path(
        ["level1", "new_value"],
        Value::Text("resurrected".to_string()),
    )?;

    // Verify level1 is accessible again
    assert_text_value(&dict.get_at_path(["level1", "new_value"])?, "resurrected");

    Ok(())
}

#[test]
fn test_path_operations_complex_scenarios() -> eidetica::Result<()> {
    let (_, _, _op, dict) = setup_complete_test_env("path_test_store")?;

    // Test multiple overlapping paths
    dict.set_at_path(
        ["user", "profile", "name"],
        Value::Text("Alice".to_string()),
    )?;
    dict.set_at_path(
        ["user", "profile", "email"],
        Value::Text("alice@example.com".to_string()),
    )?;
    dict.set_at_path(
        ["user", "settings", "theme"],
        Value::Text("dark".to_string()),
    )?;
    dict.set_at_path(
        ["user", "settings", "language"],
        Value::Text("en".to_string()),
    )?;

    // Verify all paths work
    assert_text_value(&dict.get_at_path(["user", "profile", "name"])?, "Alice");
    assert_text_value(
        &dict.get_at_path(["user", "profile", "email"])?,
        "alice@example.com",
    );
    assert_text_value(&dict.get_at_path(["user", "settings", "theme"])?, "dark");
    assert_text_value(&dict.get_at_path(["user", "settings", "language"])?, "en");

    // Verify intermediate structures
    match dict.get_at_path(["user"])? {
        Value::Doc(user_map) => {
            assert!(user_map.as_hashmap().contains_key("profile"));
            assert!(user_map.as_hashmap().contains_key("settings"));
        }
        _ => panic!("Expected user to be a map"),
    }

    match dict.get_at_path(["user", "profile"])? {
        Value::Doc(_) => {
            let profile_map = dict.get_at_path(["user", "profile"])?;
            assert_map_contains(&profile_map, &["name", "email"]);
        }
        _ => panic!("Expected user.profile to be a map"),
    }

    // Test partial deletion
    dict.set_at_path(["user", "profile"], Value::Deleted)?;

    // Profile should be gone but settings should remain
    assert_not_found_error(dict.get_at_path(["user", "profile"]));
    assert_not_found_error(dict.get_at_path(["user", "profile", "name"]));
    assert_text_value(&dict.get_at_path(["user", "settings", "theme"])?, "dark");

    Ok(())
}
