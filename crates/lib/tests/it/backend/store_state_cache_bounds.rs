//! Bounded durable Snapshot-native materialization cache tests.
//!
//! The cache substrate is the derived-namespace table itself: each live
//! derived namespace is one retained historical Snapshot materialization,
//! keyed by its full canonical `source_key` bytes. These tests prove the
//! settled contract on every backend in the matrix:
//!
//! - many historical Snapshots survive concurrently until bounds force
//!   oldest-first eviction (row and byte bounds);
//! - reads never issue a storage write per hit (batched recency only);
//! - a hot frontier entry survives cold-insertion pressure;
//! - reopened databases stay warm;
//! - concurrent publishers still agree on one winner with eviction active;
//! - multi-tip Snapshot identity is canonical without synthetic IDs.

use std::collections::BTreeMap;

use eidetica::{
    Snapshot,
    backend::{
        BackendImpl, CacheScope, DerivedCachePolicy, ProjectionDescriptor, RecordView,
        StoreStateLifecycle, StoreStateRequest,
    },
    entry::ID,
};

use crate::helpers::test_backend;

fn projection() -> ProjectionDescriptor {
    ProjectionDescriptor {
        name: "test/cache-bounds".to_string(),
        version: 1,
    }
}

fn request(database: &ID, store: &str, snapshot: &Snapshot) -> StoreStateRequest {
    StoreStateRequest {
        database: database.clone(),
        store: store.to_string(),
        lifecycle: StoreStateLifecycle::Derived,
        scope: CacheScope::Shared,
        projection: projection(),
        source_key: snapshot.cache_key_bytes(),
    }
}

async fn publish(
    backend: &dyn BackendImpl,
    request: StoreStateRequest,
    records: Vec<(Vec<u8>, Vec<u8>)>,
) -> RecordView {
    let token = backend.begin_store_state_staging(request).await.unwrap();
    if !records.is_empty() {
        backend
            .stage_store_state_records(
                &token,
                records
                    .into_iter()
                    .map(|(key, value)| (key, Some(value)))
                    .collect::<BTreeMap<_, _>>(),
            )
            .await
            .unwrap();
    }
    backend.publish_store_state(token).await.unwrap()
}

async fn get_value(backend: &dyn BackendImpl, view: &RecordView) -> Option<Vec<u8>> {
    backend.store_state_record_get(view, b"v").await.unwrap()
}

/// Shrink the cache bounds where the backend allows it.
///
/// Both in-tree backends expose the test knob; any other implementor keeps
/// its safe defaults. Returns whether the knob applied.
fn set_policy(backend: &dyn BackendImpl, policy: DerivedCachePolicy) -> bool {
    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    if let Some(sql) = backend
        .as_any()
        .downcast_ref::<eidetica::backend::database::SqlxBackend>()
    {
        sql.testing_set_derived_cache_policy(policy);
        return true;
    }
    if let Some(memory) = backend
        .as_any()
        .downcast_ref::<eidetica::backend::database::InMemory>()
    {
        memory.testing_set_derived_cache_policy(policy);
        return true;
    }
    false
}

fn row_policy(max_namespaces: usize) -> DerivedCachePolicy {
    DerivedCachePolicy {
        max_namespaces,
        max_bytes: u64::MAX,
    }
}

/// Distinct historical Snapshot materializations, one per store name.
async fn publish_history(
    backend: &dyn BackendImpl,
    database: &ID,
    count: usize,
) -> Vec<(StoreStateRequest, RecordView)> {
    let mut out = Vec::new();
    for i in 0..count {
        let snapshot = Snapshot::new(vec![ID::from_bytes(format!("tip-{i:03}"))]);
        let req = request(database, &format!("store-{i:03}"), &snapshot);
        let view = publish(
            backend,
            req.clone(),
            vec![(b"v".to_vec(), format!("value-{i:03}").into_bytes())],
        )
        .await;
        out.push((req, view));
    }
    out
}

async fn live_count(backend: &dyn BackendImpl, requests: &[StoreStateRequest]) -> usize {
    let mut live = 0;
    for req in requests {
        if backend.resolve_store_state(req).await.unwrap().is_some() {
            live += 1;
        }
    }
    live
}

