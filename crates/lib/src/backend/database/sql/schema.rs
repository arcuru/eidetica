//! SQL schema definitions and migrations.
//!
//! This module contains the database schema used by SQL backends.
//! The schema is designed to be portable between SQLite and Postgres.
//!
//! Mainline v0 migrates directly to tips-only v2 in one transaction. Version 1
//! belonged to an unsupported prefix-tracking candidate and is refused before
//! schema writes. Ordinary v2 opens do not reconstruct verification history.

use crate::Result;
use crate::backend::errors::BackendError;

use super::{SqlxBackend, SqlxResultExt};

#[cfg(test)]
mod tests;

/// Internal SQL format revision counter, independent of release versions and
/// the InMemory persistence counter. Increment for changes requiring migration,
/// with an explicit forward path from each supported stored format.
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
        -- Total record bytes (key plus value) of the published namespace.
        -- Computed atomically in the publish transaction; bounds the cache
        -- by bytes without scanning records on every eviction check.
        size_bytes BIGINT NOT NULL DEFAULT 0,
        -- Last-access tick for durable approximate LRU. Assigned from a
        -- per-backend monotonic clock, touched in memory on every hit and
        -- flushed in batches (never one write per hit). A crash loses only
        -- recent recency metadata, never cached values. Rows predating this
        -- column read as 0 and sort as coldest.
        last_access_tick BIGINT NOT NULL DEFAULT 0,
        UNIQUE (database_id, store_name, lifecycle, status, scope_user_uuid,
                projection_name, projection_version, source_key)
    )",
    "CREATE TABLE IF NOT EXISTS store_state_records (
        namespace_id TEXT NOT NULL,
        record_key BYTEA NOT NULL,
        record_value BYTEA,
        PRIMARY KEY (namespace_id, record_key),
        -- Dropping a namespace drops its records. SQLite enforces this because
        -- sqlx sets `PRAGMA foreign_keys = ON` on every connection it opens;
        -- it is off in a bare sqlite3 session, which makes this look inert.
        FOREIGN KEY (namespace_id) REFERENCES store_state_namespaces(namespace_id)
            ON DELETE CASCADE
    )",
];

/// SQL statements to create the schema tables.
///
/// Each statement uses portable SQL that works on both SQLite and PostgreSQL.
pub const CREATE_TABLES: &[&str] = &[
    // Schema version tracking
    // BIGINT (64-bit) used for portability between SQLite and PostgreSQL
    "CREATE TABLE IF NOT EXISTS schema_version (
        version BIGINT PRIMARY KEY
    )",
    // Core entry storage
    // Entries are content-addressable via hash of entry content
    "CREATE TABLE IF NOT EXISTS entries (
        id TEXT PRIMARY KEY NOT NULL,
        tree_id TEXT NOT NULL,
        is_root BIGINT NOT NULL DEFAULT 0,
        verification_status BIGINT NOT NULL DEFAULT 0,
        height BIGINT NOT NULL DEFAULT 0,
        entry_cbor BYTEA NOT NULL
    )",
    // Tree parent relationships (main tree DAG edges)
    // Each entry can have multiple parents for merge commits
    "CREATE TABLE IF NOT EXISTS tree_parents (
        child_id TEXT NOT NULL,
        parent_id TEXT NOT NULL,
        PRIMARY KEY (child_id, parent_id)
    )",
    // Subtrees - denormalized subtree data for efficient queries
    // Replaces store_memberships with additional columns for height and data.
    // `data` is the opaque payload bytes for each store (format chosen by the store).
    "CREATE TABLE IF NOT EXISTS subtrees (
        tree_id TEXT NOT NULL,
        entry_id TEXT NOT NULL,
        store_name TEXT NOT NULL,
        height BIGINT NOT NULL,
        data BLOB,
        PRIMARY KEY (entry_id, store_name)
    )",
    // Store parent relationships (per-store DAG edges)
    // Parents within a specific store context
    "CREATE TABLE IF NOT EXISTS store_parents (
        child_id TEXT NOT NULL,
        parent_id TEXT NOT NULL,
        store_name TEXT NOT NULL,
        PRIMARY KEY (child_id, parent_id, store_name)
    )",
    // Tips cache - maintained incrementally
    // Tips are entries with no children in their tree/store context
    // store_name uses empty string for tree-level tips (PostgreSQL disallows NULL in PK)
    "CREATE TABLE IF NOT EXISTS tips (
        entry_id TEXT NOT NULL,
        tree_id TEXT NOT NULL,
        store_name TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (entry_id, tree_id, store_name)
    )",
    // Only frontier-width verification state; statuses prove interior membership.
    "CREATE TABLE IF NOT EXISTS verified_tips (
        tree_id TEXT NOT NULL,
        entry_id TEXT NOT NULL,
        PRIMARY KEY (tree_id, entry_id)
    )",
    // Instance metadata (singleton row pattern)
    // Contains device key and system database IDs.
    // Uses singleton=1 constraint to ensure only one row exists.
    "CREATE TABLE IF NOT EXISTS instance_metadata (
        singleton BIGINT PRIMARY KEY DEFAULT 1 CHECK (singleton = 1),
        data TEXT NOT NULL
    )",
    // Instance secrets (singleton row pattern)
    // Contains device signing key. Stored separately from metadata.
    "CREATE TABLE IF NOT EXISTS instance_secrets (
        singleton BIGINT PRIMARY KEY DEFAULT 1 CHECK (singleton = 1),
        data TEXT NOT NULL
    )",
];

