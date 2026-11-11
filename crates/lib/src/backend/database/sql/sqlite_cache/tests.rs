use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;
use crate::backend::database::sql::{Sqlite, StagingToken};
use crate::backend::{
    BackendImpl, CacheScope, DerivedCachePolicy, ProjectionDescriptor, RecordRange, RecordView,
    StoreStateLifecycle, StoreStateRequest, VerificationStatus,
};
use crate::entry::{Entry, ID};
use sqlx::AnyPool;

struct Files {
    _dir: tempfile::TempDir,
    history: PathBuf,
    cache: PathBuf,
}

impl Files {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self {
            history: dir.path().join("history.db"),
            cache: dir.path().join("history.db.cache"),
            _dir: dir,
        }
    }

    async fn open(&self) -> SqlxBackend {
        Sqlite::open(&self.history).await.unwrap()
    }

    fn copy_from(&self, other: &Self, with_cache: bool) {
        // Callers have awaited both pool closes. Never copy only a live .db.
        std::fs::copy(&other.history, &self.history).unwrap();
        if with_cache {
            std::fs::copy(&other.cache, &self.cache).unwrap();
        }
    }
}

async fn close(mut backend: SqlxBackend) {
    backend.derived_pool.take().unwrap().close().await;
    backend.pool.take().unwrap().close().await;
}

async fn raw(path: &Path) -> AnyPool {
    sqlx::any::install_default_drivers();
    sqlx::any::AnyPoolOptions::new()
        .max_connections(1)
        .connect(&super::super::sqlite_path_url(path).unwrap())
        .await
        .unwrap()
}

fn request(store: &str, lifecycle: StoreStateLifecycle) -> StoreStateRequest {
    StoreStateRequest {
        database: ID::from_bytes("sqlite-split-db"),
        store: store.into(),
        lifecycle,
        scope: CacheScope::Shared,
        projection: ProjectionDescriptor {
            name: "test/split".into(),
            version: 1,
        },
        source_key: crate::Snapshot::from([ID::from_bytes("split-snapshot")]).cache_key_bytes(),
    }
}

async fn stage(backend: &SqlxBackend, target: StoreStateRequest) -> StagingToken {
    let token = backend.begin_store_state_staging(target).await.unwrap();
    backend
        .stage_store_state_records(&token, [(b"key".to_vec(), Some(b"value".to_vec()))].into())
        .await
        .unwrap();
    token
}

async fn publish(backend: &SqlxBackend, target: StoreStateRequest) -> RecordView {
    backend
        .publish_store_state(stage(backend, target).await)
        .await
        .unwrap()
}

async fn count(pool: &AnyPool, query: &'static str) -> i64 {
    let (n,): (i64,) = sqlx::query_as(query).fetch_one(pool).await.unwrap();
    n
}

async fn authoritative_image(pool: &AnyPool) -> Vec<Vec<(String,)>> {
    let mut image = Vec::new();
    for query in [
        "SELECT quote(id)||quote(tree_id)||quote(is_root)||quote(verification_status)||quote(height)||hex(entry_cbor) FROM entries ORDER BY id",
        "SELECT quote(child_id)||quote(parent_id) FROM tree_parents ORDER BY 1",
        "SELECT quote(tree_id)||quote(entry_id)||quote(store_name)||quote(height)||hex(data) FROM subtrees ORDER BY 1",
        "SELECT quote(child_id)||quote(parent_id)||quote(store_name) FROM store_parents ORDER BY 1",
        "SELECT quote(entry_id)||quote(tree_id)||quote(store_name) FROM tips ORDER BY 1",
        "SELECT quote(tree_id)||quote(entry_id) FROM verified_tips ORDER BY 1",
        "SELECT quote(singleton)||quote(data) FROM instance_metadata ORDER BY 1",
        "SELECT quote(singleton)||quote(data) FROM instance_secrets ORDER BY 1",
        "SELECT quote(namespace_id)||quote(database_id)||quote(store_name)||quote(lifecycle)||quote(status)||quote(scope_user_uuid)||quote(projection_name)||quote(projection_version)||hex(source_key)||quote(size_bytes)||quote(last_access_tick) FROM store_state_namespaces WHERE lifecycle = 1 ORDER BY namespace_id",
        "SELECT quote(namespace_id)||hex(record_key)||hex(record_value) FROM store_state_records ORDER BY 1",
        // Login/access timestamps legitimately change on service reads. Exact
        // credential-byte retention is checked by the upgrade scenario below.
        "SELECT quote(name) FROM local_accounts ORDER BY name",
        "SELECT quote(version) FROM schema_version",
    ] {
        image.push(sqlx::query_as(query).fetch_all(pool).await.unwrap());
    }
    image
}

