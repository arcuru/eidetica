use std::collections::BTreeMap;
use std::sync::Arc;

use eidetica::{
    backend::{
        BackendImpl, CacheScope, ProjectionDescriptor, RecordRange, StoreStateLifecycle,
        StoreStateRequest,
    },
    entry::ID,
};
use tokio::sync::Barrier;

use crate::helpers::test_backend;

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn existing_schema_v0_database_gets_store_state_tables() {
    use eidetica::backend::database::Sqlite;

    sqlx::any::install_default_drivers();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("schema-v0.db");
    let url = format!("sqlite:{}?mode=rwc", path.display());
    let pool = sqlx::AnyPool::connect(&url).await.unwrap();
    sqlx::query("CREATE TABLE schema_version (version BIGINT PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO schema_version (version) VALUES (0)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let backend = Sqlite::open(&path).await.unwrap();
    let request = request("db", "store", StoreStateLifecycle::Derived);
    let view = publish(
        &backend,
        request.clone(),
        [(b"key".to_vec(), b"value".to_vec())],
    )
    .await;
    assert_eq!(
        backend.store_state_record_get(&view, b"key").await.unwrap(),
        Some(b"value".to_vec())
    );
    assert_eq!(
        backend.resolve_store_state(&request).await.unwrap(),
        Some(view)
    );
}

fn request(database: &str, store: &str, lifecycle: StoreStateLifecycle) -> StoreStateRequest {
    StoreStateRequest {
        database: ID::from_bytes(database),
        store: store.to_string(),
        lifecycle,
        scope: CacheScope::Shared,
        projection: ProjectionDescriptor {
            name: "test/opaque".to_string(),
            version: 0,
        },
        source_key: b"snapshot".to_vec(),
    }
}

async fn publish(
    backend: &dyn BackendImpl,
    request: StoreStateRequest,
    records: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>,
) -> eidetica::backend::RecordView {
    let token = backend.begin_store_state_staging(request).await.unwrap();
    backend
        .stage_store_state_records(
            &token,
            records
                .into_iter()
                .map(|(key, value)| (key, Some(value)))
                .collect(),
        )
        .await
        .unwrap();
    backend.publish_store_state(token).await.unwrap()
}

#[tokio::test]
async fn point_lookup_namespace_separation_and_staging_invisibility() {
    let backend = test_backend().await;
    let derived = request("db", "store", StoreStateLifecycle::Derived);
    let token = backend
        .begin_store_state_staging(derived.clone())
        .await
        .unwrap();
    backend
        .stage_store_state_records(
            &token,
            BTreeMap::from([(b"key".to_vec(), Some(b"derived".to_vec()))]),
        )
        .await
        .unwrap();
    assert!(
        backend
            .resolve_store_state(&derived)
            .await
            .unwrap()
            .is_none()
    );

    let derived_view = backend.publish_store_state(token).await.unwrap();
    assert_eq!(
        backend
            .store_state_record_get(&derived_view, b"key")
            .await
            .unwrap(),
        Some(b"derived".to_vec())
    );

    let authoritative = request("db", "store", StoreStateLifecycle::Authoritative);
    let authoritative_view = publish(
        backend.as_ref(),
        authoritative.clone(),
        [(b"key".to_vec(), b"authority".to_vec())],
    )
    .await;
    assert_ne!(derived_view, authoritative_view);
    assert_eq!(
        backend
            .store_state_record_get(&authoritative_view, b"key")
            .await
            .unwrap(),
        Some(b"authority".to_vec())
    );
    assert!(
        backend
            .resolve_store_state(&derived)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        backend
            .resolve_store_state(&authoritative)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn ordered_half_open_scan_pages_without_duplicates_or_skips() {
    let backend = test_backend().await;
    let view = publish(
        backend.as_ref(),
        request("scan", "binary", StoreStateLifecycle::Derived),
        [
            (vec![0x00], vec![0]),
            (vec![0x00, 0xff], vec![1]),
            (vec![0x01], vec![2]),
            (vec![0x01, 0x00], vec![3]),
            (vec![0xff], vec![4]),
        ],
    )
    .await;

    let range = RecordRange {
        start: Some(vec![0x00]),
        end: Some(vec![0xff]),
    };
    let first = backend
        .store_state_record_scan(&view, &range, None, 2)
        .await
        .unwrap();
    assert_eq!(
        first.records.iter().map(|r| &r.0).collect::<Vec<_>>(),
        vec![&vec![0x00], &vec![0x00, 0xff]]
    );
    let second = backend
        .store_state_record_scan(&view, &range, first.next.as_deref(), 2)
        .await
        .unwrap();
    assert_eq!(
        second.records.iter().map(|r| &r.0).collect::<Vec<_>>(),
        vec![&vec![0x01], &vec![0x01, 0x00]]
    );
    assert!(second.next.is_none());
}

#[tokio::test]
async fn failed_publish_is_invisible_and_ready_derived_is_immutable() {
    let backend = test_backend().await;
    let request = request("failure", "store", StoreStateLifecycle::Derived);
    let token = backend
        .begin_store_state_staging(request.clone())
        .await
        .unwrap();
    backend
        .stage_store_state_records(&token, BTreeMap::from([(b"bad".to_vec(), None)]))
        .await
        .unwrap();
    assert!(backend.publish_store_state(token).await.is_err());
    assert!(
        backend
            .resolve_store_state(&request)
            .await
            .unwrap()
            .is_none()
    );

    let token = backend
        .begin_store_state_staging(request.clone())
        .await
        .unwrap();
    backend
        .stage_store_state_records(
            &token,
            BTreeMap::from([(b"key".to_vec(), Some(b"value".to_vec()))]),
        )
        .await
        .unwrap();
    let published = backend.publish_store_state(token.clone()).await.unwrap();
    assert!(
        backend
            .stage_store_state_records(
                &token,
                BTreeMap::from([(b"key".to_vec(), Some(b"changed".to_vec()))]),
            )
            .await
            .is_err()
    );
    assert_eq!(
        backend
            .store_state_record_get(&published, b"key")
            .await
            .unwrap(),
        Some(b"value".to_vec())
    );
}

#[tokio::test]
async fn clearing_derived_records_preserves_authoritative_bytes() {
    let backend = test_backend().await;
    let authoritative_request = request("clear", "store", StoreStateLifecycle::Authoritative);
    let authoritative = publish(
        backend.as_ref(),
        authoritative_request.clone(),
        [(b"key".to_vec(), b"authority".to_vec())],
    )
    .await;
    publish(
        backend.as_ref(),
        request("clear", "store", StoreStateLifecycle::Derived),
        [(b"key".to_vec(), b"derived".to_vec())],
    )
    .await;

    backend.clear_derived_store_state().await.unwrap();

    assert_eq!(
        backend
            .store_state_record_get(&authoritative, b"key")
            .await
            .unwrap(),
        Some(b"authority".to_vec())
    );
    assert!(
        backend
            .resolve_store_state(&authoritative_request)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        backend
            .resolve_store_state(&request("clear", "store", StoreStateLifecycle::Derived))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn zero_limit_scan_yields_an_empty_page() {
    let backend = test_backend().await;
    let view = publish(
        backend.as_ref(),
        request("zero", "limit", StoreStateLifecycle::Derived),
        [(b"key".to_vec(), b"value".to_vec())],
    )
    .await;

    let page = backend
        .store_state_record_scan(&view, &RecordRange::default(), None, 0)
        .await
        .unwrap();
    assert!(page.records.is_empty());
    assert!(page.next.is_none());
}

/// Two materializers can derive the same state from the same source at once.
/// The loser adopts the winner's namespace instead of failing, so a cold-read
/// race stays invisible to callers.
#[tokio::test]
async fn racing_publishers_of_one_target_agree_on_a_single_winner() {
    let backend = test_backend().await;
    let request = request("race", "store", StoreStateLifecycle::Derived);

    let first = backend
        .begin_store_state_staging(request.clone())
        .await
        .unwrap();
    let second = backend
        .begin_store_state_staging(request.clone())
        .await
        .unwrap();
    for token in [&first, &second] {
        backend
            .stage_store_state_records(
                token,
                BTreeMap::from([(b"key".to_vec(), Some(b"value".to_vec()))]),
            )
            .await
            .unwrap();
    }

    let winner = backend.publish_store_state(first).await.unwrap();
    let loser = backend.publish_store_state(second).await.unwrap();
    assert_eq!(winner, loser);
    assert_eq!(
        backend.resolve_store_state(&request).await.unwrap(),
        Some(winner.clone())
    );
    assert_eq!(
        backend
            .store_state_record_get(&winner, b"key")
            .await
            .unwrap(),
        Some(b"value".to_vec())
    );
}

/// Publishing the same (cloned) staging token twice must be idempotent: the
/// second publish returns the same view and the published state stays intact.
/// A repeat publish that deletes the ready namespace turns a retry into data
/// loss, so this asserts the state survives, not just the return value.
#[tokio::test]
async fn repeat_publish_of_a_cloned_token_keeps_published_state() {
    let backend = test_backend().await;
    let request = request("repeat", "store", StoreStateLifecycle::Derived);
    let token = backend
        .begin_store_state_staging(request.clone())
        .await
        .unwrap();
    backend
        .stage_store_state_records(
            &token,
            BTreeMap::from([(b"key".to_vec(), Some(b"value".to_vec()))]),
        )
        .await
        .unwrap();

    let first = backend.publish_store_state(token.clone()).await.unwrap();
    let second = backend.publish_store_state(token).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(
        backend.resolve_store_state(&request).await.unwrap(),
        Some(first.clone())
    );
    assert_eq!(
        backend
            .store_state_record_get(&first, b"key")
            .await
            .unwrap(),
        Some(b"value".to_vec())
    );
}

/// A view whose namespace was cleared (or was never ready) is an explicit
/// error — never a missing key and never an empty page. A missing key on a
/// live view still reads as absent, and a published-but-empty snapshot still
/// scans as an empty page, so callers can tell the three cases apart.
#[tokio::test]
async fn reclaimed_view_errors_instead_of_reading_missing_or_empty() {
    let backend = test_backend().await;
    let derived_request = request("cleared", "store", StoreStateLifecycle::Derived);
    let view = publish(
        backend.as_ref(),
        derived_request.clone(),
        [(b"key".to_vec(), b"value".to_vec())],
    )
    .await;
    assert_eq!(
        backend
            .store_state_record_get(&view, b"absent")
            .await
            .unwrap(),
        None
    );

    backend.clear_derived_store_state().await.unwrap();
    assert_eq!(
        backend.store_state_record_get(&view, b"key").await.unwrap(),
        Some(b"value".to_vec()),
        "an unlinked generation remains readable to an existing view"
    );
    backend.clear_derived_store_state().await.unwrap();
    assert!(
        backend
            .resolve_store_state(&derived_request)
            .await
            .unwrap()
            .is_none()
    );

    let get_err = backend
        .store_state_record_get(&view, b"key")
        .await
        .unwrap_err();
    assert!(
        get_err.is_invalid_store_state_view(),
        "cleared view must error, got: {get_err}"
    );
    let scan_err = backend
        .store_state_record_scan(&view, &RecordRange::default(), None, 16)
        .await
        .unwrap_err();
    assert!(
        scan_err.is_invalid_store_state_view(),
        "cleared view must error, got: {scan_err}"
    );
}

/// A namespace can be published with no records at all. That valid-but-empty
/// snapshot scans as an empty page — the Ok case the cleared-view test above
/// contrasts against.
#[tokio::test]
async fn valid_empty_snapshot_scans_as_an_empty_page() {
    let backend = test_backend().await;
    let view = publish(
        backend.as_ref(),
        request("empty", "store", StoreStateLifecycle::Derived),
        std::iter::empty::<(Vec<u8>, Vec<u8>)>(),
    )
    .await;

    let page = backend
        .store_state_record_scan(&view, &RecordRange::default(), None, 16)
        .await
        .unwrap();
    assert!(page.records.is_empty());
    assert!(page.next.is_none());
}

/// Eight materializers race to publish one target from a barrier start so the
/// publishes genuinely overlap. Every publisher must agree on a single winner
/// whose records stay intact — no torn namespaces, no duplicate ready state.
/// This is a regression guard on the serialization outcome, not a proof of the
/// locking: it passes by construction on any correct serialization and fails
/// on torn or duplicated state however the tasks interleave.
#[tokio::test]
async fn barrier_started_publishers_agree_on_a_single_winner() {
    const PUBLISHERS: usize = 8;
    let backend: Arc<dyn BackendImpl> = test_backend().await.into();
    let request = request("barrier", "store", StoreStateLifecycle::Derived);

    let mut tokens = Vec::with_capacity(PUBLISHERS);
    for _ in 0..PUBLISHERS {
        let token = backend
            .begin_store_state_staging(request.clone())
            .await
            .unwrap();
        backend
            .stage_store_state_records(
                &token,
                BTreeMap::from([(b"key".to_vec(), Some(b"value".to_vec()))]),
            )
            .await
            .unwrap();
        tokens.push(token);
    }

    let barrier = Arc::new(Barrier::new(PUBLISHERS));
    let mut tasks = tokio::task::JoinSet::new();
    for token in tokens {
        let backend = Arc::clone(&backend);
        let barrier = Arc::clone(&barrier);
        tasks.spawn(async move {
            barrier.wait().await;
            backend.publish_store_state(token).await
        });
    }

    let mut views = Vec::with_capacity(PUBLISHERS);
    while let Some(outcome) = tasks.join_next().await {
        views.push(outcome.unwrap().unwrap());
    }
    for view in &views {
        assert_eq!(view, &views[0]);
    }
    assert_eq!(
        backend.resolve_store_state(&request).await.unwrap(),
        Some(views[0].clone())
    );
    assert_eq!(
        backend
            .store_state_record_get(&views[0], b"key")
            .await
            .unwrap(),
        Some(b"value".to_vec())
    );
}

/// An invalid view stays invalid with `limit` 0.
///
/// The `limit == 0` fast path must not run before validation: a cleared view
/// reports `InvalidStoreStateView` here on every engine, exactly like a
/// nonzero-limit read does. (The valid empty-snapshot case above is the Ok
/// counterpart.)
#[tokio::test]
async fn invalid_view_rejects_zero_limit_scan() {
    let backend = test_backend().await;
    let derived = request("db", "zero-limit-invalid", StoreStateLifecycle::Derived);
    let view = publish(
        backend.as_ref(),
        derived.clone(),
        [(b"key".to_vec(), b"value".to_vec())],
    )
    .await;

    backend.clear_derived_store_state().await.unwrap();
    backend.clear_derived_store_state().await.unwrap();

    let err = backend
        .store_state_record_scan(&view, &RecordRange::default(), None, 0)
        .await
        .unwrap_err();
    assert!(
        err.is_invalid_store_state_view(),
        "cleared view with limit 0 must error, got: {err}"
    );
}

/// Same-token stage-vs-publish must serialize on PostgreSQL.
///
/// `StagingToken` is `Clone`, so one task can sit inside `stage` — token
/// validated, advisory locks held, writes still pending — while another task
/// publishes a clone of the same token. The stage/publish advisory lock must
/// force the publish to wait for the stage's transaction: the publish then
/// sees the staged tombstone and fails closed. Without the lock the publish
/// would flip the namespace ready first and the stage's writes (including an
/// unvalidated delete) would land in the published snapshot afterwards.
///
/// The interleaving is driven by a test-only pause gate (no sleeps): the
/// stage signals after validation and blocks until released, so the competing
/// publish deterministically runs mid-stage. This pg-only shape is
/// intentional — SQLite serializes writers with `BEGIN IMMEDIATE` and
/// `InMemory` holds one mutex per operation, so neither engine can interleave
/// a stage with a publish mid-call.
#[tokio::test]
#[cfg(feature = "postgres")]
async fn same_token_stage_vs_publish_is_serialized() {
    use std::time::Duration;

    if std::env::var("TEST_BACKEND").as_deref() != Ok("postgres") {
        return;
    }
    let url = std::env::var("TEST_POSTGRES_URL")
        .unwrap_or_else(|_| "postgres://localhost/eidetica_test".to_string());
    let backend = std::sync::Arc::new(
        eidetica::backend::database::Postgres::connect_isolated(&url)
            .await
            .expect("connect isolated postgres"),
    );
    assert!(backend.is_postgres());

    let request = request("db", "race-same-token", StoreStateLifecycle::Derived);
    let token = backend
        .begin_store_state_staging(request.clone())
        .await
        .unwrap();

    // Park the stage after token validation, before any writes.
    let gate = eidetica::backend::database::SqlxBackend::testing_register_stage_pause(
        token.testing_namespace_id(),
    )
    .await;
    let stage_backend = std::sync::Arc::clone(&backend);
    let stage_token = token.clone();
    let stage = tokio::spawn(async move {
        stage_backend
            .stage_store_state_records(
                &stage_token,
                BTreeMap::from([
                    (b"extra".to_vec(), Some(b"extra-value".to_vec())),
                    (b"doomed".to_vec(), None),
                ]),
            )
            .await
    });
    gate.wait_validated().await;

    // The competing publish runs while the stage is parked mid-transaction.
    // With the namespace lock it blocks until the stage commits, then
    // validates the staged tombstone and fails instead of publishing it.
    let publish_backend = std::sync::Arc::clone(&backend);
    let publish_token = token.clone();
    let publish_task =
        tokio::spawn(async move { publish_backend.publish_store_state(publish_token).await });
    gate.release();

    tokio::time::timeout(Duration::from_secs(30), stage)
        .await
        .expect("stage task joined")
        .expect("stage task completed")
        .expect("stage of tombstone-bearing chunk succeeds");
    let publish_result = tokio::time::timeout(Duration::from_secs(30), publish_task)
        .await
        .expect("publish task joined")
        .expect("publish task completed");
    assert!(
        publish_result.is_err(),
        "publish of a staging namespace holding an unvalidated delete must fail closed, got: {publish_result:?}"
    );
    assert_eq!(
        backend.resolve_store_state(&request).await.unwrap(),
        None,
        "failed publish must leave no ready snapshot behind"
    );

    // Hygiene: the failed publish discarded the staging namespace, so the
    // same target stages and publishes cleanly afterwards.
    let retry = publish(
        backend.as_ref(),
        request.clone(),
        [(b"ok".to_vec(), b"value".to_vec())],
    )
    .await;
    assert_eq!(
        backend.store_state_record_get(&retry, b"ok").await.unwrap(),
        Some(b"value".to_vec())
    );
}

#[tokio::test]
async fn clearing_derived_records_preserves_an_active_reader_and_rebuilds() {
    let backend = test_backend().await;
    let authoritative_request = request("pinned", "store", StoreStateLifecycle::Authoritative);
    let authoritative = publish(
        backend.as_ref(),
        authoritative_request.clone(),
        [(b"key".to_vec(), b"authority".to_vec())],
    )
    .await;
    let derived_request = request("pinned", "store", StoreStateLifecycle::Derived);
    let reader = publish(
        backend.as_ref(),
        derived_request.clone(),
        [
            (b"a".to_vec(), b"first".to_vec()),
            (b"b".to_vec(), b"second".to_vec()),
        ],
    )
    .await;

    backend.clear_derived_store_state().await.unwrap();

    // The reader resolved its view before the clear, so both its point and
    // page reads keep serving the generation it is walking.
    assert_eq!(
        backend.store_state_record_get(&reader, b"a").await.unwrap(),
        Some(b"first".to_vec())
    );
    assert_eq!(
        backend
            .store_state_record_scan(&reader, &RecordRange::default(), None, 10)
            .await
            .unwrap()
            .records,
        vec![
            (b"a".to_vec(), b"first".to_vec()),
            (b"b".to_vec(), b"second".to_vec()),
        ]
    );
    // A new lookup misses and rebuilds instead of joining the cleared one.
    assert!(
        backend
            .resolve_store_state(&derived_request)
            .await
            .unwrap()
            .is_none()
    );
    let rebuilt = publish(
        backend.as_ref(),
        derived_request.clone(),
        [(b"a".to_vec(), b"rebuilt".to_vec())],
    )
    .await;
    assert_ne!(rebuilt, reader);
    assert_eq!(
        backend
            .store_state_record_get(&rebuilt, b"a")
            .await
            .unwrap(),
        Some(b"rebuilt".to_vec())
    );
    assert_eq!(
        backend.store_state_record_get(&reader, b"a").await.unwrap(),
        Some(b"first".to_vec())
    );

    // Clearing cannot reach authoritative state in either generation.
    assert_eq!(
        backend
            .store_state_record_get(&authoritative, b"key")
            .await
            .unwrap(),
        Some(b"authority".to_vec())
    );
    assert!(
        backend
            .resolve_store_state(&authoritative_request)
            .await
            .unwrap()
            .is_some()
    );

    // The following clear reclaims the unlinked generation.
    backend.clear_derived_store_state().await.unwrap();
    assert_ne!(
        backend
            .store_state_record_get(&reader, b"a")
            .await
            .ok()
            .flatten(),
        Some(b"first".to_vec()),
        "the unlinked generation must not survive a second clear"
    );
    assert_eq!(
        backend
            .store_state_record_get(&authoritative, b"key")
            .await
            .unwrap(),
        Some(b"authority".to_vec())
    );
}
