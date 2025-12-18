//! Entry storage operations for SQL backends.
//!
//! This module implements the core CRUD operations for entries using sqlx.

use ed25519_dalek::SigningKey;

use crate::Result;
use crate::backend::VerificationStatus;
use crate::backend::errors::BackendError;
use crate::entry::{Entry, ID};

use super::{SqlxBackend, SqlxResultExt};

/// Get an entry by ID.
pub async fn get(backend: &SqlxBackend, id: &ID) -> Result<Entry> {
    let pool = backend.pool();

    let row: Option<(String,)> = sqlx::query_as("SELECT entry_json FROM entries WHERE id = $1")
        .bind(id.to_string())
        .fetch_optional(pool)
        .await
        .sql_context("Failed to get entry")?;

    match row {
        Some((json,)) => {
            let entry: Entry = serde_json::from_str(&json)
                .map_err(|e| BackendError::DeserializationFailed { source: e })?;
            Ok(entry)
        }
        None => Err(BackendError::EntryNotFound { id: id.clone() }.into()),
    }
}

/// Get the verification status of an entry.
pub async fn get_verification_status(backend: &SqlxBackend, id: &ID) -> Result<VerificationStatus> {
    let pool = backend.pool();

    let row: Option<(i64,)> =
        sqlx::query_as("SELECT verification_status FROM entries WHERE id = $1")
            .bind(id.to_string())
            .fetch_optional(pool)
            .await
            .sql_context("Failed to get verification status")?;

    match row {
        Some((status,)) => Ok(match status {
            0 => VerificationStatus::Verified,
            _ => VerificationStatus::Failed,
        }),
        None => Err(BackendError::VerificationStatusNotFound { id: id.clone() }.into()),
    }
}

