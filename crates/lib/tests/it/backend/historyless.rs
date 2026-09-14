use std::collections::BTreeMap;

#[cfg(any(feature = "sqlite", feature = "postgres"))]
use eidetica::crdt::Doc;
use eidetica::{
    Error,
    backend::{BackendError, BackendImpl, HistorylessOwner, database::InMemory},
    entry::{Entry, ID},
};

fn stores(entries: &[(&str, &[u8])]) -> BTreeMap<String, Vec<u8>> {
    entries
        .iter()
        .map(|(name, bytes)| ((*name).to_string(), bytes.to_vec()))
        .collect()
}

fn opaque_stores(
    entries: BTreeMap<String, Vec<u8>>,
) -> BTreeMap<String, eidetica::backend::HistorylessStoreMutation> {
    entries
        .into_iter()
        .map(|(name, bytes)| {
            (
                name,
                eidetica::backend::HistorylessStoreMutation {
                    projection: eidetica::backend::ProjectionDescriptor {
                        name: "eidetica/opaque".to_string(),
                        version: 1,
                    },
                    records: BTreeMap::from([(vec![0], Some(bytes))]),
                },
            )
        })
        .collect()
}

#[tokio::test]
async fn test_historyless_create_read_and_duplicate_id() {
    let backend = InMemory::new();
    let id = ID::random();
    let initial = stores(&[("_settings", br#"{"name":"local"}"#), ("data", b"v0")]);

    backend
        .create_historyless(
            &id,
            HistorylessOwner::Instance,
            opaque_stores(initial.clone()),
        )
        .await
        .unwrap();

    let snapshot = backend.read_historyless_compat(&id).await.unwrap();
    assert_eq!(snapshot.metadata.id, id);
    assert_eq!(snapshot.metadata.owner, HistorylessOwner::Instance);
    assert_eq!(snapshot.metadata.revision, 0);
    assert_eq!(snapshot.stores, initial);

    let error = backend
        .create_historyless(&id, HistorylessOwner::Instance, BTreeMap::new())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Backend(error)
            if matches!(*error, BackendError::HistorylessDatabaseAlreadyExists { .. })
    ));
}

#[tokio::test]
async fn test_historyless_id_cannot_collide_with_entry() {
    let backend = InMemory::new();
    let entry = Entry::root_builder().build().unwrap();
    let id = entry.id();
    backend.put(entry).await.unwrap();

    let error = backend
        .create_historyless(&id, HistorylessOwner::Instance, BTreeMap::new())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Backend(error)
            if matches!(*error, BackendError::HistorylessDatabaseAlreadyExists { .. })
    ));
}

#[tokio::test]
async fn test_historyless_cas_advances_once_and_stale_write_preserves_bytes() {
    let backend = InMemory::new();
    let id = ID::random();
    backend
        .create_historyless(
            &id,
            HistorylessOwner::Instance,
            opaque_stores(stores(&[("data", b"v0")])),
        )
        .await
        .unwrap();

    let revision = backend
        .replace_historyless_compat(&id, 0, stores(&[("data", b"v1"), ("other", b"one")]))
        .await
        .unwrap();
    assert_eq!(revision, 1);

    let error = backend
        .replace_historyless_compat(&id, 0, stores(&[("data", b"stale")]))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Backend(error)
            if matches!(
                *error,
                BackendError::HistorylessWriteConflict {
                    expected: 0,
                    actual: 1,
                    ..
                }
            )
    ));

    let snapshot = backend.read_historyless_compat(&id).await.unwrap();
    assert_eq!(snapshot.metadata.revision, 1);
    assert_eq!(
        snapshot.stores,
        stores(&[("data", b"v1"), ("other", b"one")])
    );
}

