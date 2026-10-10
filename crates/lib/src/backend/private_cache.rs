//! Fixed experimental resource admission. Recomputed from durable rows while
//! holding the backend's write transaction/lock; no caller-controlled counters.
use crate::{
    Result,
    backend::{BackendError, CacheScope, StoreStateRequest},
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub user: Usage,
    pub global: Usage,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            user: Usage {
                bytes: 256 << 20,
                metadata: 16 << 20,
                namespaces: 128,
                active: 2,
                outcomes: 1024,
            },
            global: Usage {
                bytes: 1024 << 20,
                metadata: 64 << 20,
                namespaces: 512,
                active: 8,
                outcomes: 4096,
            },
        }
    }
}
#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct Usage {
    pub bytes: u64,
    pub metadata: u64,
    pub namespaces: u64,
    pub active: u64,
    /// All retained tokens, including Active and terminal recovery evidence.
    pub outcomes: u64,
}
impl Usage {
    pub fn add(&mut self, other: Self) {
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.metadata = self.metadata.saturating_add(other.metadata);
        self.namespaces = self.namespaces.saturating_add(other.namespaces);
        self.active = self.active.saturating_add(other.active);
        self.outcomes = self.outcomes.saturating_add(other.outcomes);
    }
    pub fn fits(self, cap: Self) -> bool {
        self.bytes <= cap.bytes
            && self.metadata <= cap.metadata
            && self.namespaces <= cap.namespaces
            && self.active <= cap.active
            && self.outcomes <= cap.outcomes
    }
}
impl Limits {
    pub fn check(self, user: Usage, global: Usage, extra: Usage) -> Result<()> {
        let mut user = user;
        let mut global = global;
        user.add(extra);
        global.add(extra);
        if !user.fits(self.user) || !global.fits(self.global) {
            return Err(BackendError::PrivateCacheQuotaExceeded.into());
        }
        Ok(())
    }
}
pub(crate) fn namespace_metadata(request: &StoreStateRequest) -> u64 {
    // Worst-case JSON string/byte escaping plus bookkeeping, covers persisted
    // metadata and avoids pretending these are physical disk/RSS guarantees.
    2048 + 6
        * (request.database.to_string().len()
            + request.store.len()
            + request.scope.storage_key().map_or(0, str::len)
            + request.projection.name.len()) as u64
        + 4 * request.source_key.len() as u64
}
pub(crate) fn token_metadata(request: &StoreStateRequest) -> Result<u64> {
    Ok(1024 + serde_json::to_vec(request)?.len() as u64)
}
pub(crate) fn admission(request: &StoreStateRequest) -> Result<Usage> {
    let metadata = namespace_metadata(request) + token_metadata(request)?;
    Ok(Usage {
        bytes: metadata,
        metadata,
        namespaces: 1,
        active: 1,
        outcomes: 1,
    })
}
pub(crate) fn private_user(request: &StoreStateRequest) -> Option<&str> {
    match (&request.scope, request.lifecycle) {
        (CacheScope::User(user), crate::backend::StoreStateLifecycle::Derived) => Some(user),
        _ => None,
    }
}
pub(crate) fn record_bytes(key: &[u8], value: Option<&[u8]>) -> u64 {
    128 + key.len() as u64 + value.map_or(0, |v| v.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::database::InMemory;
    use crate::backend::{
        BackendImpl, ProjectionDescriptor, RecordMutation, StagingStatus, StoreStateLifecycle,
    };
    use crate::entry::ID;
    fn target(user: &str, key: u8) -> StoreStateRequest {
        StoreStateRequest {
            database: ID::from_bytes(b"quota"),
            store: "data".into(),
            lifecycle: StoreStateLifecycle::Derived,
            scope: CacheScope::User(user.into()),
            projection: ProjectionDescriptor {
                name: "quota:v0".into(),
                version: 0,
            },
            source_key: vec![key],
        }
    }
    fn put(n: usize) -> Vec<RecordMutation> {
        vec![RecordMutation::Put {
            key: vec![1],
            value: vec![42; n],
        }]
    }
    fn limits(case: usize) -> Limits {
        let mut l = Limits::default();
        let global = case >= 6;
        let cap = if global { &mut l.global } else { &mut l.user };
        match case % 6 {
            0 => {
                cap.bytes = admission(&target("a", 0)).unwrap().bytes + 129 + 16;
            }
            1 => {
                cap.namespaces = 1;
            }
            2 => {
                cap.active = 1;
            }
            3 => {
                cap.outcomes = 1;
            }
            4 => {
                cap.metadata = admission(&target("a", 0)).unwrap().metadata;
            }
            5 => {
                l.global.active = 1;
            }
            _ => unreachable!(),
        }
        l
    }
    async fn exercise(backend: &dyn BackendImpl, case: usize) {
        let first = backend
            .begin_store_state_staging(target("a", 0))
            .await
            .unwrap();
        let global = case >= 6;
        let case = case % 6;
        if case == 0 {
            backend
                .stage_store_state_ordered_chunk(&first, 0, b"exact", put(16))
                .await
                .unwrap();
            backend
                .stage_store_state_ordered_chunk(&first, 0, b"exact", put(16))
                .await
                .unwrap();
            assert!(
                backend
                    .stage_store_state_ordered_chunk(&first, 0, b"conflict", put(16))
                    .await
                    .is_err()
            );
            let error = backend
                .stage_store_state_ordered_chunk(&first, 1, b"grow", put(17))
                .await
                .unwrap_err();
            assert!(error.to_string().contains("quota"), "{error}");
            assert_eq!(
                backend.store_state_staging_status(&first).await.unwrap(),
                Some(StagingStatus::Active)
            );
            let view = backend.publish_store_state(first).await.unwrap();
            assert_eq!(
                backend.store_state_record_get(&view, &[1]).await.unwrap(),
                Some(vec![42; 16])
            );
            return;
        }
        if matches!(case, 1 | 4) {
            backend.publish_store_state(first.clone()).await.unwrap();
        }
        if case == 3 {
            backend.abort_store_state(first.clone()).await.unwrap();
        }
        let user = if case == 5 || global { "b" } else { "a" };
        let error = backend
            .begin_store_state_staging(target(user, 1))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("quota"), "{error}");
        assert!(
            backend
                .store_state_staging_token(&first.namespace_id)
                .await
                .unwrap()
                .is_some(),
            "no retained recovery evidence is evicted"
        );
    }
    #[tokio::test]
    async fn private_quota_memory_bytes_namespace_active_outcome_metadata_global() {
        for case in 0..12 {
            let mut b = InMemory::new();
            b.cache_limits = limits(case);
            exercise(&b, case).await;
        }
    }
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn private_quota_sqlite_bytes_namespace_active_outcome_metadata_global() {
        for case in 0..12 {
            let mut b = crate::backend::database::SqlxBackend::sqlite_in_memory()
                .await
                .unwrap();
            b.cache_limits = limits(case);
            exercise(&b, case).await;
        }
    }
    #[tokio::test]
    async fn private_quota_memory_concurrent_admission_and_snapshot_accounting() {
        let mut b = InMemory::new();
        b.cache_limits = limits(2);
        let (a, c) = tokio::join!(
            b.begin_store_state_staging(target("a", 0)),
            b.begin_store_state_staging(target("a", 1))
        );
        assert_eq!(usize::from(a.is_ok()) + usize::from(c.is_ok()), 1);
        let bytes = serde_json::to_vec(&b).unwrap();
        let mut restored: InMemory = serde_json::from_slice(&bytes).unwrap();
        restored.cache_limits = limits(2);
        assert!(
            restored
                .begin_store_state_staging(target("a", 2))
                .await
                .is_err()
        );
    }
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn private_quota_sqlite_concurrent_admission_and_restart_accounting() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("quota.db");
        let mut b = crate::backend::database::SqlxBackend::open_sqlite(&file)
            .await
            .unwrap();
        b.cache_limits = limits(2);
        let (a, c) = tokio::join!(
            b.begin_store_state_staging(target("a", 0)),
            b.begin_store_state_staging(target("a", 1))
        );
        assert_eq!(usize::from(a.is_ok()) + usize::from(c.is_ok()), 1);
        let first = a.or(c).unwrap();
        b.abort_store_state(first.clone()).await.unwrap();
        drop(b);
        let mut b = crate::backend::database::SqlxBackend::open_sqlite(&file)
            .await
            .unwrap();
        b.cache_limits = limits(3);
        assert!(b.begin_store_state_staging(target("a", 2)).await.is_err());
        assert_eq!(
            b.store_state_staging_status(&first).await.unwrap(),
            Some(StagingStatus::Aborted)
        );
    }
    #[tokio::test]
    async fn private_quota_never_counts_or_evicts_authoritative_records() {
        let mut b = InMemory::new();
        b.cache_limits.user.bytes = 0;
        let mut authoritative = target("a", 0);
        authoritative.lifecycle = StoreStateLifecycle::Authoritative;
        let token = b
            .begin_store_state_staging(authoritative.clone())
            .await
            .unwrap();
        b.stage_store_state_ordered_chunk(&token, 0, b"exact", put(16))
            .await
            .unwrap();
        let view = b.publish_store_state(token).await.unwrap();
        b.clear_derived_store_state().await.unwrap();
        assert_eq!(
            b.resolve_store_state(&authoritative).await.unwrap(),
            Some(view.clone())
        );
        assert_eq!(
            b.store_state_record_get(&view, &[1]).await.unwrap(),
            Some(vec![42; 16])
        );
    }

    #[cfg(all(unix, feature = "sqlite", feature = "service"))]
    #[tokio::test]
    async fn private_assistance_sqlite_read_only_full_storage_publication_failures_remain_optional()
    {
        use crate::service::{
            ServiceServer,
            client::PrivateCacheAssistance,
            protocol::{DatabaseOp as Op, ServiceResponse},
        };
        use crate::store::assistance::PrivateRepresentation;
        use crate::store::query::{QuerySource, ReadScope};
        use crate::{
            Instance, NewUser,
            auth::types::SigKey,
            store::{DocStore, Registered},
        };
        for full in [false, true] {
            let b = crate::backend::database::SqlxBackend::sqlite_in_memory()
                .await
                .unwrap();
            let (server, mut owner) =
                Instance::create_backend(Box::new(b), NewUser::passwordless("owner"))
                    .await
                    .unwrap();
            let key = owner.get_default_key().unwrap();
            let db = owner
                .create_database(crate::crdt::Doc::new(), &key)
                .await
                .unwrap();
            let tx = db.new_transaction().await.unwrap();
            tx.get_store::<DocStore>("docs")
                .await
                .unwrap()
                .set("value", "valid")
                .await
                .unwrap();
            tx.commit().await.unwrap();
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("fault.sock");
            let (stop, rx) = tokio::sync::watch::channel(());
            let daemon = tokio::spawn(
                ServiceServer::bind(server.clone(), &socket)
                    .await
                    .unwrap()
                    .run(rx),
            );
            let client = Instance::connect(format!("unix://{}", socket.display()))
                .await
                .unwrap();
            client.login_user("owner", None).await.unwrap();
            let conn = client.remote_connection().unwrap();
            let identity = SigKey::from_pubkey(&key);
            let source = conn
                .store_source(
                    db.root_id().clone(),
                    identity.clone(),
                    "docs".into(),
                    DocStore::type_id().into(),
                    QuerySource {
                        main: db.snapshot().await.unwrap(),
                        scope: ReadScope::Verified,
                    },
                )
                .await
                .unwrap();
            let remote = crate::Database::open(&client, db.root_id()).await.unwrap();
            let tx = remote.new_transaction().await.unwrap();
            let valid = tx
                .fold_raw_source::<crate::crdt::Doc>(&source)
                .await
                .unwrap();
            let representation = PrivateRepresentation {
                format: ProjectionDescriptor {
                    name: "fault:v0".into(),
                    version: 0,
                },
                configuration: vec![],
            };
            let token = match conn
                .private_assistance(
                    db.root_id().clone(),
                    identity.clone(),
                    Op::BeginPrivateAssistance {
                        source: source.clone(),
                        representation: representation.clone(),
                    },
                )
                .await
                .unwrap()
            {
                ServiceResponse::Token(t) => t,
                _ => panic!(),
            };
            conn.private_assistance(
                db.root_id().clone(),
                identity.clone(),
                Op::PrivateAssistanceChunk {
                    token: token.clone(),
                    chunk_id: 0,
                    mutations: put(16),
                },
            )
            .await
            .unwrap();
            let engine = server.backend().local_engine().unwrap();
            let sql = engine
                .as_any()
                .downcast_ref::<crate::backend::database::SqlxBackend>()
                .unwrap();
            sqlx::query("PRAGMA query_only = ON")
                .execute(sql.pool())
                .await
                .unwrap();
            assert!(
                conn.private_assistance(
                    db.root_id().clone(),
                    identity.clone(),
                    Op::FinishPrivateAssistance {
                        token: token.clone()
                    }
                )
                .await
                .is_err()
            );
            let mut sdk = PrivateCacheAssistance::default();
            assert_eq!(
                sdk.publish_best_effort(
                    &conn,
                    identity.clone(),
                    (source.clone(), representation.clone()),
                    put(16),
                    valid.clone()
                )
                .await,
                valid
            );
            sqlx::query("PRAGMA query_only = OFF")
                .execute(sql.pool())
                .await
                .unwrap();
            if full {
                let (pages,): (i64,) = sqlx::query_as("PRAGMA page_count")
                    .fetch_one(sql.pool())
                    .await
                    .unwrap();
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "PRAGMA max_page_count = {pages}"
                )))
                .execute(sql.pool())
                .await
                .unwrap();
                let error = conn
                    .private_assistance(
                        db.root_id().clone(),
                        identity.clone(),
                        Op::PrivateAssistanceChunk {
                            token: token.clone(),
                            chunk_id: 1,
                            mutations: put(200_000),
                        },
                    )
                    .await
                    .unwrap_err();
                assert!(error.to_string().contains("full"), "{error}");
                assert_eq!(
                    sdk.publish_best_effort(
                        &conn,
                        identity.clone(),
                        (source.clone(), representation.clone()),
                        put(200_000),
                        valid.clone()
                    )
                    .await,
                    valid
                );
                sqlx::query("PRAGMA max_page_count = 1073741823")
                    .execute(sql.pool())
                    .await
                    .unwrap();
            }
            assert!(matches!(
                conn.private_assistance(
                    db.root_id().clone(),
                    identity.clone(),
                    Op::LookupPrivateMaterialization {
                        source,
                        representation,
                        range: Default::default(),
                        after: None
                    }
                )
                .await
                .unwrap(),
                ServiceResponse::PrivateMaterialization(None)
            ));
            if !full {
                conn.private_assistance(
                    db.root_id().clone(),
                    identity,
                    Op::FinishPrivateAssistance { token },
                )
                .await
                .unwrap();
            }
            drop(conn);
            drop(client);
            drop(remote);
            drop(tx);
            stop.send(()).unwrap();
            daemon.await.unwrap().unwrap();
        }
    }
}