/// SQL statements to create indexes.
pub const CREATE_INDEXES: &[&str] = &[
    // Entry lookups and filtering
    "CREATE INDEX IF NOT EXISTS idx_entries_tree_id ON entries(tree_id)",
    "CREATE INDEX IF NOT EXISTS idx_entries_tree_height ON entries(tree_id, height DESC, id)",
    "CREATE INDEX IF NOT EXISTS idx_entries_verification ON entries(verification_status)",
    "CREATE INDEX IF NOT EXISTS idx_entries_is_root ON entries(is_root)",
    // Parent relationship traversal
    "CREATE INDEX IF NOT EXISTS idx_tree_parents_parent ON tree_parents(parent_id)",
    "CREATE INDEX IF NOT EXISTS idx_tree_parents_child ON tree_parents(child_id)",
    // Store-specific queries
    "CREATE INDEX IF NOT EXISTS idx_subtrees_tree_store_height ON subtrees(tree_id, store_name, height DESC, entry_id)",
    "CREATE INDEX IF NOT EXISTS idx_subtrees_store_height ON subtrees(store_name, height DESC, entry_id)",
    "CREATE INDEX IF NOT EXISTS idx_store_parents_parent ON store_parents(store_name, parent_id)",
    "CREATE INDEX IF NOT EXISTS idx_store_parents_child ON store_parents(store_name, child_id)",
    // Tip lookups
    "CREATE INDEX IF NOT EXISTS idx_tips_tree_store ON tips(tree_id, store_name)",
];