#[tokio::test]
async fn test_historyless_state_isolated_from_cache_and_history_enumeration() {
    let backend = InMemory::new();
    let id = ID::random();
    let initial = stores(&[("data", b"authoritative")]);
    backend
        .create_historyless(
            &id,
            HistorylessOwner::Instance,
            opaque_stores(initial.clone()),
        )
        .await
        .unwrap();

    backend.clear_derived_store_state().await.unwrap();

    let snapshot = backend.read_historyless_compat(&id).await.unwrap();
    assert_eq!(snapshot.stores, initial);
    assert!(!backend.all_roots().await.unwrap().contains(&id));
    assert!(backend.get_tree(&id).await.unwrap().is_empty());
    assert!(backend.get(&id).await.is_err());
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn test_historyless_sql_migrates_v0_without_changing_historical_rows() {
    use eidetica::backend::database::Sqlite;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("migration.db");
    let backend = Sqlite::open(&path).await.unwrap();
    let entry = Entry::root_builder().build().unwrap();
    let entry_id = entry.id();
    backend.put(entry).await.unwrap();
    backend.testing_prepare_historyless_v0().await.unwrap();
    drop(backend);

    let migrated = Sqlite::open(&path).await.unwrap();
    assert_eq!(migrated.get(&entry_id).await.unwrap().id(), entry_id);
    let state = migrated
        .testing_historyless_state(&ID::random(), "missing")
        .await
        .unwrap();
    assert_eq!(
        state.schema_version,
        eidetica::backend::database::sql::schema::SCHEMA_VERSION
    );
    assert_eq!(state.legacy_store_count, 0);
    assert_eq!(state.database_count, 0);
    assert_eq!(state.revision, None);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn test_historyless_sql_v1_imports_legacy_rows_and_preserves_table() {
    use eidetica::backend::database::Sqlite;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("migration-v1.db");
    let backend = Sqlite::open(&path).await.unwrap();
    let id = ID::random();
    let state = serde_json::to_vec(&Doc::new()).unwrap();
    backend
        .testing_seed_historyless_v1(&id, "docs", &state, 4)
        .await
        .unwrap();
    drop(backend);

    let migrated = Sqlite::open(&path).await.unwrap();
    let imported = migrated
        .testing_historyless_state(&id, "docs")
        .await
        .unwrap();
    assert_eq!(imported.schema_version, 2);
    assert_eq!(imported.legacy_store_count, 1);
    assert_eq!(imported.database_count, 1);
    assert_eq!(imported.revision, Some(4));
    assert_eq!(
        imported.projection_name.as_deref(),
        Some("eidetica/legacy-historyless-whole-state")
    );
    assert_eq!(imported.projection_version, Some(1));
    assert_eq!(imported.record_value, Some(state));
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn test_historyless_sqlite_backend_failure_rolls_back_every_store() {
    use eidetica::backend::database::Sqlite;

    let backend = Sqlite::in_memory().await.unwrap();
    let id = ID::random();
    let initial = stores(&[("first", b"old-first"), ("second", b"old-second")]);
    backend
        .create_historyless(
            &id,
            HistorylessOwner::Instance,
            opaque_stores(initial.clone()),
        )
        .await
        .unwrap();
    backend.testing_fail_historyless_commit_after_store(Some(1));

    let error = backend
        .replace_historyless_compat(
            &id,
            0,
            stores(&[("first", b"new-first"), ("second", b"new-second")]),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Backend(error) if matches!(*error, BackendError::HistorylessCommitFaultInjected)
    ));

    let after = backend.read_historyless_compat(&id).await.unwrap();
    assert_eq!(after.metadata.revision, 0);
    assert_eq!(after.stores, initial);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn test_historyless_sqlite_corrupt_and_overflow_revisions_preserve_state() {
    use eidetica::backend::database::Sqlite;

    let backend = Sqlite::in_memory().await.unwrap();
    let id = ID::random();
    let initial = stores(&[("data", b"old")]);
    backend
        .create_historyless(
            &id,
            HistorylessOwner::Instance,
            opaque_stores(initial.clone()),
        )
        .await
        .unwrap();
    backend
        .testing_set_sqlite_historyless_revision(&id, -1)
        .await
        .unwrap();
    let error = backend.read_historyless_compat(&id).await.unwrap_err();
    assert!(matches!(error, Error::Backend(error) if error.is_integrity_error()));
    let error = backend
        .replace_historyless_compat(&id, 0, stores(&[("data", b"new")]))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Backend(error) if error.is_integrity_error()));
    let raw = backend
        .testing_historyless_state(&id, "data")
        .await
        .unwrap();
    assert_eq!(
        (raw.revision, raw.record_value),
        (Some(-1), Some(b"old".to_vec()))
    );

    backend
        .testing_set_sqlite_historyless_revision(&id, i64::MAX)
        .await
        .unwrap();
    let error = backend
        .replace_historyless_compat(&id, i64::MAX as u64, stores(&[("data", b"new")]))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Backend(error) if error.is_integrity_error()));
    let raw = backend
        .testing_historyless_state(&id, "data")
        .await
        .unwrap();
    assert_eq!(
        (raw.revision, raw.record_value),
        (Some(i64::MAX), Some(b"old".to_vec()))
    );
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn test_historyless_postgres_backend_failure_rolls_back_every_store() {
    use eidetica::backend::database::Postgres;

    if std::env::var("TEST_BACKEND").as_deref() != Ok("postgres") {
        return;
    }
    let url = std::env::var("TEST_POSTGRES_URL").unwrap();
    let backend = Postgres::connect_isolated(&url).await.unwrap();
    let id = ID::random();
    let initial = stores(&[("first", b"old-first"), ("second", b"old-second")]);
    backend
        .create_historyless(
            &id,
            HistorylessOwner::Instance,
            opaque_stores(initial.clone()),
        )
        .await
        .unwrap();
    backend.testing_fail_historyless_commit_after_store(Some(1));

    let error = backend
        .replace_historyless_compat(
            &id,
            0,
            stores(&[("first", b"new-first"), ("second", b"new-second")]),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        Error::Backend(error) if matches!(*error, BackendError::HistorylessCommitFaultInjected)
    ));

    let after = backend.read_historyless_compat(&id).await.unwrap();
    assert_eq!(after.metadata.revision, 0);
    assert_eq!(after.stores, initial);
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn test_historyless_postgres_same_id_admission_is_serialized() {
    use eidetica::backend::database::Postgres;

    if std::env::var("TEST_BACKEND").as_deref() != Ok("postgres") {
        return;
    }
    let url = std::env::var("TEST_POSTGRES_URL").unwrap();
    let backend = std::sync::Arc::new(Postgres::connect_isolated(&url).await.unwrap());
    let entry = Entry::root_builder().build().unwrap();
    let id = entry.id();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let create_backend = backend.clone();
    let create_barrier = barrier.clone();
    let create_id = id.clone();
    let create = tokio::spawn(async move {
        create_barrier.wait().await;
        create_backend
            .create_historyless(&create_id, HistorylessOwner::Instance, BTreeMap::new())
            .await
    });
    let put_backend = backend.clone();
    let put = tokio::spawn(async move {
        barrier.wait().await;
        put_backend.put(entry).await
    });
    let (create, put) = tokio::join!(create, put);
    let create = create.unwrap();
    let put = put.unwrap();
    assert!(
        create.is_ok() ^ put.is_ok(),
        "one namespace admission must win"
    );
    assert!(
        create.is_ok()
            || matches!(create, Err(Error::Backend(error)) if matches!(*error, BackendError::HistorylessDatabaseAlreadyExists { .. })),
    );
    assert!(
        put.is_ok()
            || matches!(put, Err(Error::Backend(error)) if matches!(*error, BackendError::HistorylessDatabaseAlreadyExists { .. })),
    );
    assert_ne!(
        backend.get(&id).await.is_ok(),
        backend.read_historyless_compat(&id).await.is_ok(),
        "the ID must occupy exactly one namespace"
    );
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn test_historyless_postgres_migrates_v0_without_changing_historical_rows() {
    use eidetica::backend::database::Postgres;

    if std::env::var("TEST_BACKEND").as_deref() != Ok("postgres") {
        return;
    }
    let url = std::env::var("TEST_POSTGRES_URL").unwrap();
    let backend = Postgres::connect_isolated(&url).await.unwrap();
    let entry = Entry::root_builder().build().unwrap();
    let entry_id = entry.id();
    backend.put(entry).await.unwrap();
    backend.testing_prepare_historyless_v0().await.unwrap();

    backend.testing_initialize_schema().await.unwrap();
    assert_eq!(backend.get(&entry_id).await.unwrap().id(), entry_id);
    let state = backend
        .testing_historyless_state(&ID::random(), "missing")
        .await
        .unwrap();
    assert_eq!(
        state.schema_version,
        eidetica::backend::database::sql::schema::SCHEMA_VERSION
    );
    assert_eq!(state.legacy_store_count, 0);
    assert_eq!(state.database_count, 0);
    assert_eq!(state.revision, None);
}

#[cfg(feature = "postgres")]
#[tokio::test]
async fn test_historyless_postgres_v1_imports_legacy_rows_and_preserves_table() {
    use eidetica::backend::database::Postgres;

    if std::env::var("TEST_BACKEND").as_deref() != Ok("postgres") {
        return;
    }
    let url = std::env::var("TEST_POSTGRES_URL").unwrap();
    let backend = Postgres::connect_isolated(&url).await.unwrap();
    let id = ID::random();
    let state = serde_json::to_vec(&Doc::new()).unwrap();
    backend
        .testing_seed_historyless_v1(&id, "docs", &state, 4)
        .await
        .unwrap();

    backend.testing_initialize_schema().await.unwrap();
    let imported = backend
        .testing_historyless_state(&id, "docs")
        .await
        .unwrap();
    assert_eq!(imported.schema_version, 2);
    assert_eq!(imported.legacy_store_count, 1);
    assert_eq!(imported.database_count, 1);
    assert_eq!(imported.revision, Some(4));
    assert_eq!(
        imported.projection_name.as_deref(),
        Some("eidetica/legacy-historyless-whole-state")
    );
    assert_eq!(imported.projection_version, Some(1));
    assert_eq!(imported.record_value, Some(state));
}