#[tokio::test]
async fn physical_files_and_lifecycle_routing_preserve_authority() {
    let files = Files::new();
    let backend = files.open().await;
    assert!(files.history.is_file() && files.cache.is_file());
    for (pool, path) in [
        (backend.pool(), &files.history),
        (backend.cache_pool(), &files.cache),
    ] {
        let (_, _, opened): (i64, String, String) = sqlx::query_as("PRAGMA database_list")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(
            Path::new(&opened).canonicalize().unwrap(),
            path.canonicalize().unwrap()
        );
        assert_eq!(count(pool, "PRAGMA synchronous").await, 1);
        assert_eq!(count(pool, "PRAGMA busy_timeout").await, 5000);
        let (mode,): (String,) = sqlx::query_as("PRAGMA journal_mode")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(mode, "wal");
    }
    let derived = request("d", StoreStateLifecycle::Derived);
    let authority = request("a", StoreStateLifecycle::Authoritative);
    let dt = stage(&backend, derived.clone()).await;
    let at = stage(&backend, authority.clone()).await;
    assert!(dt.namespace_id.starts_with(NAMESPACE_PREFIX));
    assert!(!at.namespace_id.starts_with(NAMESPACE_PREFIX));
    assert_eq!(
        count(
            backend.pool(),
            "SELECT COUNT(*) FROM store_state_namespaces WHERE lifecycle = 2"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            backend.cache_pool(),
            "SELECT COUNT(*) FROM store_state_namespaces WHERE lifecycle = 2"
        )
        .await,
        1
    );
    let bad = StagingToken {
        namespace_id: dt.namespace_id.clone(),
        target: authority.clone(),
    };
    assert!(
        backend
            .stage_store_state_records(&bad, Default::default())
            .await
            .is_err()
    );
    assert!(backend.abort_store_state(bad.clone()).await.is_err());
    assert!(backend.publish_store_state(bad).await.is_err());
    let dv = backend.publish_store_state(dt).await.unwrap();
    let av = backend.publish_store_state(at).await.unwrap();
    let image = authoritative_image(backend.pool()).await;
    backend.clear_derived_store_state().await.unwrap();
    assert_eq!(
        backend.store_state_record_get(&dv, b"key").await.unwrap(),
        Some(b"value".to_vec())
    );
    backend.clear_derived_store_state().await.unwrap();
    assert!(
        backend
            .store_state_record_get(&dv, b"key")
            .await
            .unwrap_err()
            .is_invalid_store_state_view()
    );
    assert_eq!(
        backend.store_state_record_get(&av, b"key").await.unwrap(),
        Some(b"value".to_vec())
    );
    let aborted = stage(&backend, derived.clone()).await;
    backend.abort_store_state(aborted).await.unwrap();
    let adopted = publish(&backend, authority.clone()).await;
    assert_eq!(adopted, av);
    let unknown = RecordView {
        namespace_id: format!("sqlite-cache-v9:{}", uuid::Uuid::new_v4()),
    };
    assert!(
        backend
            .store_state_record_get(&unknown, b"key")
            .await
            .is_err()
    );
    assert!(
        backend
            .store_state_record_scan(&unknown, &RecordRange::default(), None, 0)
            .await
            .is_err()
    );
    assert_eq!(
        authoritative_image(backend.pool()).await,
        image,
        "C operations and adoption must not mutate H"
    );
    assert_eq!(
        count(
            backend.pool(),
            "SELECT COUNT(*) FROM store_state_namespaces WHERE lifecycle = 0"
        )
        .await,
        0
    );
    assert_eq!(count(backend.cache_pool(), "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('entries','verified_tips','instance_secrets','history_identity','schema_version')").await, 0);
    publish(
        &backend,
        request("constraint", StoreStateLifecycle::Derived),
    )
    .await;
    assert!(
        sqlx::query("UPDATE store_state_namespaces SET lifecycle = 1")
            .execute(backend.cache_pool())
            .await
            .is_err()
    );
    close(backend).await;
}

