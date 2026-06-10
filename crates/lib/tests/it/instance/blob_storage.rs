//! Tests for the public `Instance` blob API
//! (`put_blob`/`get_blob`/`get_blob_local`).
//!
//! These exercise the user-facing surface: content-addressing on `put_blob`,
//! self-verifying resolution, codec gating, and the Phase-1 size cap. They run
//! against a process-local instance because blob storage requires a concrete
//! local engine.

use eidetica::{BlobRef, backend::errors::BackendError, entry::ID};

use crate::helpers::test_local_instance;

#[tokio::test]
async fn test_instance_put_blob_ref_round_trip() {
    let instance = test_local_instance().await;

    let data = b"reference me with a size".to_vec();
    let blob_ref = instance.put_blob_ref(data.clone()).await.unwrap();

    // The typed reference carries the content address and the declared size.
    assert_eq!(blob_ref.cid(), &ID::from_bytes(&data));
    assert_eq!(blob_ref.size(), data.len() as u64);

    // ...and resolves back to the same bytes via the reference.
    assert_eq!(instance.get_blob_ref(&blob_ref).await.unwrap(), Some(data));
}

#[tokio::test]
async fn test_instance_get_blob_ref_size_mismatch_rejected() {
    let instance = test_local_instance().await;

    let data = b"declared size will be wrong".to_vec();
    let cid = instance.put_blob(data.clone()).await.unwrap();

    // A reference whose declared size disagrees with the stored bytes is
    // rejected — the CID pins identity, but length is checked separately.
    let lying = BlobRef::new(cid, (data.len() + 1) as u64);
    let err = instance
        .get_blob_ref(&lying)
        .await
        .expect_err("declared-size mismatch must be rejected");
    match err {
        eidetica::Error::Backend(b) => assert!(
            matches!(*b, BackendError::BlobSizeMismatch { .. }),
            "expected BlobSizeMismatch, got {b:?}"
        ),
        other => panic!("expected Backend error, got {other:?}"),
    }
}

#[tokio::test]
async fn test_instance_get_blob_ref_over_cap_rejected_before_fetch() {
    let instance = test_local_instance().await;

    // An over-cap declared size is refused up front (size-before-fetch): the
    // blob is not even stored, so this can only short-circuit on the size.
    let huge = BlobRef::new(
        ID::from_bytes(b"never stored"),
        eidetica::backend::DEFAULT_MAX_BLOB_BYTES as u64 + 1,
    );
    let err = instance
        .get_blob_ref(&huge)
        .await
        .expect_err("over-cap reference must be rejected before fetch");
    match err {
        eidetica::Error::Backend(b) => assert!(
            matches!(*b, BackendError::BlobTooLarge { .. }),
            "expected BlobTooLarge, got {b:?}"
        ),
        other => panic!("expected Backend error, got {other:?}"),
    }
}

/// End-to-end through the public `Instance` API against a file-based SQLite
/// backend, so a large blob travels the full hybrid disk tier (§5.2): stored on
/// disk, then whole-read and windowed-read back through `Instance::get_blob` /
/// `get_blob_range` (which dispatch to the local engine and stamp LRU). The
/// per-tier mechanics are unit-tested at the backend layer; this proves the
/// wiring holds from the top.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn test_instance_blob_round_trip_over_disk_tier() {
    use eidetica::{NewUser, backend::database::Sqlite};

    let dir = tempfile::tempdir().expect("tempdir");
    let backend = Sqlite::open(dir.path().join("inst.db"))
        .await
        .expect("open file sqlite");
    let (instance, _admin) =
        eidetica::Instance::create_backend(Box::new(backend), NewUser::passwordless("admin"))
            .await
            .expect("create instance");

    // A multi-block blob well over the 16 KiB inline threshold → on disk.
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let cid = instance.put_blob(data.clone()).await.unwrap();

    // It resolves whole and by window through the public API.
    assert_eq!(instance.get_blob(&cid).await.unwrap(), Some(data.clone()));
    assert_eq!(
        instance.get_blob_range(&cid, 70_000..70_500).await.unwrap(),
        Some(data[70_000..70_500].to_vec())
    );
    // The file is really on disk, not inline.
    assert!(
        dir.path()
            .join("inst.db.blobs")
            .join(cid.to_string())
            .exists(),
        "large blob landed in the on-disk tier"
    );
}

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
