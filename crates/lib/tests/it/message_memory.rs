//! Additional memory of the production message paths as messages grow.
//!
//! A message is measured whole, end to end, with both peers in this process:
//! `additional` is the peak of live heap beyond the owned source value and the
//! owned decoded value. Those two remain proportional to their contents by
//! design, and a decoded value's slack is excluded by taking its capacity.
//!
//! The meter counts requested bytes, so it says nothing about RSS, allocator
//! overhead or `realloc` overlap. It needs a process per test, which nextest
//! provides; run with `--no-capture` to see each measurement.

use std::sync::{Arc, Mutex};

use eidetica::{
    Entry,
    entry::ID,
    message_io::TransferBudget,
    sync::{
        Address,
        handler::SyncHandler,
        protocol::{BootstrapResponse, RequestContext, SyncRequest, SyncResponse},
        transports::{SyncTransport, http::HttpTransport, iroh::IrohTransport},
    },
};
use iroh::RelayMode;

use crate::{peak_allocation, reset_peak_allocation};

const MIB: usize = 1024 * 1024;

/// Wire sizes measured, spanning both former limits many times over.
const SIZES: [usize; 3] = [16 * MIB, 48 * MIB, 96 * MIB];

/// Allowed rise in additional memory across [`SIZES`]. Native windows and socket
/// buffers fill by the smallest size, so what remains is measurement noise; a
/// path keeping any fraction of a message would exceed this many times over.
const GROWTH_ALLOWANCE: usize = 4 * MIB;

fn entry(bytes: usize) -> Entry {
    Entry::root_builder()
        .set_subtree_data("data", vec![7u8; bytes])
        .build()
        .unwrap()
}

fn held(entry: &Entry) -> usize {
    entry.data("data").unwrap().capacity()
}

fn additional(baseline: usize, owned_result: usize) -> usize {
    peak_allocation().saturating_sub(baseline + owned_result)
}

fn assert_bounded(path: &str, samples: &[usize]) {
    for (size, additional) in SIZES.iter().zip(samples) {
        eprintln!(
            "{path}: wire {} MiB, additional {additional} bytes",
            size / MIB
        );
    }
    let growth = samples[samples.len() - 1].saturating_sub(samples[0]);
    assert!(
        growth < GROWTH_ALLOWANCE,
        "{path}: additional memory grew {growth} bytes with the message: {samples:?}"
    );
}

/// Records what arrived and answers with whatever the test left for it.
#[derive(Default)]
struct Peer {
    received: Mutex<usize>,
    response: Mutex<Option<SyncResponse>>,
}

#[async_trait::async_trait]
impl SyncHandler for Peer {
    async fn handle_request(&self, request: &SyncRequest, _: &RequestContext) -> SyncResponse {
        if let SyncRequest::SendEntries(entries) = request {
            *self.received.lock().unwrap() = entries.iter().map(held).sum();
        }
        let response = self.response.lock().unwrap().take();
        response.unwrap_or(SyncResponse::Ack)
    }
}

