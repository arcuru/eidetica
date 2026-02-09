//! Integration tests for data module functionality
//!
//! This module tests the integration between different data components
//! and end-to-end scenarios combining multiple features.

use super::helpers::*;
use eidetica::Result;
use eidetica::crdt::CRDT;
use eidetica::crdt::doc::Value;

#[tokio::test]
async fn test_end_to_end_data_workflow() -> Result<()> {
    let (db, tree, txn, dict) = setup_complete_test_env("integration_test").await?;

    // Test 1: Create nested data using path operations
    dict.set_at_path(
        &["user", "profile", "name"],
        Value::Text("Alice".to_string()),
    )
    .await?;
    dict.set_at_path(
        &["user", "profile", "email"],
        Value::Text("alice@example.com".to_string()),
    )
    .await?;
    dict.set_at_path(
        &["user", "settings", "theme"],
        Value::Text("dark".to_string()),
    )
    .await?;

    // Test 2: Verify using value editors
    let user_editor = dict.get_value_mut("user");
    let profile_editor = user_editor.get_value_mut("profile");
    assert_text_value(&profile_editor.get_value("name").await?, "Alice");

    // Test 3: Modify using value editors
    profile_editor
        .get_value_mut("age")
        .set(Value::Text("25".to_string()))
        .await?;

    // Test 4: Verify using path operations
    assert_text_value(&dict.get_at_path(&["user", "profile", "age"]).await?, "25");

    // Test 5: Create a map for serialization testing
    let user_data = dict.get("user").await?;
    match user_data {
        Value::Map(user_map) => {
            test_serialization_roundtrip(&user_map)?;
        }
        _ => panic!("Expected user to be a map"),
    }

    // Test 6: Test deletion through value editor
    user_editor.delete_child("settings").await?;
    assert_not_found_error(dict.get_at_path(&["user", "settings", "theme"]).await);

    // Test 7: Verify profile data still exists
    assert_text_value(
        &dict.get_at_path(&["user", "profile", "name"]).await?,
        "Alice",
    );
    assert_text_value(&dict.get_at_path(&["user", "profile", "age"]).await?, "25");

    txn.commit().await?;

    // Test 8: Verify persistence after commit
    let txn_viewer = tree.new_transaction().await?;
    let viewer_dict = setup_dict_subtree(&txn_viewer, "integration_test").await?;
    assert_text_value(
        &viewer_dict
            .get_at_path(&["user", "profile", "name"])
            .await?,
        "Alice",
    );
    assert_not_found_error(
        viewer_dict
            .get_at_path(&["user", "settings", "theme"])
            .await,
    );

    Ok(())
}

#[tokio::test]
async fn test_mixed_operations_consistency() -> Result<()> {
    let (_, _, txn, dict) = setup_complete_test_env("consistency_test").await?;

    // Mix path operations and value editor operations

    // 1. Use path operations to create initial structure
    dict.set_at_path(
        &["app", "config", "version"],
        Value::Text("1.0.0".to_string()),
    )
    .await?;

    // 2. Use value editor to modify
    let app_editor = dict.get_value_mut("app");
    let config_editor = app_editor.get_value_mut("config");
    config_editor
        .get_value_mut("debug")
        .set(Value::Text("true".to_string()))
        .await?;

    // 3. Use path operations to read
    assert_text_value(
        &dict.get_at_path(&["app", "config", "version"]).await?,
        "1.0.0",
    );
    assert_text_value(
        &dict.get_at_path(&["app", "config", "debug"]).await?,
        "true",
    );

    // 4. Use value editor to read
    assert_text_value(&config_editor.get_value("version").await?, "1.0.0");
    assert_text_value(&config_editor.get_value("debug").await?, "true");

    // 5. Use path operations to overwrite part of the structure
    dict.set_at_path(&["app", "config"], Value::Text("simple_config".to_string()))
        .await?;

    // 6. Verify the structure changed
    assert_text_value(
        &dict.get_at_path(&["app", "config"]).await?,
        "simple_config",
    );
    assert_not_found_error(dict.get_at_path(&["app", "config", "version"]).await);

    Ok(())
}

