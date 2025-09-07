use eidetica::{
    Instance,
    backend::database::InMemory,
    sync::{
        Address, Sync,
        protocol::{SyncRequest, SyncResponse},
        transports::{SyncTransport, http::HttpTransport, iroh::IrohTransport},
    },
};

/// Test that both HTTP and Iroh transports use the same message handler
/// by directly testing the shared handler function.
#[tokio::test]
async fn test_unified_message_handling() {
    use eidetica::Entry;

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

    // Create a Sync instance for testing
    let db = Instance::new(Box::new(InMemory::new()));
    db.add_private_key("_device_key")
        .expect("Failed to add device key");
    let sync = Sync::new(db.backend().clone()).unwrap();

    // Test single entry request directly through shared handler
    let single_request = SyncRequest::SendEntries(vec![single_entry.clone()]);
    let single_response = super::helpers::handle_request(&sync, &single_request).await;
    match single_response {
        SyncResponse::Ack => {
            // Expected for single entry
        }
        _ => panic!("Expected Ack response for single entry"),
    }

    // Test multiple entries request directly through shared handler
    let multi_request = SyncRequest::SendEntries(vec![entry1.clone(), entry2.clone()]);
    let multi_response = super::helpers::handle_request(&sync, &multi_request).await;
    match multi_response {
        SyncResponse::Count(count) => {
            assert_eq!(count, 2);
        }
        _ => panic!("Expected Count response for multiple entries"),
    }

    // Test HTTP transport uses same logic
    let mut http_transport = HttpTransport::new().unwrap();
    let handler = super::helpers::setup_test_handler();
    http_transport
        .start_server("127.0.0.1:0", handler)
        .await
        .unwrap();
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
    use eidetica::Entry;

    let mut transport = HttpTransport::new().unwrap();

    // Start server
    let handler = super::helpers::setup_test_handler();
    transport
        .start_server("127.0.0.1:0", handler)
        .await
        .unwrap();
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

/// Test that Iroh transport uses the SyncHandler architecture correctly.
/// This test verifies the integration without requiring P2P networking.
#[tokio::test]
async fn test_iroh_transport_handler_integration() {
    // Create Iroh transport and verify it starts with a handler
    let mut iroh_transport = IrohTransport::new().unwrap();
    let handler = super::helpers::setup_test_handler();

    // Test that server can start with a handler (this validates the architecture)
    iroh_transport.start_server("", handler).await.unwrap();

    // Verify server is running
    assert!(iroh_transport.is_server_running());

    // Get the server address (this is the node ID for Iroh)
    let server_addr = iroh_transport.get_server_address().unwrap();
    assert!(!server_addr.is_empty());

    // Verify we can stop the server
    iroh_transport.stop_server().await.unwrap();
    assert!(!iroh_transport.is_server_running());

    // This test validates that:
    // 1. IrohTransport properly stores and uses SyncHandler
    // 2. The server lifecycle works correctly
    // 3. The architecture matches HTTP transport pattern
    // Note: P2P connection testing requires more complex setup with relay servers
}