#[cfg(all(unix, feature = "service"))]
#[tokio::test]
async fn file_backed_service_rebuilds_historical_opaque_table_and_encrypted_rows() {
    use crate::service::ServiceServer;
    use crate::{
        Instance, NewAccount,
        crdt::Doc,
        store::{DocStore, PasswordStore, Table},
    };
    use serde::{Deserialize, Serialize};
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Row {
        value: u32,
    }

    let files = Files::new();
    let (server, admin) = Instance::create_backend(
        Box::new(files.open().await),
        NewAccount::passwordless("admin"),
    )
    .await
    .unwrap();
    let (root, historical) = {
        let mut user = server.login_account("admin", "").await.unwrap();
        let key = user.get_default_key().unwrap();
        let db = user.create_database(Doc::new(), &key).await.unwrap();
        let tx = db.new_transaction().await.unwrap();
        tx.get_store::<DocStore>("opaque")
            .await
            .unwrap()
            .set("key", "before")
            .await
            .unwrap();
        let table = tx.get_store::<Table<Row>>("rows").await.unwrap();
        table.set("a", Row { value: 1 }).await.unwrap();
        table.set("b", Row { value: 2 }).await.unwrap();
        let mut encrypted = tx
            .get_store::<PasswordStore<Table<Row>>>("encrypted")
            .await
            .unwrap();
        encrypted.initialize("secret", Doc::new()).await.unwrap();
        encrypted
            .inner()
            .await
            .unwrap()
            .set("private", Row { value: 7 })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let historical = db.snapshot().await.unwrap();
        let tx = db.new_transaction().await.unwrap();
        tx.get_store::<DocStore>("opaque")
            .await
            .unwrap()
            .set("key", "after")
            .await
            .unwrap();
        tx.get_store::<Table<Row>>("rows")
            .await
            .unwrap()
            .set("a", Row { value: 3 })
            .await
            .unwrap();
        tx.commit().await.unwrap();
        (db.root_id().clone(), historical)
    };
    drop(admin);
    let engine = server.backend().local_engine().unwrap();
    let sql = engine.as_any().downcast_ref::<SqlxBackend>().unwrap();
    let keep = publish(
        sql,
        request("authority", StoreStateLifecycle::Authoritative),
    )
    .await;
    let image = authoritative_image(sql.pool()).await;
    sql.cache_pool().close().await;
    sql.pool().close().await;
    drop(engine);
    drop(server);

    // Read both before and after offline C deletion, through a real socket
    // server backed by the two physical files (not the matrix's InMemory server).
    for round in 0..2 {
        let server = Instance::open_backend(Box::new(files.open().await))
            .await
            .unwrap();
        let socket = files._dir.path().join(format!("service-{round}.sock"));
        let service = ServiceServer::bind(server.clone(), socket.clone())
            .await
            .unwrap();
        let (shutdown, rx) = tokio::sync::watch::channel(());
        let job = tokio::spawn(service.run(rx));
        {
            let client = Instance::connect(format!("unix://{}", socket.display()))
                .await
                .unwrap();
            let user = client.login_account("admin", "").await.unwrap();
            let db = user.open_database(&root).await.unwrap();
            let current = db.get_store_viewer::<DocStore>("opaque").await.unwrap();
            assert_eq!(current.get_string("key").await.unwrap(), "after");
            let old = db.new_transaction_at(&historical).await.unwrap();
            assert_eq!(
                old.get_store::<DocStore>("opaque")
                    .await
                    .unwrap()
                    .get_string("key")
                    .await
                    .unwrap(),
                "before"
            );
            let old_rows = old.get_store::<Table<Row>>("rows").await.unwrap();
            assert_eq!(old_rows.get("a").await.unwrap().value, 1);
            let rows = db.get_store_viewer::<Table<Row>>("rows").await.unwrap();
            assert_eq!(rows.get("a").await.unwrap().value, 3);
            let first = rows.scan_page(None, 1).await.unwrap();
            let second = rows.scan_page(first.next.as_ref(), 1).await.unwrap();
            assert_eq!(first.rows[0].0, "a");
            assert_eq!(second.rows[0].0, "b");
            let tx = db.new_transaction().await.unwrap();
            let mut encrypted = tx
                .get_store::<PasswordStore<Table<Row>>>("encrypted")
                .await
                .unwrap();
            encrypted.open("secret").unwrap();
            assert_eq!(
                encrypted
                    .inner()
                    .await
                    .unwrap()
                    .get("private")
                    .await
                    .unwrap()
                    .value,
                7
            );
            let mut wrong = tx
                .get_store::<PasswordStore<Table<Row>>>("encrypted")
                .await
                .unwrap();
            assert!(
                wrong.open("wrong").is_err(),
                "a cache hit is not a decryption proof"
            );
            let local = server.backend().local_engine().unwrap();
            let backend = local.as_any().downcast_ref::<SqlxBackend>().unwrap();
            assert_eq!(authoritative_image(backend.pool()).await, image);
            assert_eq!(
                backend.store_state_record_get(&keep, b"key").await.unwrap(),
                Some(b"value".to_vec())
            );
            assert_eq!(
                count(
                    backend.pool(),
                    "SELECT COUNT(*) FROM store_state_namespaces WHERE lifecycle = 0"
                )
                .await,
                0
            );
            assert!(count(backend.cache_pool(), "SELECT COUNT(*) FROM store_state_namespaces WHERE store_name IN ('opaque','rows','encrypted')").await >= 5);
            assert!(count(backend.cache_pool(), "SELECT COUNT(*) FROM store_state_namespaces WHERE store_name = 'encrypted' AND scope_user_uuid <> ''").await > 0);
            // The encrypted row projection remains opaque ciphertext in C.
            let ciphertext: Vec<(Vec<u8>, Vec<u8>)> = sqlx::query_as("SELECT r.record_key, r.record_value FROM store_state_records r JOIN store_state_namespaces n USING(namespace_id) WHERE n.store_name = 'encrypted' AND n.projection_name LIKE '%table/rows'")
                .fetch_all(backend.cache_pool()).await.unwrap();
            assert!(!ciphertext.is_empty());
            assert!(
                ciphertext
                    .iter()
                    .all(|(key, value)| key.len() == 32
                        && !value.windows(7).any(|w| w == b"private"))
            );
            // Clear twice while Table handles still exist: whole pages must
            // rebuild, not return false missing rows from a reclaimed view.
            backend.clear_derived_store_state().await.unwrap();
            backend.clear_derived_store_state().await.unwrap();
            assert_eq!(rows.get("a").await.unwrap().value, 3);
            assert_eq!(old_rows.scan_page(None, 10).await.unwrap().rows.len(), 2);
        }
        shutdown.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), job)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let local = server.backend().local_engine().unwrap();
        let backend = local.as_any().downcast_ref::<SqlxBackend>().unwrap();
        backend.cache_pool().close().await;
        backend.pool().close().await;
        drop(local);
        drop(server);
        if round == 0 {
            for suffix in ["", "-wal", "-shm"] {
                let path = PathBuf::from(format!("{}{suffix}", files.cache.display()));
                if path.exists() {
                    std::fs::remove_file(path).unwrap();
                }
            }
        }
    }
}

