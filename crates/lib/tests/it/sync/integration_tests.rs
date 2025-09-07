use eidetica::sync::Address;

use super::helpers::*;

#[tokio::test]
async fn test_sync_with_http_transport() {
    use eidetica::Entry;

    let (_base_db, mut sync) = setup();

    // Enable HTTP transport
    sync.enable_http_transport().unwrap();

    // Start server on port 0 (OS assigns available port)
    sync.start_server_async("127.0.0.1:0").await.unwrap();

    // Get the actual bound address
    let server_addr = sync.get_server_address_async().await.unwrap();
    let http_address = Address::http(&server_addr);

    // Test the new protocol by sending entries
    let entry = Entry::builder("test_root")
        .set_subtree_data("data", r#"{"test": "value"}"#)
        .build();

    sync.send_entries_async(vec![entry], &http_address)
        .await
        .unwrap();

    // Stop server
    sync.stop_server_async().await.unwrap();
}

#[tokio::test]
async fn test_multiple_sync_instances_communication() {
    use eidetica::Entry;

    // Create two separate sync instances
    let (_base_db1, mut sync_server) = setup();
    let (_base_db2, mut sync_client) = setup();

    // Enable HTTP transport on both
    sync_server.enable_http_transport().unwrap();
    sync_client.enable_http_transport().unwrap();

    // Start server on first instance (port 0 for auto-assignment)
    sync_server.start_server_async("127.0.0.1:0").await.unwrap();

    // Get the actual bound address from the server instance
    let server_addr = sync_server.get_server_address_async().await.unwrap();

    // Test communication by sending entries from client to server
    let entry = Entry::builder("communication_test")
        .set_subtree_data("data", r#"{"message": "hello from client"}"#)
        .build();

    let http_address = Address::http(&server_addr);
    sync_client
        .send_entries_async(vec![entry], &http_address)
        .await
        .unwrap();

    // Clean up
    sync_server.stop_server_async().await.unwrap();
}

#[tokio::test]
async fn test_send_entries_http() {
    use eidetica::Entry;

    // Create two separate sync instances
    let (_base_db1, mut sync_server) = setup();
    let (_base_db2, mut sync_client) = setup();

    // Enable HTTP transport on both
    sync_server.enable_http_transport().unwrap();
    sync_client.enable_http_transport().unwrap();

    // Start server on first instance (port 0 for auto-assignment)
    sync_server.start_server_async("127.0.0.1:0").await.unwrap();

    // Get the actual bound address from the server instance
    let server_addr = sync_server.get_server_address_async().await.unwrap();

    // Create some test entries
    let entry1 = Entry::builder("test_root_1")
        .set_subtree_data("users", r#"{"user1": "data1"}"#)
        .build();
    let entry2 = Entry::builder("test_root_2")
        .set_subtree_data("users", r#"{"user2": "data2"}"#)
        .build();
    let entries = vec![entry1, entry2];

    // Send entries from client to server
    let http_address = Address::http(&server_addr);
    sync_client
        .send_entries_async(entries, &http_address)
        .await
        .unwrap();

    // Clean up
    sync_server.stop_server_async().await.unwrap();
}

#[test]
fn test_sync_without_transport_enabled() {
    use eidetica::Entry;

    let (_base_db, sync) = setup();

    // Attempting to send entries without enabling transport should fail
    let entry = Entry::builder("test").build();
    let result = sync.send_entries(vec![entry], &Address::http("127.0.0.1:8084"));
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        eidetica::Error::Sync(sync_err) => {
            assert!(sync_err.is_configuration_error());
        }
        _ => panic!("Expected Sync error, got {err:?}"),
    }
}

#[test]
fn test_sync_server_without_transport_enabled() {
    let (_base_db, mut sync) = setup();

    // Attempting to start server without enabling transport should fail
    let result = sync.start_server("127.0.0.1:8085");
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        eidetica::Error::Sync(sync_err) => {
            assert!(sync_err.is_configuration_error());
        }
        _ => panic!("Expected Sync error, got {err:?}"),
    }
}

#[test]
fn test_sync_connect_to_invalid_address() {
    use eidetica::Entry;

    let (_base_db, mut sync) = setup();
    sync.enable_http_transport().unwrap();

    // Try to send entries to a non-existent server
    let entry = Entry::builder("test").build();
    let result = sync.send_entries(vec![entry], &Address::http("127.0.0.1:19998"));
    assert!(result.is_err());
}
