//! Settings metadata tests
//!
//! This module contains tests for settings metadata management including
//! settings tips tracking, metadata propagation, and historical validation.

use eidetica::{
    Instance,
    backend::database::InMemory,
    crdt::{Doc, doc::Value},
};

#[test]
fn test_settings_tips_in_metadata() {
    // Create a backend and database
    let backend = Box::new(InMemory::new());
    let db = Instance::open(backend).expect("Failed to create test instance");

    // Add a test key
    let key_id = "test_key";
    db.add_private_key(key_id).unwrap();

    // Create initial settings
    let mut settings = Doc::new();
    settings.set("name", "test_tree".to_string());

    // Create a tree with authentication
    let tree = db.new_database(settings, key_id).unwrap();

    // Create an operation to add some data
    let op1 = tree.new_transaction().unwrap();
    let kv = op1.get_store::<eidetica::store::DocStore>("data").unwrap();
    kv.set("key1", "value1").unwrap();
    let entry1_id = op1.commit().unwrap();

    // Get the entry and check metadata
    let entry1 = tree.get_entry(&entry1_id).unwrap();
    let metadata = entry1.metadata().expect("Entry should have metadata");

    // Parse metadata and verify settings_tips field exists
    let metadata_obj: serde_json::Value = serde_json::from_str(metadata).unwrap();
    let settings_tips_array = metadata_obj
        .get("settings_tips")
        .expect("Should have settings_tips");
    assert!(
        !settings_tips_array.as_array().unwrap().is_empty(),
        "Settings tips should not be empty"
    );

    // Create another operation to modify settings
    let op2 = tree.new_transaction().unwrap();
    let settings_store = op2
        .get_store::<eidetica::store::DocStore>("_settings")
        .unwrap();
    settings_store.set("description", "A test tree").unwrap();
    let entry2_id = op2.commit().unwrap();

    // Create a third operation that doesn't modify settings
    let op3 = tree.new_transaction().unwrap();
    let kv3 = op3.get_store::<eidetica::store::DocStore>("data").unwrap();
    kv3.set("key2", "value2").unwrap();
    let entry3_id = op3.commit().unwrap();

    // Get the entries and verify settings tips
    let entry2 = tree.get_entry(&entry2_id).unwrap();
    let entry3 = tree.get_entry(&entry3_id).unwrap();

    // Parse metadata from entries
    let metadata2 = entry2.metadata().expect("Entry2 should have metadata");
    let metadata3 = entry3.metadata().expect("Entry3 should have metadata");

    let metadata2_obj: serde_json::Value = serde_json::from_str(metadata2).unwrap();
    let metadata3_obj: serde_json::Value = serde_json::from_str(metadata3).unwrap();

    let settings_tips2 = metadata2_obj
        .get("settings_tips")
        .expect("Should have settings_tips");
    let settings_tips3 = metadata3_obj
        .get("settings_tips")
        .expect("Should have settings_tips");

    assert!(
        !settings_tips2.as_array().unwrap().is_empty(),
        "Settings tips should not be empty after settings update"
    );
    assert!(
        !settings_tips3.as_array().unwrap().is_empty(),
        "Settings tips should not be empty"
    );

    // Entry 3 should have different settings tips (should include entry2)
    let tips3_array = settings_tips3.as_array().unwrap();
    assert!(
        tips3_array.contains(&serde_json::Value::String(entry2_id.to_string())),
        "Entry 3 should have entry 2 in its settings tips"
    );
}