#[tokio::test]
async fn cache_full_and_read_only_failures_do_not_mutate_history_or_publish() {
    let files = Files::new();
    let mut backend = files.open().await;
    let before = authoritative_image(backend.pool()).await;
    // One retained connection makes connection-local fault PRAGMAs deterministic.
    backend.derived_pool.take().unwrap().close().await;
    backend.derived_pool = Some(raw(&files.cache).await);
    let c = backend.cache_pool();
    let pages = count(c, "PRAGMA page_count").await;
    c.execute(sqlx::AssertSqlSafe(format!(
        "PRAGMA max_page_count = {pages}"
    )))
    .await
    .unwrap();
    let target = request("full", StoreStateLifecycle::Derived);
    let token = backend
        .begin_store_state_staging(target.clone())
        .await
        .unwrap();
    let error = backend
        .stage_store_state_records(
            &token,
            [(b"big".to_vec(), Some(vec![0x55; 1024 * 1024]))].into(),
        )
        .await
        .unwrap_err();
    assert!(!error.is_optional_store_state_unavailable(), "{error}");
    assert!(error.to_string().contains("full"), "{error}");
    assert!(
        backend
            .resolve_store_state(&target)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        count(c, "SELECT COUNT(*) FROM store_state_records").await,
        0
    );
    // SQLITE_FULL may automatically roll back a transaction. Recovery from
    // cache storage failure is close/reopen (or offline replacement), not reuse
    // of a connection whose transaction acknowledgement failed.
    close(backend).await;
    let mut backend = files.open().await;
    assert!(
        backend
            .resolve_store_state(&target)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        count(
            backend.cache_pool(),
            "SELECT COUNT(*) FROM store_state_namespaces"
        )
        .await,
        0
    );
    backend.derived_pool.take().unwrap().close().await;
    backend.derived_pool = Some(raw(&files.cache).await);
    let c = backend.cache_pool();
    let token = stage(&backend, target.clone()).await;
    c.execute("PRAGMA query_only = ON").await.unwrap();
    assert!(backend.publish_store_state(token).await.is_err());
    assert!(
        backend
            .resolve_store_state(&target)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(authoritative_image(backend.pool()).await, before);
    // H remains writable while C refuses writes.
    let entry = Entry::root_builder().build().unwrap();
    backend.put(entry.clone()).await.unwrap();
    assert_eq!(backend.get(&entry.id()).await.unwrap(), entry);
    c.execute("PRAGMA query_only = OFF").await.unwrap();
    close(backend).await;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::metadata(&files.cache).unwrap().permissions();
        std::fs::set_permissions(&files.cache, std::fs::Permissions::from_mode(0o400)).unwrap();
        let h = std::fs::read(&files.history).unwrap();
        let c = std::fs::read(&files.cache).unwrap();
        assert!(Sqlite::open(&files.history).await.is_err());
        assert_eq!(std::fs::read(&files.history).unwrap(), h);
        assert_eq!(std::fs::read(&files.cache).unwrap(), c);
        std::fs::set_permissions(&files.cache, permissions).unwrap();
        // SQLite may create read-only WAL/SHM alongside the read-only main
        // file. Offline cache replacement removes the entire C image, never
        // H's sidecars, rather than chmodding only C and leaving stale sidecars.
        for suffix in ["", "-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{suffix}", files.cache.display()));
            if path.exists() {
                std::fs::remove_file(path).unwrap();
            }
        }
    }
    close(files.open().await).await;
}

