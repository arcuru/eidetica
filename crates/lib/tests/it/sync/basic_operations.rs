use super::helpers::*;

#[tokio::test]
async fn test_sync_creation() {
    let (_base_db, sync) = setup().await;

    // Verify the sync instance was created successfully
    assert!(!sync.sync_tree_root_id().to_string().is_empty());

    // Verify we can access the sync tree
    let sync_tree = sync.sync_tree();
    assert!(!sync_tree.root_id().to_string().is_empty());
}

#[tokio::test]
async fn test_sync_load() {
    use eidetica::sync::Sync;

    let (base_db, sync) = setup().await;
    let sync_root_id = sync.sync_tree_root_id().clone();

    // Load the sync instance from the root ID
    let loaded_sync = Sync::load(base_db.clone(), &sync_root_id).await.unwrap();

    // Verify it's the same sync tree
    assert_trees_equal(&sync, &loaded_sync);
}

#[tokio::test]
async fn test_sync_settings_operations() {
    let (_base_db, sync) = setup().await;

    // Store a dummy setting
    sync.set_setting("test_setting", "test_value")
        .await
        .unwrap();

    // Retrieve the setting
    assert_setting(&sync, "test_setting", "test_value").await;

    // Test retrieving a non-existent setting
    assert_setting_not_found(&sync, "non_existent").await;

    // Update the setting
    sync.set_setting("test_setting", "updated_value")
        .await
        .unwrap();
    assert_setting(&sync, "test_setting", "updated_value").await;
}

#[tokio::test]
async fn test_sync_settings_persistence() {
    use eidetica::sync::Sync;

    let (base_db, sync) = setup().await;

    // Store a persistent setting
    sync.set_setting("persistent_setting", "persistent_value")
        .await
        .unwrap();
    let sync_root_id = sync.sync_tree_root_id().clone();

    // Load a new Sync instance from the same root ID
    let loaded_sync = Sync::load(base_db.clone(), &sync_root_id).await.unwrap();

    // Verify the setting was persisted
    assert_setting(&loaded_sync, "persistent_setting", "persistent_value").await;
}

#[tokio::test]
async fn test_sync_multiple_settings() {
    let (_base_db, sync) = setup().await;

    // Set multiple settings using helper
    let settings_to_set = &[
        ("config_option_1", "value_1"),
        ("config_option_2", "value_2"),
        ("server_url", "https://example.com"),
    ];
    set_multiple_settings(&sync, settings_to_set).await;

    // Verify all settings using helper
    assert_multiple_settings(&sync, settings_to_set).await;
}
