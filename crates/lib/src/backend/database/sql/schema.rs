//! SQL schema definitions and migrations.
//!
//! This module contains the database schema used by SQL backends.
//! The schema is designed to be portable between SQLite and Postgres.
//!
//! # Migration System
//!
//! The migration system uses code-based migrations rather than SQL files to handle
//! dialect differences between SQLite and PostgreSQL. Each migration is a function
//! that receives the backend and can execute database-specific SQL as needed.
//!
//! ## Adding a New Migration
//!
//! 1. Increment `SCHEMA_VERSION`
//! 2. Add a new `migrate_vN_to_vM` async function
//! 3. Have that function update `schema_version` to its target version inside
//!    the same transaction as its schema changes, so the version write and the
//!    schema change commit or roll back together
//! 4. Add the migration to the match statement in `run_migration`
//! 5. Document what the migration does

use crate::Result;
use crate::backend::errors::BackendError;

use super::{SqlxBackend, SqlxResultExt};

/// Current schema version.
///
/// Increment this when making schema changes that require migration.
/// Version 0 is fully unstable and should not be used in production.
pub const SCHEMA_VERSION: i64 = 2;

const CREATE_STORE_STATE_TABLES: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS store_state_namespaces (
        namespace_id TEXT PRIMARY KEY NOT NULL,
        database_id TEXT NOT NULL,
        store_name TEXT NOT NULL,
        lifecycle BIGINT NOT NULL,
        status BIGINT NOT NULL,
        scope_user_uuid TEXT NOT NULL,
        projection_name TEXT NOT NULL,
        projection_version BIGINT NOT NULL,
        source_key BYTEA NOT NULL,
        created_revision BIGINT,
        UNIQUE (database_id, store_name, lifecycle, status, scope_user_uuid,
                projection_name, projection_version, source_key)
    )",
    "CREATE TABLE IF NOT EXISTS store_state_records (
        namespace_id TEXT NOT NULL,
        record_key BYTEA NOT NULL,
        record_value BYTEA,
        PRIMARY KEY (namespace_id, record_key),
        -- sqlx enables SQLite foreign keys on every connection.
        FOREIGN KEY (namespace_id) REFERENCES store_state_namespaces(namespace_id)
            ON DELETE CASCADE
    )",
];

const CREATE_HISTORYLESS_V1_TABLES: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS historyless_databases (
        id TEXT PRIMARY KEY NOT NULL,
        owner_user_uuid TEXT,
        revision BIGINT NOT NULL CHECK (revision >= 0)
    )",
    "CREATE TABLE IF NOT EXISTS historyless_stores (
        database_id TEXT NOT NULL,
        store_name TEXT NOT NULL,
        state BLOB NOT NULL,
        PRIMARY KEY (database_id, store_name)
    )",
];

const CREATE_HISTORYLESS_V2_TABLES: &[&str] =
    &["CREATE TABLE IF NOT EXISTS historyless_store_heads (
        database_id TEXT NOT NULL,
        store_name TEXT NOT NULL,
        namespace_id TEXT NOT NULL UNIQUE,
        PRIMARY KEY (database_id, store_name),
        FOREIGN KEY (namespace_id) REFERENCES store_state_namespaces(namespace_id)
            ON DELETE CASCADE
    )"];

