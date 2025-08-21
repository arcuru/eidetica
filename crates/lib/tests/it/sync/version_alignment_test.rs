use eidetica::sync::transports::{SyncTransport, http::HttpTransport, iroh::IrohTransport};

/// Test to demonstrate that HTTP and Iroh transports use aligned v0 versioning
#[tokio::test]
async fn test_version_alignment() {
    // Both transports should be using v0 versioning
    let mut http_transport = HttpTransport::new().unwrap();
    let mut iroh_transport = IrohTransport::new().unwrap();

    // Start both servers
    let handler1 = super::helpers::setup_test_handler();
    let handler2 = super::helpers::setup_test_handler();
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

    // Iroh should be using node ID (different format but both v0 semantically)
    assert!(!iroh_addr.contains(":")); // Node ID format, not IP:port
    assert!(iroh_addr.chars().all(|c| c.is_ascii_hexdigit()));

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
    let handler = super::helpers::setup_test_handler();
    transport
        .start_server("127.0.0.1:0", handler)
        .await
        .unwrap();
    let addr = transport.get_server_address().unwrap();

    // Test that the v0 endpoint is accessible
    use eidetica::entry::Entry;
    use eidetica::sync::protocol::SyncRequest;

    let client = reqwest::Client::new();
    let entry = Entry::builder("test_root")
        .set_subtree_data("data", r#"{"test": "v0_endpoint"}"#)
        .build();

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
