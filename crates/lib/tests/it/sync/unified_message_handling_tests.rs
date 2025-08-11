use eidetica::sync::{
    protocol::{SyncRequest, SyncResponse},
    transports::{SyncTransport, http::HttpTransport},
};

/// Test that both HTTP and Iroh transports use the same message handler
/// by directly testing the shared handler function.
#[tokio::test]
async fn test_unified_message_handling() {
    use eidetica::sync::handler::handle_request;

    // Test Hello request directly through shared handler
    let hello_response = handle_request(SyncRequest::Hello).await;
    match hello_response {
        SyncResponse::Hello(msg) => {
            assert_eq!(msg, "Hello from Eidetica Sync!");
        }
        _ => panic!("Expected Hello response"),
    }

    // Test Status request directly through shared handler
    let status_response = handle_request(SyncRequest::Status).await;
    match status_response {
        SyncResponse::Status(msg) => {
            assert_eq!(msg, "Sync Status: Active");
        }
        _ => panic!("Expected Status response"),
    }

    // Test HTTP transport uses same logic
    let mut http_transport = HttpTransport::new().unwrap();
    http_transport.start_server("127.0.0.1:0").await.unwrap();
    let http_addr = http_transport.get_server_address().unwrap();

    let http_hello = http_transport
        .send_request(&http_addr, SyncRequest::Hello)
        .await
        .unwrap();

    // HTTP transport should return same response as direct handler call
    assert_eq!(http_hello, handle_request(SyncRequest::Hello).await);

    let http_status = http_transport
        .send_request(&http_addr, SyncRequest::Status)
        .await
        .unwrap();

    // HTTP transport should return same response as direct handler call
    assert_eq!(http_status, handle_request(SyncRequest::Status).await);

    // Clean up
    http_transport.stop_server().await.unwrap();
}

/// Test that the new HTTP v0 endpoint format works with JSON requests
#[tokio::test]
async fn test_http_v0_json_endpoint() {
    let mut transport = HttpTransport::new().unwrap();

    // Start server
    transport.start_server("127.0.0.1:0").await.unwrap();
    let addr = transport.get_server_address().unwrap();

    // Test direct HTTP client call to verify endpoint format
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/api/v0");

    // Send Hello as JSON POST (same as transport does internally)
    let response = client
        .post(&url)
        .json(&SyncRequest::Hello)
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());

    let sync_response: SyncResponse = response.json().await.unwrap();
    match sync_response {
        SyncResponse::Hello(msg) => {
            assert_eq!(msg, "Hello from Eidetica Sync!");
        }
        _ => panic!("Expected Hello response"),
    }

    // Send Status as JSON POST
    let response = client
        .post(&url)
        .json(&SyncRequest::Status)
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());

    let sync_response: SyncResponse = response.json().await.unwrap();
    match sync_response {
        SyncResponse::Status(msg) => {
            assert_eq!(msg, "Sync Status: Active");
        }
        _ => panic!("Expected Status response"),
    }

    // Clean up
    transport.stop_server().await.unwrap();
}

/// Test that old GET endpoints no longer work (should return 404 or method not allowed)
#[tokio::test]
async fn test_old_get_endpoints_removed() {
    let mut transport = HttpTransport::new().unwrap();

    // Start server
    transport.start_server("127.0.0.1:0").await.unwrap();
    let addr = transport.get_server_address().unwrap();

    let client = reqwest::Client::new();

    // Old endpoints should not exist
    let hello_response = client
        .get(format!("http://{addr}/api/hello"))
        .send()
        .await
        .unwrap();

    let status_response = client
        .get(format!("http://{addr}/api/status"))
        .send()
        .await
        .unwrap();

    // Should be 404 (not found) since routes don't exist
    assert_eq!(hello_response.status(), 404);
    assert_eq!(status_response.status(), 404);

    // Clean up
    transport.stop_server().await.unwrap();
}