/// SQL statements to create the schema tables.
///
/// Each statement uses portable SQL that works on both SQLite and PostgreSQL.
pub const CREATE_TABLES: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS schema_version (
        version BIGINT PRIMARY KEY
    )",
    "CREATE TABLE IF NOT EXISTS entries (
        id TEXT PRIMARY KEY NOT NULL,
        tree_id TEXT NOT NULL,
        is_root BIGINT NOT NULL DEFAULT 0,
        verification_status BIGINT NOT NULL DEFAULT 0,
        height BIGINT NOT NULL DEFAULT 0,
        entry_cbor BYTEA NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS tree_parents (
        child_id TEXT NOT NULL,
        parent_id TEXT NOT NULL,
        PRIMARY KEY (child_id, parent_id)
    )",
    "CREATE TABLE IF NOT EXISTS subtrees (
        tree_id TEXT NOT NULL,
        entry_id TEXT NOT NULL,
        store_name TEXT NOT NULL,
        height BIGINT NOT NULL,
        data BLOB,
        PRIMARY KEY (entry_id, store_name)
    )",
    "CREATE TABLE IF NOT EXISTS store_parents (
        child_id TEXT NOT NULL,
        parent_id TEXT NOT NULL,
        store_name TEXT NOT NULL,
        PRIMARY KEY (child_id, parent_id, store_name)
    )",
    "CREATE TABLE IF NOT EXISTS tips (
        entry_id TEXT NOT NULL,
        tree_id TEXT NOT NULL,
        store_name TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (entry_id, tree_id, store_name)
    )",
    "CREATE TABLE IF NOT EXISTS instance_metadata (
        singleton BIGINT PRIMARY KEY DEFAULT 1 CHECK (singleton = 1),
        data TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS instance_secrets (
        singleton BIGINT PRIMARY KEY DEFAULT 1 CHECK (singleton = 1),
        data TEXT NOT NULL
    )",
];

/// SQL statements to create indexes.
pub const CREATE_INDEXES: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS idx_entries_tree_id ON entries(tree_id)",
    "CREATE INDEX IF NOT EXISTS idx_entries_tree_height ON entries(tree_id, height DESC, id)",
    "CREATE INDEX IF NOT EXISTS idx_entries_verification ON entries(verification_status)",
    "CREATE INDEX IF NOT EXISTS idx_entries_is_root ON entries(is_root)",
    "CREATE INDEX IF NOT EXISTS idx_tree_parents_parent ON tree_parents(parent_id)",
    "CREATE INDEX IF NOT EXISTS idx_tree_parents_child ON tree_parents(child_id)",
    "CREATE INDEX IF NOT EXISTS idx_subtrees_tree_store_height ON subtrees(tree_id, store_name, height DESC, entry_id)",
    "CREATE INDEX IF NOT EXISTS idx_subtrees_store_height ON subtrees(store_name, height DESC, entry_id)",
    "CREATE INDEX IF NOT EXISTS idx_store_parents_parent ON store_parents(store_name, parent_id)",
    "CREATE INDEX IF NOT EXISTS idx_store_parents_child ON store_parents(store_name, child_id)",
    "CREATE INDEX IF NOT EXISTS idx_tips_tree_store ON tips(tree_id, store_name)",
];

/// Initialize the database schema.
pub async fn initialize(backend: &SqlxBackend) -> Result<()> {
    let pool = backend.pool();
    sqlx::query(CREATE_TABLES[0])
        .execute(pool)
        .await
        .sql_context("Schema version table creation failed")?;
    let row: Option<(i64,)> = sqlx::query_as("SELECT version FROM schema_version")
        .fetch_optional(pool)
        .await
        .sql_context("Failed to check schema version")?;

    if let Some((current_version,)) = row
        && current_version > SCHEMA_VERSION
    {
        return Err(BackendError::SqlxError {
            reason: format!(
                "Database schema version {current_version} is newer than supported version {SCHEMA_VERSION}"
            ),
            source: None,
        }
        .into());
    }

    let blob_type = if backend.is_sqlite() { "BLOB" } else { "BYTEA" };
    for statement in &CREATE_TABLES[1..] {
        sqlx::query(&statement.replace("BLOB", blob_type))
            .execute(pool)
            .await
            .sql_context("Schema creation failed")?;
    }

    // The generic Store-state tables are part of schema v0 and remain created
    // independently of the versioned historyless migration.
    initialize_store_state_tables(backend).await?;

    if row.is_none() {
        initialize_current_version(backend).await?;
    } else if let Some((current_version,)) = row
        && current_version < SCHEMA_VERSION
    {
        migrate(backend, current_version, SCHEMA_VERSION).await?;
    }

    for statement in CREATE_INDEXES {
        sqlx::query(statement)
            .execute(pool)
            .await
            .sql_context("Index creation failed")?;
    }
    Ok(())
}

