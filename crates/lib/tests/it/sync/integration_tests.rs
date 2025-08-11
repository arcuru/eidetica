use super::helpers::*;

#[tokio::test]
async fn test_sync_with_http_transport() {
    let (_base_db, mut sync) = setup();

    // Enable HTTP transport
    sync.enable_http_transport().unwrap();

    // Start server on port 0 (OS assigns available port)
    sync.start_server_async("127.0.0.1:0").await.unwrap();

    // Get the actual bound address
    let server_addr = sync.get_server_address().unwrap();

    // Connect as client to the dynamically assigned port
    let (hello, status) = sync.connect_async(&server_addr).await.unwrap();

    assert_eq!(hello, "Hello from Eidetica Sync!");
    assert_eq!(status, "Sync Status: Active");

    // Stop server
    sync.stop_server_async().await.unwrap();
}

#[tokio::test]
async fn test_multiple_sync_instances_communication() {
    // Create two separate sync instances
    let (_base_db1, mut sync_server) = setup();
    let (_base_db2, mut sync_client) = setup();

    // Enable HTTP transport on both
    sync_server.enable_http_transport().unwrap();
    sync_client.enable_http_transport().unwrap();

    // Start server on first instance (port 0 for auto-assignment)
    sync_server.start_server_async("127.0.0.1:0").await.unwrap();

    // Get the actual bound address from the server instance
    let server_addr = sync_server.get_server_address().unwrap();

    // Connect from second instance to the dynamically assigned port
    let (hello, status) = sync_client.connect_async(&server_addr).await.unwrap();

    assert_eq!(hello, "Hello from Eidetica Sync!");
    assert_eq!(status, "Sync Status: Active");

    // Clean up
    sync_server.stop_server_async().await.unwrap();
}

#[test]
fn test_sync_without_transport_enabled() {
    let (_base_db, sync) = setup();

    // Attempting to connect without enabling transport should fail
    let result = sync.connect("127.0.0.1:8084");
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
    let (_base_db, mut sync) = setup();
    sync.enable_http_transport().unwrap();

    // Try to connect to a non-existent server
    let result = sync.connect("127.0.0.1:19998");
    assert!(result.is_err());
}
