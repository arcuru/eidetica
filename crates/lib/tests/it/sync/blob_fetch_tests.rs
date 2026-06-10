//! Tests for content-addressed blob fetch over sync (design §5.4 / §10.1).
//!
//! Two layers: the server-side `FetchBlobs` handler (deterministic, no
//! transport), and the end-to-end `Instance::get_blob` peer-fetch leg over a
//! real HTTP transport.

use std::time::Duration;

use eidetica::entry::ID;
use eidetica::sync::{peer_types::Address, transports::http::HttpTransport};
use tokio::time::sleep;

use super::helpers::{handle_request, setup};
use crate::helpers::test_local_instance as test_instance;

/// The `FetchBlobs` handler serves only the CIDs it holds, omits the rest, and
/// skips non-blob (non-raw) CIDs — the by-CID, no-enumeration contract.
#[tokio::test]
async fn test_fetch_blobs_handler_serves_only_requested_held_blobs() {
    let (instance, sync) = setup().await;

    let present = instance.put_blob(b"held bytes".to_vec()).await.unwrap();
    let absent = ID::from_bytes(b"never stored");
    // A dag-cbor (entry-shaped) CID is not a blob address; it must be skipped.
    let non_raw = ID::from_dagcbor_bytes(b"not a blob");

    let request = eidetica::sync::protocol::SyncRequest::FetchBlobs {
        cids: vec![present.clone(), absent.clone(), non_raw.clone()],
    };

    let response = handle_request(&sync, &request).await;

    let blobs = match response {
        eidetica::sync::protocol::SyncResponse::Blobs(b) => b,
        other => panic!("expected Blobs response, got {other:?}"),
    };

    // Only the held raw blob comes back; absent + non-raw are omitted.
    assert_eq!(blobs.len(), 1, "only the held blob should be served");
    assert_eq!(blobs[0].0, present);
    assert_eq!(blobs[0].1, b"held bytes");
    // Self-verifying: the served bytes hash to the requested CID.
    assert_eq!(ID::from_bytes(&blobs[0].1), present);
}

/// An empty CID list yields an empty response — no enumeration, no leakage.
#[tokio::test]
async fn test_fetch_blobs_handler_empty_request() {
    let (instance, sync) = setup().await;
    let _ = instance.put_blob(b"something held".to_vec()).await.unwrap();

    let request = eidetica::sync::protocol::SyncRequest::FetchBlobs { cids: vec![] };
    let response = handle_request(&sync, &request).await;

    match response {
        eidetica::sync::protocol::SyncResponse::Blobs(b) => {
            assert!(b.is_empty(), "empty request must not reveal held blobs");
        }
        other => panic!("expected Blobs response, got {other:?}"),
    }
}

/// End-to-end: a blob held only by a peer is resolved by `Instance::get_blob`
/// (local miss → ask known peers → verify → persist), then served locally.
#[tokio::test]
async fn test_get_blob_fetches_from_peer_over_http() -> eidetica::Result<()> {
    // Holder (server) and fetcher (client).
    let holder = test_instance().await;
    let fetcher = test_instance().await;
    holder.enable_sync().await?;
    fetcher.enable_sync().await?;

    let holder_sync = holder.sync().expect("holder sync");
    let fetcher_sync = fetcher.sync().expect("fetcher sync");

    // Holder runs an HTTP server; fetcher just needs a transport to send with.
    holder_sync
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;
    fetcher_sync
        .register_transport("http", HttpTransport::builder())
        .await?;
    holder_sync.accept_connections().await?;
    let holder_addr = Address::http(holder_sync.get_server_address().await?);
    sleep(Duration::from_millis(100)).await;

    // The fetcher knows the holder as a peer (blobs are global — no tree/db,
    // no auth; the CID is the capability).
    let holder_pubkey = holder_sync.get_device_pubkey()?;
    fetcher_sync
        .register_peer(&holder_pubkey, Some("holder"))
        .await?;
    fetcher_sync
        .add_peer_address(&holder_pubkey, holder_addr)
        .await?;

    // Holder stores a blob; fetcher does not have it.
    let data = b"bytes that live only on the peer".to_vec();
    let cid = holder.put_blob(data.clone()).await?;
    assert!(
        fetcher.get_blob_local(&cid).await?.is_none(),
        "precondition: fetcher must not hold the blob locally"
    );

    // get_blob resolves it from the peer, verifies, and returns it.
    let got = fetcher.get_blob(&cid).await?;
    assert_eq!(
        got.as_deref(),
        Some(data.as_slice()),
        "get_blob must resolve the blob from the holding peer"
    );

    // ...and persists it locally, so a subsequent local read hits.
    assert_eq!(
        fetcher.get_blob_local(&cid).await?.as_deref(),
        Some(data.as_slice()),
        "a peer-fetched blob must be persisted locally"
    );

    Ok(())
}

