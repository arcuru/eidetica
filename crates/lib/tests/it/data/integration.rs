//! Integration tests for data module functionality
//!
//! This module tests the integration between different data components
//! and end-to-end scenarios combining multiple features.

use super::helpers::*;
use crate::helpers::build_complex_merge_data;
use eidetica::Result;
use eidetica::crdt::CRDT;
use eidetica::crdt::doc::Value;

#[tokio::test]
async fn test_end_to_end_data_workflow() -> Result<()> {
    let (_instance, tree, txn, dict) = setup_complete_test_env("integration_test").await?;

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

    let user_editor = dict.get_value_mut("user");
    let profile_editor = user_editor.get_value_mut("profile");
    assert_text_value(&profile_editor.get_value("name").await?, "Alice");

    profile_editor
        .get_value_mut("age")
        .set(Value::Text("25".to_string()))
        .await?;

    assert_text_value(&dict.get_at_path(&["user", "profile", "age"]).await?, "25");

    let user_data = dict.get("user").await?;
    match user_data {
        Value::Doc(user_map) => {
            assert_serialization_roundtrip(&user_map)?;
        }
        _ => panic!("Expected user to be a map"),
    }

    user_editor.delete_child("settings").await?;
    assert_not_found_error(dict.get_at_path(&["user", "settings", "theme"]).await);

    assert_text_value(
        &dict.get_at_path(&["user", "profile", "name"]).await?,
        "Alice",
    );
    assert_text_value(&dict.get_at_path(&["user", "profile", "age"]).await?, "25");

    txn.commit().await?;

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
    let (_instance, _tree, _txn, dict) = setup_complete_test_env("consistency_test").await?;

    dict.set_at_path(
        &["app", "config", "version"],
        Value::Text("1.0.0".to_string()),
    )
    .await?;

    let app_editor = dict.get_value_mut("app");
    let config_editor = app_editor.get_value_mut("config");
    config_editor
        .get_value_mut("debug")
        .set(Value::Text("true".to_string()))
        .await?;

    assert_text_value(
        &dict.get_at_path(&["app", "config", "version"]).await?,
        "1.0.0",
    );
    assert_text_value(
        &dict.get_at_path(&["app", "config", "debug"]).await?,
        "true",
    );

    assert_text_value(&config_editor.get_value("version").await?, "1.0.0");
    assert_text_value(&config_editor.get_value("debug").await?, "true");

    dict.set_at_path(&["app", "config"], Value::Text("simple_config".to_string()))
        .await?;

    assert_text_value(
        &dict.get_at_path(&["app", "config"]).await?,
        "simple_config",
    );
    assert_type_error(dict.get_at_path(["app", "config", "version"]).await);

    Ok(())
}

#[tokio::test]
async fn test_merge_operations_with_editors() -> Result<()> {
    let (_instance, _tree, _txn, dict) = setup_complete_test_env("merge_test").await?;

    let (map1, map2) = build_complex_merge_data();

    dict.get_root_mut().set(Value::Doc(map1)).await?;

    // Verify initial state through path operations
    assert_text_value(&dict.get_at_path(&["level1", "key1"]).await?, "value1");
    assert_text_value(&dict.get_at_path(&["top_level_key"]).await?, "top_value");

    let current_map = match dict.get_root_mut().get().await? {
        Value::Doc(m) => m,
        _ => panic!("Expected root to be a map"),
    };

    let merged = current_map.merge(&map2)?;

    dict.get_root_mut().set(Value::Doc(merged)).await?;

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
    let (_instance, _tree, _txn, dict) = setup_complete_test_env("error_test").await?;

    assert_not_found_error(dict.get_at_path(&["nonexistent", "path"]).await);

    let editor = dict.get_value_mut("nonexistent");
    assert_not_found_error(editor.get().await);

    dict.set_at_path(&["string_value"], Value::Text("test".to_string()))
        .await?;

    // Both APIs should give type errors when trying to traverse a string as a map
    assert_type_error(dict.get_at_path(&["string_value", "field"]).await);

    let string_editor = dict.get_value_mut("string_value");
    assert_type_error(string_editor.get_value("field").await);

    let root_editor = dict.get_root_mut();
    assert_type_error(root_editor.set(Value::Text("not_a_map".to_string())).await);

    assert_type_error(
        dict.set_at_path([] as [&str; 0], Value::Text("not_a_map".to_string()))
            .await,
    );

    Ok(())
}

#[tokio::test]
async fn test_path_and_editor_operations_with_deep_nesting() -> Result<()> {
    let (_instance, _tree, _txn, dict) = setup_complete_test_env("deep_nesting_test").await?;

    // Create a deeply nested structure (10 levels deep)
    let mut path = Vec::new();
    for i in 0..10 {
        path.push(format!("level_{i}"));
    }
    path.push("final_value".to_string());

    let path_refs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();

    dict.set_at_path(&path_refs, Value::Text("deep_data".to_string()))
        .await?;
    assert_text_value(&dict.get_at_path(&path_refs).await?, "deep_data");

    let mut editor = dict.get_value_mut(path_refs[0]);
    for path_segment in &path_refs[1..path_refs.len() - 1] {
        editor = editor.get_value_mut(*path_segment);
    }

    editor
        .get_value_mut(path_refs[path_refs.len() - 1])
        .set(Value::Text("modified_deep_data".to_string()))
        .await?;

    assert_text_value(&dict.get_at_path(&path_refs).await?, "modified_deep_data");

    Ok(())
}
