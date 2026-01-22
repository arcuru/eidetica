use eidetica::sync::{
    Address,
    protocol::{SyncRequest, SyncResponse},
    transports::{SyncTransport, http::HttpTransport},
};

#[tokio::test]
async fn test_http_transport_server_lifecycle() {
    let mut transport = HttpTransport::builder()
        .bind("127.0.0.1:0")
        .build_sync()
        .unwrap();

    // Server should not be running initially
    assert!(!transport.is_server_running());

    // Start server
    let (_instance, handler) = super::helpers::setup_test_handler().await;
    transport.start_server(handler).await.unwrap();
    assert!(transport.is_server_running());

    // Stop server
    transport.stop_server().await.unwrap();
    assert!(!transport.is_server_running());
}

#[tokio::test]
async fn test_http_transport_double_start_error() {
    let mut transport = HttpTransport::builder()
        .bind("127.0.0.1:0")
        .build_sync()
        .unwrap();

    // Start server
    let (_instance, handler) = super::helpers::setup_test_handler().await;
    transport.start_server(handler).await.unwrap();

    // Attempting to start again should fail
    let (_instance2, handler) = super::helpers::setup_test_handler().await;
    let result = transport.start_server(handler).await;
    assert!(result.is_err());

    // Clean up
    transport.stop_server().await.unwrap();
}

#[tokio::test]
async fn test_http_transport_stop_without_start() {
    let mut transport = HttpTransport::builder()
        .bind("127.0.0.1:0")
        .build_sync()
        .unwrap();

    // Attempting to stop when not running should fail
    let result = transport.stop_server().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_http_transport_client_server_communication() {
    use eidetica::Entry;

    let mut server_transport = HttpTransport::builder()
        .bind("127.0.0.1:0")
        .build_sync()
        .unwrap();
    let client_transport = HttpTransport::new().unwrap();

    // Start server on port 0 for dynamic assignment
    let (_instance, handler) = super::helpers::setup_test_handler().await;
    server_transport.start_server(handler).await.unwrap();

    // Get the actual bound address
    let addr = server_transport.get_server_address().unwrap();
    let http_address = Address::http(&addr);

    // Send a single entry - should get Ack response
    let single_entry = Entry::root_builder()
        .set_subtree_data("data", r#"{"single": "entry"}"#)
        .build()
        .expect("Entry should build successfully");

    let single_request = SyncRequest::SendEntries(vec![single_entry]);
    let single_response = client_transport
        .send_request(&http_address, &single_request)
        .await
        .unwrap();

    match single_response {
        SyncResponse::Ack => {
            // Expected for single entry
        }
        _ => panic!("Expected Ack response for single entry"),
    }

    // Send multiple entries - should get Count response
    let entry1 = Entry::root_builder()
        .set_subtree_data("data", r#"{"entry": "1"}"#)
        .build()
        .expect("Entry should build successfully");
    let entry2 = Entry::root_builder()
        .set_subtree_data("data", r#"{"entry": "2"}"#)
        .build()
        .expect("Entry should build successfully");

    let multi_request = SyncRequest::SendEntries(vec![entry1, entry2]);
    let multi_response = client_transport
        .send_request(&http_address, &multi_request)
        .await
        .unwrap();

    match multi_response {
        SyncResponse::Count(count) => {
            assert_eq!(count, 2);
        }
        _ => panic!("Expected Count response for multiple entries"),
    }

    // Clean up
    server_transport.stop_server().await.unwrap();
}

#[tokio::test]
async fn test_http_transport_connection_refused() {
    use eidetica::Entry;

    let transport = HttpTransport::new().unwrap();

    // Try to connect to a server that's not running on a high port
    // Using a high port that's unlikely to be in use
    let entry = Entry::root_builder()
        .build()
        .expect("Root entry should build successfully");
    let unreachable_address = Address::http("127.0.0.1:59999");
    let request = SyncRequest::SendEntries(vec![entry]);
    let result = transport.send_request(&unreachable_address, &request).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_http_transport_get_server_address() {
    let mut transport = HttpTransport::builder()
        .bind("127.0.0.1:0")
        .build_sync()
        .unwrap();

    // Should return error when no server is running
    let result = transport.get_server_address();
    assert!(result.is_err());

    // Start server on port 0
    let (_instance, handler) = super::helpers::setup_test_handler().await;
    transport.start_server(handler).await.unwrap();

    // Should return the actual bound address
    let addr = transport.get_server_address().unwrap();
    assert!(addr.starts_with("127.0.0.1:"));
    assert_ne!(addr, "127.0.0.1:0"); // Should be a real port number

    // Stop server
    transport.stop_server().await.unwrap();

    // Should return error again after stopping
    let result = transport.get_server_address();
    assert!(result.is_err());
}
