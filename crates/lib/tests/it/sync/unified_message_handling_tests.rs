use eidetica::sync::{
    Address,
    protocol::{SyncRequest, SyncResponse},
    transports::{SyncTransport, http::HttpTransport},
};

/// Test that both HTTP and Iroh transports use the same message handler
/// by directly testing the shared handler function.
#[tokio::test]
async fn test_unified_message_handling() {
    use eidetica::entry::Entry;
    use eidetica::sync::handler::handle_request;

    // Create test entries
    let single_entry = Entry::builder("test_root")
        .set_subtree_data("data", r#"{"test": "single"}"#)
        .build();
    let entry1 = Entry::builder("test_root_1")
        .set_subtree_data("data", r#"{"test": "multi1"}"#)
        .build();
    let entry2 = Entry::builder("test_root_2")
        .set_subtree_data("data", r#"{"test": "multi2"}"#)
        .build();

    // Test single entry request directly through shared handler
    let single_request = SyncRequest::SendEntries(vec![single_entry.clone()]);
    let single_response = handle_request(&single_request).await;
    match single_response {
        SyncResponse::Ack => {
            // Expected for single entry
        }
        _ => panic!("Expected Ack response for single entry"),
    }

    // Test multiple entries request directly through shared handler
    let multi_request = SyncRequest::SendEntries(vec![entry1.clone(), entry2.clone()]);
    let multi_response = handle_request(&multi_request).await;
    match multi_response {
        SyncResponse::Count(count) => {
            assert_eq!(count, 2);
        }
        _ => panic!("Expected Count response for multiple entries"),
    }

    // Test HTTP transport uses same logic
    let mut http_transport = HttpTransport::new().unwrap();
    http_transport.start_server("127.0.0.1:0").await.unwrap();
    let http_addr = http_transport.get_server_address().unwrap();
    let http_address = Address::http(&http_addr);

    let http_single = http_transport
        .send_request(&http_address, &SyncRequest::SendEntries(vec![single_entry]))
        .await
        .unwrap();

    // HTTP transport should return same response as direct handler call
    assert_eq!(http_single, SyncResponse::Ack);

    let http_multi = http_transport
        .send_request(
            &http_address,
            &SyncRequest::SendEntries(vec![entry1, entry2]),
        )
        .await
        .unwrap();

    // HTTP transport should return same response as direct handler call
    assert_eq!(http_multi, SyncResponse::Count(2));

    // Clean up
    http_transport.stop_server().await.unwrap();
}

/// Test that the new HTTP v0 endpoint format works with JSON requests
#[tokio::test]
async fn test_http_v0_json_endpoint() {
    use eidetica::entry::Entry;

    let mut transport = HttpTransport::new().unwrap();

    // Start server
    transport.start_server("127.0.0.1:0").await.unwrap();
    let addr = transport.get_server_address().unwrap();

    // Test direct HTTP client call to verify endpoint format
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/api/v0");

    // Send single entry as JSON POST (same as transport does internally)
    let entry = Entry::builder("test_root")
        .set_subtree_data("data", r#"{"test": "direct_http"}"#)
        .build();

    let request = SyncRequest::SendEntries(vec![entry]);
    let response = client.post(&url).json(&request).send().await.unwrap();

    assert!(response.status().is_success());

    let sync_response: SyncResponse = response.json().await.unwrap();
    match sync_response {
        SyncResponse::Ack => {
            // Expected for single entry
        }
        _ => panic!("Expected Ack response"),
    }

    // Send multiple entries as JSON POST
    let entry1 = Entry::builder("test_root_1")
        .set_subtree_data("data", r#"{"test": "direct_http_1"}"#)
        .build();
    let entry2 = Entry::builder("test_root_2")
        .set_subtree_data("data", r#"{"test": "direct_http_2"}"#)
        .build();

    let multi_request = SyncRequest::SendEntries(vec![entry1, entry2]);
    let response = client.post(&url).json(&multi_request).send().await.unwrap();

    assert!(response.status().is_success());

    let sync_response: SyncResponse = response.json().await.unwrap();
    match sync_response {
        SyncResponse::Count(count) => {
            assert_eq!(count, 2);
        }
        _ => panic!("Expected Count response"),
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
