use eidetica::sync::{Address, transports::http::HttpTransport};

use super::helpers::*;

#[tokio::test]
async fn test_sync_with_http_transport() {
    use eidetica::Entry;

    let (_base_db, sync) = setup().await;

    // Enable HTTP transport and start server
    sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    sync.accept_connections().await.unwrap();

    // Get the actual bound address
    let server_addr = sync.get_server_address().await.unwrap();
    let http_address = Address::http(&server_addr);

    // Test the new protocol by sending entries
    let entry = Entry::root_builder()
        .set_subtree_data("data", r#"{"test": "value"}"#)
        .build()
        .expect("Entry should build successfully");

    sync.send_entries(vec![entry], &http_address).await.unwrap();

    // Stop server
    sync.stop_server().await.unwrap();
}

#[tokio::test]
async fn test_multiple_sync_instances_communication() {
    use eidetica::Entry;

    // Create two separate sync instances
    let (_base_db1, sync_server) = setup().await;
    let (_base_db2, sync_client) = setup().await;

    // Enable HTTP transport on both
    sync_server
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    sync_client
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Start server on first instance
    sync_server.accept_connections().await.unwrap();

    // Get the actual bound address from the server instance
    let server_addr = sync_server.get_server_address().await.unwrap();

    // Test communication by sending entries from client to server
    let entry = Entry::root_builder()
        .set_subtree_data("data", r#"{"message": "hello from client"}"#)
        .build()
        .expect("Entry should build successfully");

    let http_address = Address::http(&server_addr);
    sync_client
        .send_entries(vec![entry], &http_address)
        .await
        .unwrap();

    // Clean up
    sync_server.stop_server().await.unwrap();
}

#[tokio::test]
async fn test_send_entries_http() {
    use eidetica::Entry;

    // Create two separate sync instances
    let (_base_db1, sync_server) = setup().await;
    let (_base_db2, sync_client) = setup().await;

    // Enable HTTP transport on both
    sync_server
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    sync_client
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Start server on first instance
    sync_server.accept_connections().await.unwrap();

    // Get the actual bound address from the server instance
    let server_addr = sync_server.get_server_address().await.unwrap();

    // Create some test entries
    let entry1 = Entry::root_builder()
        .set_subtree_data("users", r#"{"user1": "data1"}"#)
        .build()
        .expect("Entry should build successfully");
    let entry2 = Entry::root_builder()
        .set_subtree_data("users", r#"{"user2": "data2"}"#)
        .build()
        .expect("Entry should build successfully");
    let entries = vec![entry1, entry2];

    // Send entries from client to server
    let http_address = Address::http(&server_addr);
    sync_client
        .send_entries(entries, &http_address)
        .await
        .unwrap();

    // Clean up
    sync_server.stop_server().await.unwrap();
}

#[tokio::test]
async fn test_sync_without_transport_enabled() {
    use eidetica::Entry;

    let (_base_db, sync) = setup().await;

    // Attempting to send entries without enabling transport should fail
    let entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let result = sync
        .send_entries(vec![entry], &Address::http("127.0.0.1:8084"))
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        eidetica::Error::Sync(sync_err) => {
            assert!(sync_err.is_configuration_error());
        }
        _ => panic!("Expected Sync error, got {err:?}"),
    }
}

#[tokio::test]
async fn test_sync_server_without_transport_enabled() {
    let (_base_db, sync) = setup().await;

    // Attempting to start server without enabling transport should fail
    let result = sync.accept_connections().await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        eidetica::Error::Sync(sync_err) => {
            assert!(sync_err.is_configuration_error());
        }
        _ => panic!("Expected Sync error, got {err:?}"),
    }
}

#[tokio::test]
async fn test_sync_connect_to_invalid_address() {
    use eidetica::Entry;

    let (_base_db, sync) = setup().await;
    sync.register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    // Try to send entries to a non-existent server
    let entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let result = sync
        .send_entries(vec![entry], &Address::http("127.0.0.1:19998"))
        .await;
    assert!(result.is_err());
}
