use sqlx::{AnyPool, Executor, any::AnyPoolOptions};

use super::*;
use crate::auth::crypto::PrivateKey;
#[cfg(feature = "postgres")]
use crate::backend::database::sql::{StorageOwner, release_failed_postgres_claim};
use crate::backend::{
    BackendImpl, CacheScope, InstanceMetadata, InstanceSecrets, ProjectionDescriptor,
    StoreStateLifecycle, StoreStateRequest,
};
use crate::entry::Entry;

/// Raw fixture access is separate from the real constructor under test.
struct TestDatabase {
    pool: AnyPool,
    url: String,
    #[cfg(feature = "postgres")]
    schema: Option<String>,
    _directory: Option<tempfile::TempDir>,
}

impl TestDatabase {
    async fn new() -> Option<Self> {
        sqlx::any::install_default_drivers();
        #[cfg(feature = "postgres")]
        if std::env::var("TEST_BACKEND").as_deref() == Ok("postgres") {
            let url = std::env::var("TEST_POSTGRES_URL")
                .unwrap_or_else(|_| "postgres://localhost/eidetica_test".to_string());
            let schema = format!("schema_test_{}", uuid::Uuid::new_v4().simple());
            let pool = AnyPoolOptions::new()
                .max_connections(1)
                .connect(&url)
                .await
                .unwrap();
            // SAFETY: the schema identifier is generated solely from a UUID.
            pool.execute(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
                .await
                .unwrap();
            pool.execute(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}")))
                .await
                .unwrap();
            return Some(Self {
                pool,
                url,
                schema: Some(schema),
                _directory: None,
            });
        }
        #[cfg(feature = "sqlite")]
        if std::env::var("TEST_BACKEND").as_deref() == Ok("sqlite") {
            let directory = tempfile::tempdir().unwrap();
            let url = format!(
                "sqlite:{}?mode=rwc",
                directory.path().join("schema.db").display()
            );
            let pool = AnyPoolOptions::new()
                .max_connections(1)
                .connect(&url)
                .await
                .unwrap();
            return Some(Self {
                pool,
                url,
                #[cfg(feature = "postgres")]
                schema: None,
                _directory: Some(directory),
            });
        }
        None
    }

    async fn open(&self) -> Result<SqlxBackend> {
        #[cfg(feature = "postgres")]
        if let Some(schema) = &self.schema {
            return SqlxBackend::connect_postgres_with_schema(&self.url, Some(schema.clone()))
                .await;
        }
        #[cfg(feature = "sqlite")]
        return SqlxBackend::connect_sqlite(&self.url).await;
        #[cfg(not(feature = "sqlite"))]
        unreachable!("only PostgreSQL fixtures are enabled");
    }

    async fn close_backend(&self, mut backend: SqlxBackend) {
        // Wait for server-side pool locks to be released before reopening.
        #[cfg(feature = "postgres")]
        if let Some(StorageOwner::Postgres { _connection }) = backend._owner.take() {
            release_failed_postgres_claim(backend.pool.take(), _connection.into_inner()).await;
            return;
        }
        backend.pool.take().unwrap().close().await;
    }

    async fn cleanup(self) {
        #[cfg(feature = "postgres")]
        if let Some(schema) = &self.schema {
            // SAFETY: the schema identifier is generated solely from a UUID.
            self.pool
                .execute(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
                .await
                .unwrap();
        }
        self.pool.close().await;
    }

    async fn marker(&self, version: Option<i64>) {
        self.pool.execute(CREATE_TABLES[0]).await.unwrap();
        if let Some(version) = version {
            sqlx::query("INSERT INTO schema_version (version) VALUES ($1)")
                .bind(version)
                .execute(&self.pool)
                .await
                .unwrap();
        }
    }

    async fn versions(&self) -> Vec<(i64,)> {
        sqlx::query_as("SELECT version FROM schema_version ORDER BY version")
            .fetch_all(&self.pool)
            .await
            .unwrap()
    }

    async fn catalog(&self) -> Vec<(String, String)> {
        #[cfg(feature = "postgres")]
        if self.schema.is_some() {
            // Ownership bookkeeping is intentionally performed before schema initialization.
            // Exclude only that existing runtime table and its index, not backend schema DDL.
            return sqlx::query_as(
                "SELECT 'relation:' || c.relname, c.relkind::text
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE n.nspname = current_schema() AND c.relname NOT IN
                     ('_eidetica_storage_owner', '_eidetica_storage_owner_pkey')
                 UNION ALL
                 SELECT 'column:' || table_name || ':' || column_name,
                     data_type || ':' || is_nullable || ':' || COALESCE(column_default, '')
                 FROM information_schema.columns
                 WHERE table_schema = current_schema() AND table_name <> '_eidetica_storage_owner'
                 UNION ALL
                 SELECT 'index:' || indexname, indexdef FROM pg_indexes
                 WHERE schemaname = current_schema() AND tablename <> '_eidetica_storage_owner'
                 UNION ALL
                 SELECT 'constraint:' || c.conname, pg_get_constraintdef(c.oid)
                 FROM pg_constraint c JOIN pg_namespace n ON n.oid = c.connamespace
                     JOIN pg_class t ON t.oid = c.conrelid
                 WHERE n.nspname = current_schema()
                     AND t.relname <> '_eidetica_storage_owner'
                 ORDER BY 1, 2",
            )
            .fetch_all(&self.pool)
            .await
            .unwrap();
        }
        sqlx::query_as(
            "SELECT type || ':' || name, COALESCE(sql, '') FROM sqlite_master ORDER BY 1, 2",
        )
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn populate(&self, backend: &SqlxBackend) {
        let root = Entry::root_builder()
            .set_subtree_data("data", b"original root".to_vec())
            .build()
            .unwrap();
        let root_id = root.id();
        backend.put(root).await.unwrap();
        let child = Entry::builder(root_id.clone())
            .add_parent(root_id.clone())
            .add_subtree_parent("data", root_id.clone())
            .set_subtree_data("data", b"original child".to_vec())
            .build()
            .unwrap();
        backend.put(child).await.unwrap();
        let signing_key = PrivateKey::generate();
        backend
            .set_instance_metadata(&InstanceMetadata {
                id: signing_key.public_key(),
                users_db: root_id.clone(),
                databases_db: root_id.clone(),
                sync_db: None,
            })
            .await
            .unwrap();
        backend
            .set_instance_secrets(&InstanceSecrets { signing_key })
            .await
            .unwrap();
        let token = backend
            .begin_store_state_staging(StoreStateRequest {
                database: root_id,
                store: "data".to_string(),
                lifecycle: StoreStateLifecycle::Authoritative,
                scope: CacheScope::Shared,
                projection: ProjectionDescriptor {
                    name: "test/opaque".to_string(),
                    version: 0,
                },
                source_key: b"source".to_vec(),
            })
            .await
            .unwrap();
        backend
            .stage_store_state_records(
                &token,
                [(b"key".to_vec(), Some(b"authority".to_vec()))].into(),
            )
            .await
            .unwrap();
        backend.publish_store_state(token).await.unwrap();
    }

    async fn content(&self) -> Content {
        Content {
            entries: sqlx::query_as("SELECT id, tree_id, is_root, verification_status, height, entry_cbor FROM entries ORDER BY id")
                .fetch_all(&self.pool).await.unwrap(),
            tree_parents: sqlx::query_as("SELECT child_id, parent_id FROM tree_parents ORDER BY 1, 2")
                .fetch_all(&self.pool).await.unwrap(),
            subtrees: sqlx::query_as("SELECT tree_id, entry_id, store_name, height, data FROM subtrees ORDER BY 1, 2, 3")
                .fetch_all(&self.pool).await.unwrap(),
            store_parents: sqlx::query_as("SELECT child_id, parent_id, store_name FROM store_parents ORDER BY 1, 2, 3")
                .fetch_all(&self.pool).await.unwrap(),
            tips: sqlx::query_as("SELECT entry_id, tree_id, store_name FROM tips ORDER BY 1, 2, 3")
                .fetch_all(&self.pool).await.unwrap(),
            metadata: sqlx::query_as("SELECT singleton, data FROM instance_metadata")
                .fetch_all(&self.pool).await.unwrap(),
            secrets: sqlx::query_as("SELECT singleton, data FROM instance_secrets")
                .fetch_all(&self.pool).await.unwrap(),
            namespaces: sqlx::query_as("SELECT namespace_id, database_id, store_name, lifecycle, status, scope_user_uuid, projection_name, projection_version, source_key, created_revision FROM store_state_namespaces ORDER BY namespace_id")
                .fetch_all(&self.pool).await.unwrap(),
            records: sqlx::query_as("SELECT namespace_id, record_key, record_value FROM store_state_records ORDER BY 1, 2")
                .fetch_all(&self.pool).await.unwrap(),
        }
    }
}

type EntryRow = (String, String, i64, i64, i64, Vec<u8>);
type SubtreeRow = (String, String, String, i64, Option<Vec<u8>>);
type NamespaceRow = (
    String,
    String,
    String,
    i64,
    i64,
    String,
    String,
    i64,
    Vec<u8>,
    Option<i64>,
);
type RecordRow = (String, Vec<u8>, Option<Vec<u8>>);

#[derive(Debug, PartialEq)]
struct Content {
    entries: Vec<EntryRow>,
    tree_parents: Vec<(String, String)>,
    subtrees: Vec<SubtreeRow>,
    store_parents: Vec<(String, String, String)>,
    tips: Vec<(String, String, String)>,
    metadata: Vec<(i64, String)>,
    secrets: Vec<(i64, String)>,
    namespaces: Vec<NamespaceRow>,
    records: Vec<RecordRow>,
}

async fn assert_future_refused(db: &TestDatabase, result: Result<SqlxBackend>) {
    match result {
        Ok(backend) => {
            db.close_backend(backend).await;
            panic!("a newer SQL schema must be refused");
        }
        Err(error) => {
            assert!(
                matches!(error, crate::Error::Backend(ref inner) if matches!(**inner, BackendError::SqlxError { .. })),
                "expected explicit backend version error, got {error}"
            );
            assert!(
                error.to_string().contains(&format!(
                    "Unsupported SQL schema version {}; supported version is {SCHEMA_VERSION}",
                    SCHEMA_VERSION + 1
                )),
                "unexpected error: {error}"
            );
        }
    }
}

#[tokio::test]
async fn future_schema_refused_before_initialization_ddl() {
    let Some(db) = TestDatabase::new().await else {
        return;
    };
    db.marker(Some(SCHEMA_VERSION + 1)).await;
    db.pool
        .execute("CREATE TABLE sentinel (value TEXT PRIMARY KEY)")
        .await
        .unwrap();
    db.pool
        .execute("INSERT INTO sentinel (value) VALUES ('untouched')")
        .await
        .unwrap();
    let before = db.catalog().await;
    let result = db.open().await;
    let after = db.catalog().await;
    let sentinel: Vec<(String,)> = sqlx::query_as("SELECT value FROM sentinel")
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        before, after,
        "refusal must not add or change tables, columns, constraints or indexes"
    );
    assert_eq!(sentinel, vec![("untouched".to_string(),)]);
    assert_eq!(db.versions().await, vec![(SCHEMA_VERSION + 1,)]);
    assert_future_refused(&db, result).await;
    db.cleanup().await;
}

#[tokio::test]
async fn future_schema_preserves_populated_authoritative_data() {
    let Some(db) = TestDatabase::new().await else {
        return;
    };
    let backend = db.open().await.unwrap();
    db.populate(&backend).await;
    db.close_backend(backend).await;
    sqlx::query("UPDATE schema_version SET version = $1")
        .bind(SCHEMA_VERSION + 1)
        .execute(&db.pool)
        .await
        .unwrap();
    let catalog = db.catalog().await;
    let content = db.content().await;
    assert_eq!(content.entries.len(), 2);
    assert_eq!(content.metadata.len(), 1);
    assert_eq!(content.secrets.len(), 1);
    assert_eq!(content.records.len(), 1);
    let result = db.open().await;
    assert_eq!(catalog, db.catalog().await);
    assert_eq!(content, db.content().await);
    assert_eq!(db.versions().await, vec![(SCHEMA_VERSION + 1,)]);
    assert_future_refused(&db, result).await;
    db.cleanup().await;
}

#[tokio::test]
async fn fresh_and_current_schema_initialize() {
    let Some(db) = TestDatabase::new().await else {
        return;
    };
    let backend = db.open().await.unwrap();
    db.populate(&backend).await;
    db.close_backend(backend).await;
    assert_eq!(db.versions().await, vec![(SCHEMA_VERSION,)]);
    let catalog = db.catalog().await;
    let content = db.content().await;
    let reopened = db.open().await.unwrap();
    assert_eq!(catalog, db.catalog().await);
    assert_eq!(content, db.content().await);
    db.close_backend(reopened).await;
    db.cleanup().await;
}

#[tokio::test]
async fn current_schema_gets_additive_store_state_tables() {
    let Some(db) = TestDatabase::new().await else {
        return;
    };
    let backend = db.open().await.unwrap();
    db.populate(&backend).await;
    db.close_backend(backend).await;
    let mut content = db.content().await;
    // Model the old version-0 layout without Store-state tables, retaining Entries
    // and instance metadata. Only this disposable fixture is altered.
    db.pool
        .execute("DROP TABLE store_state_records")
        .await
        .unwrap();
    db.pool
        .execute("DROP TABLE store_state_namespaces")
        .await
        .unwrap();
    content.namespaces.clear();
    content.records.clear();

    let backend = db.open().await.unwrap();
    assert_eq!(content, db.content().await);
    db.populate(&backend).await;
    assert_eq!(db.content().await.records.len(), 1);
    assert_eq!(db.versions().await, vec![(SCHEMA_VERSION,)]);
    db.close_backend(backend).await;
    db.cleanup().await;
}

#[tokio::test]
async fn absent_schema_version_row_initializes() {
    let Some(db) = TestDatabase::new().await else {
        return;
    };
    db.marker(None).await;
    let backend = db.open().await.unwrap();
    db.populate(&backend).await;
    assert_eq!(db.versions().await, vec![(SCHEMA_VERSION,)]);
    db.close_backend(backend).await;
    db.cleanup().await;
}

#[tokio::test]
async fn lower_schema_version_keeps_unknown_migration_error() {
    let Some(db) = TestDatabase::new().await else {
        return;
    };
    // v0 is the oldest migratable schema; nothing migrates from below it.
    let unsupported = -1;
    db.marker(Some(unsupported)).await;
    let error = match db.open().await {
        Ok(backend) => {
            db.close_backend(backend).await;
            panic!("no migration from below v0 is implemented");
        }
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains(&format!("Unknown migration path: v{unsupported} to v0")),
        "unexpected error: {error}"
    );
    assert_eq!(db.versions().await, vec![(unsupported,)]);
    db.cleanup().await;
}

include!("mainline_v0.rs");

/// Real mainline DDL, not a current database with its version marker changed.
async fn mainline_fixture(db: &TestDatabase) -> (Entry, Entry, Entry) {
    for statement in MAINLINE_V0.split(';').filter(|s| !s.trim().is_empty()) {
        let statement = statement.replace("BLOB", "BYTEA");
        db.pool
            .execute(sqlx::AssertSqlSafe(statement))
            .await
            .unwrap();
    }
    let root = Entry::root_builder().build().unwrap();
    let parent = Entry::builder(root.id())
        .add_parent(root.id())
        .set_subtree_data("test", b"parent")
        .build()
        .unwrap();
    let child = Entry::builder(root.id())
        .add_parent(parent.id())
        .add_subtree_parent("test", parent.id())
        .set_subtree_data("test", b"child")
        .build()
        .unwrap();
    for (entry, status) in [(&root, 0i64), (&parent, 2), (&child, 0)] {
        sqlx::query("INSERT INTO entries (id, tree_id, is_root, verification_status, height, entry_cbor) VALUES ($1,$2,$3,$4,$5,$6)")
            .bind(entry.id().to_string()).bind(root.id().to_string()).bind(i64::from(entry.is_root()))
            .bind(status).bind(99i64).bind(entry.to_dagcbor().unwrap()).execute(&db.pool).await.unwrap();
        for parent in entry.parents().unwrap() {
            sqlx::query("INSERT INTO tree_parents (child_id, parent_id) VALUES ($1,$2)")
                .bind(entry.id().to_string())
                .bind(parent.to_string())
                .execute(&db.pool)
                .await
                .unwrap();
        }
    }
    (root, parent, child)
}

#[tokio::test]
async fn mainline_migration_repairs_statuses_and_retains_only_tips() {
    use crate::backend::VerificationStatus;
    let Some(db) = TestDatabase::new().await else {
        return;
    };
    let (root, parent, child) = mainline_fixture(&db).await;
    let backend = db.open().await.unwrap();
    assert_eq!(db.versions().await, vec![(SCHEMA_VERSION,)]);
    assert_eq!(
        backend.get_verification_status(&child.id()).await.unwrap(),
        VerificationStatus::Unverified
    );
    assert_eq!(
        backend.verified_snapshot(&root.id()).await.unwrap().tips(),
        &[root.id()]
    );
    for entry in [&root, &parent, &child] {
        assert_eq!(&backend.get(&entry.id()).await.unwrap(), entry);
    }
    assert_eq!(
        backend
            .repair_verification(&root.id())
            .await
            .unwrap()
            .demoted,
        0
    );
    for entry in [&parent, &child] {
        backend
            .update_verification_status(&entry.id(), VerificationStatus::Verified)
            .await
            .unwrap();
    }
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM verified_tips")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        count, 1,
        "interior Verified entries must have no retained membership rows"
    );
    assert_eq!(
        backend.verified_snapshot(&root.id()).await.unwrap().tips(),
        &[child.id()]
    );
    db.close_backend(backend).await;
    db.cleanup().await;
}

/// Real v0 data survives migration and reopen; only the inconsistent child label
/// changes. Both derived and authoritative Store state retain their original keys.
#[tokio::test]
async fn mainline_migration_preserves_data_metadata_and_store_state() {
    let Some(db) = TestDatabase::new().await else {
        return;
    };
    let (root, parent, child) = mainline_fixture(&db).await;
    let secrets = InstanceSecrets {
        signing_key: PrivateKey::generate(),
    };
    let metadata = InstanceMetadata {
        id: secrets.signing_key.public_key(),
        users_db: root.id(),
        databases_db: root.id(),
        sync_db: Some(root.id()),
    };
    for (statement, data) in [
        (
            "INSERT INTO instance_metadata (singleton, data) VALUES (1, $1)",
            serde_json::to_string(&metadata).unwrap(),
        ),
        (
            "INSERT INTO instance_secrets (singleton, data) VALUES (1, $1)",
            serde_json::to_string(&secrets).unwrap(),
        ),
    ] {
        sqlx::query(statement)
            .bind(data)
            .execute(&db.pool)
            .await
            .unwrap();
    }
    for entry in [&parent, &child] {
        sqlx::query("INSERT INTO subtrees (tree_id, entry_id, store_name, height, data) VALUES ($1, $2, 'test', 99, $3)")
            .bind(root.id().to_string()).bind(entry.id().to_string())
            .bind(entry.data("test").unwrap()).execute(&db.pool).await.unwrap();
    }
    sqlx::query(
        "INSERT INTO store_parents (child_id, parent_id, store_name) VALUES ($1, $2, 'test')",
    )
    .bind(child.id().to_string())
    .bind(parent.id().to_string())
    .execute(&db.pool)
    .await
    .unwrap();
    // A pre-existing terminal rejection is not cleared by format migration.
    sqlx::query("UPDATE entries SET verification_status = $1 WHERE id = $2")
        .bind(crate::backend::VerificationStatus::Failed.as_db_int())
        .bind(parent.id().to_string())
        .execute(&db.pool)
        .await
        .unwrap();
    for store in ["", "test"] {
        sqlx::query("INSERT INTO tips (tree_id, entry_id, store_name) VALUES ($1, $2, $3)")
            .bind(root.id().to_string())
            .bind(child.id().to_string())
            .bind(store)
            .execute(&db.pool)
            .await
            .unwrap();
    }
    let request = StoreStateRequest {
        database: root.id(),
        store: "data".into(),
        lifecycle: StoreStateLifecycle::Derived,
        scope: CacheScope::Shared,
        projection: ProjectionDescriptor {
            name: "eidetica/opaque".into(),
            version: 0,
        },
        source_key: child.id().to_string().into_bytes(),
    };
    sqlx::query(
        "INSERT INTO store_state_namespaces
         (namespace_id, database_id, store_name, lifecycle, status, scope_user_uuid,
          projection_name, projection_version, source_key, created_revision)
         VALUES ('warm-mainline', $1, $2, $3, 1, '', $4, 0, $5, NULL)",
    )
    .bind(root.id().to_string())
    .bind(&request.store)
    .bind(request.lifecycle.as_db_int())
    .bind(&request.projection.name)
    .bind(&request.source_key)
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO store_state_records (namespace_id, record_key, record_value)
         VALUES ('warm-mainline', $1, $2)",
    )
    .bind(b"value".to_vec())
    .bind(b"unchanged mainline materialization".to_vec())
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO store_state_namespaces SELECT 'authoritative-mainline', database_id, store_name, $1, status, scope_user_uuid, projection_name, projection_version, source_key, created_revision FROM store_state_namespaces WHERE namespace_id = 'warm-mainline'")
        .bind(StoreStateLifecycle::Authoritative.as_db_int()).execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO store_state_records (namespace_id, record_key, record_value) VALUES ('authoritative-mainline', $1, $2)")
        .bind(b"key".to_vec()).bind(b"authoritative mainline record".to_vec())
        .execute(&db.pool).await.unwrap();
    let mut expected = db.content().await;
    assert_eq!(expected.entries.len(), 3);
    assert_eq!(expected.subtrees.len(), 2);
    assert_eq!(expected.tree_parents.len(), 2);
    assert_eq!(expected.store_parents.len(), 1);
    assert_eq!(expected.tips.len(), 2);
    assert_eq!(expected.metadata.len(), 1);
    assert_eq!(expected.secrets.len(), 1);
    assert_eq!(expected.namespaces.len(), 2);
    assert_eq!(expected.records.len(), 2);
    expected
        .entries
        .iter_mut()
        .find(|row| row.0 == child.id().to_string())
        .unwrap()
        .3 = crate::backend::VerificationStatus::Unverified.as_db_int();
    for _ in 0..2 {
        let backend = db.open().await.unwrap();
        assert_eq!(db.versions().await, vec![(SCHEMA_VERSION,)]);
        assert_eq!(
            backend.verified_snapshot(&root.id()).await.unwrap().tips(),
            &[root.id()]
        );
        let view = backend
            .resolve_store_state(&request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(view.namespace_id, "warm-mainline");
        assert_eq!(
            backend
                .store_state_record_get(&view, b"value")
                .await
                .unwrap(),
            Some(b"unchanged mainline materialization".to_vec())
        );
        assert_eq!(db.content().await, expected);
        db.close_backend(backend).await;
    }
    db.cleanup().await;
}

#[tokio::test]
async fn migration_failure_rolls_back_schema_statuses_and_version() {
    let Some(db) = TestDatabase::new().await else {
        return;
    };
    let (_, _, child) = mainline_fixture(&db).await;
    sqlx::query("UPDATE entries SET entry_cbor = $1 WHERE id = $2")
        .bind(b"invalid".to_vec())
        .bind(child.id().to_string())
        .execute(&db.pool)
        .await
        .unwrap();
    let before_catalog = db.catalog().await;
    let before_content = db.content().await;
    assert!(db.open().await.is_err());
    assert_eq!(db.versions().await, vec![(0,)]);
    assert_eq!(db.catalog().await, before_catalog);
    assert_eq!(db.content().await, before_content);
    db.cleanup().await;
}

#[tokio::test]
async fn candidate_prefix_format_is_refused_without_ddl_or_data_changes() {
    let Some(db) = TestDatabase::new().await else {
        return;
    };
    mainline_fixture(&db).await;
    db.pool
        .execute("CREATE TABLE verified_prefix (tree_id TEXT, entry_id TEXT, is_frontier BIGINT)")
        .await
        .unwrap();
    db.pool
        .execute("UPDATE schema_version SET version = 1")
        .await
        .unwrap();
    let before_catalog = db.catalog().await;
    let before_content = db.content().await;
    let error = match db.open().await {
        Ok(_) => panic!("old candidate must be refused"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("Unsupported prefix-tracking candidate format"),
        "{error}"
    );
    assert_eq!(db.versions().await, vec![(1,)]);
    assert_eq!(db.catalog().await, before_catalog);
    assert_eq!(db.content().await, before_content);
    db.cleanup().await;
}

#[tokio::test]
async fn status_and_tip_failure_roll_back_together() {
    use crate::backend::VerificationStatus;
    let Some(db) = TestDatabase::new().await else {
        return;
    };
    let (root, parent, child) = mainline_fixture(&db).await;
    let backend = db.open().await.unwrap();
    for entry in [&parent, &child] {
        backend
            .update_verification_status(&entry.id(), VerificationStatus::Verified)
            .await
            .unwrap();
    }
    let next = Entry::builder(root.id())
        .add_parent(child.id())
        .set_subtree_data("test", b"next")
        .build()
        .unwrap();
    backend.put(next.clone()).await.unwrap();
    if backend.is_sqlite() {
        db.pool.execute("CREATE TRIGGER fail_tip BEFORE INSERT ON verified_tips BEGIN SELECT RAISE(ABORT, 'injected tip failure'); END").await.unwrap();
    } else {
        db.pool.execute("CREATE FUNCTION fail_tip() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected tip failure'; END $$").await.unwrap();
        db.pool.execute("CREATE TRIGGER fail_tip BEFORE INSERT ON verified_tips FOR EACH ROW EXECUTE FUNCTION fail_tip()").await.unwrap();
    }
    let before = db.content().await;
    let tips = backend.verified_snapshot(&root.id()).await.unwrap();
    assert!(
        backend
            .update_verification_status(&parent.id(), VerificationStatus::Unverified)
            .await
            .is_err()
    );
    assert_eq!(db.content().await, before);
    assert_eq!(backend.verified_snapshot(&root.id()).await.unwrap(), tips);
    assert!(
        backend
            .update_verification_status(&next.id(), VerificationStatus::Verified)
            .await
            .is_err()
    );
    assert_eq!(db.content().await, before);
    assert_eq!(backend.verified_snapshot(&root.id()).await.unwrap(), tips);
    assert!(backend.repair_verification(&root.id()).await.is_err());
    assert_eq!(db.content().await, before);
    assert_eq!(backend.verified_snapshot(&root.id()).await.unwrap(), tips);
    db.close_backend(backend).await;
    db.cleanup().await;
}
