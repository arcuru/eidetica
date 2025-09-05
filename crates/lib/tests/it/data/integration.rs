//! Integration tests for data module functionality
//!
//! This module tests the integration between different data components
//! and end-to-end scenarios combining multiple features.

use super::helpers::*;
use eidetica::crdt::CRDT;
use eidetica::crdt::doc::Value;

#[test]
fn test_end_to_end_data_workflow() -> eidetica::Result<()> {
    let (db, tree, op, dict) = setup_complete_test_env("integration_test")?;

    // Test 1: Create nested data using path operations
    dict.set_at_path(&["user", "profile", "name"], Value::Text("Alice".to_string()))?;
    dict.set_at_path(&["user", "profile", "email"], Value::Text("alice@example.com".to_string()))?;
    dict.set_at_path(&["user", "settings", "theme"], Value::Text("dark".to_string()))?;

    // Test 2: Verify using value editors
    let user_editor = dict.get_value_mut("user");
    let profile_editor = user_editor.get_value_mut("profile");
    assert_text_value(&profile_editor.get_value("name")?, "Alice");

    // Test 3: Modify using value editors
    profile_editor.get_value_mut("age").set(Value::Text("25".to_string()))?;

    // Test 4: Verify using path operations
    assert_text_value(&dict.get_at_path(&["user", "profile", "age"])?, "25");

    // Test 5: Create a map for serialization testing
    let user_data = dict.get("user")?;
    match user_data {
        Value::Map(user_map) => {
            test_serialization_roundtrip(&user_map)?;
        }
        _ => panic!("Expected user to be a map"),
    }

    // Test 6: Test deletion through value editor
    user_editor.delete_child("settings")?;
    assert_not_found_error(dict.get_at_path(&["user", "settings", "theme"]));

    // Test 7: Verify profile data still exists
    assert_text_value(&dict.get_at_path(&["user", "profile", "name"])?, "Alice");
    assert_text_value(&dict.get_at_path(&["user", "profile", "age"])?, "25");

    op.commit()?;

    // Test 8: Verify persistence after commit
    let viewer_op = tree.new_transaction()?;
    let viewer_dict = setup_dict_subtree(&viewer_op, "integration_test")?;
    assert_text_value(&viewer_dict.get_at_path(&["user", "profile", "name"])?, "Alice");
    assert_not_found_error(viewer_dict.get_at_path(&["user", "settings", "theme"]));

    Ok(())
}

#[test]
fn test_mixed_operations_consistency() -> eidetica::Result<()> {
    let (_, _, op, dict) = setup_complete_test_env("consistency_test")?;

    // Mix path operations and value editor operations
    
    // 1. Use path operations to create initial structure
    dict.set_at_path(&["app", "config", "version"], Value::Text("1.0.0".to_string()))?;
    
    // 2. Use value editor to modify
    let app_editor = dict.get_value_mut("app");
    let config_editor = app_editor.get_value_mut("config");
    config_editor.get_value_mut("debug").set(Value::Text("true".to_string()))?;
    
    // 3. Use path operations to read
    assert_text_value(&dict.get_at_path(&["app", "config", "version"])?, "1.0.0");
    assert_text_value(&dict.get_at_path(&["app", "config", "debug"])?, "true");
    
    // 4. Use value editor to read
    assert_text_value(&config_editor.get_value("version")?, "1.0.0");
    assert_text_value(&config_editor.get_value("debug")?, "true");
    
    // 5. Use path operations to overwrite part of the structure
    dict.set_at_path(&["app", "config"], Value::Text("simple_config".to_string()))?;
    
    // 6. Verify the structure changed
    assert_text_value(&dict.get_at_path(&["app", "config"])?, "simple_config");
    assert_not_found_error(dict.get_at_path(&["app", "config", "version"]));
    
    Ok(())
}

#[test]
fn test_merge_operations_with_editors() -> eidetica::Result<()> {
    let (_, _, op, dict) = setup_complete_test_env("merge_test")?;

    // Create initial structure using helpers
    let (map1, map2) = build_complex_merge_data();
    
    // Set the initial state using value editor
    dict.get_root_mut().set(Value::Map(map1))?;
    
    // Verify initial state through path operations
    assert_text_value(&dict.get_at_path(&["level1", "key1"])?, "value1");
    assert_text_value(&dict.get_at_path(&["top_level_key"])?, "top_value");
    
    // Get the current state and merge with map2
    let current_map = match dict.get_root_mut().get()? {
        Value::Map(m) => m,
        _ => panic!("Expected root to be a map"),
    };
    
    let merged = current_map.merge(&map2)?;
    
    // Set the merged result back
    dict.get_root_mut().set(Value::Map(merged))?;
    
    // Verify merge results using path operations
    assert_text_value(&dict.get_at_path(&["level1", "key1"])?, "value1"); // preserved
    assert_text_value(&dict.get_at_path(&["level1", "key2"])?, "value2"); // added
    assert_text_value(&dict.get_at_path(&["level1", "to_update"])?, "updated_value"); // updated
    assert_not_found_error(dict.get_at_path(&["level1", "to_delete"])); // deleted
    assert_not_found_error(dict.get_at_path(&["top_level_key"])); // deleted
    assert_text_value(&dict.get_at_path(&["new_top_key"])?, "new_top_value"); // added
    
    Ok(())
}

#[test]
fn test_error_handling_across_apis() -> eidetica::Result<()> {
    let (_, _, op, dict) = setup_complete_test_env("error_test")?;

    // Test 1: Path operations error handling
    assert_not_found_error(dict.get_at_path(&["nonexistent", "path"]));
    
    // Test 2: Value editor error handling  
    let editor = dict.get_value_mut("nonexistent");
    assert_not_found_error(editor.get());
    
    // Test 3: Type error consistency
    dict.set_at_path(&["string_value"], Value::Text("test".to_string()))?;
    
    // Both APIs should give type errors when trying to traverse a string as a map
    assert_type_error(dict.get_at_path(&["string_value", "field"]));
    
    let string_editor = dict.get_value_mut("string_value");
    assert_type_error(string_editor.get_value("field"));
    
    // Test 4: Root-level type restrictions
    let root_editor = dict.get_root_mut();
    assert_type_error(root_editor.set(Value::Text("not_a_map".to_string())));
    
    assert_type_error(dict.set_at_path(&[], Value::Text("not_a_map".to_string())));
    
    Ok(())
}

#[test]
fn test_performance_with_deep_nesting() -> eidetica::Result<()> {
    let (_, _, op, dict) = setup_complete_test_env("performance_test")?;

    // Create a deeply nested structure (10 levels deep)
    let mut path = Vec::new();
    for i in 0..10 {
        path.push(format!("level_{i}"));
    }
    path.push("final_value".to_string());
    
    let path_refs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
    
    // Test path operations with deep nesting
    dict.set_at_path(&path_refs, Value::Text("deep_data".to_string()))?;
    assert_text_value(&dict.get_at_path(&path_refs)?, "deep_data");
    
    // Test value editor operations with deep nesting
    let mut editor = dict.get_value_mut(&path_refs[0]);
    for path_segment in &path_refs[1..path_refs.len()-1] {
        editor = editor.get_value_mut(path_segment);
    }
    
    // Modify the final value using editor
    editor.get_value_mut(&path_refs[path_refs.len()-1]).set(Value::Text("modified_deep_data".to_string()))?;
    
    // Verify through path operations
    assert_text_value(&dict.get_at_path(&path_refs)?, "modified_deep_data");
    
    Ok(())
}