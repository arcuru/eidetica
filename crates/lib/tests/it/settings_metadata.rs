use eidetica::backend::database::InMemory;
use eidetica::basedb::BaseDB;
use eidetica::crdt::Map;
use eidetica::crdt::map::Value;

#[test]
fn test_settings_tips_in_metadata() {
    // Create a backend and database
    let backend = Box::new(InMemory::new());
    let db = BaseDB::new(backend);

    // Add a test key
    let key_id = "test_key";
    db.add_private_key(key_id).unwrap();

    // Create initial settings
    let mut settings = Map::new();
    settings.set("name".to_string(), "test_tree".to_string());

    // Create a tree with authentication
    let tree = db.new_tree(settings, key_id).unwrap();

    // Create an operation to add some data
    let op1 = tree.new_operation().unwrap();
    let kv = op1.get_subtree::<eidetica::subtree::Dict>("data").unwrap();
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
    let op2 = tree.new_operation().unwrap();
    let settings_store = op2
        .get_subtree::<eidetica::subtree::Dict>("_settings")
        .unwrap();
    settings_store.set("description", "A test tree").unwrap();
    let entry2_id = op2.commit().unwrap();

    // Create a third operation that doesn't modify settings
    let op3 = tree.new_operation().unwrap();
    let kv3 = op3.get_subtree::<eidetica::subtree::Dict>("data").unwrap();
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
    let db = BaseDB::new(backend);

    // Add a test key
    let key_id = "test_key";
    db.add_private_key(key_id).unwrap();

    // Create initial settings with some data
    let mut settings = Map::new();
    settings.set("name".to_string(), "test_tree".to_string());
    settings.set("version".to_string(), "1.0".to_string());

    // Create a tree
    let tree = db.new_tree(settings.clone(), key_id).unwrap();

    // Get the root entry and verify it has _settings subtree
    let root_entry = tree.get_root().unwrap();

    // Entry shouldn't know about settings - that's AtomicOp's job
    // But we can verify the entry has the _settings subtree data
    let settings_data = root_entry.data("_settings").unwrap();
    let parsed_settings: Map = serde_json::from_str(settings_data).unwrap();

    // Verify the settings contain what we expect
    match parsed_settings.get("name").unwrap() {
        Value::Text(s) => assert_eq!(s, "test_tree"),
        _ => panic!("Expected string value for name"),
    }
    match parsed_settings.get("version").unwrap() {
        Value::Text(s) => assert_eq!(s, "1.0"),
        _ => panic!("Expected string value for version"),
    }

    // AtomicOp should be able to get settings properly
    let op = tree.new_operation().unwrap();
    let op_settings = op.get_settings().unwrap();
    match op_settings.get("name").unwrap() {
        Value::Text(s) => assert_eq!(s, "test_tree"),
        _ => panic!("Expected string value for name"),
    }
}

#[test]
fn test_settings_tips_propagation() {
    // Create a backend and database
    let backend = Box::new(InMemory::new());
    let db = BaseDB::new(backend);

    // Add a test key
    let key_id = "test_key";
    db.add_private_key(key_id).unwrap();

    // Create a tree
    let settings = Map::new();
    let tree = db.new_tree(settings, key_id).unwrap();

    // Create a chain of entries
    let op1 = tree.new_operation().unwrap();
    let kv = op1.get_subtree::<eidetica::subtree::Dict>("data").unwrap();
    kv.set("entry", "1").unwrap();
    let entry1_id = op1.commit().unwrap();

    // Modify settings
    let op2 = tree.new_operation().unwrap();
    let settings_store = op2
        .get_subtree::<eidetica::subtree::Dict>("_settings")
        .unwrap();
    settings_store.set("updated", "true").unwrap();
    let entry2_id = op2.commit().unwrap();

    // Create another entry after settings change
    let op3 = tree.new_operation().unwrap();
    let kv = op3.get_subtree::<eidetica::subtree::Dict>("data").unwrap();
    kv.set("entry", "3").unwrap();
    let entry3_id = op3.commit().unwrap();

    // Get all entries
    let entry1 = tree.get_entry(&entry1_id).unwrap();
    let entry2 = tree.get_entry(&entry2_id).unwrap();
    let entry3 = tree.get_entry(&entry3_id).unwrap();

    // Parse settings tips from metadata
    let parse_tips = |entry: &eidetica::entry::Entry| -> Vec<String> {
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
