use eidetica::{
    Entry,
    sync::{
        protocol::{SyncRequest, SyncResponse},
        transports::framing::{self, Frame},
        transports::{SyncTransport, http::HttpTransport, iroh::IrohTransport},
    },
};
use iroh::RelayMode;

/// Test to demonstrate that HTTP and Iroh transports use aligned v1 versioning
#[tokio::test]
async fn test_version_alignment() {
    // Both transports should be using v1 versioning
    let http_transport = HttpTransport::builder()
        .bind("127.0.0.1:0")
        .build_sync()
        .unwrap();
    let iroh_transport = IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .build()
        .unwrap();

    // Start both servers
    let (_instance1, handler1) = super::helpers::setup_test_handler().await;
    let (_instance2, handler2) = super::helpers::setup_test_handler().await;
    http_transport.start_server(handler1).await.unwrap();
    iroh_transport.start_server(handler2).await.unwrap();

    // Get server addresses
    let http_addr = http_transport.get_server_address().unwrap();
    let iroh_addr = iroh_transport.get_server_address().unwrap();

    // HTTP should be using v1 endpoint
    assert!(http_addr.starts_with("127.0.0.1:"));

    // Iroh address should be base64url-encoded
    assert!(
        !iroh_addr.starts_with('{'),
        "Iroh address should be base64url-encoded, not raw JSON: {iroh_addr}"
    );

    // Both should be ready for v1 API calls
    // (The actual API calls are tested in other test files)

    // Clean up
    http_transport.stop_server().await.unwrap();
    iroh_transport.stop_server().await.unwrap();
}

/// Test HTTP v1 endpoint format explicitly
#[tokio::test]
async fn test_http_v1_endpoint_format() {
    let transport = HttpTransport::builder()
        .bind("127.0.0.1:0")
        .build_sync()
        .unwrap();
    let (_instance, handler) = super::helpers::setup_test_handler().await;
    transport.start_server(handler).await.unwrap();
    let addr = transport.get_server_address().unwrap();

    // Test that the v1 endpoint is accessible
    let client = reqwest::Client::new();
    let entry = Entry::root_builder()
        .set_subtree_data("data", r#"{"test": "v1_endpoint"}"#)
        .build()
        .expect("Entry should build successfully");

    let request = SyncRequest::SendEntries(vec![]);
    let _entry = entry;

    let response = client
        .post(format!("http://{addr}/api/v1"))
        .body(framing::encode(&Frame::Request(request.clone())).unwrap())
        .send()
        .await
        .unwrap();

    assert!(response.status().is_success());

    let bytes = response.bytes().await.unwrap();
    let mut decoder = framing::Decoder::default();
    let mut collector = framing::Collector::new(
        request,
        tokio::time::Instant::now() + std::time::Duration::from_secs(30),
    )
    .unwrap();
    framing::receive_chunk(&mut decoder, &mut collector, &bytes).unwrap();
    assert_eq!(collector.finish(decoder).unwrap(), SyncResponse::Ack);

    transport.stop_server().await.unwrap();
}
