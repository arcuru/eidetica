use super::helpers::*;
use std::sync::Arc;

#[test]
fn test_sync_creation() {
    let (_base_db, sync) = setup();

    // Verify the sync instance was created successfully
    assert!(!sync.sync_tree_root_id().to_string().is_empty());

    // Verify we can access the sync tree
    let sync_tree = sync.sync_tree();
    assert!(!sync_tree.root_id().to_string().is_empty());
}

#[test]
fn test_sync_load() {
    use eidetica::sync::Sync;

    let (base_db, sync) = setup();
    let sync_root_id = sync.sync_tree_root_id().clone();

    // Load the sync instance from the root ID
    let loaded_sync = Sync::load(Arc::clone(base_db.backend()), &sync_root_id).unwrap();

    // Verify it's the same sync tree
    assert_trees_equal(&sync, &loaded_sync);
}

#[test]
fn test_sync_settings_operations() {
    let (_base_db, mut sync) = setup();

    // Store a dummy setting
    sync.set_setting("test_setting", "test_value").unwrap();

    // Retrieve the setting
    assert_setting(&sync, "test_setting", "test_value");

    // Test retrieving a non-existent setting
    assert_setting_not_found(&sync, "non_existent");

    // Update the setting
    sync.set_setting("test_setting", "updated_value").unwrap();
    assert_setting(&sync, "test_setting", "updated_value");
}

#[test]
fn test_sync_settings_persistence() {
    use eidetica::sync::Sync;

    let (base_db, mut sync) = setup();

    // Store a persistent setting
    sync.set_setting("persistent_setting", "persistent_value")
        .unwrap();
    let sync_root_id = sync.sync_tree_root_id().clone();

    // Load a new Sync instance from the same root ID
    let loaded_sync = Sync::load(Arc::clone(base_db.backend()), &sync_root_id).unwrap();

    // Verify the setting was persisted
    assert_setting(&loaded_sync, "persistent_setting", "persistent_value");
}

#[test]
fn test_sync_multiple_settings() {
    let (_base_db, mut sync) = setup();

    // Set multiple settings using helper
    let settings_to_set = &[
        ("config_option_1", "value_1"),
        ("config_option_2", "value_2"),
        ("server_url", "https://example.com"),
    ];
    set_multiple_settings(&mut sync, settings_to_set);

    // Verify all settings using helper
    assert_multiple_settings(&sync, settings_to_set);
}