/// Store an entry with the given verification status.
pub async fn put(
    backend: &SqlxBackend,
    verification_status: VerificationStatus,
    entry: Entry,
) -> Result<()> {
    // Validate entry before storing
    entry.validate()?;

    let pool = backend.pool();
    let id = entry.id();
    let raw_tree_id = entry.root();
    let is_root = entry.is_root();

    // For root entries, the tree_id to store is the entry's own ID
    // (entry.root() returns empty string for roots)
    let tree_id = if is_root {
        id.clone()
    } else {
        raw_tree_id.clone()
    };

    let entry_json = serde_json::to_string(&entry)
        .map_err(|e| BackendError::SerializationFailed { source: e })?;

    let status_int: i64 = match verification_status {
        VerificationStatus::Verified => 0,
        VerificationStatus::Failed => 1,
    };

    // Use a transaction for atomicity
    let mut tx = pool
        .begin()
        .await
        .sql_context("Failed to begin transaction")?;

    // Insert or update entry (different syntax for SQLite vs Postgres)
    let is_root_int: i64 = if is_root { 1 } else { 0 };
    if backend.is_sqlite() {
        sqlx::query(
            "INSERT OR REPLACE INTO entries (id, tree_id, is_root, verification_status, entry_json)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id.to_string())
        .bind(tree_id.to_string())
        .bind(is_root_int)
        .bind(status_int)
        .bind(&entry_json)
        .execute(&mut *tx)
        .await
        .sql_context("Failed to insert entry")?;
    } else {
        // PostgreSQL uses ON CONFLICT
        sqlx::query(
            "INSERT INTO entries (id, tree_id, is_root, verification_status, entry_json)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (id) DO UPDATE SET
                tree_id = EXCLUDED.tree_id,
                is_root = EXCLUDED.is_root,
                verification_status = EXCLUDED.verification_status,
                entry_json = EXCLUDED.entry_json",
        )
        .bind(id.to_string())
        .bind(tree_id.to_string())
        .bind(is_root_int)
        .bind(status_int)
        .bind(&entry_json)
        .execute(&mut *tx)
        .await
        .sql_context("Failed to insert entry")?;
    }

    // Clear existing parent relationships for this entry
    sqlx::query("DELETE FROM tree_parents WHERE child_id = $1")
        .bind(id.to_string())
        .execute(&mut *tx)
        .await
        .sql_context("Failed to clear tree parents")?;

    sqlx::query("DELETE FROM store_parents WHERE child_id = $1")
        .bind(id.to_string())
        .execute(&mut *tx)
        .await
        .sql_context("Failed to clear store parents")?;

    sqlx::query("DELETE FROM store_memberships WHERE entry_id = $1")
        .bind(id.to_string())
        .execute(&mut *tx)
        .await
        .sql_context("Failed to clear store memberships")?;

    // Insert tree parent relationships
    for parent_id in entry.parents()? {
        insert_or_ignore(
            backend,
            &mut tx,
            "tree_parents",
            &["child_id", "parent_id"],
            &[id.to_string(), parent_id.to_string()],
        )
        .await?;
    }

    // Insert store memberships and store parent relationships
    for store_name in entry.subtrees() {
        insert_or_ignore(
            backend,
            &mut tx,
            "store_memberships",
            &["entry_id", "store_name"],
            &[id.to_string(), store_name.clone()],
        )
        .await?;

        if let Ok(store_parents) = entry.subtree_parents(&store_name) {
            for parent_id in store_parents {
                insert_or_ignore(
                    backend,
                    &mut tx,
                    "store_parents",
                    &["child_id", "parent_id", "store_name"],
                    &[id.to_string(), parent_id.to_string(), store_name.clone()],
                )
                .await?;
            }
        }
    }

    // Update tips incrementally
    update_tips_for_entry(backend, &mut tx, &id, &tree_id, &entry).await?;

    tx.commit()
        .await
        .sql_context("Failed to commit transaction")?;

    Ok(())
}

/// Helper to insert with OR IGNORE semantics (SQLite) or ON CONFLICT DO NOTHING (Postgres)
async fn insert_or_ignore(
    backend: &SqlxBackend,
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    table: &str,
    columns: &[&str],
    values: &[String],
) -> Result<()> {
    let cols = columns.join(", ");
    let placeholders: Vec<String> = (1..=columns.len()).map(|i| format!("${i}")).collect();
    let placeholders = placeholders.join(", ");

    let sql = if backend.is_sqlite() {
        format!("INSERT OR IGNORE INTO {table} ({cols}) VALUES ({placeholders})")
    } else {
        format!("INSERT INTO {table} ({cols}) VALUES ({placeholders}) ON CONFLICT DO NOTHING")
    };

    let mut query = sqlx::query(&sql);
    for value in values {
        query = query.bind(value);
    }

    query
        .execute(&mut **tx)
        .await
        .sql_context(&format!("Failed to insert into {table}"))?;

    Ok(())
}

/// Update the tips table when a new entry is added.
async fn update_tips_for_entry(
    backend: &SqlxBackend,
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    entry_id: &ID,
    tree_id: &ID,
    entry: &Entry,
) -> Result<()> {
    // The new entry is initially a tip (at tree level)
    // Note: empty string '' used for tree-level (PostgreSQL doesn't allow NULL in PK)
    insert_or_ignore(
        backend,
        tx,
        "tips",
        &["entry_id", "tree_id", "store_name"],
        &[entry_id.to_string(), tree_id.to_string(), String::new()],
    )
    .await?;

    // Remove parents from tree tips (they now have children)
    if let Ok(parents) = entry.parents() {
        for parent_id in parents {
            sqlx::query(
                "DELETE FROM tips WHERE entry_id = $1 AND tree_id = $2 AND store_name = $3",
            )
            .bind(parent_id.to_string())
            .bind(tree_id.to_string())
            .bind("")
            .execute(&mut **tx)
            .await
            .sql_context("Failed to delete tip")?;
        }
    }

    // Handle store-level tips
    for store_name in entry.subtrees() {
        // New entry is a tip in this store
        insert_or_ignore(
            backend,
            tx,
            "tips",
            &["entry_id", "tree_id", "store_name"],
            &[
                entry_id.to_string(),
                tree_id.to_string(),
                store_name.clone(),
            ],
        )
        .await?;

        // Remove parents from store tips
        if let Ok(store_parents) = entry.subtree_parents(&store_name) {
            for parent_id in store_parents {
                sqlx::query(
                    "DELETE FROM tips WHERE entry_id = $1 AND tree_id = $2 AND store_name = $3",
                )
                .bind(parent_id.to_string())
                .bind(tree_id.to_string())
                .bind(&store_name)
                .execute(&mut **tx)
                .await
                .sql_context("Failed to delete store tip")?;
            }
        }
    }

    Ok(())
}

/// Update the verification status of an entry.
pub async fn update_verification_status(
    backend: &SqlxBackend,
    id: &ID,
    verification_status: VerificationStatus,
) -> Result<()> {
    let pool = backend.pool();

    let status_int: i64 = match verification_status {
        VerificationStatus::Verified => 0,
        VerificationStatus::Failed => 1,
    };

    let result = sqlx::query("UPDATE entries SET verification_status = $1 WHERE id = $2")
        .bind(status_int)
        .bind(id.to_string())
        .execute(pool)
        .await
        .sql_context("Failed to update verification status")?;

    if result.rows_affected() == 0 {
        return Err(BackendError::EntryNotFound { id: id.clone() }.into());
    }

    Ok(())
}

/// Get all entry IDs with a specific verification status.
pub async fn get_entries_by_verification_status(
    backend: &SqlxBackend,
    status: VerificationStatus,
) -> Result<Vec<ID>> {
    let pool = backend.pool();

    let status_int: i64 = match status {
        VerificationStatus::Verified => 0,
        VerificationStatus::Failed => 1,
    };

    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT id FROM entries WHERE verification_status = $1")
            .bind(status_int)
            .fetch_all(pool)
            .await
            .sql_context("Failed to get entries by status")?;

    Ok(rows.into_iter().map(|(id,)| ID::from(id)).collect())
}

/// Get all root entry IDs.
pub async fn all_roots(backend: &SqlxBackend) -> Result<Vec<ID>> {
    let pool = backend.pool();

    let rows: Vec<(String,)> = sqlx::query_as("SELECT id FROM entries WHERE is_root = 1")
        .fetch_all(pool)
        .await
        .sql_context("Failed to get all roots")?;

    Ok(rows.into_iter().map(|(id,)| ID::from(id)).collect())
}

/// Get all entries in a tree, sorted by height.
pub async fn get_tree(backend: &SqlxBackend, tree: &ID) -> Result<Vec<Entry>> {
    let pool = backend.pool();

    let rows: Vec<(String,)> = sqlx::query_as("SELECT entry_json FROM entries WHERE tree_id = $1")
        .bind(tree.to_string())
        .fetch_all(pool)
        .await
        .sql_context("Failed to get tree")?;

    let mut entries = Vec::with_capacity(rows.len());
    for (json,) in rows {
        let entry: Entry = serde_json::from_str(&json)
            .map_err(|e| BackendError::DeserializationFailed { source: e })?;
        entries.push(entry);
    }

    // Sort by height using the heights table
    super::cache::sort_entries_by_height(backend, tree, None, &mut entries).await?;

    Ok(entries)
}

/// Get all entries in a store, sorted by height.
pub async fn get_store(backend: &SqlxBackend, tree: &ID, store: &str) -> Result<Vec<Entry>> {
    let pool = backend.pool();

    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT e.entry_json
         FROM entries e
         JOIN store_memberships sm ON sm.entry_id = e.id
         WHERE e.tree_id = $1 AND sm.store_name = $2",
    )
    .bind(tree.to_string())
    .bind(store)
    .fetch_all(pool)
    .await
    .sql_context("Failed to get store")?;

    let mut entries = Vec::with_capacity(rows.len());
    for (json,) in rows {
        let entry: Entry = serde_json::from_str(&json)
            .map_err(|e| BackendError::DeserializationFailed { source: e })?;
        entries.push(entry);
    }

    // Sort by store height using the heights table
    super::cache::sort_entries_by_height(backend, tree, Some(store), &mut entries).await?;

    Ok(entries)
}

