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
        // Reuse the existing awaited release barrier rather than race production Drop.
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
                matches!(error, crate::Error::Backend(ref inner) if matches!(**inner, BackendError::SqlxError { source: None, .. })),
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
    db.marker(Some(SCHEMA_VERSION - 1)).await;
    let error = match db.open().await {
        Ok(backend) => {
            db.close_backend(backend).await;
            panic!("no lower-version migration is implemented");
        }
        Err(error) => error,
    };
    assert!(
        error.to_string().contains(&format!(
            "Unknown migration path: v{} to v{SCHEMA_VERSION}",
            SCHEMA_VERSION - 1
        )),
        "unexpected error: {error}"
    );
    assert_eq!(db.versions().await, vec![(SCHEMA_VERSION - 1,)]);
    db.cleanup().await;
}
