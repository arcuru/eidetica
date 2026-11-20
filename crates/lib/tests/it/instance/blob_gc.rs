//! Local blob cache GC and trusted engine retention substrate tests.
use crate::helpers::test_backend;
use eidetica::{
    FixedClock, GcOptions, Instance, NewAccount, backend::database::InMemory, entry::ID,
};
use std::sync::Arc;
async fn instance_with_clock(clock: Arc<FixedClock>) -> (Instance, eidetica::AccountSession) {
    Instance::create_backend_with_clock(
        test_backend().await,
        clock,
        NewAccount::passwordless("admin"),
    )
    .await
    .unwrap()
}
/// A size target evicts least-recently-used unpinned blobs first.
#[tokio::test]
async fn test_gc_lru_eviction_to_target() {
    let (instance, _user) = instance_with_clock(Arc::new(FixedClock::new(1_000))).await;

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
    let (instance, _user) = instance_with_clock(Arc::new(FixedClock::new(1_000))).await;

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
    let (instance, _user) = instance_with_clock(clock.clone()).await;

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

#[tokio::test]
async fn test_gc_maximum_grace_window_keeps_fresh_blobs() {
    let (instance, _user) = instance_with_clock(Arc::new(FixedClock::new(1_000))).await;
    let cid = instance.put_blob(b"fresh".to_vec()).await.unwrap();
    let report = instance
        .gc_blobs(GcOptions {
            max_total_bytes: None,
            min_age_ms: u64::MAX,
        })
        .await
        .unwrap();
    assert_eq!(report.evicted_count, 0);
    assert_eq!(
        instance.get_blob_local(&cid).await.unwrap(),
        Some(b"fresh".to_vec())
    );
}

/// Pause a write after its bytes have been published, before its access stamp.
/// GC must not see that intermediate state as an aged, unpinned blob.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_blob_gc_cannot_collect_a_put_before_its_access_stamp() {
    use eidetica::{Clock, SystemClock};
    use std::{
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        time::Duration,
    };

    #[derive(Debug)]
    struct PauseClock {
        armed: AtomicBool,
        paused: tokio::sync::Notify,
        resume: Mutex<mpsc::Receiver<()>>,
    }
    impl Clock for PauseClock {
        fn now_millis(&self) -> u64 {
            if self.armed.swap(false, Ordering::SeqCst) {
                self.paused.notify_one();
                self.resume
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .expect("release paused put");
            }
            100_000
        }
        fn now_rfc3339(&self) -> String {
            SystemClock.now_rfc3339()
        }
    }
    let (resume, rx) = mpsc::channel();
    let clock = Arc::new(PauseClock {
        armed: AtomicBool::new(false),
        paused: tokio::sync::Notify::new(),
        resume: Mutex::new(rx),
    });
    let (instance, _admin) = Instance::create_backend_with_clock(
        test_backend().await,
        clock.clone(),
        NewAccount::passwordless("admin"),
    )
    .await
    .unwrap();
    clock.armed.store(true, Ordering::SeqCst);
    let writer = instance.clone();
    // Keep the synchronous Clock pause off Tokio's I/O/timer workers. SQL
    // completion may run on the driver thread, which must remain free for GC.
    let runtime = tokio::runtime::Handle::current();
    let put = std::thread::spawn(move || runtime.block_on(writer.put_blob(b"in flight".to_vec())));
    tokio::time::timeout(Duration::from_secs(5), clock.paused.notified())
        .await
        .unwrap();
    let mut gc = Box::pin(instance.gc_blobs(GcOptions::default()));
    let early = tokio::time::timeout(Duration::from_millis(50), &mut gc)
        .await
        .ok();
    resume.send(()).unwrap();
    let cid = put.join().unwrap().unwrap();
    let report = match early {
        Some(result) => result.unwrap(),
        None => tokio::time::timeout(Duration::from_secs(5), gc)
            .await
            .unwrap()
            .unwrap(),
    };
    assert_eq!(
        report.evicted_count, 0,
        "GC must wait for publication and its fresh access stamp"
    );
    assert_eq!(
        instance.get_blob_local(&cid).await.unwrap(),
        Some(b"in flight".to_vec())
    );
}
