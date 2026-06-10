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

/// Hybrid inline/disk tier (§5.2): on a file-based SQLite backend, blobs larger
/// than the 16 KiB threshold are stored as content-addressed files on disk
/// (`location = 1`, `data` NULL) while small blobs stay inline — and both tiers
/// round-trip, range-read, and delete through the same `BackendImpl` surface.
///
/// This is the one §4.3 test that reaches *past* the trait to the filesystem,
/// so it constructs the backend directly (a file DB in a tempdir) instead of
/// going through `test_backend()`.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn test_blob_hybrid_disk_tier() {
    use eidetica::backend::BackendImpl;
    use eidetica::backend::database::Sqlite;

    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("test.db");
    let backend = Sqlite::open(&db_path).await.expect("open file sqlite");

    // The on-disk tier lives in a sibling "<db>.blobs" directory.
    let blob_dir = dir.path().join("test.db.blobs");

    // --- Large blob (>16 KiB) → on disk ---
    let big: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let big_cid = ID::from_bytes(&big);
    backend.put_blob(&big_cid, big.clone()).await.unwrap();

    // The bytes live in a file named for the CID, not inline in SQL.
    let big_file = blob_dir.join(big_cid.to_string());
    assert!(
        big_file.exists(),
        "a >16 KiB blob is written to the on-disk tier"
    );
    assert_eq!(
        std::fs::metadata(&big_file).unwrap().len(),
        big.len() as u64,
        "the on-disk file holds exactly the blob bytes"
    );

    // Whole-read, windowed read, and header all work against the on-disk tier.
    assert_eq!(backend.get_blob(&big_cid).await.unwrap(), Some(big.clone()));
    assert_eq!(
        backend
            .get_blob_range(&big_cid, 40_000..40_123)
            .await
            .unwrap()
            .as_deref(),
        Some(&big[40_000..40_123]),
        "pread window from disk matches"
    );
    assert_eq!(
        backend
            .get_blob_range(&big_cid, 99_990..1_000_000)
            .await
            .unwrap()
            .as_deref(),
        Some(&big[99_990..]),
        "over-long end clamps to the file tail"
    );
    let (size, outboard) = backend.get_blob_header(&big_cid).await.unwrap().unwrap();
    assert_eq!(size, big.len() as u64);
    assert!(
        !outboard.is_empty(),
        "outboard persists in SQL for disk blobs"
    );

    // --- Small blob (≤16 KiB) → inline, no file ---
    let small = vec![7u8; 4096];
    let small_cid = ID::from_bytes(&small);
    backend.put_blob(&small_cid, small.clone()).await.unwrap();
    assert!(
        !blob_dir.join(small_cid.to_string()).exists(),
        "a small blob stays inline and creates no file"
    );
    assert_eq!(backend.get_blob(&small_cid).await.unwrap(), Some(small));

    // --- Dedup: re-storing the large blob is a cheap no-op ---
    backend.put_blob(&big_cid, big.clone()).await.unwrap();
    assert!(big_file.exists());
    assert_eq!(backend.get_blob(&big_cid).await.unwrap(), Some(big));

    // --- Delete unlinks the on-disk file ---
    assert!(backend.delete_blob(&big_cid).await.unwrap());
    assert!(
        !big_file.exists(),
        "deleting an on-disk blob removes its file"
    );
    assert!(backend.get_blob(&big_cid).await.unwrap().is_none());
    assert!(!backend.has_blob(&big_cid).await.unwrap());
}

/// The same hybrid disk tier on **Postgres**: the storage logic is
/// db-agnostic (it branches only on `blob_dir()`), so attaching a blob dir via
/// `with_blob_dir` puts large blobs on the instance's local disk while Postgres
/// holds the metadata — clients reach blobs through the instance, not Postgres.
///
/// Needs a live server, so it self-skips unless `TEST_POSTGRES_URL` is set
/// (GitHub CI provides one; the local nix gate does not).
#[cfg(feature = "postgres")]
#[tokio::test]
async fn test_blob_hybrid_disk_tier_postgres() {
    use eidetica::backend::BackendImpl;
    use eidetica::backend::database::Postgres;

    let Ok(url) = std::env::var("TEST_POSTGRES_URL") else {
        eprintln!("skipping: TEST_POSTGRES_URL not set");
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let backend = Postgres::connect_isolated(&url)
        .await
        .expect("connect postgres")
        .with_blob_dir(dir.path());

    // Large blob → on disk (location=1, BYTEA NULL); bytes in a CID-named file.
    let big: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let big_cid = ID::from_bytes(&big);
    backend.put_blob(&big_cid, big.clone()).await.unwrap();

    let big_file = dir.path().join(big_cid.to_string());
    assert!(big_file.exists(), "large blob on disk, not in BYTEA");
    assert_eq!(backend.get_blob(&big_cid).await.unwrap(), Some(big.clone()));
    assert_eq!(
        backend
            .get_blob_range(&big_cid, 40_000..40_123)
            .await
            .unwrap()
            .as_deref(),
        Some(&big[40_000..40_123]),
        "pread window from disk matches"
    );
    let (size, outboard) = backend.get_blob_header(&big_cid).await.unwrap().unwrap();
    assert_eq!(size, big.len() as u64);
    assert!(!outboard.is_empty());

    // Small blob → inline in BYTEA, no file.
    let small = vec![7u8; 4096];
    let small_cid = ID::from_bytes(&small);
    backend.put_blob(&small_cid, small.clone()).await.unwrap();
    assert!(
        !dir.path().join(small_cid.to_string()).exists(),
        "small blob stays inline"
    );
    assert_eq!(backend.get_blob(&small_cid).await.unwrap(), Some(small));

    // Delete unlinks the file.
    assert!(backend.delete_blob(&big_cid).await.unwrap());
    assert!(!big_file.exists());
    assert!(backend.get_blob(&big_cid).await.unwrap().is_none());
}