#[tokio::test]
async fn many_historical_snapshots_survive_until_row_bound() {
    let backend = test_backend().await;
    if !set_policy(&*backend, row_policy(4)) {
        return;
    }
    let database = ID::from_bytes("history-db");
    let published = publish_history(&*backend, &database, 6).await;

    // Six inserts against a four-namespace bound: the two oldest are
    // unlinked, the four newest stay live with intact values.
    let requests: Vec<_> = published.iter().map(|(req, _)| req.clone()).collect();
    assert_eq!(live_count(&*backend, &requests).await, 4);
    for (i, (req, _)) in published.iter().enumerate() {
        let resolved = backend.resolve_store_state(req).await.unwrap();
        if i < 2 {
            assert!(resolved.is_none(), "coldest entry {i} must be evicted");
        } else {
            let view = resolved.unwrap();
            assert_eq!(
                get_value(&*backend, &view).await,
                Some(format!("value-{i:03}").into_bytes()),
                "retained entry {i} must keep its value"
            );
        }
    }
}

#[tokio::test]
async fn byte_bound_trims_largest_cold_tail_first() {
    let backend = test_backend().await;
    // ~201 bytes per namespace; two live namespaces exceed 250.
    if !set_policy(
        &*backend,
        DerivedCachePolicy {
            max_namespaces: 1000,
            max_bytes: 250,
        },
    ) {
        return;
    }
    let database = ID::from_bytes("bytes-db");
    let big = vec![7u8; 200];
    let mut requests = Vec::new();
    for i in 0..3 {
        let snapshot = Snapshot::new(vec![ID::from_bytes(format!("b-tip-{i}"))]);
        let req = request(&database, &format!("b-store-{i}"), &snapshot);
        publish(&*backend, req.clone(), vec![(b"k".to_vec(), big.clone())]).await;
        requests.push(req);
    }
    // Only the newest fits the byte bound; its bytes are intact.
    assert_eq!(live_count(&*backend, &requests).await, 1);
    let view = backend
        .resolve_store_state(&requests[2])
        .await
        .unwrap()
        .expect("newest entry must survive the byte bound");
    assert_eq!(
        backend.store_state_record_get(&view, b"k").await.unwrap(),
        Some(big)
    );
}

#[tokio::test]
async fn hot_frontier_survives_cold_insertion_pressure() {
    let backend = test_backend().await;
    if !set_policy(&*backend, row_policy(3)) {
        return;
    }
    let database = ID::from_bytes("frontier-db");
    // The entry standing in for a current raw/verified frontier
    // materialization: read on every cycle, so always the hottest.
    let frontier_snapshot = Snapshot::new(vec![ID::from_bytes("frontier-tip")]);
    let frontier_req = request(&database, "frontier-store", &frontier_snapshot);
    let frontier_view = publish(
        &*backend,
        frontier_req.clone(),
        vec![(b"v".to_vec(), b"frontier".to_vec())],
    )
    .await;
    assert_eq!(
        get_value(&*backend, &frontier_view).await,
        Some(b"frontier".to_vec())
    );

    // Cold history churns underneath while the frontier is re-read each
    // cycle. Bound is 3: after four live namespaces the oldest *cold*
    // entry must go, never the hot frontier.
    for i in 0..4 {
        let snapshot = Snapshot::new(vec![ID::from_bytes(format!("cold-{i}"))]);
        let req = request(&database, &format!("cold-store-{i}"), &snapshot);
        publish(&*backend, req, vec![(b"v".to_vec(), b"cold".to_vec())]).await;
        // Current-frontier read on every cycle.
        let hot = backend
            .resolve_store_state(&frontier_req)
            .await
            .unwrap()
            .expect("hot frontier must survive eviction pressure");
        assert_eq!(get_value(&*backend, &hot).await, Some(b"frontier".to_vec()));
    }
}

#[tokio::test]
async fn multi_tip_snapshot_identity_is_canonical() {
    let backend = test_backend().await;
    if !set_policy(&*backend, row_policy(16)) {
        return;
    }
    let database = ID::from_bytes("identity-db");
    let tip_a = ID::from_bytes("tip-a");
    let tip_b = ID::from_bytes("tip-b");

    // Same tip set in different construction orders: one cache entry.
    let ordered = Snapshot::new(vec![tip_a.clone(), tip_b.clone()]);
    let reversed = Snapshot::new(vec![tip_b.clone(), tip_a.clone()]);
    assert_eq!(ordered.cache_key_bytes(), reversed.cache_key_bytes());
    let view = publish(
        &*backend,
        request(&database, "merge-store", &ordered),
        vec![(b"v".to_vec(), b"merged".to_vec())],
    )
    .await;
    assert_eq!(
        backend
            .resolve_store_state(&request(&database, "merge-store", &reversed))
            .await
            .unwrap(),
        Some(view),
        "reordered multi-tip Snapshot must resolve the same entry"
    );

    // A different tip set is a different entry; a single tip shares the
    // same namespace shape without any synthetic merge ID.
    let other = Snapshot::new(vec![tip_a.clone()]);
    assert!(
        backend
            .resolve_store_state(&request(&database, "merge-store", &other))
            .await
            .unwrap()
            .is_none(),
        "different Snapshot must miss"
    );
    let single_view = publish(
        &*backend,
        request(&database, "merge-store", &other),
        vec![(b"v".to_vec(), b"single".to_vec())],
    )
    .await;
    assert_eq!(
        get_value(&*backend, &single_view).await,
        Some(b"single".to_vec())
    );
}

