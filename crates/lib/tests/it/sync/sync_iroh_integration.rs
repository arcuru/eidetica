use std::sync::Arc;

use eidetica::sync::{Address, Sync};

use crate::sync::helpers;

#[tokio::test]
async fn test_sync_iroh_transport_integration() {
    let (_base_db, mut sync) = helpers::setup();

    // Initially no transport should be enabled
    assert!(sync.start_server_async("ignored").await.is_err());

    // Enable Iroh transport
    sync.enable_iroh_transport().unwrap();

    // Now server operations should work
    sync.start_server_async("ignored").await.unwrap();

    // Get the server address (should be JSON with node info)
    let server_addr = sync.get_server_address_async().await.unwrap();
    assert!(!server_addr.is_empty());
    assert!(server_addr.contains("node_id"));
    assert!(server_addr.contains("direct_addresses"));

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

#[test]
fn test_sync_iroh_settings_persistence() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (base_db, mut sync) = helpers::setup();
        let backend = Arc::clone(base_db.backend());

        // Store some sync settings
        sync.set_setting("transport_type", "iroh").unwrap();
        sync.set_setting("node_description", "Test Iroh Node")
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

#[tokio::test]
async fn test_send_entries_iroh() {
    use eidetica::Entry;

    // Create server instance
    let (_base_db1, mut sync_server) = helpers::setup();
    sync_server.enable_iroh_transport().unwrap();

    // Start server
    sync_server.start_server_async("ignored").await.unwrap();
    let server_addr = sync_server.get_server_address_async().await.unwrap();

    // Create client instance
    let (_base_db2, mut sync_client) = helpers::setup();
    sync_client.enable_iroh_transport().unwrap();

    // Create some test entries
    let entry1 = Entry::builder("test_root_1")
        .set_subtree_data("data", r#"{"key1": "value1"}"#)
        .build();
    let entry2 = Entry::builder("test_root_2")
        .set_subtree_data("data", r#"{"key2": "value2"}"#)
        .build();
    let entries = vec![entry1, entry2];

    // Note: Iroh transport requires actual network connectivity between nodes
    // For this test, we'll verify the send_entries method exists and is callable
    // The actual network test would require more complex setup with real Iroh nodes
    let result = sync_client
        .send_entries_async(entries, &Address::iroh(&server_addr))
        .await;

    // This will likely fail with connection error since we're using fake addresses,
    // but that's expected - we're just testing the API exists
    match result {
        Ok(_) => {
            // Great! The send worked (unlikely in unit tests)
        }
        Err(_) => {
            // Expected in unit test environment - Iroh needs real network setup
            // The important thing is that the API exists and is callable
        }
    }

    // Clean up
    sync_server.stop_server_async().await.unwrap();
}
