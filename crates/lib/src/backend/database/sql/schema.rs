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
pub const SCHEMA_VERSION: i64 = 1;

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
    // Retained verified prefix/frontier - maintained incrementally at the
    // `update_verification_status` boundary, rebuilt on migration/repair.
    //
    // Both ancestor-closed prefix membership and maximal-frontier membership
    // are stored: frontier-only rows cannot cheaply answer whether every
    // parent is eligible during out-of-order promotion. `tips` stays the
    // durable raw frontier; `verified_prefix WHERE is_frontier = 1` is the
    // durable verified frontier.
    "CREATE TABLE IF NOT EXISTS verified_prefix (
        tree_id TEXT NOT NULL,
        entry_id TEXT NOT NULL,
        is_frontier BIGINT NOT NULL,
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
    // Retained verified-frontier lookups
    "CREATE INDEX IF NOT EXISTS idx_verified_prefix_frontier ON verified_prefix(tree_id, is_frontier)",
];

/// Initialize the database schema.
///
/// Creates tables and indexes if they don't exist, and handles migrations
/// if the schema version has changed.
pub async fn initialize(backend: &SqlxBackend) -> Result<()> {
    let pool = backend.pool();

    // Create tables, adapting dialect-specific types
    let blob_type = if backend.is_sqlite() { "BLOB" } else { "BYTEA" };
    for statement in CREATE_TABLES {
        let statement = statement.replace("BLOB", blob_type);
        sqlx::query(&statement)
            .execute(pool)
            .await
            .sql_context("Schema creation failed")?;
    }

    // Check current schema version
    let row: Option<(i64,)> = sqlx::query_as("SELECT version FROM schema_version")
        .fetch_optional(pool)
        .await
        .sql_context("Failed to check schema version")?;

    initialize_store_state_tables(backend).await?;

    if row.is_none() {
        sqlx::query("INSERT INTO schema_version (version) VALUES ($1)")
            .bind(SCHEMA_VERSION)
            .execute(pool)
            .await
            .sql_context("Failed to initialize schema version")?;
    } else if let Some((current_version,)) = row
        && current_version < SCHEMA_VERSION
    {
        // Run migrations
        migrate(backend, current_version, SCHEMA_VERSION).await?;
    }

    // Create indexes
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
        .sql_context("Failed to begin schema initialization")?;
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

/// Run migrations sequentially from one schema version to another.
///
/// Migrations are run one step at a time, each advancing the schema by a single
/// version. This function only tracks the step it is on; persisting the new
/// `schema_version` is the responsibility of each migration function, which
/// writes it in the same transaction as its schema changes. A failed step
/// therefore leaves the recorded version at the last successfully committed
/// migration.
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

/// Execute a single migration step.
///
/// Each migration is a separate async function that handles the schema change.
/// Add new migrations here as match arms.
///
/// # Adding a New Migration
///
/// When incrementing `SCHEMA_VERSION`, add a match arm here:
///
/// ```ignore
/// match (from, to) {
///     (1, 2) => migrate_v1_to_v2(backend).await,
///     // ... existing migrations ...
///     _ => { /* error handling */ }
/// }
/// ```
///
/// The migration function is responsible for persisting the new
/// `schema_version` itself, inside the same transaction as its schema changes.
async fn run_migration(backend: &SqlxBackend, from: i64, to: i64) -> Result<()> {
    match (from, to) {
        (0, 1) => migrate_v0_to_v1(backend).await,
        _ => Err(BackendError::SqlxError {
            reason: format!(
                "Unknown migration path: v{from} to v{to}. \
                 This likely means SCHEMA_VERSION was incremented without adding a migration."
            ),
            source: None,
        }
        .into()),
    }
}

/// Migrate v0 to v1: add the retained verified prefix/frontier table and
/// reconstruct every tree's verified state from entries, parent edges, and
/// statuses in deterministic topological order.
///
/// Table creation, per-tree rebuilds, and the `schema_version` write commit
/// atomically: a crash leaves version 0 behind and the migration simply
/// re-runs (rebuilds are idempotent — each tree's rows are deleted before
/// its recomputed rows are inserted).
async fn migrate_v0_to_v1(backend: &SqlxBackend) -> Result<()> {
    let mut tx = backend
        .pool()
        .begin()
        .await
        .sql_context("Failed to begin migration transaction")?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS verified_prefix (
            tree_id TEXT NOT NULL,
            entry_id TEXT NOT NULL,
            is_frontier BIGINT NOT NULL,
            PRIMARY KEY (tree_id, entry_id)
        )",
    )
    .execute(&mut *tx)
    .await
    .sql_context("Migration v0 to v1 failed: could not create verified_prefix")?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_verified_prefix_frontier \
         ON verified_prefix(tree_id, is_frontier)",
    )
    .execute(&mut *tx)
    .await
    .sql_context("Migration v0 to v1 failed: could not create verified_prefix index")?;

    let trees: Vec<(String,)> = sqlx::query_as("SELECT DISTINCT tree_id FROM entries")
        .fetch_all(&mut *tx)
        .await
        .sql_context("Migration v0 to v1 failed: could not list trees")?;
    for (tree_id,) in trees {
        let tree = crate::entry::ID::parse(&tree_id)?;
        super::verified::rebuild_in_tx(&mut tx, &tree).await?;
    }

    sqlx::query("UPDATE schema_version SET version = 1")
        .execute(&mut *tx)
        .await
        .sql_context("Migration v0 to v1 failed: could not update schema version")?;

    tx.commit()
        .await
        .sql_context("Migration v0 to v1 failed: could not commit")?;
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

    /// v0 databases gain the retained verified state on open: the migration
    /// rebuilds every tree's frontier from entries, edges, and statuses, and
    /// records version 1 atomically with the schema change.
    #[tokio::test]
    async fn v0_to_v1_rebuilds_verified_frontier() {
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
        sqlx::query("DROP TABLE verified_prefix")
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
        assert_eq!(version.0, 1);
        let after = super::super::verified::verified_snapshot(&backend, &tree)
            .await
            .unwrap();
        assert_eq!(before, after);
    }

    /// Reopening a v1 database reads the persisted frontier without
    /// rebuilding: wiping the table without resetting the version leaves the
    /// (now empty) retained state in place rather than resurrecting it.
    #[tokio::test]
    async fn v1_reopen_reads_persisted_state_without_rebuilding() {
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
        // Wipe retained rows but keep version 1: a plain open must not
        // rescan history to fill them back in.
        sqlx::query("DELETE FROM verified_prefix")
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