#[tokio::test]
async fn reads_issue_no_write_per_hit() {
    let backend = test_backend().await;
    let database = ID::from_bytes("recency-db");
    let snapshot = Snapshot::new(vec![ID::from_bytes("recency-tip")]);
    let req = request(&database, "recency-store", &snapshot);
    let view = publish(
        &*backend,
        req.clone(),
        vec![(b"v".to_vec(), b"recency".to_vec())],
    )
    .await;

    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    let sql = backend
        .as_any()
        .downcast_ref::<eidetica::backend::database::SqlxBackend>();
    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    let flushes_before = sql.map(|sql| sql.testing_recency_flush_count());

    // Reads below the batch threshold flush nothing at all.
    for _ in 0..63 {
        let hit = backend.resolve_store_state(&req).await.unwrap().unwrap();
        assert_eq!(get_value(&*backend, &hit).await, Some(b"recency".to_vec()));
    }
    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    if let (Some(sql), Some(before)) = (sql, flushes_before) {
        assert_eq!(
            sql.testing_recency_flush_count(),
            before,
            "63 hits must flush zero times"
        );
    }

    // Past the threshold, flushes stay amortized: one batched write per 64
    // hits, never one per hit.
    for _ in 0..200 {
        let hit = backend.resolve_store_state(&req).await.unwrap().unwrap();
        assert_eq!(get_value(&*backend, &hit).await, Some(b"recency".to_vec()));
    }
    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    if let (Some(sql), Some(before)) = (sql, flushes_before) {
        let flushes = sql.testing_recency_flush_count() - before;
        assert!(
            flushes <= 5,
            "263 hits flushed {flushes} times: recency must stay batched"
        );
    }
    // The published view still reads through every path.
    assert_eq!(get_value(&*backend, &view).await, Some(b"recency".to_vec()));
}