#[tokio::test]
async fn cache_path_rejects_sqlite_sidecars_before_creating_history() {
    for (history_name, cache_name) in [
        ("history.db", "history.db-wal"),
        ("history.db", "history.db-shm"),
        ("history.db", "history.db-journal"),
        ("cache.db-wal", "cache.db"),
        ("cache.db-shm", "cache.db"),
        ("cache.db-journal", "cache.db"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let history = dir.path().join(history_name);
        let cache = dir.path().join(cache_name);
        assert!(
            Sqlite::open_with_cache_path(&history, &cache, DerivedCachePolicy::persistent())
                .await
                .is_err(),
            "accepted {history_name} / {cache_name}"
        );
        assert!(
            !history.exists() && !cache.exists(),
            "path collision must refuse before SQLite touches either image"
        );
    }
}

#[tokio::test]
async fn parsed_url_cache_suffix_and_file_backed_budget_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let history = dir.path().join("history ?#%.db");
    let mut cache = history.as_os_str().to_owned();
    cache.push(".cache");
    let cache = PathBuf::from(cache);
    let url = super::super::sqlite_path_url(&history).unwrap();
    // The parsed filename, not escaped URL text or query, determines C.
    let backend = SqlxBackend::connect_sqlite(&format!("{url}&cache=private"))
        .await
        .unwrap();
    assert!(history.is_file() && cache.is_file());
    for store in ["one", "two", "three"] {
        publish(&backend, request(store, StoreStateLifecycle::Derived)).await;
    }
    let image = authoritative_image(backend.pool()).await;
    close(backend).await;
    let policy = DerivedCachePolicy {
        max_namespaces: 1,
        max_bytes: 64,
    };
    let backend = Sqlite::open_with_cache_policy(&history, policy)
        .await
        .unwrap();
    assert_eq!(
        count(
            backend.cache_pool(),
            "SELECT COUNT(*) FROM store_state_namespaces WHERE status = 1"
        )
        .await,
        1
    );
    assert_eq!(authoritative_image(backend.pool()).await, image);
    let target = request("oversized", StoreStateLifecycle::Derived);
    let token = backend
        .begin_store_state_staging(target.clone())
        .await
        .unwrap();
    backend
        .stage_store_state_records(&token, [(b"large".to_vec(), Some(vec![7; 128]))].into())
        .await
        .unwrap();
    assert!(
        backend
            .publish_store_state(token)
            .await
            .unwrap_err()
            .is_store_state_not_retained()
    );
    assert!(
        backend
            .resolve_store_state(&target)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        count(
            backend.cache_pool(),
            "SELECT COUNT(*) FROM store_state_namespaces WHERE status = 1"
        )
        .await,
        1
    );
    close(backend).await;
    let backend = Sqlite::open_with_cache_policy(
        &history,
        DerivedCachePolicy {
            max_namespaces: 0,
            max_bytes: 0,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        count(
            backend.cache_pool(),
            "SELECT COUNT(*) FROM store_state_namespaces"
        )
        .await,
        0
    );
    assert!(
        backend
            .begin_store_state_staging(target)
            .await
            .unwrap_err()
            .is_store_state_not_retained()
    );
    assert_eq!(authoritative_image(backend.pool()).await, image);
    close(backend).await;
}

#[tokio::test]
async fn snapshot_keys_survive_unrelated_writes_restart_and_history_restore() {
    let files = Files::new();
    let backup = Files::new();
    let backend = files.open().await;
    let root = Entry::root_builder()
        .set_subtree_data("data", b"before")
        .build()
        .unwrap();
    backend.put(root.clone()).await.unwrap();
    backend
        .update_verification_status(&root.id(), VerificationStatus::Verified)
        .await
        .unwrap();
    let before = backend.verified_snapshot(&root.id()).await.unwrap();
    let mut target = request("data", StoreStateLifecycle::Derived);
    target.database = root.id();
    target.source_key = before.cache_key_bytes();
    let view = publish(&backend, target.clone()).await;
    close(backend).await;
    backup.copy_from(&files, false);

    let backend = files.open().await;
    assert_eq!(
        backend.resolve_store_state(&target).await.unwrap(),
        Some(view.clone())
    );
    let child = Entry::builder(root.id())
        .add_parent(root.id())
        .set_height(1)
        .set_subtree_data("data", b"after")
        .build()
        .unwrap();
    backend.put(child.clone()).await.unwrap();
    // An unverified child does not alter the selected trusted Snapshot.
    assert_eq!(backend.verified_snapshot(&root.id()).await.unwrap(), before);
    backend
        .update_verification_status(&child.id(), VerificationStatus::Verified)
        .await
        .unwrap();
    let after = backend.verified_snapshot(&root.id()).await.unwrap();
    assert_ne!(before, after);
    let mut newer = target.clone();
    newer.source_key = after.cache_key_bytes();
    assert!(backend.resolve_store_state(&newer).await.unwrap().is_none());
    let new_view = publish(&backend, newer.clone()).await;
    assert_ne!(view, new_view);
    backend
        .put(
            Entry::root_builder()
                .set_subtree_data("unrelated", b"write")
                .build()
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        backend.resolve_store_state(&target).await.unwrap(),
        Some(view.clone())
    );
    close(backend).await;
    // Restore only authoritative history; keep both immutable Snapshot caches.
    std::fs::copy(&backup.history, &files.history).unwrap();
    let backend = files.open().await;
    assert_eq!(backend.verified_snapshot(&root.id()).await.unwrap(), before);
    assert_eq!(
        backend.resolve_store_state(&target).await.unwrap(),
        Some(view)
    );
    assert_eq!(
        backend.resolve_store_state(&newer).await.unwrap(),
        Some(new_view)
    );
    assert!(backend.get(&child.id()).await.is_err());
    let image = authoritative_image(backend.pool()).await;
    close(backend).await;
    std::fs::remove_file(&files.cache).unwrap();
    let backend = files.open().await;
    assert!(
        backend
            .resolve_store_state(&target)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(authoritative_image(backend.pool()).await, image);
    close(backend).await;
}

#[tokio::test]
async fn directory_bootstrap_reuses_instance_key_without_premature_metadata() {
    use crate::{Instance, NewAccount};
    let files = Files::new();
    let root = files._dir.path().join("caches");
    std::fs::create_dir(&root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o2770)).unwrap();
    }
    let permissions = std::fs::metadata(&root).unwrap().permissions();
    let mut identities = Vec::new();
    for name in ["one.db", "two.db"] {
        let path = files._dir.path().join(name);
        let backend =
            SqlxBackend::open_sqlite_with_cache_dir(&path, &root, DerivedCachePolicy::persistent())
                .await
                .unwrap();
        assert!(backend.get_instance_metadata().await.unwrap().is_none());
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            identities.len() * 2
        );
        let (instance, admin) =
            Instance::create_backend(Box::new(backend), NewAccount::passwordless("admin"))
                .await
                .unwrap();
        let id = instance.id();
        let engine = instance.backend().local_engine().unwrap();
        let sql = engine.as_any().downcast_ref::<SqlxBackend>().unwrap();
        assert_eq!(sql.get_instance_metadata().await.unwrap().unwrap().id, id);
        // Initialization has used memory, not incomplete persisted metadata to name C.
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            identities.len() * 2
        );
        sql.cache_pool().close().await;
        sql.pool().close().await;
        drop(engine);
        drop(admin);
        drop(instance);
        let backend =
            SqlxBackend::open_sqlite_with_cache_dir(&path, &root, DerivedCachePolicy::persistent())
                .await
                .unwrap();
        let expected = root.join(format!(
            "{}.sqlite",
            hex::encode(id.to_prefixed_string().as_bytes())
        ));
        assert!(expected.is_file());
        identities.push(expected);
        assert!(
            backend
                .local_account_names()
                .await
                .unwrap()
                .contains(&"login:admin".into())
        );
        close(backend).await;
        // Each persistent image has the stable owner sidecar; WAL/SHM disappear on close.
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            identities.len() * 2
        );
    }
    assert_ne!(identities[0], identities[1]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode(),
            permissions.mode()
        );
    }
}

