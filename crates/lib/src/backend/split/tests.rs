//! Mixed primary/cache engines through [`SplitBackend`].

use std::path::{Path, PathBuf};

use super::*;
use crate::backend::database::{InMemory, SqliteCache, SqlxBackend};
use crate::backend::{CacheScope, DerivedCachePolicy, InstanceMetadata, ProjectionDescriptor};

const NO_RETENTION: DerivedCachePolicy = DerivedCachePolicy {
    max_namespaces: 0,
    max_bytes: 0,
};

#[derive(Clone)]
enum Primary {
    Sqlite(PathBuf),
    #[cfg(feature = "postgres")]
    Postgres(String),
}

#[derive(Clone, Copy, Debug)]
enum Cache {
    Sqlite,
    Memory,
}

fn postgres_url() -> String {
    std::env::var("TEST_POSTGRES_URL")
        .unwrap_or_else(|_| "postgres://localhost/eidetica_test".to_string())
}

/// SQLite always; PostgreSQL under the PostgreSQL matrix, in a fresh schema so
/// a restart can reopen the same primary.
async fn primaries(dir: &Path) -> Vec<Primary> {
    #[allow(unused_mut)]
    let mut primaries = vec![Primary::Sqlite(dir.join("history.db"))];
    #[cfg(feature = "postgres")]
    if std::env::var("TEST_BACKEND").as_deref() == Ok("postgres") {
        use sqlx::Executor;
        sqlx::any::install_default_drivers();
        let schema = format!("split_{}", uuid::Uuid::new_v4().simple());
        let admin = sqlx::AnyPool::connect(&postgres_url()).await.unwrap();
        // SAFETY: the test schema is generated solely from a UUID.
        admin
            .execute(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .await
            .unwrap();
        primaries.push(Primary::Postgres(schema));
    }
    primaries
}

async fn open_primary(primary: &Primary) -> SqlxBackend {
    match primary {
        Primary::Sqlite(path) => SqlxBackend::open_sqlite_with_memory_cache(path, NO_RETENTION)
            .await
            .unwrap(),
        #[cfg(feature = "postgres")]
        Primary::Postgres(schema) => {
            // A dropped owner's sessions may still be exiting.
            SqlxBackend::test_await_postgres_namespace_release(
                &postgres_url(),
                schema,
                std::time::Duration::from_secs(10),
            )
            .await
            .unwrap();
            let backend =
                SqlxBackend::test_connect_postgres_schema(&postgres_url(), schema.clone())
                    .await
                    .unwrap();
            *backend.derived_cache_policy.write().unwrap() = NO_RETENTION;
            backend
        }
    }
}

async fn open_cache(cache: Cache, path: &Path) -> Box<dyn DerivedCacheStorage> {
    match cache {
        Cache::Sqlite => Box::new(
            SqliteCache::open(path, DerivedCachePolicy::persistent())
                .await
                .unwrap(),
        ),
        Cache::Memory => Box::new(InMemory::new()),
    }
}

async fn open_split(primary: &Primary, cache: Cache, cache_path: &Path) -> SplitBackend {
    SplitBackend::new(
        Box::new(open_primary(primary).await),
        open_cache(cache, cache_path).await,
    )
}

fn sql(split: &SplitBackend) -> &SqlxBackend {
    split.as_any().downcast_ref::<SqlxBackend>().unwrap()
}

fn request(store: &str, lifecycle: StoreStateLifecycle) -> StoreStateRequest {
    StoreStateRequest {
        database: ID::from_bytes("split-db"),
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

async fn publish(backend: &dyn BackendImpl, target: StoreStateRequest) -> RecordView {
    let token = backend.begin_store_state_staging(target).await.unwrap();
    backend
        .stage_store_state_records(
            &token,
            [
                (b"a".to_vec(), Some(b"1".to_vec())),
                (b"b".to_vec(), Some(b"2".to_vec())),
            ]
            .into(),
        )
        .await
        .unwrap();
    backend.publish_store_state(token).await.unwrap()
}

async fn derived_rows_in_primary(primary: &SqlxBackend) -> i64 {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM store_state_namespaces WHERE lifecycle <> 1")
            .fetch_one(primary.pool())
            .await
            .unwrap();
    count
}

fn is_invalid_view(error: &crate::Error) -> bool {
    matches!(error, crate::Error::Backend(e) if matches!(e.as_ref(), BackendError::InvalidStoreStateView))
}

fn is_invalid_token(error: &crate::Error) -> bool {
    matches!(error, crate::Error::Backend(e) if matches!(e.as_ref(), BackendError::InvalidStoreStateStagingToken))
}

#[tokio::test]
async fn mixed_backends_keep_derived_state_in_the_selected_cache() {
    let dir = tempfile::tempdir().unwrap();
    for primary in primaries(dir.path()).await {
        for cache in [Cache::Sqlite, Cache::Memory] {
            let cache_path = dir
                .path()
                .join(format!("{cache:?}-{}.sqlite", uuid::Uuid::new_v4()));
            let derived = request(&format!("derived-{cache:?}"), StoreStateLifecycle::Derived);
            let authority = request(
                &format!("authority-{cache:?}"),
                StoreStateLifecycle::Authoritative,
            );
            {
                let split = open_split(&primary, cache, &cache_path).await;
                let view = publish(&split, derived.clone()).await;
                let kept = publish(&split, authority.clone()).await;
                assert_eq!(
                    split.resolve_store_state(&derived).await.unwrap(),
                    Some(view.clone())
                );
                assert_eq!(
                    split.store_state_record_get(&view, b"b").await.unwrap(),
                    Some(b"2".to_vec())
                );
                let page = split
                    .store_state_record_scan(&view, &RecordRange::default(), Some(b"a"), 10)
                    .await
                    .unwrap();
                assert_eq!(page.records, vec![(b"b".to_vec(), b"2".to_vec())]);
                assert_eq!(
                    split.store_state_record_get(&kept, b"a").await.unwrap(),
                    Some(b"1".to_vec())
                );
                // Authoritative state is primary-owned; Derived never is.
                let primary_sql = sql(&split);
                assert_eq!(derived_rows_in_primary(primary_sql).await, 0);
                assert!(
                    primary_sql
                        .resolve_store_state(&derived)
                        .await
                        .unwrap()
                        .is_none()
                );
                assert!(
                    primary_sql
                        .resolve_store_state(&authority)
                        .await
                        .unwrap()
                        .is_some()
                );

                // Opaque handles return only to their issuing storage.
                let inner = view.namespace_id.strip_prefix(CACHE_TAG).unwrap();
                for forged in [format!("{PRIMARY_TAG}{inner}"), inner.to_owned()] {
                    let forged = RecordView {
                        namespace_id: forged,
                    };
                    let error = split
                        .store_state_record_get(&forged, b"a")
                        .await
                        .unwrap_err();
                    assert!(is_invalid_view(&error), "{error:?}");
                }
                let token = split
                    .begin_store_state_staging(derived.clone())
                    .await
                    .unwrap();
                let inner = token.namespace_id.strip_prefix(CACHE_TAG).unwrap();
                let forged = StagingToken {
                    namespace_id: format!("{PRIMARY_TAG}{inner}"),
                    target: token.target.clone(),
                };
                let error = split.publish_store_state(forged).await.unwrap_err();
                assert!(is_invalid_token(&error), "{error:?}");
                let relabelled = StagingToken {
                    namespace_id: token.namespace_id.clone(),
                    target: authority.clone(),
                };
                let error = split
                    .stage_store_state_records(&relabelled, Default::default())
                    .await
                    .unwrap_err();
                assert!(is_invalid_token(&error), "{error:?}");
                split.abort_store_state(token).await.unwrap();
            }

            // Restart: Authoritative state survives with the primary; Derived
            // state survives only in a persistent cache, never in the primary.
            let split = open_split(&primary, cache, &cache_path).await;
            assert!(
                split
                    .resolve_store_state(&authority)
                    .await
                    .unwrap()
                    .is_some()
            );
            let reopened = split.resolve_store_state(&derived).await.unwrap();
            match cache {
                Cache::Sqlite => {
                    let view = reopened.expect("persistent cache survives restart");
                    assert_eq!(
                        split.store_state_record_get(&view, b"a").await.unwrap(),
                        Some(b"1".to_vec())
                    );
                }
                Cache::Memory => assert!(reopened.is_none()),
            }
            assert_eq!(derived_rows_in_primary(sql(&split)).await, 0);
        }
    }
}

#[tokio::test]
async fn clear_keeps_live_views_for_one_generation_in_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    for primary in primaries(dir.path()).await {
        for cache in [Cache::Sqlite, Cache::Memory] {
            let split = open_split(
                &primary,
                cache,
                &dir.path()
                    .join(format!("clear-{}.sqlite", uuid::Uuid::new_v4())),
            )
            .await;
            let derived = request("clear", StoreStateLifecycle::Derived);
            let authority = request("clear-authority", StoreStateLifecycle::Authoritative);
            let view = publish(&split, derived.clone()).await;
            let kept = publish(&split, authority.clone()).await;
            split.clear_derived_store_state().await.unwrap();
            assert!(split.resolve_store_state(&derived).await.unwrap().is_none());
            assert_eq!(
                split.store_state_record_get(&view, b"a").await.unwrap(),
                Some(b"1".to_vec()),
                "a resolved view reads until the following clear"
            );
            split.clear_derived_store_state().await.unwrap();
            let error = split.store_state_record_get(&view, b"a").await.unwrap_err();
            assert!(is_invalid_view(&error), "{error:?}");
            assert_eq!(
                split.store_state_record_get(&kept, b"a").await.unwrap(),
                Some(b"1".to_vec())
            );
        }
    }
}

/// Selected cache bounds apply; the primary's disabled bounds do not.
#[tokio::test]
async fn cache_policy_belongs_to_the_cache_engine() {
    let dir = tempfile::tempdir().unwrap();
    let tight = DerivedCachePolicy {
        max_namespaces: 1,
        max_bytes: 1024,
    };
    for primary in primaries(dir.path()).await {
        let caches: [Box<dyn DerivedCacheStorage>; 2] = [
            Box::new(
                SqliteCache::open(
                    &dir.path()
                        .join(format!("bounds-{}.sqlite", uuid::Uuid::new_v4())),
                    tight,
                )
                .await
                .unwrap(),
            ),
            Box::new(InMemory::new_with_cache_policy(tight)),
        ];
        for cache in caches {
            let split = SplitBackend::new(Box::new(open_primary(&primary).await), cache);
            let first = request("first", StoreStateLifecycle::Derived);
            let second = request("second", StoreStateLifecycle::Derived);
            let old = publish(&split, first.clone()).await;
            publish(&split, second.clone()).await;
            assert!(split.resolve_store_state(&first).await.unwrap().is_none());
            assert!(split.resolve_store_state(&second).await.unwrap().is_some());
            assert_eq!(
                split.store_state_record_get(&old, b"a").await.unwrap(),
                Some(b"1".to_vec()),
                "an evicted view stays readable until the next eviction pass"
            );
        }
    }
}

struct FailingReset;

#[async_trait]
impl DerivedCacheStorage for FailingReset {
    async fn resolve_store_state(&self, _: &StoreStateRequest) -> Result<Option<RecordView>> {
        Ok(None)
    }
    async fn begin_store_state_staging(&self, _: StoreStateRequest) -> Result<StagingToken> {
        unreachable!()
    }
    async fn stage_store_state_records(&self, _: &StagingToken, _: RecordMutations) -> Result<()> {
        unreachable!()
    }
    async fn publish_store_state(&self, _: StagingToken) -> Result<RecordView> {
        unreachable!()
    }
    async fn abort_store_state(&self, _: StagingToken) -> Result<()> {
        unreachable!()
    }
    async fn store_state_record_get(&self, _: &RecordView, _: &[u8]) -> Result<Option<Vec<u8>>> {
        unreachable!()
    }
    async fn store_state_record_scan(
        &self,
        _: &RecordView,
        _: &RecordRange,
        _: Option<&[u8]>,
        _: usize,
    ) -> Result<RecordPage> {
        unreachable!()
    }
    async fn clear_derived_store_state(&self) -> Result<()> {
        unreachable!()
    }
    async fn reset_derived_store_state(&self) -> Result<()> {
        Err(BackendError::StateInconsistency {
            reason: "injected cache reset failure".into(),
        }
        .into())
    }
}

#[tokio::test]
async fn trust_reset_discards_the_cache_before_primary_trust() {
    let dir = tempfile::tempdir().unwrap();
    for primary in primaries(dir.path()).await {
        let cache_path = dir
            .path()
            .join(format!("reset-{}.sqlite", uuid::Uuid::new_v4()));
        let entry = Entry::root_builder().build().unwrap();
        let derived = request("reset", StoreStateLifecycle::Derived);
        {
            let split = open_split(&primary, Cache::Sqlite, &cache_path).await;
            split.put(entry.clone()).await.unwrap();
            split
                .update_verification_status(&entry.id(), VerificationStatus::Verified)
                .await
                .unwrap();
            publish(&split, derived.clone()).await;
        }
        // A cache that cannot be discarded aborts before trust changes.
        let failing = SplitBackend::new(
            Box::new(open_primary(&primary).await),
            Box::new(FailingReset),
        );
        failing.reset_local_verification().await.unwrap_err();
        assert_eq!(
            failing.get_verification_status(&entry.id()).await.unwrap(),
            VerificationStatus::Verified
        );
        drop(failing);

        {
            let split = open_split(&primary, Cache::Sqlite, &cache_path).await;
            assert!(split.resolve_store_state(&derived).await.unwrap().is_some());
            split.reset_local_verification().await.unwrap();
            assert!(split.resolve_store_state(&derived).await.unwrap().is_none());
            assert_eq!(
                split.get_verification_status(&entry.id()).await.unwrap(),
                VerificationStatus::Unverified
            );
            assert!(
                split
                    .verified_snapshot(&entry.id())
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
        let split = open_split(&primary, Cache::Sqlite, &cache_path).await;
        assert!(split.resolve_store_state(&derived).await.unwrap().is_none());
    }
}

#[tokio::test]
async fn sqlite_cache_files_refuse_foreign_or_shared_use() {
    let dir = tempfile::tempdir().unwrap();
    // A history database is not a cache file: refused and left intact.
    let history = dir.path().join("history.db");
    drop(
        SqlxBackend::open_sqlite_with_memory_cache(&history, NO_RETENTION)
            .await
            .unwrap(),
    );
    let before = std::fs::read(&history).unwrap();
    assert!(
        SqliteCache::open(&history, DerivedCachePolicy::persistent())
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&history).unwrap(), before);

    // One owner per cache file.
    let path = dir.path().join("owned.sqlite");
    let owner = SqliteCache::open(&path, DerivedCachePolicy::persistent())
        .await
        .unwrap();
    assert!(!owner.is_in_memory());
    assert!(
        SqliteCache::open(&path, DerivedCachePolicy::persistent())
            .await
            .is_err()
    );
    drop(owner);

    // Instance caches are named by the existing device key; first
    // initialization (no metadata) stays in memory.
    let memory = SqliteCache::open_for_instance(dir.path(), None, DerivedCachePolicy::persistent())
        .await
        .unwrap();
    assert!(memory.is_in_memory());
    let device = crate::auth::crypto::generate_keypair().1;
    let metadata = InstanceMetadata {
        id: device.clone(),
        users_db: ID::from_bytes("users"),
        databases_db: ID::from_bytes("databases"),
        sync_db: None,
    };
    let file = SqliteCache::open_for_instance(
        dir.path(),
        Some(&metadata),
        DerivedCachePolicy::persistent(),
    )
    .await
    .unwrap();
    assert!(!file.is_in_memory());
    assert!(dir.path().join(instance_file_name(&metadata)).is_file());
}

fn instance_file_name(metadata: &InstanceMetadata) -> String {
    format!(
        "{}.sqlite",
        hex::encode(metadata.id.to_prefixed_string().as_bytes())
    )
}

/// Test handle that keeps the cache reachable after the split takes it.
struct Shared(std::sync::Arc<SqliteCache>);
derived_cache_storage_via!(Shared, |this| this.0.testing_backend());

async fn derived_namespaces(pool: &sqlx::AnyPool) -> i64 {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM store_state_namespaces WHERE lifecycle = 0")
            .fetch_one(pool)
            .await
            .unwrap();
    count
}

/// End to end through an Instance: Table/Doc reads materialize into the
/// selected cache, survive restart, and rebuild from primary history after
/// the cache file is removed offline.
#[tokio::test]
async fn instance_reads_reconstruct_after_offline_cache_loss() {
    use crate::{
        Instance, NewAccount,
        crdt::Doc,
        store::{DocStore, Table},
    };
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Row {
        value: u32,
    }

    let dir = tempfile::tempdir().unwrap();
    for primary in primaries(dir.path()).await {
        let cache_path = dir
            .path()
            .join(format!("instance-{}.sqlite", uuid::Uuid::new_v4()));
        let open = || async {
            let cache = std::sync::Arc::new(
                SqliteCache::open(&cache_path, DerivedCachePolicy::persistent())
                    .await
                    .unwrap(),
            );
            let split = SplitBackend::new(
                Box::new(open_primary(&primary).await),
                Box::new(Shared(cache.clone())),
            );
            (split, cache)
        };
        let (split, cache) = open().await;
        let (instance, _) =
            Instance::create_backend(Box::new(split), NewAccount::passwordless("admin"))
                .await
                .unwrap();
        let root = {
            let mut user = instance.login_account("admin", "").await.unwrap();
            let key = user.get_default_key().unwrap();
            let db = user.create_database(Doc::new(), &key).await.unwrap();
            let tx = db.new_transaction().await.unwrap();
            tx.get_store::<DocStore>("doc")
                .await
                .unwrap()
                .set("key", "value")
                .await
                .unwrap();
            let rows = tx.get_store::<Table<Row>>("rows").await.unwrap();
            rows.set("a", Row { value: 1 }).await.unwrap();
            rows.set("b", Row { value: 2 }).await.unwrap();
            tx.commit().await.unwrap();
            let rows = db.get_store_viewer::<Table<Row>>("rows").await.unwrap();
            assert_eq!(rows.get("b").await.unwrap().value, 2);
            db.root_id().clone()
        };
        let local = instance.backend().local_engine().unwrap();
        let primary_sql = local.as_any().downcast_ref::<SqlxBackend>().unwrap();
        assert_eq!(derived_rows_in_primary(primary_sql).await, 0);
        assert!(derived_namespaces(cache.testing_backend().cache_pool()).await > 0);
        primary_sql.pool().close().await;
        drop(local);
        drop(instance);
        cache.testing_backend().cache_pool().close().await;
        drop(cache);

        for round in 0..2 {
            let (split, cache) = open().await;
            if round == 0 {
                assert!(
                    derived_namespaces(cache.testing_backend().cache_pool()).await > 0,
                    "a persistent cache survives restart"
                );
            } else {
                assert_eq!(
                    derived_namespaces(cache.testing_backend().cache_pool()).await,
                    0
                );
            }
            let instance = Instance::open_backend(Box::new(split)).await.unwrap();
            {
                let user = instance.login_account("admin", "").await.unwrap();
                let db = user.open_database(&root).await.unwrap();
                let doc = db.get_store_viewer::<DocStore>("doc").await.unwrap();
                assert_eq!(doc.get_string("key").await.unwrap(), "value");
                let rows = db.get_store_viewer::<Table<Row>>("rows").await.unwrap();
                assert_eq!(rows.get("a").await.unwrap().value, 1);
                let page = rows.scan_page(None, 10).await.unwrap();
                assert_eq!(page.rows.len(), 2);
            }
            assert!(derived_namespaces(cache.testing_backend().cache_pool()).await > 0);
            let local = instance.backend().local_engine().unwrap();
            let primary_sql = local.as_any().downcast_ref::<SqlxBackend>().unwrap();
            assert_eq!(derived_rows_in_primary(primary_sql).await, 0);
            primary_sql.pool().close().await;
            drop(local);
            drop(instance);
            cache.testing_backend().cache_pool().close().await;
            drop(cache);
            // Offline cache recreation: remove the image, keep the owner sidecar.
            for suffix in ["", "-wal", "-shm", "-journal"] {
                let path = PathBuf::from(format!("{}{suffix}", cache_path.display()));
                if path.exists() {
                    std::fs::remove_file(path).unwrap();
                }
            }
        }
    }
}