#[tokio::test]
async fn concurrent_publishers_agree_with_eviction_active() {
    use std::sync::Arc;
    use tokio::sync::Barrier;

    let backend: Arc<dyn BackendImpl> = test_backend().await.into();
    if !set_policy(&*backend, row_policy(4)) {
        return;
    }
    let database = ID::from_bytes("race-db");
    let snapshot = Snapshot::new(vec![ID::from_bytes("race-tip")]);
    let req = request(&database, "race-store", &snapshot);

    // Eight barrier-started publishers of one target: one winner, every
    // loser adopts it, the value is intact, and the bound still holds.
    let barrier = Arc::new(Barrier::new(8));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let backend = backend.clone();
        let barrier = barrier.clone();
        let req = req.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            publish(&*backend, req, vec![(b"v".to_vec(), b"winner".to_vec())]).await
        }));
    }
    let mut views = Vec::new();
    for handle in handles {
        views.push(handle.await.unwrap());
    }
    let winner = backend.resolve_store_state(&req).await.unwrap().unwrap();
    for view in &views {
        assert_eq!(*view, winner, "every publisher must agree on one winner");
    }
    assert_eq!(
        get_value(&*backend, &winner).await,
        Some(b"winner".to_vec())
    );

    // Concurrent distinct publishes also stay within the bound.
    let barrier = Arc::new(Barrier::new(8));
    let mut handles = Vec::new();
    for i in 0..8 {
        let backend = backend.clone();
        let barrier = barrier.clone();
        let database = database.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            let snapshot = Snapshot::new(vec![ID::from_bytes(format!("distinct-{i}"))]);
            publish(
                &*backend,
                request(&database, &format!("distinct-store-{i}"), &snapshot),
                vec![(b"v".to_vec(), b"distinct".to_vec())],
            )
            .await
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }
    // One final sequential publish forces a quiescent serial eviction pass
    // that sees every concurrent commit, so the bound holds deterministically
    // (concurrent trims alone are only opportunistically serial).
    let final_snapshot = Snapshot::new(vec![ID::from_bytes("distinct-final")]);
    let final_req = request(&database, "distinct-store-final", &final_snapshot);
    publish(
        &*backend,
        final_req.clone(),
        vec![(b"v".to_vec(), b"distinct".to_vec())],
    )
    .await;
    // Ten publishes against a bound of four: at most four live, every live
    // entry value-intact, and the just-published entry always among them.
    // The race winner is colder than eight newer inserts, so correct LRU
    // may have evicted it; that is the policy working, not a regression.
    let mut probed = vec![(req.clone(), b"winner".to_vec())];
    for i in 0..8 {
        let snapshot = Snapshot::new(vec![ID::from_bytes(format!("distinct-{i}"))]);
        probed.push((
            request(&database, &format!("distinct-store-{i}"), &snapshot),
            b"distinct".to_vec(),
        ));
    }
    probed.push((final_req.clone(), b"distinct".to_vec()));
    let mut live = 0;
    for (req, expected) in &probed {
        if let Some(view) = backend.resolve_store_state(req).await.unwrap() {
            live += 1;
            assert_eq!(
                get_value(&*backend, &view).await,
                Some(expected.clone()),
                "every retained entry must keep its value"
            );
        }
    }
    assert!(
        live <= 4,
        "row bound must hold after concurrent publishes, found {live} live"
    );
    assert!(
        backend
            .resolve_store_state(&final_req)
            .await
            .unwrap()
            .is_some(),
        "just-published entry is protected from its own eviction pass"
    );
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn reopened_sqlite_keeps_warm_entries() {
    use eidetica::backend::database::Sqlite;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("warm.db");
    let database = ID::from_bytes("reopen-db");
    let snapshot = Snapshot::new(vec![ID::from_bytes("reopen-tip")]);
    let req = request(&database, "reopen-store", &snapshot);
    {
        let backend = Sqlite::open(&path).await.unwrap();
        publish(
            &backend,
            req.clone(),
            vec![(b"v".to_vec(), b"warm".to_vec())],
        )
        .await;
    }
    // Reopen the same file: the derived row must still resolve with its
    // value intact (migration preserves rows, never clears warm entries).
    let backend = Sqlite::open(&path).await.unwrap();
    let view = backend
        .resolve_store_state(&req)
        .await
        .unwrap()
        .expect("warm entry must survive reopen");
    assert_eq!(get_value(&backend, &view).await, Some(b"warm".to_vec()));
    // Bounds still enforced after reopen: shrinking to one namespace evicts
    // the reopened-cold entry in favor of the fresh publish.
    backend.testing_set_derived_cache_policy(row_policy(1));
    let snapshot2 = Snapshot::new(vec![ID::from_bytes("reopen-tip-2")]);
    let req2 = request(&database, "reopen-store-2", &snapshot2);
    publish(
        &backend,
        req2.clone(),
        vec![(b"v".to_vec(), b"fresh".to_vec())],
    )
    .await;
    assert!(
        backend.resolve_store_state(&req2).await.unwrap().is_some(),
        "fresh publish must survive"
    );
    assert!(
        backend.resolve_store_state(&req).await.unwrap().is_none(),
        "reopened-cold entry must yield to the fresh publish under a bound of 1"
    );
}

#[cfg(all(feature = "postgres", feature = "testing"))]
#[tokio::test]
async fn reopened_postgres_keeps_warm_entries() {
    use eidetica::backend::database::sql::SqlxBackend;

    if std::env::var("TEST_BACKEND").as_deref() != Ok("postgres") {
        return;
    }
    let url = std::env::var("TEST_POSTGRES_URL")
        .unwrap_or_else(|_| "postgres://localhost/eidetica_test".to_string());
    let schema = format!("reopen_{}", uuid::Uuid::new_v4().simple());
    let database = ID::from_bytes("reopen-pg-db");
    let snapshot = Snapshot::new(vec![ID::from_bytes("reopen-pg-tip")]);
    let req = request(&database, "reopen-pg-store", &snapshot);
    {
        let backend = SqlxBackend::test_connect_postgres_schema(&url, schema.clone())
            .await
            .unwrap();
        publish(
            &backend,
            req.clone(),
            vec![(b"v".to_vec(), b"warm".to_vec())],
        )
        .await;
    }
    let backend = SqlxBackend::test_connect_postgres_schema(&url, schema)
        .await
        .unwrap();
    let view = backend
        .resolve_store_state(&req)
        .await
        .unwrap()
        .expect("warm entry must survive postgres reopen");
    assert_eq!(get_value(&backend, &view).await, Some(b"warm".to_vec()));
}
