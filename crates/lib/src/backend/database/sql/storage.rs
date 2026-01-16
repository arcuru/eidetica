//! Entry storage operations for SQL backends.
//!
//! This module implements the core CRUD operations for entries using sqlx.

use ed25519_dalek::SigningKey;

use crate::Result;
use crate::backend::errors::BackendError;
use crate::backend::{InstanceMetadata, VerificationStatus};
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

    // Check if entry already exists - entries are content-addressable and immutable
    let existing_status: Option<(i64,)> =
        sqlx::query_as("SELECT verification_status FROM entries WHERE id = $1")
            .bind(id.to_string())
            .fetch_optional(&mut *tx)
            .await
            .sql_context("Failed to check entry existence")?;

    if let Some((existing_status_int,)) = existing_status {
        // Entry exists - content is immutable, but verification_status may need update
        if existing_status_int != status_int {
            sqlx::query("UPDATE entries SET verification_status = $1 WHERE id = $2")
                .bind(status_int)
                .bind(id.to_string())
                .execute(&mut *tx)
                .await
                .sql_context("Failed to update verification status")?;
            tx.commit()
                .await
                .sql_context("Failed to commit transaction")?;
        }
        // Relationships are immutable - no need to update or delete
        return Ok(());
    }

    // Insert new entry (we've already confirmed it doesn't exist)
    let is_root_int: i64 = if is_root { 1 } else { 0 };
    sqlx::query(
        "INSERT INTO entries (id, tree_id, is_root, verification_status, entry_json)
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
///
/// Tips are entries with no children. This function handles out-of-order arrival
/// by checking if the new entry already has children before adding it as a tip.
async fn update_tips_for_entry(
    backend: &SqlxBackend,
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    entry_id: &ID,
    tree_id: &ID,
    entry: &Entry,
) -> Result<()> {
    // Check if this entry already has children in the tree (out-of-order arrival)
    let has_tree_children: Option<(i32,)> =
        sqlx::query_as("SELECT 1 FROM tree_parents WHERE parent_id = $1 LIMIT 1")
            .bind(entry_id.to_string())
            .fetch_optional(&mut **tx)
            .await
            .sql_context("Failed to check for tree children")?;

    // Only add as tree-level tip if no children exist
    // Note: empty string '' used for tree-level (PostgreSQL doesn't allow NULL in PK)
    if has_tree_children.is_none() {
        insert_or_ignore(
            backend,
            tx,
            "tips",
            &["entry_id", "tree_id", "store_name"],
            &[entry_id.to_string(), tree_id.to_string(), String::new()],
        )
        .await?;
    }

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
        // Check if this entry already has children in this store (out-of-order arrival)
        let has_store_children: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM store_parents WHERE parent_id = $1 AND store_name = $2 LIMIT 1",
        )
        .bind(entry_id.to_string())
        .bind(&store_name)
        .fetch_optional(&mut **tx)
        .await
        .sql_context("Failed to check for store children")?;

        // Only add as store-level tip if no children exist in this store
        if has_store_children.is_none() {
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
        }

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

    // Sort by height (heights are stored in entries)
    super::cache::sort_entries_by_height(&mut entries);

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

    // Sort by store height (heights are stored in entries)
    super::cache::sort_entries_by_subtree_height(&mut entries, store)?;

    Ok(entries)
}

// === Private Key Storage (deprecated - backward compatibility) ===

/// Store a private key (no-op - private keys are in InstanceMetadata or User API).
pub async fn store_private_key(
    _backend: &SqlxBackend,
    _key_name: &str,
    _private_key: SigningKey,
) -> Result<()> {
    // Private keys are no longer stored separately - device key is in InstanceMetadata
    // User private keys are stored in the _users database via the User API
    Ok(())
}

/// Get a private key by name.
///
/// Note: User private keys are managed through the User API, not stored directly in backend.
/// This function exists for interface compatibility but always returns None.
pub async fn get_private_key(
    _backend: &SqlxBackend,
    _key_name: &str,
) -> Result<Option<SigningKey>> {
    Ok(None)
}

/// List all private key names.
///
/// Note: User private keys are managed through the User API, not stored directly in backend.
/// This function exists for interface compatibility but always returns empty.
pub async fn list_private_keys(_backend: &SqlxBackend) -> Result<Vec<String>> {
    Ok(vec![])
}

/// Remove a private key by name (no-op).
pub async fn remove_private_key(_backend: &SqlxBackend, _key_name: &str) -> Result<()> {
    // Private keys are no longer stored separately
    Ok(())
}

// === Instance Metadata ===

/// Get instance metadata if it exists.
pub async fn get_instance_metadata(backend: &SqlxBackend) -> Result<Option<InstanceMetadata>> {
    let pool = backend.pool();

    let row: Option<(String,)> =
        sqlx::query_as("SELECT data FROM instance_metadata WHERE singleton = 1")
            .fetch_optional(pool)
            .await
            .sql_context("Failed to get instance metadata")?;

    match row {
        Some((json,)) => {
            let metadata: InstanceMetadata = serde_json::from_str(&json)
                .map_err(|e| BackendError::DeserializationFailed { source: e })?;
            Ok(Some(metadata))
        }
        None => Ok(None),
    }
}

/// Set instance metadata.
pub async fn set_instance_metadata(
    backend: &SqlxBackend,
    metadata: &InstanceMetadata,
) -> Result<()> {
    let pool = backend.pool();
    let json = serde_json::to_string(metadata)
        .map_err(|e| BackendError::SerializationFailed { source: e })?;

    if backend.is_sqlite() {
        sqlx::query("INSERT OR REPLACE INTO instance_metadata (singleton, data) VALUES (1, $1)")
            .bind(&json)
            .execute(pool)
            .await
            .sql_context("Failed to set instance metadata")?;
    } else {
        sqlx::query(
            "INSERT INTO instance_metadata (singleton, data) VALUES (1, $1)
             ON CONFLICT (singleton) DO UPDATE SET data = EXCLUDED.data",
        )
        .bind(&json)
        .execute(pool)
        .await
        .sql_context("Failed to set instance metadata")?;
    }

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