/// Initialize the database schema.
///
/// Creates tables and indexes if they don't exist, and handles migrations
/// if the schema version has changed. Refuses newer schemas before initialization DDL.
pub async fn initialize(backend: &SqlxBackend) -> Result<()> {
    let pool = backend.pool();

    // A fresh database has no version table. Inspect existing markers before any
    // schema DDL so an unsupported database is not modified by initialization.
    let exists_query = if backend.is_sqlite() {
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_version'"
    } else {
        "SELECT COUNT(*) FROM pg_catalog.pg_class WHERE oid = to_regclass('schema_version')"
    };
    let (exists,): (i64,) = sqlx::query_as(exists_query)
        .fetch_one(pool)
        .await
        .sql_context("Failed to check schema version table")?;
    let row: Option<(i64,)> = if exists > 0 {
        sqlx::query_as("SELECT version FROM schema_version")
            .fetch_optional(pool)
            .await
            .sql_context("Failed to check schema version")?
    } else {
        None
    };
    if let Some((stored_version,)) = row
        && stored_version > SCHEMA_VERSION
    {
        return Err(BackendError::SqlxError {
            reason: format!(
                "Unsupported SQL schema version {stored_version}; supported version is {SCHEMA_VERSION}"
            ),
            source: None,
        }
        .into());
    }

    // Version 1 was an unreleased prefix-tracking candidate, not mainline.
    // Never reinterpret it or silently erase its data. A trust reset is not a migration.
    let prefix_query = if backend.is_sqlite() {
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'verified_prefix'"
    } else {
        "SELECT COUNT(*) FROM pg_catalog.pg_class WHERE oid = to_regclass('verified_prefix')"
    };
    let (prefix_tables,): (i64,) = sqlx::query_as(prefix_query)
        .fetch_one(pool)
        .await
        .sql_context("Failed to inspect legacy verification storage")?;
    if row == Some((1,)) || prefix_tables != 0 {
        return Err(BackendError::SqlxError {
            reason: "Unsupported prefix-tracking candidate format; back up and recreate with a supported format (trust reset does not convert schemas)".into(),
            source: None,
        }.into());
    }
    if let Some((version,)) = row
        && version < 0
    {
        return Err(BackendError::SqlxError {
            reason: format!("Unknown migration path: v{version} to v{}", version + 1),
            source: None,
        }
        .into());
    }

    // Schema, repair, cache-column backfill, and version publication roll back
    // together. Backend ownership excludes other instances during open.
    let mut tx = pool
        .begin()
        .await
        .sql_context("Failed to begin schema initialization")?;
    if backend.is_sqlite() {
        sqlx::query("COMMIT; BEGIN IMMEDIATE")
            .execute(&mut *tx)
            .await
            .sql_context("Failed to lock schema initialization")?;
    }
    let blob_type = if backend.is_sqlite() { "BLOB" } else { "BYTEA" };
    for statement in CREATE_TABLES.iter().chain(CREATE_STORE_STATE_TABLES) {
        // SAFETY: schema statements and dialect replacements are constants.
        let statement = statement
            .replace("BYTEA", blob_type)
            .replace("BLOB", blob_type);
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(&mut *tx)
            .await
            .sql_context("Schema creation failed")?;
    }
    ensure_store_state_cache_columns(backend, &mut tx).await?;
    for statement in CREATE_INDEXES {
        sqlx::query(*statement)
            .execute(&mut *tx)
            .await
            .sql_context("Index creation failed")?;
    }
    if row == Some((0,)) {
        let trees: Vec<(String,)> = sqlx::query_as("SELECT DISTINCT tree_id FROM entries")
            .fetch_all(&mut *tx)
            .await
            .sql_context("Failed to list migration trees")?;
        for (tree,) in trees {
            super::verified::repair_in_tx(&mut tx, &crate::entry::ID::parse(&tree)?).await?;
        }
        sqlx::query("UPDATE schema_version SET version = $1")
            .bind(SCHEMA_VERSION)
            .execute(&mut *tx)
            .await
            .sql_context("Failed to publish schema migration")?;
    } else if row.is_none() {
        sqlx::query("INSERT INTO schema_version (version) VALUES ($1)")
            .bind(SCHEMA_VERSION)
            .execute(&mut *tx)
            .await
            .sql_context("Failed to initialize schema version")?;
    }
    tx.commit()
        .await
        .sql_context("Failed to commit schema initialization")
}