#[test]
fn test_entry_get_settings_from_subtree() {
    // Create a backend and database
    let backend = Box::new(InMemory::new());
    let db = Instance::open(backend).expect("Failed to create test instance");

    // Add a test key
    let key_id = "test_key";
    db.add_private_key(key_id).unwrap();

    // Create initial settings with some data
    let mut settings = Doc::new();
    settings.set("name", "test_tree".to_string());
    settings.set("version", "1.0".to_string());

    // Create a tree
    let tree = db.new_database(settings.clone(), key_id).unwrap();

    // Get the root entry and verify it has _settings subtree
    let root_entry = tree.get_root().unwrap();

    // Entry shouldn't know about settings - that's Transaction's job
    // But we can verify the entry has the _settings subtree data
    let settings_data = root_entry.data("_settings").unwrap();
    let parsed_settings: Doc = serde_json::from_str(settings_data).unwrap();

    // Verify the settings contain what we expect
    match parsed_settings.get("name").unwrap() {
        Value::Text(s) => assert_eq!(s, "test_tree"),
        _ => panic!("Expected string value for name"),
    }
    match parsed_settings.get("version").unwrap() {
        Value::Text(s) => assert_eq!(s, "1.0"),
        _ => panic!("Expected string value for version"),
    }

    // Transaction should be able to get settings properly
    let op = tree.new_transaction().unwrap();
    let op_settings = op.get_settings().unwrap();
    let name = op_settings.get_name().unwrap();
    assert_eq!(name, "test_tree");
}

#[test]
fn test_settings_tips_propagation() {
    // Create a backend and database
    let backend = Box::new(InMemory::new());
    let db = Instance::open(backend).expect("Failed to create test instance");

    // Add a test key
    let key_id = "test_key";
    db.add_private_key(key_id).unwrap();

    // Create a tree
    let settings = Doc::new();
    let tree = db.new_database(settings, key_id).unwrap();

    // Create a chain of entries
    let op1 = tree.new_transaction().unwrap();
    let kv = op1.get_store::<eidetica::store::DocStore>("data").unwrap();
    kv.set("entry", "1").unwrap();
    let entry1_id = op1.commit().unwrap();

    // Modify settings
    let op2 = tree.new_transaction().unwrap();
    let settings_store = op2
        .get_store::<eidetica::store::DocStore>("_settings")
        .unwrap();
    settings_store.set("updated", "true").unwrap();
    let entry2_id = op2.commit().unwrap();

    // Create another entry after settings change
    let op3 = tree.new_transaction().unwrap();
    let kv = op3.get_store::<eidetica::store::DocStore>("data").unwrap();
    kv.set("entry", "3").unwrap();
    let entry3_id = op3.commit().unwrap();

    // Get all entries
    let entry1 = tree.get_entry(&entry1_id).unwrap();
    let entry2 = tree.get_entry(&entry2_id).unwrap();
    let entry3 = tree.get_entry(&entry3_id).unwrap();

    // Parse settings tips from metadata
    let parse_tips = |entry: &eidetica::Entry| -> Vec<String> {
        if let Some(metadata_str) = entry.metadata()
            && let Ok(metadata_obj) = serde_json::from_str::<serde_json::Value>(metadata_str)
            && let Some(tips_array) = metadata_obj.get("settings_tips")
        {
            return tips_array
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
        }
        Vec::new()
    };

    let tips1 = parse_tips(&entry1);
    let tips2 = parse_tips(&entry2);
    let tips3 = parse_tips(&entry3);

    // Entry 1 and 2 should have the same initial settings tips
    assert_eq!(
        tips1, tips2,
        "First two entries should have same settings tips"
    );

    // Entry 3 should have different settings tips (after settings update)
    assert_ne!(
        tips2, tips3,
        "Entry after settings update should have different tips"
    );

    // Entry 3's tips should include entry 2 (the settings update)
    assert!(
        tips3.contains(&entry2_id.to_string()),
        "New settings tips should include the settings update entry"
    );
}

