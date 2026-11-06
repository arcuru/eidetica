//! Requested-heap evidence for the production encoders and native send paths.
//!
//! The opposite peer drains bytes without decoding an owned result. Therefore
//! no future result capacity is subtracted from a historical peak: even an
//! exact-capacity encoded copy dropped before decoding must be visible.
//! Receiver output allocation is intentionally not measured by this oracle;
//! fixed decoder queues/reader buffers are inspected separately. This is not
//! RSS, allocator overhead/realloc overlap, storage or a process-wide bound.
//! Nextest supplies one process per test, isolating the global peak meter.

use std::sync::{Arc, Mutex};

use eidetica::{
    Entry,
    entry::ID,
    sync::{
        Address,
        handler::SyncHandler,
        protocol::{BootstrapResponse, RequestContext, SyncRequest, SyncResponse},
        transports::{
            SyncTransport,
            http::{HttpTransport, SYNC_MEDIA_TYPE, SYNC_PATH},
            iroh::IrohTransport,
        },
    },
};
use futures_util::StreamExt;
use iroh::{
    Endpoint, RelayMode,
    endpoint::{PortmapperConfig, presets},
};
use iroh_tickets::{Ticket, endpoint::EndpointTicket};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::{peak_allocation, reset_peak_allocation};

const MIB: usize = 1024 * 1024;
const SIZES: [usize; 3] = [16 * MIB, 48 * MIB, 96 * MIB];
const GROWTH_ALLOWANCE: usize = 4 * MIB;

fn entry(bytes: usize) -> Entry {
    Entry::root_builder()
        .set_subtree_data("data", vec![7u8; bytes])
        .build()
        .unwrap()
}

fn envelope(value: &impl serde::Serialize) -> Vec<u8> {
    let body = cbor4ii::serde::to_vec(Vec::new(), value).unwrap();
    let mut message = (body.len() as u64).to_be_bytes().to_vec();
    message.extend(body);
    message
}

fn overhead(baseline: usize) -> usize {
    peak_allocation().saturating_sub(baseline)
}

fn assert_bounded(path: &str, samples: &[usize]) {
    for (size, extra) in SIZES.iter().zip(samples) {
        eprintln!(
            "{path}: payload {} MiB, peak above initial owned source {extra} bytes",
            size / MIB
        );
    }
    let growth = samples[samples.len() - 1].saturating_sub(samples[0]);
    assert!(
        growth < GROWTH_ALLOWANCE,
        "{path}: extra heap grew {growth} bytes: {samples:?}"
    );
}

/// Drain one envelope without creating a decoded value, using copy's fixed I/O
/// buffer. This observes the complete body and (for peers) the required FIN.
async fn drain(reader: &mut (impl AsyncRead + Unpin), end: bool) -> u64 {
    let length = reader.read_u64().await.unwrap();
    let read = tokio::io::copy(&mut (&mut *reader).take(length), &mut tokio::io::sink())
        .await
        .unwrap();
    assert_eq!(read, length);
    if end {
        assert_eq!(reader.read(&mut [0u8; 1]).await.unwrap(), 0);
    }
    length
}

#[derive(Default)]
struct Peer(Mutex<Option<SyncResponse>>);

impl Peer {
    fn respond_with(&self, source: Entry) {
        *self.0.lock().unwrap() = Some(SyncResponse::Bootstrap(BootstrapResponse {
            tree_id: ID::default(),
            root_entry: source,
            all_entries: Vec::new(),
            key_approved: false,
            granted_permission: None,
        }));
    }
}

