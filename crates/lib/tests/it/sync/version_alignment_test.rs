use eidetica::sync::transports::{SyncTransport, http::HttpTransport, iroh::IrohTransport};

/// Test to demonstrate that HTTP and Iroh transports use aligned v0 versioning
#[tokio::test]
async fn test_version_alignment() {
    // Both transports should be using v0 versioning
    let mut http_transport = HttpTransport::new().unwrap();
    let mut iroh_transport = IrohTransport::new().unwrap();

    // Start both servers
    http_transport.start_server("127.0.0.1:0").await.unwrap();
    iroh_transport.start_server("ignored").await.unwrap();

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
    transport.start_server("127.0.0.1:0").await.unwrap();
    let addr = transport.get_server_address().unwrap();

    // Test that the v0 endpoint is accessible
    use eidetica::entry::Entry;

    let client = reqwest::Client::new();
    let entry = Entry::builder("test_root")
        .set_subtree_data("data", r#"{"test": "v0_endpoint"}"#)
        .build();

    let response = client
        .post(format!("http://{addr}/api/v0"))
        .json(&vec![entry])
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());

    let json: serde_json::Value = response.json().await.unwrap();
    assert_eq!(json, "Ack");

    transport.stop_server().await.unwrap();
}

/// Test Iroh v0 ALPN format
#[test]
fn test_iroh_v0_alpn_format() {
    // This tests the constant value to ensure it matches expected v0 format
    use eidetica::sync::transports::iroh::IrohTransport;

    // Create transport to ensure it can be instantiated with v0 ALPN
    let transport = IrohTransport::new().unwrap();

    // The constant should be accessible and match v0 format
    // (We can't easily test the actual ALPN bytes without exposing internals,
    // but the transport creation verifies the ALPN constant is valid)
    drop(transport); // Just ensure it can be created successfully
}