#[test]
fn test_settings_metadata_with_complex_operations() {
    // Test settings metadata handling with complex operations
    let backend = Box::new(InMemory::new());
    let db = Instance::open(backend).expect("Failed to create test instance");
    let key_id = "complex_key";
    db.add_private_key(key_id).unwrap();

    // Create tree with initial settings
    let mut initial_settings = Doc::new();
    initial_settings.set("name", "ComplexTree".to_string());
    initial_settings.set("version", "1.0".to_string());
    let tree = db.new_database(initial_settings, key_id).unwrap();

    // Create several data operations
    let mut data_entry_ids = Vec::new();
    for i in 0..3 {
        let op = tree.new_transaction().unwrap();
        let data_store = op.get_store::<eidetica::store::DocStore>("data").unwrap();
        data_store.set("counter", i.to_string()).unwrap();
        data_store
            .set(format!("data_{i}"), format!("value_{i}"))
            .unwrap();
        let entry_id = op.commit().unwrap();
        data_entry_ids.push(entry_id);
    }

    // Update settings
    let settings_op = tree.new_transaction().unwrap();
    let settings_store = settings_op
        .get_store::<eidetica::store::DocStore>("_settings")
        .unwrap();
    settings_store
        .set("description", "Updated with metadata")
        .unwrap();
    settings_store.set("version", "2.0").unwrap();
    let settings_entry_id = settings_op.commit().unwrap();

    // Create more data operations after settings update
    let mut post_settings_entry_ids = Vec::new();
    for i in 3..6 {
        let op = tree.new_transaction().unwrap();
        let data_store = op.get_store::<eidetica::store::DocStore>("data").unwrap();
        data_store.set("counter", i.to_string()).unwrap();
        data_store
            .set(format!("data_{i}"), format!("value_{i}"))
            .unwrap();
        let entry_id = op.commit().unwrap();
        post_settings_entry_ids.push(entry_id);
    }

    // Verify metadata propagation
    let parse_settings_tips = |entry_id: &str| -> Vec<String> {
        let entry = tree.get_entry(entry_id).unwrap();
        if let Some(metadata_str) = entry.metadata() {
            let metadata_obj: serde_json::Value = serde_json::from_str(metadata_str).unwrap();
            if let Some(tips_array) = metadata_obj.get("settings_tips") {
                return tips_array
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_string())
                    .collect();
            }
        }
        Vec::new()
    };

    // Pre-settings entries should have same settings tips
    let pre_tips = parse_settings_tips(&data_entry_ids[0]);
    for entry_id in &data_entry_ids[1..] {
        let tips = parse_settings_tips(entry_id);
        assert_eq!(pre_tips, tips, "Pre-settings entries should have same tips");
    }

    // Post-settings entries should have different tips (including settings update)
    let post_tips = parse_settings_tips(&post_settings_entry_ids[0]);
    assert_ne!(
        pre_tips, post_tips,
        "Post-settings entries should have different tips"
    );
    assert!(
        post_tips.contains(&settings_entry_id.to_string()),
        "Post-settings entries should include settings update"
    );

    // All post-settings entries should have same tips
    for entry_id in &post_settings_entry_ids[1..] {
        let tips = parse_settings_tips(entry_id);
        assert_eq!(
            post_tips, tips,
            "All post-settings entries should have same tips"
        );
    }
}

