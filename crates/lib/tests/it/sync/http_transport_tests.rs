use eidetica::sync::{
    protocol::{SyncRequest, SyncResponse},
    transports::{SyncTransport, http::HttpTransport},
};

#[tokio::test]
async fn test_http_transport_server_lifecycle() {
    let mut transport = HttpTransport::new().unwrap();

    // Server should not be running initially
    assert!(!transport.is_server_running());

    // Start server
    transport.start_server("127.0.0.1:0").await.unwrap();
    assert!(transport.is_server_running());

    // Stop server
    transport.stop_server().await.unwrap();
    assert!(!transport.is_server_running());
}

#[tokio::test]
async fn test_http_transport_double_start_error() {
    let mut transport = HttpTransport::new().unwrap();

    // Start server
    transport.start_server("127.0.0.1:0").await.unwrap();

    // Attempting to start again should fail
    let result = transport.start_server("127.0.0.1:0").await;
    assert!(result.is_err());

    // Clean up
    transport.stop_server().await.unwrap();
}

#[tokio::test]
async fn test_http_transport_stop_without_start() {
    let mut transport = HttpTransport::new().unwrap();

    // Attempting to stop when not running should fail
    let result = transport.stop_server().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_http_transport_client_server_communication() {
    let mut server_transport = HttpTransport::new().unwrap();
    let client_transport = HttpTransport::new().unwrap();

    // Start server on port 0 for dynamic assignment
    server_transport.start_server("127.0.0.1:0").await.unwrap();

    // Get the actual bound address
    let addr = server_transport.get_server_address().unwrap();

    // Send Hello request
    let hello_response = client_transport
        .send_request(&addr, SyncRequest::Hello)
        .await
        .unwrap();

    match hello_response {
        SyncResponse::Hello(msg) => {
            assert_eq!(msg, "Hello from Eidetica Sync!");
        }
        _ => panic!("Expected Hello response"),
    }

    // Send Status request
    let status_response = client_transport
        .send_request(&addr, SyncRequest::Status)
        .await
        .unwrap();

    match status_response {
        SyncResponse::Status(msg) => {
            assert_eq!(msg, "Sync Status: Active");
        }
        _ => panic!("Expected Status response"),
    }

    // Clean up
    server_transport.stop_server().await.unwrap();
}

#[tokio::test]
async fn test_http_transport_connection_refused() {
    let transport = HttpTransport::new().unwrap();

    // Try to connect to a server that's not running on a high port
    // Using a high port that's unlikely to be in use
    let result = transport
        .send_request("127.0.0.1:59999", SyncRequest::Hello)
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_http_transport_get_server_address() {
    let mut transport = HttpTransport::new().unwrap();

    // Should return error when no server is running
    let result = transport.get_server_address();
    assert!(result.is_err());

    // Start server on port 0
    transport.start_server("127.0.0.1:0").await.unwrap();

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
