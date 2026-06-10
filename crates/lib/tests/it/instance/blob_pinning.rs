//! Tests for blob pinning + garbage collection (Phase 1.5, §6).
//!
//! Pins are instance-local retention assertions keyed by `(user, database,
//! cid)`; GC is an explicit, size-targeted pass that LRU-evicts unpinned blobs.
//! These run against a process-local instance with a caller-held [`FixedClock`]
//! so last-access ordering (LRU) and the grace window are deterministic.

use std::sync::Arc;

use eidetica::{FixedClock, GcOptions, Instance, NewUser, backend::database::InMemory, entry::ID};

const USER: &str = "user-uuid-1";

/// Build a local instance whose clock the test holds, so it can advance time
/// and reason about `last_accessed`. The clock auto-advances 1ms per
/// `now_millis()` unless explicitly advanced/held.
async fn instance_with_clock(clock: Arc<FixedClock>) -> Instance {
    let (instance, _admin) = Instance::create_backend_with_clock(
        Box::new(InMemory::new()),
        clock,
        NewUser::passwordless("admin"),
    )
    .await
    .expect("create local test instance");
    instance
}

/// GC with no size target evicts every unpinned blob but keeps pinned ones.
#[tokio::test]
async fn test_gc_evicts_unpinned_keeps_pinned() {
    let instance = instance_with_clock(Arc::new(FixedClock::new(1_000))).await;

    let keep = instance.put_blob(b"keep me".to_vec()).await.unwrap();
    let drop = instance.put_blob(b"drop me".to_vec()).await.unwrap();
    instance.pin_blob(USER, None, &keep).await.unwrap();

    let report = instance
        .gc_blobs(GcOptions {
            max_total_bytes: None,
            min_age_ms: 0,
        })
        .await
        .unwrap();

    assert_eq!(report.evicted_count, 1);
    assert_eq!(report.reclaimed_bytes, "drop me".len() as u64);
    assert_eq!(report.retained_bytes, "keep me".len() as u64);
    assert_eq!(report.pinned_bytes, "keep me".len() as u64);

    assert!(
        instance.get_blob_local(&keep).await.unwrap().is_some(),
        "pinned blob survives GC"
    );
    assert!(
        instance.get_blob_local(&drop).await.unwrap().is_none(),
        "unpinned blob is evicted"
    );
}

/// Un-pinning a blob makes it collectable; a redundant unpin returns false.
#[tokio::test]
async fn test_unpin_makes_collectable() {
    let instance = instance_with_clock(Arc::new(FixedClock::new(1_000))).await;

    let cid = instance
        .put_blob(b"pinned then freed".to_vec())
        .await
        .unwrap();
    instance.pin_blob(USER, None, &cid).await.unwrap();

    // Pinned → survives.
    let r = instance
        .gc_blobs(GcOptions {
            max_total_bytes: None,
            min_age_ms: 0,
        })
        .await
        .unwrap();
    assert_eq!(r.evicted_count, 0);
    assert!(instance.get_blob_local(&cid).await.unwrap().is_some());

    // Unpin → collectable.
    assert!(
        instance.unpin_blob(USER, None, &cid).await.unwrap(),
        "the pin existed"
    );
    let r = instance
        .gc_blobs(GcOptions {
            max_total_bytes: None,
            min_age_ms: 0,
        })
        .await
        .unwrap();
    assert_eq!(r.evicted_count, 1);
    assert!(instance.get_blob_local(&cid).await.unwrap().is_none());

    // Removing a pin that no longer exists is a no-op false.
    assert!(!instance.unpin_blob(USER, None, &cid).await.unwrap());
}

/// A size target evicts least-recently-used unpinned blobs first.
#[tokio::test]
async fn test_gc_lru_eviction_to_target() {
    let instance = instance_with_clock(Arc::new(FixedClock::new(1_000))).await;

    // Three equal-size, distinct blobs, stored in order (auto-advancing clock
    // ⇒ a < b < c by last_accessed).
    let a = instance.put_blob(vec![b'a'; 10]).await.unwrap();
    let b = instance.put_blob(vec![b'b'; 10]).await.unwrap();
    let c = instance.put_blob(vec![b'c'; 10]).await.unwrap();

    // Re-read A so it becomes most-recently-used; B is now the LRU.
    instance.get_blob_local(&a).await.unwrap();

    // 30 bytes held; reduce to ≤ 20 ⇒ evict exactly one (the LRU, B).
    let report = instance
        .gc_blobs(GcOptions {
            max_total_bytes: Some(20),
            min_age_ms: 0,
        })
        .await
        .unwrap();

    assert_eq!(report.evicted_count, 1);
    assert_eq!(report.reclaimed_bytes, 10);
    assert_eq!(report.retained_bytes, 20);

    assert!(instance.get_blob_local(&a).await.unwrap().is_some());
    assert!(
        instance.get_blob_local(&b).await.unwrap().is_none(),
        "the least-recently-used blob (B) is evicted"
    );
    assert!(instance.get_blob_local(&c).await.unwrap().is_some());
}

