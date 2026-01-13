use super::super::sync::helpers;

#[tokio::test]
#[cfg_attr(miri, ignore)] // tracing uses SystemTime::now() which Miri blocks
async fn test_instance_sync_initialization() {
    let base_db = helpers::setup_instance_with_initialized().await;

    // Verify sync is initialized and accessible
    let sync_ref = base_db.sync().expect("Sync should be initialized");

    // Verify we can access the sync tree through Instance
    assert!(!sync_ref.sync_tree_root_id().to_string().is_empty());
}

#[tokio::test]
async fn test_instance_sync_load() {
    let base_db = helpers::setup_instance_with_initialized().await;

    // Get the sync tree root ID for verification
    let sync_root_id = base_db
        .sync()
        .expect("Sync should be initialized")
        .sync_tree_root_id()
        .clone();

    // Test that the sync tree root ID is accessible and valid
    assert!(!sync_root_id.to_string().is_empty());

    // Note: Loading sync from a tree root ID requires the same backend instance
    // This test demonstrates the API structure for when persistent storage is used
    // In a real scenario with shared/persistent backends, you could:
    // let base_db2 = Instance::new(same_backend).with_sync_from_tree(&sync_root_id).unwrap();
}