#[test]
fn test_settings_metadata_with_branching() {
    // Test settings metadata with branching scenarios
    let backend = Box::new(InMemory::new());
    let db = Instance::open(backend).expect("Failed to create test instance");
    let key_id = "branch_key";
    db.add_private_key(key_id).unwrap();

    let tree = db.new_database(Doc::new(), key_id).unwrap();

    // Create base entry
    let base_op = tree.new_transaction().unwrap();
    let base_store = base_op
        .get_store::<eidetica::store::DocStore>("data")
        .unwrap();
    base_store.set("base", "true").unwrap();
    let base_id = base_op.commit().unwrap();

    // Create two branches from base
    let branch1_op = tree
        .new_transaction_with_tips(std::slice::from_ref(&base_id))
        .unwrap();
    let branch1_store = branch1_op
        .get_store::<eidetica::store::DocStore>("data")
        .unwrap();
    branch1_store.set("branch", "1").unwrap();
    let branch1_id = branch1_op.commit().unwrap();

    let branch2_op = tree
        .new_transaction_with_tips(std::slice::from_ref(&base_id))
        .unwrap();
    let branch2_store = branch2_op
        .get_store::<eidetica::store::DocStore>("data")
        .unwrap();
    branch2_store.set("branch", "2").unwrap();
    let branch2_id = branch2_op.commit().unwrap();

    // Update settings on one branch
    let settings_op = tree
        .new_transaction_with_tips(std::slice::from_ref(&branch1_id))
        .unwrap();
    let settings_store = settings_op
        .get_store::<eidetica::store::DocStore>("_settings")
        .unwrap();
    settings_store.set("branch_settings", "updated").unwrap();
    let settings_id = settings_op.commit().unwrap();

    // Create merge operation
    let merge_tips = vec![settings_id.clone(), branch2_id.clone()];
    let merge_op = tree.new_transaction_with_tips(&merge_tips).unwrap();
    let merge_store = merge_op
        .get_store::<eidetica::store::DocStore>("data")
        .unwrap();
    merge_store.set("merged", "true").unwrap();
    let merge_id = merge_op.commit().unwrap();

    // Verify settings tips in merge operation
    let merge_entry = tree.get_entry(&merge_id).unwrap();
    let metadata_str = merge_entry
        .metadata()
        .expect("Merge entry should have metadata");
    let metadata_obj: serde_json::Value = serde_json::from_str(metadata_str).unwrap();
    let settings_tips = metadata_obj
        .get("settings_tips")
        .expect("Should have settings_tips")
        .as_array()
        .unwrap();

    // Merge should have settings tips that include the settings update
    let settings_tips_strings: Vec<String> = settings_tips
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        settings_tips_strings.contains(&settings_id.to_string()),
        "Merge should include settings update in tips"
    );
}

#[test]
fn test_metadata_consistency_across_operations() {
    // Test that metadata is consistently tracked across different operation types
    let backend = Box::new(InMemory::new());
    let db = Instance::open(backend).expect("Failed to create test instance");
    let key_id = "consistency_key";
    db.add_private_key(key_id).unwrap();

    let mut settings = Doc::new();
    settings.set("initial", "true".to_string());
    let tree = db.new_database(settings, key_id).unwrap();

    // Create authenticated operation (tree already configured with key_id)
    let auth_op = tree.new_transaction().unwrap();
    let auth_store = auth_op
        .get_store::<eidetica::store::DocStore>("auth_data")
        .unwrap();
    auth_store.set("authenticated", "true").unwrap();
    let auth_id = auth_op.commit().unwrap();

    // Create regular operation
    let regular_op = tree.new_transaction().unwrap();
    let regular_store = regular_op
        .get_store::<eidetica::store::DocStore>("regular_data")
        .unwrap();
    regular_store.set("regular", "true").unwrap();
    let regular_id = regular_op.commit().unwrap();

    // Both should have consistent metadata
    let auth_entry = tree.get_entry(&auth_id).unwrap();
    let regular_entry = tree.get_entry(&regular_id).unwrap();

    assert!(
        auth_entry.metadata().is_some(),
        "Auth entry should have metadata"
    );
    assert!(
        regular_entry.metadata().is_some(),
        "Regular entry should have metadata"
    );

    // Parse and compare settings tips
    let get_settings_tips = |entry: &eidetica::Entry| -> Vec<String> {
        let metadata_str = entry.metadata().unwrap();
        let metadata_obj: serde_json::Value = serde_json::from_str(metadata_str).unwrap();
        metadata_obj
            .get("settings_tips")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };

    let auth_tips = get_settings_tips(&auth_entry);
    let regular_tips = get_settings_tips(&regular_entry);

    // Since no settings were changed between operations, tips should be same
    assert_eq!(
        auth_tips, regular_tips,
        "Operations without settings changes should have same tips"
    );
}
