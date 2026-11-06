use eidetica::{
    Entry,
    sync::{
        peer_types::Address,
        protocol::{SyncRequest, SyncResponse},
        transports::{
            SyncTransport,
            http::{HttpTransport, SYNC_PATH},
            iroh::IrohTransport,
        },
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

    // HTTP should be using the v1 endpoint
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

/// Test the HTTP endpoint format and its refusal of old clients explicitly
#[tokio::test]
async fn test_http_v1_endpoint_format() {
    let transport = HttpTransport::builder()
        .bind("127.0.0.1:0")
        .build_sync()
        .unwrap();
    let (_instance, handler) = super::helpers::setup_test_handler().await;
    transport.start_server(handler).await.unwrap();
    let addr = transport.get_server_address().unwrap();

    // Test that the versioned endpoint is what the transport actually uses
    let entry = Entry::root_builder()
        .set_subtree_data("data", r#"{"test": "v1_endpoint"}"#)
        .build()
        .expect("Entry should build successfully");

    let request = SyncRequest::SendEntries(vec![entry]);
    let client = HttpTransport::new().unwrap();
    let response = client
        .send_request(&Address::http(&addr), &request)
        .await
        .unwrap();
    assert!(matches!(response, SyncResponse::Ack));

    // An old peer's route is gone: no JSON operation reaches the handler.
    let legacy = reqwest::Client::new()
        .post(format!("http://{addr}/api/v0"))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(legacy.status(), reqwest::StatusCode::NOT_FOUND);

    // A JSON body at the current route is refused, not misread.
    let wrong_media = reqwest::Client::new()
        .post(format!("http://{addr}{SYNC_PATH}"))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(
        wrong_media.status(),
        reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE
    );

    transport.stop_server().await.unwrap();
}

/// A complete CBOR request is not dispatchable until its declared envelope and
/// native body both finish. Bad boundaries must not insert even the first Entry.
#[tokio::test]
async fn test_http_message_boundaries_before_dispatch() {
    use eidetica::sync::transports::http::SYNC_MEDIA_TYPE;
    let transport = HttpTransport::builder()
        .bind("127.0.0.1:0")
        .build_sync()
        .unwrap();
    let (instance, handler) = super::helpers::setup_test_handler().await;
    transport.start_server(handler).await.unwrap();
    let address = transport.get_server_address().unwrap();
    let entry = Entry::root_builder()
        .set_subtree_data("data", "not partial")
        .build()
        .unwrap();
    let id = entry.id();
    let body = cbor4ii::serde::to_vec(Vec::new(), &SyncRequest::SendEntries(vec![entry])).unwrap();
    let envelope = |length: u64, bytes: &[u8]| {
        let mut message = length.to_be_bytes().to_vec();
        message.extend_from_slice(bytes);
        message
    };
    for message in [
        envelope(body.len() as u64 + 1, &body), // truncated declared body
        envelope(body.len() as u64, &[body.as_slice(), &[0]].concat()), // after envelope
        envelope(body.len() as u64 + 1, &[body.as_slice(), &[0]].concat()), // after value
        envelope(u64::MAX, &[]),
        envelope(1, &[0xff]), // malformed CBOR
    ] {
        let response = reqwest::Client::new()
            .post(format!("http://{address}{SYNC_PATH}"))
            .header("content-type", SYNC_MEDIA_TYPE)
            .body(message)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_client_error(), "{response:?}");
        assert!(instance.backend().get(&id).await.is_err());
    }
    transport.stop_server().await.unwrap();
}

#[tokio::test]
async fn test_iroh_message_boundaries_and_legacy_alpn() {
    use iroh::{Endpoint, endpoint::presets};
    use iroh_tickets::{Ticket, endpoint::EndpointTicket};
    let transport = IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .mdns(false)
        .n0_dns(false)
        .build()
        .unwrap();
    let (instance, handler) = super::helpers::setup_test_handler().await;
    transport.start_server(handler).await.unwrap();
    let ticket = EndpointTicket::decode_string(&transport.get_server_address().unwrap()).unwrap();
    let endpoint = Endpoint::builder(presets::N0)
        .relay_mode(RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let old = endpoint
        .connect(ticket.endpoint_addr().clone(), b"eidetica/v0")
        .await;
    assert!(old.is_err(), "a v0 operation must not reach a v1 peer");
    let connection = endpoint
        .connect(ticket.endpoint_addr().clone(), b"eidetica/v1")
        .await
        .unwrap();
    let entry = Entry::root_builder()
        .set_subtree_data("data", "not partial")
        .build()
        .unwrap();
    let id = entry.id();
    let body = cbor4ii::serde::to_vec(Vec::new(), &SyncRequest::SendEntries(vec![entry])).unwrap();
    for (declared, bytes) in [
        (body.len() as u64 + 1, body.clone()),
        (body.len() as u64, [body.as_slice(), &[0]].concat()),
        (body.len() as u64 + 1, [body.as_slice(), &[0]].concat()),
    ] {
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        send.write_all(&declared.to_be_bytes()).await.unwrap();
        send.write_all(&bytes).await.unwrap();
        send.finish().unwrap();
        // The stream closes/resets without an acknowledgement. This is after
        // parsing and before dispatch, not just a transport status assertion.
        let reply = tokio::time::timeout(std::time::Duration::from_secs(5), recv.read_to_end(4096))
            .await
            .unwrap();
        assert!(
            !reply.as_ref().is_ok_and(|reply| !reply.is_empty()),
            "{reply:?}"
        );
        assert!(instance.backend().get(&id).await.is_err());
    }
    connection.close(0u32.into(), b"done");
    endpoint.close().await;
    transport.stop_server().await.unwrap();
}