async fn initialize_store_state_tables(backend: &SqlxBackend) -> Result<()> {
    let mut tx = backend
        .pool()
        .begin()
        .await
        .sql_context("Failed to begin Store-state schema initialization")?;
    let blob_type = if backend.is_sqlite() { "BLOB" } else { "BYTEA" };
    for statement in CREATE_STORE_STATE_TABLES {
        sqlx::query(&statement.replace("BYTEA", blob_type))
            .execute(&mut *tx)
            .await
            .sql_context("Failed to create Store-state tables")?;
    }
    tx.commit()
        .await
        .sql_context("Failed to commit Store-state table initialization")
}

async fn initialize_current_version(backend: &SqlxBackend) -> Result<()> {
    let mut tx = backend
        .pool()
        .begin()
        .await
        .sql_context("Failed to begin historyless schema initialization")?;
    let blob_type = if backend.is_sqlite() { "BLOB" } else { "BYTEA" };
    for statement in CREATE_HISTORYLESS_V1_TABLES
        .iter()
        .chain(CREATE_HISTORYLESS_V2_TABLES)
    {
        sqlx::query(&statement.replace("BLOB", blob_type))
            .execute(&mut *tx)
            .await
            .sql_context("Failed to create historyless tables")?;
    }
    sqlx::query("INSERT INTO schema_version (version) VALUES ($1)")
        .bind(SCHEMA_VERSION)
        .execute(&mut *tx)
        .await
        .sql_context("Failed to initialize schema version")?;
    tx.commit()
        .await
        .sql_context("Failed to commit historyless schema initialization")
}

async fn migrate(backend: &SqlxBackend, from: i64, to: i64) -> Result<()> {
    tracing::info!(from, to, "Starting SQL schema migration");
    let mut current = from;
    while current < to {
        let next = current + 1;
        tracing::info!(from = current, to = next, "Running migration");
        run_migration(backend, current, next).await?;
        tracing::info!(version = next, "Migration completed");
        current = next;
    }
    tracing::info!(from, to, "All migrations completed successfully");
    Ok(())
}

async fn run_migration(backend: &SqlxBackend, from: i64, to: i64) -> Result<()> {
    match (from, to) {
        (0, 1) => migrate_v0_to_v1(backend).await,
        (1, 2) => migrate_v1_to_v2(backend).await,
        _ => Err(BackendError::SqlxError {
            reason: format!("Unknown migration path: v{from} to v{to}"),
            source: None,
        }
        .into()),
    }
}

async fn migrate_v0_to_v1(backend: &SqlxBackend) -> Result<()> {
    let mut tx = backend
        .pool()
        .begin()
        .await
        .sql_context("Failed to begin v0 to v1 migration")?;
    let blob_type = if backend.is_sqlite() { "BLOB" } else { "BYTEA" };
    for statement in CREATE_HISTORYLESS_V1_TABLES {
        sqlx::query(&statement.replace("BLOB", blob_type))
            .execute(&mut *tx)
            .await
            .sql_context("Failed to create historyless tables")?;
    }
    sqlx::query("UPDATE schema_version SET version = 1")
        .execute(&mut *tx)
        .await
        .sql_context("Failed to update schema version")?;
    tx.commit()
        .await
        .sql_context("Failed to commit v0 to v1 migration")
}