#[tokio::test]
async fn test_merge_operations_with_editors() -> Result<()> {
    let (_, _, txn, dict) = setup_complete_test_env("merge_test").await?;

    // Create initial structure using helpers
    let (map1, map2) = build_complex_merge_data();

    // Set the initial state using value editor
    dict.get_root_mut().set(Value::Map(map1)).await?;

    // Verify initial state through path operations
    assert_text_value(&dict.get_at_path(&["level1", "key1"]).await?, "value1");
    assert_text_value(&dict.get_at_path(&["top_level_key"]).await?, "top_value");

    // Get the current state and merge with map2
    let current_map = match dict.get_root_mut().get().await? {
        Value::Map(m) => m,
        _ => panic!("Expected root to be a map"),
    };

    let merged = current_map.merge(&map2)?;

    // Set the merged result back
    dict.get_root_mut().set(Value::Map(merged)).await?;

    // Verify merge results using path operations
    assert_text_value(&dict.get_at_path(&["level1", "key1"]).await?, "value1"); // preserved
    assert_text_value(&dict.get_at_path(&["level1", "key2"]).await?, "value2"); // added
    assert_text_value(
        &dict.get_at_path(&["level1", "to_update"]).await?,
        "updated_value",
    ); // updated
    assert_not_found_error(dict.get_at_path(&["level1", "to_delete"]).await); // deleted
    assert_not_found_error(dict.get_at_path(&["top_level_key"]).await); // deleted
    assert_text_value(&dict.get_at_path(&["new_top_key"]).await?, "new_top_value"); // added

    Ok(())
}

#[tokio::test]
async fn test_error_handling_across_apis() -> Result<()> {
    let (_, _, txn, dict) = setup_complete_test_env("error_test").await?;

    // Test 1: Path operations error handling
    assert_not_found_error(dict.get_at_path(&["nonexistent", "path"]).await);

    // Test 2: Value editor error handling
    let editor = dict.get_value_mut("nonexistent");
    assert_not_found_error(editor.get().await);

    // Test 3: Type error consistency
    dict.set_at_path(&["string_value"], Value::Text("test".to_string()))
        .await?;

    // Both APIs should give type errors when trying to traverse a string as a map
    assert_type_error(dict.get_at_path(&["string_value", "field"]).await);

    let string_editor = dict.get_value_mut("string_value");
    assert_type_error(string_editor.get_value("field").await);

    // Test 4: Root-level type restrictions
    let root_editor = dict.get_root_mut();
    assert_type_error(root_editor.set(Value::Text("not_a_map".to_string())).await);

    assert_type_error(
        dict.set_at_path(&[], Value::Text("not_a_map".to_string()))
            .await,
    );

    Ok(())
}

#[tokio::test]
async fn test_performance_with_deep_nesting() -> Result<()> {
    let (_, _, txn, dict) = setup_complete_test_env("performance_test").await?;

    // Create a deeply nested structure (10 levels deep)
    let mut path = Vec::new();
    for i in 0..10 {
        path.push(format!("level_{i}"));
    }
    path.push("final_value".to_string());

    let path_refs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();

    // Test path operations with deep nesting
    dict.set_at_path(&path_refs, Value::Text("deep_data".to_string()))
        .await?;
    assert_text_value(&dict.get_at_path(&path_refs).await?, "deep_data");

    // Test value editor operations with deep nesting
    let mut editor = dict.get_value_mut(&path_refs[0]);
    for path_segment in &path_refs[1..path_refs.len() - 1] {
        editor = editor.get_value_mut(path_segment);
    }

    // Modify the final value using editor
    editor
        .get_value_mut(&path_refs[path_refs.len() - 1])
        .set(Value::Text("modified_deep_data".to_string()))
        .await?;

    // Verify through path operations
    assert_text_value(&dict.get_at_path(&path_refs).await?, "modified_deep_data");

    Ok(())
}