#[tokio::test]
async fn v4_credentials_and_authority_survive_split_but_unreleased_layouts_refuse() {
    let files = Files::new();
    let backend = files.open().await;
    let keep = publish(
        &backend,
        request("authority", StoreStateLifecycle::Authoritative),
    )
    .await;
    backend
        .compare_exchange_local_account_record("credential", None, b"opaque local bundle")
        .await
        .unwrap();
    close(backend).await;
    // Seed the landed single-file v4 layout, including a legacy Derived row.
    let h = raw(&files.history).await;
    h.execute("INSERT INTO store_state_namespaces SELECT 'legacy-derived', database_id, store_name, 0, status, scope_user_uuid, projection_name, projection_version, source_key, created_revision, size_bytes, last_access_tick FROM store_state_namespaces").await.unwrap();
    h.execute("INSERT INTO store_state_records SELECT 'legacy-derived', record_key, record_value FROM store_state_records WHERE namespace_id <> 'legacy-derived'").await.unwrap();
    h.close().await;
    let backend = files.open().await;
    assert_eq!(
        count(backend.pool(), "SELECT version FROM schema_version").await,
        4
    );
    assert_eq!(
        backend
            .local_account_record("credential")
            .await
            .unwrap()
            .unwrap(),
        b"opaque local bundle"
    );
    assert_eq!(
        backend
            .store_state_record_get(&keep, b"key")
            .await
            .unwrap()
            .unwrap(),
        b"value"
    );
    assert_eq!(
        count(
            backend.pool(),
            "SELECT COUNT(*) FROM store_state_namespaces"
        )
        .await,
        1
    );
    close(backend).await;
    let h = raw(&files.history).await;
    h.execute(
        "CREATE TABLE history_identity (singleton INTEGER, storage_uuid TEXT, history_stamp TEXT)",
    )
    .await
    .unwrap();
    h.close().await;
    let before = std::fs::read(&files.history).unwrap();
    assert!(
        Sqlite::open(&files.history)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("history_identity")
    );
    assert_eq!(std::fs::read(&files.history).unwrap(), before);
}