async fn migrate_v1_to_v2(backend: &SqlxBackend) -> Result<()> {
    let mut tx = backend
        .pool()
        .begin()
        .await
        .sql_context("Failed to begin v1 to v2 migration")?;
    let blob_type = if backend.is_sqlite() { "BLOB" } else { "BYTEA" };
    for statement in CREATE_STORE_STATE_TABLES {
        sqlx::query(&statement.replace("BYTEA", blob_type))
            .execute(&mut *tx)
            .await
            .sql_context("Failed to create Store-state tables")?;
    }
    for statement in CREATE_HISTORYLESS_V2_TABLES {
        sqlx::query(&statement.replace("BLOB", blob_type))
            .execute(&mut *tx)
            .await
            .sql_context("Failed to create historyless Store heads")?;
    }
    let legacy_rows: Vec<(String, String, Vec<u8>, i64)> = sqlx::query_as(
        "SELECT s.database_id, s.store_name, s.state, d.revision
         FROM historyless_stores s
         JOIN historyless_databases d ON d.id = s.database_id",
    )
    .fetch_all(&mut *tx)
    .await
    .sql_context("Failed to read legacy historyless Stores")?;
    for (database, store, state, revision) in legacy_rows {
        let namespace = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO store_state_namespaces
             (namespace_id, database_id, store_name, lifecycle, status, scope_user_uuid,
              projection_name, projection_version, source_key, created_revision)
             VALUES ($1, $2, $3, 1, 1, '',
                     'eidetica/legacy-historyless-whole-state', 1, $4, $5)",
        )
        .bind(&namespace)
        .bind(&database)
        .bind(&store)
        .bind(Vec::<u8>::new())
        .bind(revision)
        .execute(&mut *tx)
        .await
        .sql_context("Failed to migrate legacy historyless namespace")?;
        sqlx::query(
            "INSERT INTO store_state_records (namespace_id, record_key, record_value)
             VALUES ($1, $2, $3)",
        )
        .bind(&namespace)
        .bind(vec![0u8])
        .bind(state)
        .execute(&mut *tx)
        .await
        .sql_context("Failed to migrate legacy historyless state")?;
        sqlx::query(
            "INSERT INTO historyless_store_heads (database_id, store_name, namespace_id)
             VALUES ($1, $2, $3)",
        )
        .bind(database)
        .bind(store)
        .bind(namespace)
        .execute(&mut *tx)
        .await
        .sql_context("Failed to migrate legacy historyless head")?;
    }
    sqlx::query("UPDATE schema_version SET version = 2")
        .execute(&mut *tx)
        .await
        .sql_context("Failed to update schema version")?;
    tx.commit()
        .await
        .sql_context("Failed to commit v1 to v2 migration")
}

#[cfg(feature = "testing")]
pub(crate) async fn testing_prepare_historyless_v0(backend: &SqlxBackend) -> Result<()> {
    let mut tx = backend
        .pool()
        .begin()
        .await
        .sql_context("Failed to begin historyless v0 fixture setup")?;
    sqlx::query("UPDATE schema_version SET version = 0")
        .execute(&mut *tx)
        .await
        .sql_context("Failed to set schema version 0")?;
    for statement in [
        "DROP TABLE historyless_store_heads",
        "DROP TABLE historyless_stores",
        "DROP TABLE historyless_databases",
    ] {
        sqlx::query(statement)
            .execute(&mut *tx)
            .await
            .sql_context("Failed to remove historyless table from v0 fixture")?;
    }
    tx.commit()
        .await
        .sql_context("Failed to commit historyless v0 fixture setup")
}

#[cfg(feature = "testing")]
pub(crate) async fn testing_set_schema_version(backend: &SqlxBackend, version: i64) -> Result<()> {
    sqlx::query("UPDATE schema_version SET version = $1")
        .bind(version)
        .execute(backend.pool())
        .await
        .sql_context("Failed to set test schema version")?;
    Ok(())
}

#[cfg(feature = "testing")]
pub(crate) async fn testing_add_future_schema_marker(backend: &SqlxBackend) -> Result<()> {
    sqlx::query("CREATE TABLE future_schema_marker (id BIGINT PRIMARY KEY)")
        .execute(backend.pool())
        .await
        .sql_context("Failed to add future schema marker")?;
    Ok(())
}

#[cfg(feature = "testing")]
pub(crate) async fn testing_seed_historyless_v1(
    backend: &SqlxBackend,
    id: &crate::entry::ID,
    store: &str,
    state: &[u8],
    revision: i64,
) -> Result<()> {
    let mut tx = backend
        .pool()
        .begin()
        .await
        .sql_context("Failed to begin historyless v1 fixture setup")?;
    sqlx::query("DELETE FROM historyless_store_heads")
        .execute(&mut *tx)
        .await
        .sql_context("Failed to clear historyless heads")?;
    sqlx::query("DELETE FROM store_state_records")
        .execute(&mut *tx)
        .await
        .sql_context("Failed to clear Store-state records")?;
    sqlx::query("DELETE FROM store_state_namespaces")
        .execute(&mut *tx)
        .await
        .sql_context("Failed to clear Store-state namespaces")?;
    sqlx::query(
        "INSERT INTO historyless_databases (id, owner_user_uuid, revision) VALUES ($1, NULL, $2)",
    )
    .bind(id.to_string())
    .bind(revision)
    .execute(&mut *tx)
    .await
    .sql_context("Failed to seed legacy historyless database")?;
    sqlx::query(
        "INSERT INTO historyless_stores (database_id, store_name, state) VALUES ($1, $2, $3)",
    )
    .bind(id.to_string())
    .bind(store)
    .bind(state)
    .execute(&mut *tx)
    .await
    .sql_context("Failed to seed legacy historyless Store")?;
    sqlx::query("UPDATE schema_version SET version = 1")
        .execute(&mut *tx)
        .await
        .sql_context("Failed to set schema version 1")?;
    tx.commit()
        .await
        .sql_context("Failed to commit historyless v1 fixture setup")
}

