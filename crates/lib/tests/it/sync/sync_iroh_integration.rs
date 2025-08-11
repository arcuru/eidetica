use crate::sync::helpers;
use eidetica::sync::Sync;
use std::sync::Arc;

#[tokio::test]
async fn test_sync_iroh_transport_integration() {
    let (_base_db, mut sync) = helpers::setup();

    // Initially no transport should be enabled
    assert!(sync.start_server_async("ignored").await.is_err());

    // Enable Iroh transport
    sync.enable_iroh_transport().unwrap();

    // Now server operations should work
    sync.start_server_async("ignored").await.unwrap();

    // Get the server address (should be node ID)
    let server_addr = sync.get_server_address().unwrap();
    assert!(!server_addr.is_empty());
    assert!(server_addr.chars().all(|c| c.is_ascii_hexdigit()));

    // Stop the server
    sync.stop_server_async().await.unwrap();
}

#[test]
fn test_sync_iroh_transport_blocking_interface() {
    let (_base_db, mut sync) = helpers::setup();
    sync.enable_iroh_transport().unwrap();

    // Test blocking interface (creates runtime internally)
    sync.start_server("ignored").unwrap();

    let server_addr = sync.get_server_address().unwrap();
    assert!(!server_addr.is_empty());

    sync.stop_server().unwrap();
}

#[tokio::test]
async fn test_sync_transport_switching() {
    let (_base_db, mut sync) = helpers::setup();

    // Start with HTTP transport
    sync.enable_http_transport().unwrap();
    sync.start_server_async("127.0.0.1:0").await.unwrap();

    let http_addr = sync.get_server_address().unwrap();
    assert!(http_addr.contains(":"));

    sync.stop_server_async().await.unwrap();

    // Switch to Iroh transport
    sync.enable_iroh_transport().unwrap();
    sync.start_server_async("ignored").await.unwrap();

    let iroh_addr = sync.get_server_address().unwrap();
    assert!(!iroh_addr.contains(":")); // Node ID format, no port
    assert!(iroh_addr.chars().all(|c| c.is_ascii_hexdigit()));

    sync.stop_server_async().await.unwrap();
}

#[test]
fn test_sync_iroh_settings_persistence() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (base_db, mut sync) = helpers::setup();
        let backend = Arc::clone(base_db.backend());

        // Store some sync settings
        sync.set_setting("transport_type", "iroh", "test_key")
            .unwrap();
        sync.set_setting("node_description", "Test Iroh Node", "test_key")
            .unwrap();

        // Verify settings can be retrieved
        assert_eq!(
            sync.get_setting("transport_type").unwrap(),
            Some("iroh".to_string())
        );
        assert_eq!(
            sync.get_setting("node_description").unwrap(),
            Some("Test Iroh Node".to_string())
        );

        // Create a new Sync instance from the same tree (simulating restart)
        let sync_tree_id = sync.sync_tree_root_id().clone();
        let sync2 = Sync::load(backend, &sync_tree_id).unwrap();

        // Settings should be preserved
        assert_eq!(
            sync2.get_setting("transport_type").unwrap(),
            Some("iroh".to_string())
        );
        assert_eq!(
            sync2.get_setting("node_description").unwrap(),
            Some("Test Iroh Node".to_string())
        );
    });
}