#[tokio::test]
async fn cache_file_validation_and_failed_offline_reset_preserve_trust() {
    let files = Files::new();
    let backend = files.open().await;
    let target = request("reset", StoreStateLifecycle::Derived);
    let view = publish(&backend, target.clone()).await;
    let entry = Entry::root_builder().build().unwrap();
    backend.put(entry.clone()).await.unwrap();
    backend
        .update_verification_status(&entry.id(), VerificationStatus::Verified)
        .await
        .unwrap();
    let image = authoritative_image(backend.pool()).await;
    backend.cache_pool().execute("CREATE TRIGGER refuse_clear BEFORE DELETE ON store_state_namespaces BEGIN SELECT RAISE(ABORT, 'clear failure'); END").await.unwrap();
    assert!(backend.reset_local_verification().await.is_err());
    assert_eq!(authoritative_image(backend.pool()).await, image);
    assert_eq!(
        backend.resolve_store_state(&target).await.unwrap(),
        Some(view)
    );
    backend
        .cache_pool()
        .execute("DROP TRIGGER refuse_clear")
        .await
        .unwrap();
    backend.reset_local_verification().await.unwrap();
    assert!(
        backend
            .verified_snapshot(&entry.id())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        backend
            .resolve_store_state(&target)
            .await
            .unwrap()
            .is_none()
    );
    close(backend).await;
    let c = raw(&files.cache).await;
    c.execute("UPDATE cache_format SET version = 999")
        .await
        .unwrap();
    c.close().await;
    let h = std::fs::read(&files.history).unwrap();
    assert!(Sqlite::open(&files.history).await.is_err());
    assert_eq!(std::fs::read(&files.history).unwrap(), h);
    std::fs::remove_file(&files.cache).unwrap();
    let c = raw(&files.cache).await;
    c.execute("CREATE TABLE unrelated (value TEXT)")
        .await
        .unwrap();
    c.execute("INSERT INTO unrelated VALUES ('keep')")
        .await
        .unwrap();
    c.close().await;
    let c = std::fs::read(&files.cache).unwrap();
    assert!(Sqlite::open(&files.history).await.is_err());
    assert_eq!(std::fs::read(&files.cache).unwrap(), c);
}

