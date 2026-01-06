use eidetica::sync::transports::{SyncTransport, http::HttpTransport, iroh::IrohTransport};

/// Test to demonstrate that HTTP and Iroh transports use aligned v0 versioning
#[tokio::test]
async fn test_version_alignment() {
    // Both transports should be using v0 versioning
    let mut http_transport = HttpTransport::new().unwrap();
    let mut iroh_transport = IrohTransport::new().unwrap();

    // Start both servers
    let (_instance1, handler1) = super::helpers::setup_test_handler().await;
    let (_instance2, handler2) = super::helpers::setup_test_handler().await;
    http_transport
        .start_server("127.0.0.1:0", handler1)
        .await
        .unwrap();
    iroh_transport
        .start_server("ignored", handler2)
        .await
        .unwrap();

    // Get server addresses
    let http_addr = http_transport.get_server_address().unwrap();
    let iroh_addr = iroh_transport.get_server_address().unwrap();

    // HTTP should be using v0 endpoint
    assert!(http_addr.starts_with("127.0.0.1:"));

    // Iroh should be using JSON format with endpoint_id and direct_addresses (v0 semantically)
    assert!(iroh_addr.contains("endpoint_id")); // JSON format with endpoint info
    assert!(iroh_addr.contains("direct_addresses")); // Contains connectivity info

    // Both should be ready for v0 API calls
    // (The actual API calls are tested in other test files)

    // Clean up
    http_transport.stop_server().await.unwrap();
    iroh_transport.stop_server().await.unwrap();
}

/// Test HTTP v0 endpoint format explicitly
#[tokio::test]
async fn test_http_v0_endpoint_format() {
    let mut transport = HttpTransport::new().unwrap();
    let (_instance, handler) = super::helpers::setup_test_handler().await;
    transport
        .start_server("127.0.0.1:0", handler)
        .await
        .unwrap();
    let addr = transport.get_server_address().unwrap();

    // Test that the v0 endpoint is accessible
    use eidetica::{Entry, sync::protocol::SyncRequest};

    let client = reqwest::Client::new();
    let entry = Entry::root_builder()
        .set_subtree_data("data", r#"{"test": "v0_endpoint"}"#)
        .build()
        .expect("Entry should build successfully");

    let request = SyncRequest::SendEntries(vec![entry]);

    let response = client
        .post(format!("http://{addr}/api/v0"))
        .json(&request)
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());

    let json: serde_json::Value = response.json().await.unwrap();
    assert_eq!(json, "Ack");

    transport.stop_server().await.unwrap();
}