#[cfg(feature = "testing")]
pub(crate) async fn testing_historyless_state(
    backend: &SqlxBackend,
    id: &crate::entry::ID,
    store: &str,
) -> Result<super::SqlHistorylessTestState> {
    let (schema_version,): (i64,) = sqlx::query_as("SELECT version FROM schema_version")
        .fetch_one(backend.pool())
        .await
        .sql_context("Failed to inspect schema version")?;
    let (database_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM historyless_databases")
        .fetch_one(backend.pool())
        .await
        .sql_context("Failed to inspect historyless databases")?;
    let (legacy_store_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM historyless_stores")
        .fetch_one(backend.pool())
        .await
        .sql_context("Failed to inspect legacy historyless Stores")?;
    let revision: Option<(i64,)> =
        sqlx::query_as("SELECT revision FROM historyless_databases WHERE id = $1")
            .bind(id.to_string())
            .fetch_optional(backend.pool())
            .await
            .sql_context("Failed to inspect historyless revision")?;
    let record: Option<(String, i64, Vec<u8>)> = sqlx::query_as(
        "SELECT n.projection_name, n.projection_version, r.record_value
         FROM historyless_store_heads h
         JOIN store_state_namespaces n ON n.namespace_id = h.namespace_id
         JOIN store_state_records r ON r.namespace_id = n.namespace_id
         WHERE h.database_id = $1 AND h.store_name = $2 AND r.record_key = $3",
    )
    .bind(id.to_string())
    .bind(store)
    .bind(vec![0u8])
    .fetch_optional(backend.pool())
    .await
    .sql_context("Failed to inspect historyless Store state")?;
    let (projection_name, projection_version, record_value) = match record {
        Some((name, version, value)) => (Some(name), Some(version), Some(value)),
        None => (None, None, None),
    };
    Ok(super::SqlHistorylessTestState {
        schema_version,
        database_count,
        legacy_store_count,
        revision: revision.map(|(revision,)| revision),
        projection_name,
        projection_version,
        record_value,
    })
}

#[cfg(feature = "testing")]
pub(crate) async fn testing_historyless_namespace_count(
    backend: &SqlxBackend,
    id: &crate::entry::ID,
    store: &str,
) -> Result<i64> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM store_state_namespaces
         WHERE database_id = $1 AND store_name = $2 AND lifecycle = 1 AND status = 1",
    )
    .bind(id.to_string())
    .bind(store)
    .fetch_one(backend.pool())
    .await
    .sql_context("Failed to count historyless Store revisions")?;
    Ok(count)
}

#[cfg(all(feature = "sqlite", feature = "testing"))]
pub(crate) async fn testing_set_sqlite_historyless_revision(
    backend: &SqlxBackend,
    id: &crate::entry::ID,
    revision: i64,
) -> Result<()> {
    let mut connection = backend
        .pool()
        .acquire()
        .await
        .sql_context("Failed to acquire historyless corruption test connection")?;
    sqlx::query("PRAGMA ignore_check_constraints = ON")
        .execute(&mut *connection)
        .await
        .sql_context("Failed to disable SQLite checks for corruption test")?;
    sqlx::query("UPDATE historyless_databases SET revision = $1 WHERE id = $2")
        .bind(revision)
        .bind(id.to_string())
        .execute(&mut *connection)
        .await
        .sql_context("Failed to set historyless corruption test revision")?;
    Ok(())
}
