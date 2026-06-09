//! Tests for the public `Instance` blob API
//! (`put_blob`/`get_blob`/`get_blob_local`).
//!
//! These exercise the user-facing surface: content-addressing on `put_blob`,
//! self-verifying resolution, codec gating, and the Phase-1 size cap. They run
//! against a process-local instance because blob storage requires a concrete
//! local engine.

use eidetica::{backend::errors::BackendError, entry::ID};

use crate::helpers::test_local_instance;

#[tokio::test]
async fn test_instance_put_blob_returns_content_address() {
    let instance = test_local_instance().await;

    let data = b"attach me to a store".to_vec();
    let cid = instance.put_blob(data.clone()).await.unwrap();

    // The returned address is exactly the content hash of the bytes.
    assert_eq!(cid, ID::from_bytes(&data));
    assert!(cid.is_raw(), "blob address is raw codec");

    // ...and resolves back to the same bytes.
    assert_eq!(instance.get_blob(&cid).await.unwrap(), Some(data.clone()));
    assert_eq!(instance.get_blob_local(&cid).await.unwrap(), Some(data));
}

#[tokio::test]
async fn test_instance_put_blob_idempotent() {
    let instance = test_local_instance().await;

    let data = b"store me twice".to_vec();
    let first = instance.put_blob(data.clone()).await.unwrap();
    let second = instance.put_blob(data.clone()).await.unwrap();

    assert_eq!(first, second, "same bytes yield the same CID");
    assert_eq!(instance.get_blob(&first).await.unwrap(), Some(data));
}

#[tokio::test]
async fn test_instance_get_blob_absent_is_none() {
    let instance = test_local_instance().await;

    // A well-formed raw CID we never stored resolves to None. This instance has
    // no sync enabled, so there are no peers to fetch from — the blob is simply
    // absent.
    let cid = ID::from_bytes(b"never stored");
    assert!(instance.get_blob(&cid).await.unwrap().is_none());
    assert!(instance.get_blob_local(&cid).await.unwrap().is_none());
}

#[tokio::test]
async fn test_instance_get_blob_rejects_non_raw_codec() {
    let instance = test_local_instance().await;

    // A dag-cbor (0x71) address — produced here via from_dagcbor_bytes, the
    // same codec entries use — is not a raw blob and must be rejected rather
    // than silently missing.
    let entry_like = ID::from_dagcbor_bytes(b"some dag-cbor content");
    assert!(!entry_like.is_raw());

    let err = instance
        .get_blob(&entry_like)
        .await
        .expect_err("non-raw codec must be rejected");
    match err {
        eidetica::Error::Backend(b) => assert!(
            matches!(*b, BackendError::BlobInvalidCodec { .. }),
            "expected BlobInvalidCodec, got {b:?}"
        ),
        other => panic!("expected Backend error, got {other:?}"),
    }
}

#[tokio::test]
async fn test_instance_get_blob_range() {
    let instance = test_local_instance().await;

    let data = b"0123456789".to_vec();
    let cid = instance.put_blob(data.clone()).await.unwrap();

    // A middle slice.
    assert_eq!(
        instance.get_blob_range(&cid, 2..5).await.unwrap(),
        Some(b"234".to_vec())
    );
    // The full range.
    assert_eq!(
        instance.get_blob_range(&cid, 0..10).await.unwrap(),
        Some(data.clone())
    );
    // An over-long end clamps to the available tail.
    assert_eq!(
        instance.get_blob_range(&cid, 7..100).await.unwrap(),
        Some(b"789".to_vec())
    );
    // A start at/after the end yields an empty slice (the blob exists).
    assert_eq!(
        instance.get_blob_range(&cid, 10..20).await.unwrap(),
        Some(Vec::new())
    );
    // A degenerate (start > end) range is empty, not a panic. Built from
    // values so it isn't flagged as a const reversed-empty range.
    let (lo, hi) = (5u64, 3u64);
    assert_eq!(
        instance.get_blob_range(&cid, lo..hi).await.unwrap(),
        Some(Vec::new())
    );
}

#[tokio::test]
async fn test_instance_get_blob_range_absent_is_none() {
    let instance = test_local_instance().await;

    let cid = ID::from_bytes(b"never stored");
    assert!(instance.get_blob_range(&cid, 0..4).await.unwrap().is_none());
}

#[tokio::test]
async fn test_instance_get_blob_range_rejects_non_raw_codec() {
    let instance = test_local_instance().await;

    let entry_like = ID::from_dagcbor_bytes(b"some dag-cbor content");
    let err = instance
        .get_blob_range(&entry_like, 0..4)
        .await
        .expect_err("non-raw codec must be rejected");
    match err {
        eidetica::Error::Backend(b) => assert!(
            matches!(*b, BackendError::BlobInvalidCodec { .. }),
            "expected BlobInvalidCodec, got {b:?}"
        ),
        other => panic!("expected Backend error, got {other:?}"),
    }
}

#[tokio::test]
async fn test_instance_put_blob_size_cap() {
    let instance = test_local_instance().await;

    // Exceeding the Phase-1 hard cap is rejected up front (a content address
    // does not bound its payload size).
    let too_big = vec![0u8; eidetica::backend::DEFAULT_MAX_BLOB_BYTES + 1];
    let err = instance
        .put_blob(too_big)
        .await
        .expect_err("over-cap blob must be rejected");
    match err {
        eidetica::Error::Backend(b) => assert!(
            matches!(*b, BackendError::BlobTooLarge { .. }),
            "expected BlobTooLarge, got {b:?}"
        ),
        other => panic!("expected Backend error, got {other:?}"),
    }
}
