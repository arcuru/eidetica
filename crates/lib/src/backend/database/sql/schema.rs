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
//! 3. Add the migration to the match statement in `run_migration`
//! 4. Document what the migration does

use crate::Result;
use crate::backend::errors::BackendError;

use super::SqlxBackend;

/// Current schema version.
///
/// Increment this when making schema changes that require migration.
pub const SCHEMA_VERSION: i64 = 1;

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
        entry_json TEXT NOT NULL
    )",
    // Tree parent relationships (main tree DAG edges)
    // Each entry can have multiple parents for merge commits
    "CREATE TABLE IF NOT EXISTS tree_parents (
        child_id TEXT NOT NULL,
        parent_id TEXT NOT NULL,
        PRIMARY KEY (child_id, parent_id)
    )",
    // Store memberships - which stores an entry contains
    "CREATE TABLE IF NOT EXISTS store_memberships (
        entry_id TEXT NOT NULL,
        store_name TEXT NOT NULL,
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
    // Heights cache - computed values, persisted
    // Heights are immutable per entry once computed
    // store_name uses empty string for tree-level heights (PostgreSQL disallows NULL in PK)
    "CREATE TABLE IF NOT EXISTS heights (
        entry_id TEXT NOT NULL,
        tree_id TEXT NOT NULL,
        store_name TEXT NOT NULL DEFAULT '',
        height BIGINT NOT NULL,
        PRIMARY KEY (entry_id, tree_id, store_name)
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
    // Private key storage
    // BYTEA is PostgreSQL binary type and SQLite maps it to BLOB affinity
    "CREATE TABLE IF NOT EXISTS private_keys (
        key_name TEXT PRIMARY KEY NOT NULL,
        key_bytes BYTEA NOT NULL
    )",
    // CRDT state cache
    "CREATE TABLE IF NOT EXISTS crdt_cache (
        entry_id TEXT NOT NULL,
        store_name TEXT NOT NULL,
        state TEXT NOT NULL,
        PRIMARY KEY (entry_id, store_name)
    )",
];

/// SQL statements to create indexes.
pub const CREATE_INDEXES: &[&str] = &[
    // Entry lookups and filtering
    "CREATE INDEX IF NOT EXISTS idx_entries_tree_id ON entries(tree_id)",
    "CREATE INDEX IF NOT EXISTS idx_entries_verification ON entries(verification_status)",
    "CREATE INDEX IF NOT EXISTS idx_entries_is_root ON entries(is_root)",
    // Parent relationship traversal
    "CREATE INDEX IF NOT EXISTS idx_tree_parents_parent ON tree_parents(parent_id)",
    "CREATE INDEX IF NOT EXISTS idx_tree_parents_child ON tree_parents(child_id)",
    // Store-specific queries
    "CREATE INDEX IF NOT EXISTS idx_store_memberships_store ON store_memberships(store_name, entry_id)",
    "CREATE INDEX IF NOT EXISTS idx_store_parents_parent ON store_parents(store_name, parent_id)",
    "CREATE INDEX IF NOT EXISTS idx_store_parents_child ON store_parents(store_name, child_id)",
    // Height and tip lookups
    "CREATE INDEX IF NOT EXISTS idx_heights_tree_store ON heights(tree_id, store_name)",
    "CREATE INDEX IF NOT EXISTS idx_tips_tree_store ON tips(tree_id, store_name)",
];

/// Initialize the database schema.
///
/// Creates tables and indexes if they don't exist, and handles migrations
/// if the schema version has changed.
pub async fn initialize(backend: &SqlxBackend) -> Result<()> {
    let pool = backend.pool();

    // Create tables
    for statement in CREATE_TABLES {
        sqlx::query(statement)
            .execute(pool)
            .await
            .map_err(|e| BackendError::SqlxError {
                reason: format!("Schema creation failed: {e} - SQL: {statement}"),
                source: Some(e),
            })?;
    }

    // Check current schema version
    let row: Option<(i64,)> = sqlx::query_as("SELECT version FROM schema_version")
        .fetch_optional(pool)
        .await
        .map_err(|e| BackendError::SqlxError {
            reason: format!("Failed to check schema version: {e}"),
            source: Some(e),
        })?;

    if row.is_none() {
        // First initialization
        sqlx::query("INSERT INTO schema_version (version) VALUES ($1)")
            .bind(SCHEMA_VERSION)
            .execute(pool)
            .await
            .map_err(|e| BackendError::SqlxError {
                reason: format!("Failed to initialize schema version: {e}"),
                source: Some(e),
            })?;
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
            .map_err(|e| BackendError::SqlxError {
                reason: format!("Index creation failed: {e} - SQL: {statement}"),
                source: Some(e),
            })?;
    }

    Ok(())
}

/// Run migrations sequentially from one schema version to another.
///
/// Migrations are run one at a time, incrementing the version after each.
/// This allows for proper error handling and rollback semantics.
async fn migrate(backend: &SqlxBackend, from: i64, to: i64) -> Result<()> {
    tracing::info!(from, to, "Starting SQL schema migration");

    let mut current = from;
    while current < to {
        let next = current + 1;
        tracing::info!(from = current, to = next, "Running migration");

        run_migration(backend, current, next).await?;

        // Update schema version after successful migration
        sqlx::query("UPDATE schema_version SET version = $1")
            .bind(next)
            .execute(backend.pool())
            .await
            .map_err(|e| BackendError::SqlxError {
                reason: format!("Failed to update schema version to {next}: {e}"),
                source: Some(e),
            })?;

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
/// match from {
///     1 => migrate_v1_to_v2(backend).await,
///     // ... existing migrations ...
///     _ => { /* error handling */ }
/// }
/// ```
async fn run_migration(backend: &SqlxBackend, from: i64, to: i64) -> Result<()> {
    // When adding the first migration, replace this with:
    //
    // match from {
    //     1 => migrate_v1_to_v2(backend).await,
    //     _ => Err(BackendError::SqlxError { ... }.into()),
    // }
    //
    // For now, since there are no migrations yet, any attempt to migrate is an error.

    // Suppress unused variable warning until migrations are added
    let _ = backend;

    Err(BackendError::SqlxError {
        reason: format!(
            "Unknown migration path: v{from} to v{to}. \
             This likely means SCHEMA_VERSION was incremented without adding a migration."
        ),
        source: None,
    }
    .into())
}