/// An unknown CID that no peer holds resolves to `None`, not an error.
#[tokio::test]
async fn test_get_blob_absent_on_peer_is_none() -> eidetica::Result<()> {
    let holder = test_instance().await;
    let fetcher = test_instance().await;
    holder.enable_sync().await?;
    fetcher.enable_sync().await?;

    let holder_sync = holder.sync().expect("holder sync");
    let fetcher_sync = fetcher.sync().expect("fetcher sync");

    holder_sync
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;
    fetcher_sync
        .register_transport("http", HttpTransport::builder())
        .await?;
    holder_sync.accept_connections().await?;
    let holder_addr = Address::http(holder_sync.get_server_address().await?);
    sleep(Duration::from_millis(100)).await;

    let holder_pubkey = holder_sync.get_device_pubkey()?;
    fetcher_sync
        .register_peer(&holder_pubkey, Some("holder"))
        .await?;
    fetcher_sync
        .add_peer_address(&holder_pubkey, holder_addr)
        .await?;

    let missing = ID::from_bytes(b"no peer has these bytes");
    assert!(
        fetcher.get_blob(&missing).await?.is_none(),
        "a CID no peer holds must resolve to None"
    );

    Ok(())
}

/// Wire up two sync nodes over HTTP and register the holder as the fetcher's
/// peer. Returns (holder, fetcher).
async fn two_synced_nodes() -> (eidetica::Instance, eidetica::Instance) {
    let holder = test_instance().await;
    let fetcher = test_instance().await;
    holder.enable_sync().await.unwrap();
    fetcher.enable_sync().await.unwrap();

    let holder_sync = holder.sync().expect("holder sync");
    let fetcher_sync = fetcher.sync().expect("fetcher sync");

    holder_sync
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    fetcher_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();
    holder_sync.accept_connections().await.unwrap();
    let holder_addr = Address::http(holder_sync.get_server_address().await.unwrap());
    sleep(Duration::from_millis(100)).await;

    let holder_pubkey = holder_sync.get_device_pubkey().unwrap();
    fetcher_sync
        .register_peer(&holder_pubkey, Some("holder"))
        .await
        .unwrap();
    fetcher_sync
        .add_peer_address(&holder_pubkey, holder_addr)
        .await
        .unwrap();

    (holder, fetcher)
}

/// End-to-end: `get_blob_range` streams a verified byte range from a peer (bao),
/// for a blob the fetcher does not hold locally.
#[tokio::test]
async fn test_get_blob_range_streams_from_peer_over_http() -> eidetica::Result<()> {
    let (holder, fetcher) = two_synced_nodes().await;

    // A blob big enough to span several 16 KiB bao chunk groups.
    let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let cid = holder.put_blob(data.clone()).await?;
    assert!(fetcher.get_blob_local(&cid).await?.is_none());

    // A sub-range that crosses chunk boundaries.
    let range = 40_000u64..60_123u64;
    let got = fetcher.get_blob_range(&cid, range.clone()).await?;
    assert_eq!(
        got.as_deref(),
        Some(&data[range.start as usize..range.end as usize]),
        "get_blob_range must stream the verified range from the peer"
    );

    // The whole blob via range also works.
    let whole = fetcher.get_blob_range(&cid, 0..data.len() as u64).await?;
    assert_eq!(whole.as_deref(), Some(data.as_slice()));

    Ok(())
}

/// A range fetch for a blob no peer holds resolves to `None`.
#[tokio::test]
async fn test_get_blob_range_absent_on_peer_is_none() -> eidetica::Result<()> {
    let (_holder, fetcher) = two_synced_nodes().await;

    let missing = ID::from_bytes(b"no peer has this for range fetch");
    assert!(fetcher.get_blob_range(&missing, 0..16).await?.is_none());

    Ok(())
}