/// GC is a no-op when already under the size target.
#[tokio::test]
async fn test_gc_noop_when_under_target() {
    let instance = instance_with_clock(Arc::new(FixedClock::new(1_000))).await;

    let cid = instance.put_blob(vec![0u8; 10]).await.unwrap();
    let report = instance
        .gc_blobs(GcOptions {
            max_total_bytes: Some(1_000),
            min_age_ms: 0,
        })
        .await
        .unwrap();

    assert_eq!(report.evicted_count, 0);
    assert_eq!(report.retained_bytes, 10);
    assert!(instance.get_blob_local(&cid).await.unwrap().is_some());
}

/// The grace window protects a just-written, not-yet-pinned blob from a
/// concurrent sweep; once it ages out it becomes collectable.
#[tokio::test]
async fn test_gc_grace_window_protects_fresh() {
    let clock = Arc::new(FixedClock::new(1_000));
    let instance = instance_with_clock(clock.clone()).await;

    let cid = instance.put_blob(b"fresh".to_vec()).await.unwrap();

    // Large grace ⇒ fresh unpinned blob protected.
    let report = instance
        .gc_blobs(GcOptions {
            max_total_bytes: None,
            min_age_ms: 60_000,
        })
        .await
        .unwrap();
    assert_eq!(report.evicted_count, 0);
    assert!(instance.get_blob_local(&cid).await.unwrap().is_some());

    // Advance past the grace window ⇒ now collectable.
    clock.advance(120_000);
    let report = instance
        .gc_blobs(GcOptions {
            max_total_bytes: None,
            min_age_ms: 60_000,
        })
        .await
        .unwrap();
    assert_eq!(report.evicted_count, 1);
    assert!(instance.get_blob_local(&cid).await.unwrap().is_none());
}

/// `pinned_size_by_user` sums distinct pinned blobs per user (provenance), each
/// counted once even across databases, and isolates users from one another.
#[tokio::test]
async fn test_pinned_size_by_user() {
    let instance = instance_with_clock(Arc::new(FixedClock::new(1_000))).await;

    let a = instance.put_blob(vec![1u8; 100]).await.unwrap();
    let b = instance.put_blob(vec![2u8; 50]).await.unwrap();

    assert_eq!(instance.pinned_size_by_user(USER).await.unwrap(), 0);

    instance.pin_blob(USER, None, &a).await.unwrap();
    instance.pin_blob(USER, None, &b).await.unwrap();
    assert_eq!(instance.pinned_size_by_user(USER).await.unwrap(), 150);

    // Pinning A again under a specific database for the same user counts once.
    let db = ID::from_dagcbor_bytes(b"some-database-root");
    instance.pin_blob(USER, Some(&db), &a).await.unwrap();
    assert_eq!(instance.pinned_size_by_user(USER).await.unwrap(), 150);

    // Another user's pins are accounted separately.
    instance.pin_blob("other-user", None, &a).await.unwrap();
    assert_eq!(instance.pinned_size_by_user(USER).await.unwrap(), 150);
    assert_eq!(
        instance.pinned_size_by_user("other-user").await.unwrap(),
        100
    );
}

/// Pinning a non-raw (e.g. dag-cbor entry) address is rejected, not silently
/// accepted — only raw-codec blobs are pinnable.
#[tokio::test]
async fn test_pin_rejects_non_raw_codec() {
    use eidetica::backend::errors::BackendError;

    let instance = instance_with_clock(Arc::new(FixedClock::new(1_000))).await;

    let entry_like = ID::from_dagcbor_bytes(b"not a blob");
    assert!(!entry_like.is_raw());

    let err = instance
        .pin_blob(USER, None, &entry_like)
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

/// Pins and per-blob access times survive an InMemory snapshot round-trip.
#[tokio::test]
async fn test_pins_and_access_persist_across_serialization() {
    use eidetica::backend::BackendImpl;

    let backend = InMemory::new();
    let data = b"persist me".to_vec();
    let cid = ID::from_bytes(&data);

    backend.put_blob(&cid, data).await.unwrap();
    backend.touch_blob_accessed(&cid, 4242).await.unwrap();
    backend.pin_blob("u1", "", &cid).await.unwrap();

    let json = serde_json::to_string(&backend).unwrap();
    let restored: InMemory = serde_json::from_str(&json).unwrap();

    assert!(
        restored.pinned_cids().await.unwrap().contains(&cid),
        "pins survive serialization"
    );
    let meta = restored.all_blob_meta().await.unwrap();
    assert_eq!(meta.len(), 1);
    assert_eq!(meta[0].cid, cid);
    assert_eq!(
        meta[0].last_accessed, 4242,
        "last_accessed survives serialization"
    );
}
