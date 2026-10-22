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

    // Schema, repair, and version publication roll back
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

    /// v0 databases gain the retained verified state on open: the migration
    /// rebuilds every tree's frontier from entries, edges, and statuses, and
    /// records version 2 atomically with the schema change.
    #[tokio::test]
    async fn v0_to_v2_rebuilds_verified_frontier() {
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
        }
        for entry in &entries {
            super::super::verified::update_verification_status(
                &backend,
                &entry.id(),
                VerificationStatus::Verified,
            )
            .await
            .unwrap();
        }
        let before = super::super::verified::verified_snapshot(&backend, &tree)
            .await
            .unwrap();
        assert_eq!(before.tips(), &[entries[2].id()]);

        // Simulate a v0 database: drop the new table, reset the version.
        sqlx::query("DROP TABLE verified_tips")
            .execute(backend.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE schema_version SET version = 0")
            .execute(backend.pool())
            .await
            .unwrap();

        // Re-open runs the migration.
        initialize(&backend).await.unwrap();

        let version: (i64,) = sqlx::query_as("SELECT version FROM schema_version")
            .fetch_one(backend.pool())
            .await
            .unwrap();
        assert_eq!(version.0, 2);
        let after = super::super::verified::verified_snapshot(&backend, &tree)
            .await
            .unwrap();
        assert_eq!(before, after);
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
        // Repair restores the same frontier the migration would compute.
        let repaired = super::super::verified::rebuild_verified_state(&backend, &tree)
            .await
            .unwrap();
        assert_eq!(repaired.tips(), &[entries[2].id()]);
    }
}
