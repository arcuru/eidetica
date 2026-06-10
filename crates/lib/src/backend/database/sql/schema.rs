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

use super::{SqlxBackend, SqlxResultExt};

/// Current schema version.
///
/// Increment this when making schema changes that require migration.
/// Version 0 is fully unstable and should not be used in production.
pub const SCHEMA_VERSION: i64 = 0;

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
    // CRDT state cache (v2: scope-keyed)
    //
    // `scope_user_uuid` is the trust scope: the empty string `''` encodes
    // `CacheScope::Shared` (daemon-computed, visible to every user with
    // database read permission); any other string encodes
    // `CacheScope::User(uuid)` (client-attested bytes, visible only to that
    // user). An empty-string sentinel is used because PostgreSQL disallows
    // NULL in primary-key columns; user UUIDs are never empty so the
    // mapping is unambiguous. `state` is opaque bytes (plaintext for Shared;
    // ciphertext or plaintext for User, decided client-side).
    //
    // The pre-unification table `crdt_cache` (no scope column) is left
    // untouched if it exists on an upgraded database — it just becomes
    // unreferenced. The cache is performance state, so cold-rebuilding into
    // `crdt_cache_v2` on first load is fine.
    "CREATE TABLE IF NOT EXISTS crdt_cache_v2 (
        scope_user_uuid TEXT NOT NULL,
        entry_id TEXT NOT NULL,
        store_name TEXT NOT NULL,
        state BLOB NOT NULL,
        PRIMARY KEY (scope_user_uuid, entry_id, store_name)
    )",
    // Content-addressed blob storage (durable, out-of-band from the entry DAG).
    //
    // A blob is keyed by `cid` (the raw-codec `0x55` BLAKE3 CIDv1 of its
    // bytes), giving global dedup for free. `size` caches the byte length so
    // callers don't have to materialize the blob to learn how big it is.
    // `location` reserves the eventual hybrid inline/disk split: Phase 1 always
    // stores inline (`location = 0`, bytes in `data`); a future disk tier will
    // use `location = 1` with `data` NULL and the bytes at a content-addressed
    // path. Persisting `size`/`location` now keeps the table shape stable so
    // the disk tier lands with no migration.
    //
    // `last_accessed` (epoch ms) drives LRU eviction in `gc_blobs` (§6). It is
    // stamped to *now* on every put and every local read hit. Unlike
    // `crdt_cache_v2`, a blob is durable owned content and is evicted ONLY by an
    // explicit GC pass, and even then only when it is not pinned (see
    // `blob_pins`). This column is part of the unreleased Phase-1 baseline (no
    // migration; SCHEMA_VERSION stays 0/unstable).
    "CREATE TABLE IF NOT EXISTS blobs (
        cid           TEXT PRIMARY KEY,
        size          BIGINT NOT NULL,
        location      BIGINT NOT NULL,
        last_accessed BIGINT NOT NULL DEFAULT 0,
        data          BLOB
    )",
    // Blob pins (Phase 1.5, §6) — the local GC root set.
    //
    // A pin is an instance-LOCAL retention assertion (it does NOT replicate as
    // an entry, so it is not a frozen wire surface). Keyed by
    // `(user_id, database_id, blob_cid)`: a blob is retained while ANY row names
    // its CID, and the `(user, database)` decomposition lets us un-pin and
    // attribute pinned size per user (`pinned_size_by_user`, for provenance /
    // quota). `database_id` uses the empty-string sentinel for "not tied to a
    // specific database" (PostgreSQL disallows NULL in a PK column; database IDs
    // are CIDs and never empty, matching the `tips` / `crdt_cache_v2`
    // convention). GC's root set is `SELECT DISTINCT blob_cid FROM blob_pins`.
    "CREATE TABLE IF NOT EXISTS blob_pins (
        user_id      TEXT NOT NULL,
        database_id  TEXT NOT NULL DEFAULT '',
        blob_cid     TEXT NOT NULL,
        PRIMARY KEY (user_id, database_id, blob_cid)
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
    // Blob pin reverse lookups (DISTINCT blob_cid for the GC root set; the
    // PK prefix already covers per-user queries).
    "CREATE INDEX IF NOT EXISTS idx_blob_pins_cid ON blob_pins(blob_cid)",
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

    if row.is_none() {
        // First initialization
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
            .sql_context("Failed to update schema version")?;

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