async fn measure_transport<T: SyncTransport>(
    name: &str,
    build: impl Fn() -> T,
    address: impl Fn(String) -> Address,
) {
    let peer = Arc::new(Peer::default());
    let server = build();
    server.start_server(peer.clone()).await.unwrap();
    let target = address(server.get_server_address().unwrap());
    let client = build();

    // One exchange first, so connection setup and lazy state are not measured.
    client.send_owned_entries(&target, vec![]).await.unwrap();

    let mut uploads = Vec::new();
    for size in SIZES {
        let source = entry(size);
        let baseline = reset_peak_allocation();
        client
            .send_owned_entries(&target, vec![source])
            .await
            .unwrap();
        uploads.push(additional(baseline, *peer.received.lock().unwrap()));
    }
    assert_bounded(&format!("{name} upload"), &uploads);

    let mut downloads = Vec::new();
    for size in SIZES {
        let source = entry(size);
        *peer.response.lock().unwrap() = Some(SyncResponse::Bootstrap(BootstrapResponse {
            tree_id: ID::default(),
            root_entry: source,
            all_entries: Vec::new(),
            key_approved: false,
            granted_permission: None,
        }));
        let baseline = reset_peak_allocation();
        let response = client
            .send_request_owned(&target, SyncRequest::SendEntries(vec![]))
            .await
            .unwrap();
        let SyncResponse::Bootstrap(response) = response else {
            panic!("expected the prepared response, got {response:?}");
        };
        downloads.push(additional(baseline, held(&response.root_entry)));
    }
    assert_bounded(&format!("{name} download"), &downloads);

    server.stop_server().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn http_additional_memory_does_not_grow_with_the_message() {
    measure_transport(
        "http",
        || {
            HttpTransport::builder()
                .bind("127.0.0.1:0")
                .transfer_budget(TransferBudget::default())
                .build_sync()
                .unwrap()
        },
        Address::http,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn iroh_additional_memory_does_not_grow_with_the_message() {
    measure_transport(
        "iroh",
        || {
            IrohTransport::builder()
                .relay_mode(RelayMode::Disabled)
                .mdns(false)
                .n0_dns(false)
                .build()
                .unwrap()
        },
        Address::iroh,
    )
    .await;
}

/// The positive control: this meter must see a path that does keep a whole
/// encoded message, or the bounds above would prove nothing.
#[test]
fn the_meter_sees_a_whole_encoded_copy() {
    let source = entry(SIZES[0]);
    let baseline = reset_peak_allocation();
    let encoded = cbor4ii::serde::to_vec(Vec::new(), &source).unwrap();
    assert!(encoded.len() > SIZES[0]);
    assert!(additional(baseline, 0) >= SIZES[0]);
}

#[cfg(all(unix, feature = "service"))]
mod service {
    use eidetica::{
        auth::{crypto::generate_keypair, types::SigKey},
        service::protocol::{
            AuthenticatedDbRequest, DatabaseOp, ServerFrame, ServiceRequest, ServiceResponse,
            read_classified_request, read_classified_response, write_owned_frame,
        },
    };
    use tokio::net::UnixStream;

    use super::*;

    /// The service wire end to end, through the classifying decoders.
    #[tokio::test(flavor = "multi_thread")]
    async fn service_additional_memory_does_not_grow_with_the_message() {
        let (_, public_key) = generate_keypair();
        let (mut near, mut far) = UnixStream::pair().unwrap();

        let mut uploads = Vec::new();
        for size in SIZES {
            let request = ServiceRequest::AuthenticatedDb(Box::new(AuthenticatedDbRequest {
                root_id: ID::default(),
                identity: SigKey::from_pubkey(&public_key),
                op: DatabaseOp::SubmitSignedEntry {
                    entry: Box::new(entry(size)),
                },
            }));
            let baseline = reset_peak_allocation();
            let (sent, received) = tokio::join!(
                write_owned_frame(&mut near, request),
                read_classified_request(&mut far),
            );
            sent.unwrap();
            let Some(ServiceRequest::AuthenticatedDb(request)) = received.unwrap() else {
                panic!("expected the request back");
            };
            let DatabaseOp::SubmitSignedEntry { entry } = &request.op else {
                panic!("expected the same operation");
            };
            uploads.push(additional(baseline, held(entry)));
        }
        assert_bounded("service upload", &uploads);

        let mut downloads = Vec::new();
        for size in SIZES {
            let frame = ServerFrame::Response(Box::new(ServiceResponse::Entry(entry(size))));
            let baseline = reset_peak_allocation();
            let (sent, received) = tokio::join!(
                write_owned_frame(&mut near, frame),
                read_classified_response(&mut far),
            );
            sent.unwrap();
            let Some(ServiceResponse::Entry(entry)) = received.unwrap() else {
                panic!("expected the entry back");
            };
            downloads.push(additional(baseline, held(&entry)));
        }
        assert_bounded("service download", &downloads);
    }
}
