//! Tests for device identity functionality in sync module.

use super::helpers::*;

#[tokio::test]
async fn test_device_id_functionality() {
    let (base_db, _sync) = setup().await;

    // Get device ID from Instance
    let base_db_device_id = base_db.device_id_string().await.unwrap();

    // Verify device ID format (should be ed25519:base64)
    assert!(base_db_device_id.starts_with("ed25519:"));
    assert!(base_db_device_id.len() > 8); // More than just the prefix
}

#[tokio::test]
async fn test_device_id_unique_across_databases() {
    // Create two separate databases
    let (instance1, _user1) = crate::helpers::setup_db().await;
    let (instance2, _user2) = crate::helpers::setup_db().await;

    let base_db1 = std::sync::Arc::new(instance1);
    let base_db2 = std::sync::Arc::new(instance2);

    // Device IDs should be different (each Instance generates its own unique device key)
    let device_id_1 = base_db1.device_id_string().await.unwrap();
    let device_id_2 = base_db2.device_id_string().await.unwrap();

    assert_ne!(device_id_1, device_id_2);
}