#[async_trait::async_trait]
impl SyncHandler for Peer {
    async fn handle_request(&self, _: &SyncRequest, _: &RequestContext) -> SyncResponse {
        self.0.lock().unwrap().take().unwrap_or(SyncResponse::Ack)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn http_send_buffers_do_not_grow_with_the_message() {
    use axum::{Router, body::Body, http::header, routing::post};
    let router = Router::new().route(
        SYNC_PATH,
        post(|body: Body| async {
            let mut chunks = body.into_data_stream();
            let mut bytes = 0;
            while let Some(chunk) = chunks.next().await {
                bytes += chunk.unwrap().len();
            }
            assert!(bytes >= 8);
            (
                [(header::CONTENT_TYPE, SYNC_MEDIA_TYPE)],
                envelope(&SyncResponse::Ack),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = Address::http(listener.local_addr().unwrap().to_string());
    let raw = tokio::spawn(async { axum::serve(listener, router).await.unwrap() });
    let client = HttpTransport::new().unwrap();
    client.send_owned_entries(&target, vec![]).await.unwrap();
    let mut uploads = Vec::new();
    for size in SIZES {
        let request = SyncRequest::SendEntries(vec![entry(size)]);
        let baseline = reset_peak_allocation();
        client.send_request_owned(&target, request).await.unwrap();
        uploads.push(overhead(baseline));
    }
    assert_bounded("http upload encoder/native send", &uploads);
    raw.abort();
    let _ = raw.await;

    let peer = Arc::new(Peer::default());
    let server = HttpTransport::builder()
        .bind("127.0.0.1:0")
        .build_sync()
        .unwrap();
    server.start_server(peer.clone()).await.unwrap();
    let url = format!("http://{}{SYNC_PATH}", server.get_server_address().unwrap());
    let client = reqwest::Client::new();
    let request = envelope(&SyncRequest::SendEntries(vec![]));
    let mut downloads = Vec::new();
    // The first exchange warms the HTTP connection before measurement.
    for size in [0].into_iter().chain(SIZES) {
        peer.respond_with(entry(size));
        let baseline = reset_peak_allocation();
        let mut response = client
            .post(&url)
            .header("content-type", SYNC_MEDIA_TYPE)
            .body(request.clone())
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        let mut bytes = 0;
        while let Some(chunk) = response.chunk().await.unwrap() {
            bytes += chunk.len();
        }
        assert!(bytes > size);
        if size != 0 {
            downloads.push(overhead(baseline));
        }
    }
    assert_bounded("http download encoder/native send", &downloads);
    server.stop_server().await.unwrap();
}

async fn endpoint() -> Endpoint {
    Endpoint::builder(presets::N0)
        .alpns(vec![b"eidetica/v1".to_vec()])
        .relay_mode(RelayMode::Disabled)
        .clear_address_lookup()
        .portmapper_config(PortmapperConfig::Disabled)
        .bind()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn iroh_send_buffers_do_not_grow_with_the_message() {
    let raw = endpoint().await;
    let target = Address::iroh(EndpointTicket::new(raw.addr()).encode_string());
    let accept = tokio::spawn({
        let endpoint = raw.clone();
        async move {
            for _ in 0..=SIZES.len() {
                let connection = endpoint.accept().await.unwrap().await.unwrap();
                let (mut send, mut recv) = connection.accept_bi().await.unwrap();
                assert!(drain(&mut recv, true).await > 0);
                send.write_all(&envelope(&SyncResponse::Ack)).await.unwrap();
                send.finish().unwrap();
                let _ = send.stopped().await;
            }
        }
    });
    let client = IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .portmapper_config(PortmapperConfig::Disabled)
        .mdns(false)
        .n0_dns(false)
        .build()
        .unwrap();
    client.send_owned_entries(&target, vec![]).await.unwrap();
    let mut uploads = Vec::new();
    for size in SIZES {
        let request = SyncRequest::SendEntries(vec![entry(size)]);
        let baseline = reset_peak_allocation();
        client.send_request_owned(&target, request).await.unwrap();
        uploads.push(overhead(baseline));
    }
    accept.await.unwrap();
    assert_bounded("iroh upload encoder/native send", &uploads);
    raw.close().await;

    let peer = Arc::new(Peer::default());
    let server = IrohTransport::builder()
        .relay_mode(RelayMode::Disabled)
        .portmapper_config(PortmapperConfig::Disabled)
        .mdns(false)
        .n0_dns(false)
        .build()
        .unwrap();
    server.start_server(peer.clone()).await.unwrap();
    let target = EndpointTicket::decode_string(&server.get_server_address().unwrap()).unwrap();
    let raw = endpoint().await;
    let connection = raw
        .connect(target.endpoint_addr().clone(), b"eidetica/v1")
        .await
        .unwrap();
    let request = envelope(&SyncRequest::SendEntries(vec![]));
    let mut downloads = Vec::new();
    for size in [0].into_iter().chain(SIZES) {
        peer.respond_with(entry(size));
        let baseline = reset_peak_allocation();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        send.write_all(&request).await.unwrap();
        send.finish().unwrap();
        assert!(drain(&mut recv, true).await > size as u64);
        downloads.push(overhead(baseline));
    }
    assert_bounded("iroh download encoder/native send", &downloads[1..]);
    connection.close(0u32.into(), b"done");
    raw.close().await;
    server.stop_server().await.unwrap();
}

/// An exact-capacity copy must be visible even when freed before the oracle is
/// queried. The same mutation is also exercised in the production encoder.
#[test]
fn the_meter_sees_a_dropped_exact_capacity_encoded_copy() {
    let source = entry(SIZES[0]);
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    cbor4ii::serde::to_writer(&mut count, &source).unwrap();
    let baseline = reset_peak_allocation();
    let encoded = cbor4ii::serde::to_vec(Vec::with_capacity(count.0), &source).unwrap();
    assert_eq!(encoded.len(), count.0);
    std::hint::black_box(&encoded);
    drop(encoded);
    assert!(overhead(baseline) >= count.0);
}

#[cfg(all(unix, feature = "service"))]
mod service {
    use super::*;
    use eidetica::{
        auth::{crypto::generate_keypair, types::SigKey},
        service::protocol::{
            AuthenticatedDbRequest, DatabaseOp, ServerFrame, ServiceRequest, ServiceResponse,
            write_owned_frame,
        },
    };
    use tokio::net::UnixStream;

    #[tokio::test(flavor = "multi_thread")]
    async fn service_send_buffers_do_not_grow_with_the_message() {
        let (_, public_key) = generate_keypair();
        let (mut near, mut far) = UnixStream::pair().unwrap();
        let mut uploads = Vec::new();
        let mut downloads = Vec::new();
        for size in SIZES {
            let request = ServiceRequest::AuthenticatedDb(Box::new(AuthenticatedDbRequest {
                root_id: ID::default(),
                identity: SigKey::from_pubkey(&public_key),
                op: DatabaseOp::SubmitSignedEntry {
                    entry: Box::new(entry(size)),
                },
            }));
            let baseline = reset_peak_allocation();
            let (sent, bytes) = tokio::join!(
                write_owned_frame(&mut near, request),
                drain(&mut far, false)
            );
            sent.unwrap();
            assert!(bytes > size as u64);
            uploads.push(overhead(baseline));

            let frame = ServerFrame::Response(Box::new(ServiceResponse::Entry(entry(size))));
            let baseline = reset_peak_allocation();
            let (sent, bytes) =
                tokio::join!(write_owned_frame(&mut near, frame), drain(&mut far, false));
            sent.unwrap();
            assert!(bytes > size as u64);
            downloads.push(overhead(baseline));
        }
        assert_bounded("service upload encoder/native send", &uploads);
        assert_bounded("service download encoder/native send", &downloads);
    }
}