/// Bring pre-existing `store_state_namespaces` tables up to the cache-bound
/// columns without touching their rows.
///
/// Fresh databases already have the columns from `CREATE_STORE_STATE_TABLES`,
/// so this is a no-op for them. Databases created before the bounded-cache
/// work keep every derived row (warm entries stay warm); missing columns are
/// added and `size_bytes` is backfilled from the records themselves, while
/// `last_access_tick` starts at 0 (coldest) for those rows. Deliberately not
/// a `schema_version` bump: the versioned chain rebuilds verified state,
/// which this change has no reason to redo.
async fn ensure_store_state_cache_columns(
    backend: &SqlxBackend,
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
) -> Result<()> {
    let existing: Vec<(String,)> = if backend.is_sqlite() {
        sqlx::query_as("SELECT name FROM pragma_table_info('store_state_namespaces')")
            .fetch_all(&mut **tx)
            .await
            .sql_context("Failed to inspect Store-state columns")?
    } else {
        sqlx::query_as(
            "SELECT column_name::text FROM information_schema.columns
             WHERE table_schema = current_schema()
               AND table_name = 'store_state_namespaces'",
        )
        .fetch_all(&mut **tx)
        .await
        .sql_context("Failed to inspect Store-state columns")?
    };
    for (column, ddl) in [
        (
            "size_bytes",
            "ALTER TABLE store_state_namespaces ADD COLUMN size_bytes BIGINT NOT NULL DEFAULT 0",
        ),
        (
            "last_access_tick",
            "ALTER TABLE store_state_namespaces ADD COLUMN last_access_tick BIGINT NOT NULL DEFAULT 0",
        ),
    ] {
        if !existing.iter().any(|(name,)| name == column) {
            sqlx::query(ddl)
                .execute(&mut **tx)
                .await
                .sql_context("Failed to add Store-state cache column")?;
        }
    }
    // Backfill sizes for ready rows that predate the column. Staging rows
    // get their true size in the publish transaction; recomputing them here
    // would only be overwritten.
    let key_len = if backend.is_sqlite() {
        "length(r.record_key)"
    } else {
        "octet_length(r.record_key)"
    };
    let value_len = if backend.is_sqlite() {
        "length(r.record_value)"
    } else {
        "octet_length(r.record_value)"
    };
    // SAFETY: both length expressions are dialect constants; no external
    // data is interpolated into the backfill statement.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE store_state_namespaces AS n SET size_bytes = COALESCE(
             (SELECT SUM({key_len} + {value_len}) FROM store_state_records AS r
              WHERE r.namespace_id = n.namespace_id), 0)
         WHERE n.size_bytes = 0 AND n.status IN (1, 2, 3)"
    )))
    .execute(&mut **tx)
    .await
    .sql_context("Failed to backfill Store-state sizes")?;
    Ok(())
}

#[cfg(all(test, feature = "sqlite"))]
mod migration_tests {
    use super::*;
    use crate::entry::Entry;

    fn test_chain() -> Vec<Entry> {
        let root = Entry::root_builder().build().expect("root builds");
        let root_id = root.id();
        let a = Entry::builder(root_id.clone())
            .set_height(1)
            .add_parent(root_id.clone())
            .set_subtree_data("test", b"a")
            .build()
            .expect("child a builds");
        let b = Entry::builder(root_id.clone())
            .set_height(2)
            .add_parent(a.id())
            .set_subtree_data("test", b"b")
            .build()
            .expect("child b builds");
        vec![root, a, b]
    }

    /// Reopening a v2 database reads the persisted frontier without
    /// rebuilding: wiping the table without resetting the version leaves the
    /// (now empty) retained state in place rather than resurrecting it.
    #[tokio::test]
    async fn v2_reopen_reads_persisted_state_without_rebuilding() {
        use crate::backend::VerificationStatus;

        let backend = super::super::Sqlite::in_memory()
            .await
            .expect("sqlite backend");
        let entries = test_chain();
        let tree = entries[0].id();
        for entry in &entries {
            super::super::storage::put(&backend, entry.clone())
                .await
                .unwrap();
            super::super::verified::update_verification_status(
                &backend,
                &entry.id(),
                VerificationStatus::Verified,
            )
            .await
            .unwrap();
        }
        // Wipe retained rows but keep version 2: a plain open must not
        // rescan history to fill them back in.
        sqlx::query("DELETE FROM verified_tips")
            .execute(backend.pool())
            .await
            .unwrap();
        initialize(&backend).await.unwrap();
        let snapshot = super::super::verified::verified_snapshot(&backend, &tree)
            .await
            .unwrap();
        assert!(snapshot.is_empty());
        // Explicit repair restores the frontier without demoting valid labels.
        let repaired = super::super::verified::repair_verification(&backend, &tree)
            .await
            .unwrap();
        assert_eq!(repaired.demoted, 0);
        assert_eq!(repaired.snapshot.tips(), &[entries[2].id()]);
    }
}