#[tokio::test]
async fn ownership_paths_and_memory_configuration_are_separate() {
    let files = Files::new();
    let custom = files._dir.path().join("elsewhere.cache");
    let backend =
        Sqlite::open_with_cache_path(&files.history, &custom, DerivedCachePolicy::persistent())
            .await
            .unwrap();
    assert!(custom.is_file());
    assert!(!files.cache.exists());
    let second_history = files._dir.path().join("second.db");
    let error =
        Sqlite::open_with_cache_path(&second_history, &custom, DerivedCachePolicy::persistent())
            .await
            .err()
            .unwrap();
    assert!(
        matches!(error, crate::Error::Backend(ref e) if matches!(**e, BackendError::StorageAlreadyOwned { .. }))
    );
    assert!(
        !second_history.exists(),
        "claim refusal precedes H initialization"
    );
    close(backend).await;
    let lock =
        super::super::prepare_sqlite(&super::super::sqlite_path_url(&custom).unwrap()).unwrap();
    assert!(
        Sqlite::open_with_cache_path(&files.history, &custom, DerivedCachePolicy::persistent())
            .await
            .is_err()
    );
    drop(lock);
    for alias in [
        &files.history,
        &super::super::sqlite_owner_lock_path(&files.history),
    ] {
        assert!(
            Sqlite::open_with_cache_path(&files.history, alias, DerivedCachePolicy::persistent())
                .await
                .is_err()
        );
    }
    #[cfg(unix)]
    {
        let alias = files._dir.path().join("alias");
        std::os::unix::fs::symlink(&files.history, &alias).unwrap();
        assert!(
            Sqlite::open_with_cache_path(&files.history, &alias, DerivedCachePolicy::persistent())
                .await
                .is_err()
        );
    }
    let memory = Sqlite::in_memory().await.unwrap();
    assert!(memory.derived_pool.is_some());
    assert_eq!(
        count(
            memory.pool(),
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'history_identity'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            memory.cache_pool(),
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'history_identity'"
        )
        .await,
        0
    );
    assert!(
        SqlxBackend::connect_sqlite_with_cache_path(
            "sqlite::memory:",
            &custom,
            DerivedCachePolicy::default()
        )
        .await
        .is_err()
    );
    close(memory).await;
}