// === Private Key Storage ===

/// Store a private key.
pub async fn store_private_key(
    backend: &SqlxBackend,
    key_name: &str,
    private_key: SigningKey,
) -> Result<()> {
    let pool = backend.pool();
    let key_bytes = private_key.to_bytes().to_vec();

    if backend.is_sqlite() {
        sqlx::query("INSERT OR REPLACE INTO private_keys (key_name, key_bytes) VALUES ($1, $2)")
            .bind(key_name)
            .bind(&key_bytes)
            .execute(pool)
            .await
            .sql_context("Failed to store private key")?;
    } else {
        sqlx::query(
            "INSERT INTO private_keys (key_name, key_bytes) VALUES ($1, $2)
             ON CONFLICT (key_name) DO UPDATE SET key_bytes = EXCLUDED.key_bytes",
        )
        .bind(key_name)
        .bind(&key_bytes)
        .execute(pool)
        .await
        .sql_context("Failed to store private key")?;
    }

    Ok(())
}

/// Get a private key by name.
pub async fn get_private_key(backend: &SqlxBackend, key_name: &str) -> Result<Option<SigningKey>> {
    let pool = backend.pool();

    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT key_bytes FROM private_keys WHERE key_name = $1")
            .bind(key_name)
            .fetch_optional(pool)
            .await
            .sql_context("Failed to get private key")?;

    match row {
        Some((bytes,)) => {
            let key_bytes: [u8; 32] = bytes.try_into().map_err(|_| BackendError::CacheError {
                reason: "Invalid key bytes length".to_string(),
            })?;
            Ok(Some(SigningKey::from_bytes(&key_bytes)))
        }
        None => Ok(None),
    }
}

