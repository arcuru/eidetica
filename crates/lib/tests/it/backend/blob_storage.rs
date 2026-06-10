//! Tests for content-addressed blob storage on the backend
//! (`put_blob`/`get_blob`/`has_blob`).
//!
//! These run against whichever backend `TEST_BACKEND` selects (InMemory by
//! default, SQLite/Postgres under their features), so the same invariants are
//! exercised across every storage engine.

use eidetica::{
    backend::errors::BackendError,
    entry::{Entry, ID},
};

use super::helpers::test_backend;

#[tokio::test]
async fn test_blob_put_get_roundtrip() {
    let backend = test_backend().await;

    let data = b"hello blob world".to_vec();
    let cid = ID::from_bytes(&data);

    assert!(!backend.has_blob(&cid).await.unwrap(), "absent before put");
    assert!(
        backend.get_blob(&cid).await.unwrap().is_none(),
        "get_blob returns None before put"
    );

    backend.put_blob(&cid, data.clone()).await.unwrap();

    assert!(backend.has_blob(&cid).await.unwrap(), "present after put");
    assert_eq!(
        backend.get_blob(&cid).await.unwrap().as_deref(),
        Some(data.as_slice()),
        "round-tripped bytes match"
    );
}

#[tokio::test]
async fn test_blob_range_and_header_windowed_read() {
    let backend = test_backend().await;

    // A multi-block blob (>16 KiB) so windowed reads cross bao block groups and
    // the persisted outboard is non-trivial.
    let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let cid = ID::from_bytes(&data);
    backend.put_blob(&cid, data.clone()).await.unwrap();

    // A ranged read returns exactly the clamped slice without whole-loading.
    assert_eq!(
        backend
            .get_blob_range(&cid, 40_000..40_123)
            .await
            .unwrap()
            .as_deref(),
        Some(&data[40_000..40_123])
    );
    // Over-long end clamps to the tail; the full range returns everything.
    assert_eq!(
        backend
            .get_blob_range(&cid, 99_990..1_000_000)
            .await
            .unwrap()
            .as_deref(),
        Some(&data[99_990..])
    );
    assert_eq!(
        backend
            .get_blob_range(&cid, 0..data.len() as u64)
            .await
            .unwrap()
            .as_deref(),
        Some(data.as_slice())
    );
    // An empty/past-the-end range on a present blob is empty bytes, not None.
    assert_eq!(
        backend
            .get_blob_range(&cid, 500_000..500_001)
            .await
            .unwrap(),
        Some(Vec::new())
    );

    // The header reports the true size and a non-empty outboard for serving.
    let (size, outboard) = backend.get_blob_header(&cid).await.unwrap().unwrap();
    assert_eq!(size, data.len() as u64);
    assert!(
        !outboard.is_empty(),
        "a multi-block blob has interior nodes"
    );

    // Both are None for an absent blob.
    let absent = ID::from_bytes(b"never stored");
    assert!(
        backend
            .get_blob_range(&absent, 0..4)
            .await
            .unwrap()
            .is_none()
    );
    assert!(backend.get_blob_header(&absent).await.unwrap().is_none());
}

#[tokio::test]
async fn test_blob_dedup_idempotent() {
    let backend = test_backend().await;

    let data = b"deduplicate me".to_vec();
    let cid = ID::from_bytes(&data);

    // Storing the same bytes twice is a no-op and does not error.
    backend.put_blob(&cid, data.clone()).await.unwrap();
    backend.put_blob(&cid, data.clone()).await.unwrap();

    assert_eq!(backend.get_blob(&cid).await.unwrap(), Some(data));
}

#[tokio::test]
async fn test_blob_hash_mismatch_rejected() {
    let backend = test_backend().await;

    // A CID that does not match the bytes must be rejected — this is the
    // self-verifying guarantee that lets `get_blob` callers trust the bytes.
    let wrong_cid = ID::from_bytes(b"some other content");
    let data = b"the actual bytes".to_vec();

    let err = backend
        .put_blob(&wrong_cid, data)
        .await
        .expect_err("mismatched cid must be rejected");
    match err {
        eidetica::Error::Backend(b) => {
            assert!(
                matches!(*b, BackendError::BlobHashMismatch { .. }),
                "expected BlobHashMismatch, got {b:?}"
            );
        }
        other => panic!("expected Backend error, got {other:?}"),
    }

    // Nothing should have been persisted under the wrong CID.
    assert!(!backend.has_blob(&wrong_cid).await.unwrap());
}

#[tokio::test]
async fn test_blob_empty_bytes() {
    let backend = test_backend().await;

    let data: Vec<u8> = Vec::new();
    let cid = ID::from_bytes(&data);

    backend.put_blob(&cid, data.clone()).await.unwrap();
    assert!(backend.has_blob(&cid).await.unwrap());
    assert_eq!(backend.get_blob(&cid).await.unwrap(), Some(data));
}

#[tokio::test]
async fn test_blob_address_is_raw_codec() {
    // A blob CID is raw-codec (0x55); an entry ID is dag-cbor (0x71). The two
    // address spaces are distinguishable, which is what the codec branch relies
    // on (and what keeps a blob CID from colliding with an entry ID).
    let blob_cid = ID::from_bytes(b"i am a blob");
    assert!(blob_cid.is_raw(), "blob CID should be raw codec");

    let entry = Entry::root_builder()
        .build()
        .expect("root entry should build");
    assert!(!entry.id().is_raw(), "entry ID should be dag-cbor, not raw");
}

#[tokio::test]
async fn test_blobs_are_independent_keys() {
    let backend = test_backend().await;

    let a = b"first blob".to_vec();
    let b = b"second blob".to_vec();
    let cid_a = ID::from_bytes(&a);
    let cid_b = ID::from_bytes(&b);
    assert_ne!(cid_a, cid_b);

    backend.put_blob(&cid_a, a.clone()).await.unwrap();
    assert!(backend.has_blob(&cid_a).await.unwrap());
    assert!(
        !backend.has_blob(&cid_b).await.unwrap(),
        "storing one blob must not imply another is present"
    );

    backend.put_blob(&cid_b, b.clone()).await.unwrap();
    assert_eq!(backend.get_blob(&cid_a).await.unwrap(), Some(a));
    assert_eq!(backend.get_blob(&cid_b).await.unwrap(), Some(b));
}
