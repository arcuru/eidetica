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

    // A well-formed raw CID we never stored resolves to None (Phase 1 has no
    // peer fetch, so an unknown local blob is simply absent).
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