/// List all private key names.
pub async fn list_private_keys(backend: &SqlxBackend) -> Result<Vec<String>> {
    let pool = backend.pool();

    let rows: Vec<(String,)> = sqlx::query_as("SELECT key_name FROM private_keys")
        .fetch_all(pool)
        .await
        .sql_context("Failed to list private keys")?;

    Ok(rows.into_iter().map(|(name,)| name).collect())
}

/// Remove a private key by name.
pub async fn remove_private_key(backend: &SqlxBackend, key_name: &str) -> Result<()> {
    let pool = backend.pool();

    sqlx::query("DELETE FROM private_keys WHERE key_name = $1")
        .bind(key_name)
        .execute(pool)
        .await
        .sql_context("Failed to remove private key")?;

    Ok(())
}

// === CRDT Cache ===

/// Get cached CRDT state.
pub async fn get_cached_crdt_state(
    backend: &SqlxBackend,
    entry_id: &ID,
    store: &str,
) -> Result<Option<String>> {
    let pool = backend.pool();

    let row: Option<(String,)> =
        sqlx::query_as("SELECT state FROM crdt_cache WHERE entry_id = $1 AND store_name = $2")
            .bind(entry_id.to_string())
            .bind(store)
            .fetch_optional(pool)
            .await
            .sql_context("Failed to get cached CRDT state")?;

    Ok(row.map(|(state,)| state))
}

/// Cache CRDT state.
pub async fn cache_crdt_state(
    backend: &SqlxBackend,
    entry_id: &ID,
    store: &str,
    state: String,
) -> Result<()> {
    let pool = backend.pool();

    if backend.is_sqlite() {
        sqlx::query(
            "INSERT OR REPLACE INTO crdt_cache (entry_id, store_name, state) VALUES ($1, $2, $3)",
        )
        .bind(entry_id.to_string())
        .bind(store)
        .bind(&state)
        .execute(pool)
        .await
        .sql_context("Failed to cache CRDT state")?;
    } else {
        sqlx::query(
            "INSERT INTO crdt_cache (entry_id, store_name, state) VALUES ($1, $2, $3)
             ON CONFLICT (entry_id, store_name) DO UPDATE SET state = EXCLUDED.state",
        )
        .bind(entry_id.to_string())
        .bind(store)
        .bind(&state)
        .execute(pool)
        .await
        .sql_context("Failed to cache CRDT state")?;
    }

    Ok(())
}

/// Clear all cached CRDT state.
pub async fn clear_crdt_cache(backend: &SqlxBackend) -> Result<()> {
    let pool = backend.pool();

    sqlx::query("DELETE FROM crdt_cache")
        .execute(pool)
        .await
        .sql_context("Failed to clear CRDT cache")?;

    Ok(())
}
