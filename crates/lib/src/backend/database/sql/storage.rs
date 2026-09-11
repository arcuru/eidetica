//! Entry storage operations for SQL backends.
//!
//! This module implements the core CRUD operations for entries using sqlx.

use crate::Result;
use crate::backend::errors::BackendError;
use crate::backend::{
    CacheScope, InstanceMetadata, InstanceSecrets, RecordMutations, RecordPage, RecordRange,
    RecordView, StagingToken, StoreStateLifecycle, StoreStateRequest, VerificationStatus,
};
use crate::entry::{Entry, ID};

use super::{SqlxBackend, SqlxResultExt};
use crate::backend::database::sorting;

fn store_state_scope(scope: &CacheScope) -> &str {
    match scope {
        CacheScope::Shared => "",
        CacheScope::User(user) => user,
    }
}

/// Canonical identity of a Store-state target, for cross-transaction locking.
fn store_state_target_key(target: &StoreStateRequest) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}",
        target.database,
        target.store,
        target.lifecycle.as_db_int(),
        store_state_scope(&target.scope),
        target.projection.name,
        target.projection.version,
        hex::encode(&target.source_key),
    )
}

/// Serialize a stage/publish critical section on PostgreSQL.
///
/// SQLite serializes writers through `BEGIN IMMEDIATE`, so this is a no-op
/// there. PostgreSQL allows concurrent writers: without a shared lock, a
/// publish can slip between another task's stage validation and its writes,
/// or two publishers can interleave validation and publication, leaving a
/// ready namespace that gains records afterwards or holds unvalidated
/// deletes. Both paths lock the target first and the staging namespace
/// second, so concurrent holders always acquire in one order. The locks are
/// transaction-scoped and release on commit or rollback.
async fn lock_store_state_namespace(
    backend: &SqlxBackend,
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    target: &StoreStateRequest,
    namespace_id: &str,
) -> Result<()> {
    if backend.is_sqlite() {
        return Ok(());
    }
    for key in [store_state_target_key(target), namespace_id.to_string()] {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(key)
            .execute(&mut **tx)
            .await
            .sql_context("Failed to lock Store-state namespace")?;
    }
    Ok(())
}

pub async fn resolve_store_state(
    backend: &SqlxBackend,
    request: &StoreStateRequest,
) -> Result<Option<RecordView>> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT namespace_id FROM store_state_namespaces
         WHERE database_id = $1 AND store_name = $2 AND lifecycle = $3 AND status = 1
           AND scope_user_uuid = $4 AND projection_name = $5
           AND projection_version = $6 AND source_key = $7",
    )
    .bind(request.database.to_string())
    .bind(&request.store)
    .bind(request.lifecycle.as_db_int())
    .bind(store_state_scope(&request.scope))
    .bind(&request.projection.name)
    .bind(i64::from(request.projection.version))
    .bind(&request.source_key)
    .fetch_optional(backend.pool())
    .await
    .sql_context("Failed to resolve Store-state namespace")?;
    Ok(row.map(|(namespace_id,)| RecordView { namespace_id }))
}

pub async fn begin_store_state_staging(
    backend: &SqlxBackend,
    request: StoreStateRequest,
) -> Result<StagingToken> {
    if request.lifecycle == StoreStateLifecycle::Staging {
        return Err(BackendError::InvalidStoreStateStagingToken.into());
    }
    let namespace_id = uuid::Uuid::new_v4().to_string();
    let staging_source = namespace_id.as_bytes();
    sqlx::query(
        "INSERT INTO store_state_namespaces
         (namespace_id, database_id, store_name, lifecycle, status, scope_user_uuid,
          projection_name, projection_version, source_key, created_revision)
         VALUES ($1, $2, $3, $4, 0, $5, $6, $7, $8, NULL)",
    )
    .bind(&namespace_id)
    .bind(request.database.to_string())
    .bind(&request.store)
    .bind(StoreStateLifecycle::Staging.as_db_int())
    .bind(store_state_scope(&request.scope))
    .bind(&request.projection.name)
    .bind(i64::from(request.projection.version))
    .bind(staging_source)
    .execute(backend.pool())
    .await
    .sql_context("Failed to begin Store-state staging")?;
    Ok(StagingToken {
        namespace_id,
        target: request,
    })
}

pub async fn stage_store_state_records(
    backend: &SqlxBackend,
    token: &StagingToken,
    records: RecordMutations,
) -> Result<()> {
    let mut tx = backend
        .pool()
        .begin()
        .await
        .sql_context("Failed to stage Store-state records")?;
    if backend.is_sqlite() {
        sqlx::query("COMMIT; BEGIN IMMEDIATE")
            .execute(&mut *tx)
            .await
            .sql_context("Failed to lock Store-state staging transaction")?;
    }
    lock_store_state_namespace(backend, &mut tx, &token.target, &token.namespace_id).await?;
    let staging: Option<(i64,)> = sqlx::query_as(
        "SELECT lifecycle FROM store_state_namespaces WHERE namespace_id = $1 AND status = 0",
    )
    .bind(&token.namespace_id)
    .fetch_optional(&mut *tx)
    .await
    .sql_context("Failed to validate Store-state staging token")?;
    if staging != Some((StoreStateLifecycle::Staging.as_db_int(),)) {
        return Err(BackendError::InvalidStoreStateStagingToken.into());
    }
    #[cfg(feature = "testing")]
    super::fire_store_state_stage_pause(&token.namespace_id).await;
    for (key, value) in records {
        sqlx::query(
            "INSERT INTO store_state_records (namespace_id, record_key, record_value)
             VALUES ($1, $2, $3)
             ON CONFLICT (namespace_id, record_key)
             DO UPDATE SET record_value = EXCLUDED.record_value",
        )
        .bind(&token.namespace_id)
        .bind(key)
        .bind(value)
        .execute(&mut *tx)
        .await
        .sql_context("Failed to stage Store-state record")?;
    }
    tx.commit()
        .await
        .sql_context("Failed to commit Store-state record chunk")
}

/// Make a staging namespace ready.
///
/// A concurrent materializer can publish the same target first. That is not an
/// error: both callers derived the same state from the same source, so the
/// winner's namespace is adopted and this one is discarded. Any other failure
/// also discards the staging namespace, because the caller has surrendered its
/// token and can no longer abort it.
pub async fn publish_store_state(backend: &SqlxBackend, token: StagingToken) -> Result<RecordView> {
    match publish_staged_namespace(backend, &token).await {
        Ok(view) => Ok(view),
        Err(err) => {
            let winner = resolve_store_state(backend, &token.target).await?;
            discard_staging_namespace(backend, &token.namespace_id).await?;
            winner.ok_or(err)
        }
    }
}

async fn publish_staged_namespace(
    backend: &SqlxBackend,
    token: &StagingToken,
) -> Result<RecordView> {
    let mut tx = backend
        .pool()
        .begin()
        .await
        .sql_context("Failed to publish Store-state")?;
    if backend.is_sqlite() {
        sqlx::query("COMMIT; BEGIN IMMEDIATE")
            .execute(&mut *tx)
            .await
            .sql_context("Failed to lock Store-state publish transaction")?;
    }
    lock_store_state_namespace(backend, &mut tx, &token.target, &token.namespace_id).await?;
    let deletes: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM store_state_records WHERE namespace_id = $1 AND record_value IS NULL",
    )
    .bind(&token.namespace_id)
    .fetch_one(&mut *tx)
    .await
    .sql_context("Failed to validate Store-state records")?;
    if deletes.0 != 0 {
        return Err(BackendError::InvalidStoreStateStagingToken.into());
    }
    let result = sqlx::query(
        "UPDATE store_state_namespaces SET lifecycle = $1, status = 1, source_key = $2
         WHERE namespace_id = $3 AND lifecycle = $4 AND status = 0",
    )
    .bind(token.target.lifecycle.as_db_int())
    .bind(&token.target.source_key)
    .bind(&token.namespace_id)
    .bind(StoreStateLifecycle::Staging.as_db_int())
    .execute(&mut *tx)
    .await
    .sql_context("Failed to publish Store-state namespace")?;
    if result.rows_affected() != 1 {
        return Err(BackendError::InvalidStoreStateStagingToken.into());
    }
    tx.commit()
        .await
        .sql_context("Failed to commit Store-state publish")?;
    Ok(RecordView {
        namespace_id: token.namespace_id.clone(),
    })
}

/// Drop an unpublished namespace. Its records go with it through the schema's
/// `ON DELETE CASCADE`.
async fn discard_staging_namespace(backend: &SqlxBackend, namespace_id: &str) -> Result<()> {
    sqlx::query("DELETE FROM store_state_namespaces WHERE namespace_id = $1 AND status = 0")
        .bind(namespace_id)
        .execute(backend.pool())
        .await
        .sql_context("Failed to discard Store-state staging namespace")?;
    Ok(())
}

pub async fn abort_store_state(backend: &SqlxBackend, token: StagingToken) -> Result<()> {
    discard_staging_namespace(backend, &token.namespace_id).await
}

pub async fn store_state_record_get(
    backend: &SqlxBackend,
    view: &RecordView,
    key: &[u8],
) -> Result<Option<Vec<u8>>> {
    // One statement validates the view and reads the key, so a concurrent
    // clear either lands fully before this snapshot (invalid view) or fully
    // after it (live read) — it can never peel the namespace away mid-read
    // into a false missing key. Ready namespaces hold no NULL values (publish
    // rejects them), so a NULL read is an absent key, not an empty value.
    let row: Option<(i64, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT namespaces.status, records.record_value
         FROM store_state_namespaces AS namespaces
         LEFT JOIN store_state_records AS records
           ON records.namespace_id = namespaces.namespace_id AND records.record_key = $2
         WHERE namespaces.namespace_id = $1",
    )
    .bind(&view.namespace_id)
    .bind(key)
    .fetch_optional(backend.pool())
    .await
    .sql_context("Failed to get Store-state record")?;
    let Some((status, value)) = row else {
        return Err(BackendError::InvalidStoreStateView.into());
    };
    if !matches!(status, 1 | 2) {
        return Err(BackendError::InvalidStoreStateView.into());
    }
    Ok(value)
}

pub async fn store_state_record_scan(
    backend: &SqlxBackend,
    view: &RecordView,
    range: &RecordRange,
    after: Option<&[u8]>,
    limit: usize,
) -> Result<RecordPage> {
    let mut tx = backend
        .pool()
        .begin()
        .await
        .sql_context("Failed to scan Store-state records")?;
    // Validate the view and pin it: the shared lock blocks a concurrent clear
    // of this namespace until the page is read, so validation and the read
    // cannot straddle a clear into a false empty page. A SQLite read
    // transaction already sees a stable snapshot, so plain validation suffices
    // there. Validation runs before the `limit == 0` shortcut so an invalid
    // view errors instead of reading as an empty page.
    let validation = if backend.is_sqlite() {
        "SELECT status FROM store_state_namespaces WHERE namespace_id = $1"
    } else {
        "SELECT status FROM store_state_namespaces WHERE namespace_id = $1 FOR SHARE"
    };
    let status: Option<(i64,)> = sqlx::query_as(validation)
        .bind(&view.namespace_id)
        .fetch_optional(&mut *tx)
        .await
        .sql_context("Failed to validate Store-state view")?;
    if !matches!(status, Some((1 | 2,))) {
        return Err(BackendError::InvalidStoreStateView.into());
    }
    if limit == 0 {
        return Ok(RecordPage::default());
    }
    let sql_limit = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
    let rows: Vec<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "SELECT r.record_key, r.record_value FROM store_state_records r
         JOIN store_state_namespaces n ON n.namespace_id = r.namespace_id
         WHERE r.namespace_id = $1 AND n.status IN (1, 2)
           AND ($2 IS NULL OR r.record_key >= $2)
           AND ($3 IS NULL OR r.record_key < $3)
           AND ($4 IS NULL OR r.record_key > $4)
         ORDER BY r.record_key ASC LIMIT $5",
    )
    .bind(&view.namespace_id)
    .bind(range.start.as_deref())
    .bind(range.end.as_deref())
    .bind(after)
    .bind(sql_limit)
    .fetch_all(&mut *tx)
    .await
    .sql_context("Failed to scan Store-state records")?;
    tx.commit()
        .await
        .sql_context("Failed to commit Store-state record scan")?;
    let mut records = rows;
    let has_more = records.len() > limit;
    records.truncate(limit);
    let next = if has_more {
        records.last().map(|record| record.0.clone())
    } else {
        None
    };
    Ok(RecordPage { records, next })
}

/// Unlink every ready derived namespace and reclaim the previously unlinked
/// generation.
///
/// Clearing is two-phase because a reader that already resolved a view keeps
/// reading through it: unlinking removes the namespace from resolution, so the
/// next miss rebuilds, while the records stay readable until the following
/// clear reclaims them. Authoritative namespaces are never selected.
pub async fn clear_derived_store_state(backend: &SqlxBackend) -> Result<()> {
    let mut tx = backend
        .pool()
        .begin()
        .await
        .sql_context("Failed to begin derived Store-state clear")?;
    if backend.is_sqlite() {
        sqlx::query("COMMIT; BEGIN IMMEDIATE")
            .execute(&mut *tx)
            .await
            .sql_context("Failed to lock derived Store-state clear")?;
    }
    sqlx::query("DELETE FROM store_state_namespaces WHERE lifecycle = $1 AND status = 2")
        .bind(StoreStateLifecycle::Derived.as_db_int())
        .execute(&mut *tx)
        .await
        .sql_context("Failed to reclaim unlinked derived Store-state namespaces")?;
    sqlx::query("UPDATE store_state_namespaces SET status = 2 WHERE lifecycle = $1 AND status = 1")
        .bind(StoreStateLifecycle::Derived.as_db_int())
        .execute(&mut *tx)
        .await
        .sql_context("Failed to unlink derived Store-state namespaces")?;
    tx.commit()
        .await
        .sql_context("Failed to commit derived Store-state clear")
}

/// Get an entry by ID.
pub async fn get(backend: &SqlxBackend, id: &ID) -> Result<Entry> {
    let pool = backend.pool();

    let row: Option<(Vec<u8>,)> = sqlx::query_as("SELECT entry_cbor FROM entries WHERE id = $1")
        .bind(id.to_string())
        .fetch_optional(pool)
        .await
        .sql_context("Failed to get entry")?;

    match row {
        Some((bytes,)) => {
            let entry: Entry =
                serde_ipld_dagcbor::from_slice(&bytes).map_err(|e| BackendError::SqlxError {
                    reason: format!("CBOR deserialization failed: {e}"),
                    source: None,
                })?;
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
        Some((status,)) => VerificationStatus::from_db_int(status),
        None => Err(BackendError::VerificationStatusNotFound { id: id.clone() }.into()),
    }
}

/// Store an entry.
///
/// A *new* entry is stored as [`VerificationStatus::Unverified`]; the storage
/// path never accepts a caller-chosen status. Promotion to `Verified` is done
/// separately by the local validation pass via `update_verification_status`.
/// An entry already held is left untouched (content and status): a re-`put`
/// never demotes a prior local promotion.
pub async fn put(backend: &SqlxBackend, entry: Entry) -> Result<()> {
    // Validate entry before storing
    entry.validate()?;

    let pool = backend.pool();
    let id = entry.id();
    let is_root = entry.is_root();

    // For root entries, the tree_id is the entry's own ID.
    // For non-root entries, entry.root() returns Some(root_id).
    let tree_id = entry.root().unwrap_or_else(|| id.clone());

    let entry_cbor = serde_ipld_dagcbor::to_vec(&entry).map_err(|e| BackendError::SqlxError {
        reason: format!("CBOR serialization failed: {e}"),
        source: None,
    })?;

    // A newly stored entry is always Unverified; an already-held entry is
    // returned early below without its status being touched.
    let status_int: i64 = VerificationStatus::Unverified.as_db_int();

    // Use a transaction for atomicity.
    //
    // For SQLite, immediately upgrade to BEGIN IMMEDIATE so the write lock is
    // taken at transaction start rather than at first-INSERT time. sqlx's
    // default `pool.begin()` issues BEGIN DEFERRED for SQLite, which starts
    // as a read transaction. If two such transactions both read and then race
    // to upgrade to a write transaction, the loser receives SQLITE_BUSY
    // (code 5) or SQLITE_BUSY_SNAPSHOT (code 517) immediately — SQLite skips
    // its busy_handler in this case to prevent deadlock, so `busy_timeout`
    // does not help.
    //
    // BEGIN IMMEDIATE acquires the RESERVED lock up-front. Contending
    // transactions wait at BEGIN time (where busy_handler IS invoked, so the
    // configured busy_timeout applies) rather than failing mid-tx.
    //
    // The depth tracking in sqlx's Transaction state machine remains
    // consistent: it still sees one BEGIN ... COMMIT pair from its
    // perspective; the COMMIT we issue below is invisible to it.
    let mut tx = pool
        .begin()
        .await
        .sql_context("Failed to begin transaction")?;
    if backend.is_sqlite() {
        sqlx::query("COMMIT; BEGIN IMMEDIATE")
            .execute(&mut *tx)
            .await
            .sql_context("Failed to upgrade to IMMEDIATE transaction")?;
    }

    // Check if entry already exists - entries are content-addressable and immutable
    let existing_status: Option<(i64,)> =
        sqlx::query_as("SELECT verification_status FROM entries WHERE id = $1")
            .bind(id.to_string())
            .fetch_optional(&mut *tx)
            .await
            .sql_context("Failed to check entry existence")?;

    if existing_status.is_some() {
        // Entry exists. Content is content-addressed and immutable, and its
        // relationship set with it, so a re-`put` is a no-op. Critically we
        // do NOT touch `verification_status`: re-receiving an entry this node
        // already holds is routine on overlapping/bootstrap sync, and must
        // not demote a prior local `Verified` promotion back to `Unverified`.
        // Status is owned solely by the local validation pass via
        // `update_verification_status`.
        return Ok(());
    }

    // Insert new entry (we've already confirmed it doesn't exist)
    let is_root_int: i64 = if is_root { 1 } else { 0 };
    let tree_height = entry.height() as i64;
    sqlx::query(
        "INSERT INTO entries (id, tree_id, is_root, verification_status, height, entry_cbor)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id.to_string())
    .bind(tree_id.to_string())
    .bind(is_root_int)
    .bind(status_int)
    .bind(tree_height)
    .bind(&entry_cbor)
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

    // Insert subtrees (denormalized subtree data) and store parent relationships
    for store_name in entry.subtrees() {
        // Get resolved subtree height (falls back to tree height if not set)
        let subtree_height = entry.subtree_height(&store_name).unwrap_or(entry.height()) as i64;
        // Get subtree data (may be None if entry participates but has no data changes)
        let subtree_data = entry.data(&store_name).ok();

        insert_subtree(
            backend,
            &mut tx,
            &tree_id,
            &id,
            &store_name,
            subtree_height,
            subtree_data.map(|v| v.as_slice()),
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
    update_tips_for_entry(backend, &mut tx, &tree_id, &entry).await?;

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

/// Helper to insert subtree data with proper handling of nullable data column.
async fn insert_subtree(
    backend: &SqlxBackend,
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    tree_id: &ID,
    entry_id: &ID,
    store_name: &str,
    height: i64,
    data: Option<&[u8]>,
) -> Result<()> {
    let sql = if backend.is_sqlite() {
        "INSERT OR IGNORE INTO subtrees (tree_id, entry_id, store_name, height, data)
         VALUES ($1, $2, $3, $4, $5)"
    } else {
        "INSERT INTO subtrees (tree_id, entry_id, store_name, height, data)
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING"
    };

    sqlx::query(sql)
        .bind(tree_id.to_string())
        .bind(entry_id.to_string())
        .bind(store_name)
        .bind(height)
        // TODO(perf): copying to Vec just to satisfy sqlx's bind lifetime — adds an
        // allocation per subtree write on this hot path. Investigate binding the
        // borrowed `&[u8]` directly (via a wrapper that implements `Encode<'q>` for
        // both sqlite and postgres BLOB).
        .bind(data.map(|b| b.to_vec()))
        .execute(&mut **tx)
        .await
        .sql_context("Failed to insert subtree")?;

    Ok(())
}

/// Update the tips table when a new entry is added.
///
/// Tips are entries with no children. This function handles out-of-order arrival
/// by checking if the new entry already has children before adding it as a tip.
async fn update_tips_for_entry(
    backend: &SqlxBackend,
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    tree_id: &ID,
    entry: &Entry,
) -> Result<()> {
    let entry_id = entry.id_ref();

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

    let status_int: i64 = verification_status.as_db_int();

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

    let status_int: i64 = status.as_db_int();

    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT id FROM entries WHERE verification_status = $1")
            .bind(status_int)
            .fetch_all(pool)
            .await
            .sql_context("Failed to get entries by status")?;

    rows.into_iter().map(|(id,)| ID::parse(&id)).collect()
}

/// Get all root entry IDs.
pub async fn all_roots(backend: &SqlxBackend) -> Result<Vec<ID>> {
    let pool = backend.pool();

    let rows: Vec<(String,)> = sqlx::query_as("SELECT id FROM entries WHERE is_root = 1")
        .fetch_all(pool)
        .await
        .sql_context("Failed to get all roots")?;

    rows.into_iter().map(|(id,)| ID::parse(&id)).collect()
}

/// Get all entries in a tree, sorted by height.
pub async fn get_tree(backend: &SqlxBackend, tree: &ID) -> Result<Vec<Entry>> {
    let pool = backend.pool();

    let rows: Vec<(Vec<u8>,)> = sqlx::query_as("SELECT entry_cbor FROM entries WHERE tree_id = $1")
        .bind(tree.to_string())
        .fetch_all(pool)
        .await
        .sql_context("Failed to get tree")?;

    let mut entries = Vec::with_capacity(rows.len());
    for (bytes,) in rows {
        let entry: Entry =
            serde_ipld_dagcbor::from_slice(&bytes).map_err(|e| BackendError::SqlxError {
                reason: format!("CBOR deserialization failed: {e}"),
                source: None,
            })?;
        entries.push(entry);
    }

    // Sort by height (heights are stored in entries)
    sorting::sort_entries_by_height(&mut entries);

    Ok(entries)
}

/// Get all entries in a store, sorted by height.
pub async fn get_store(backend: &SqlxBackend, tree: &ID, store: &str) -> Result<Vec<Entry>> {
    let pool = backend.pool();

    let rows: Vec<(Vec<u8>,)> = sqlx::query_as(
        "SELECT e.entry_cbor
         FROM entries e
         JOIN subtrees s ON s.entry_id = e.id
         WHERE e.tree_id = $1 AND s.store_name = $2",
    )
    .bind(tree.to_string())
    .bind(store)
    .fetch_all(pool)
    .await
    .sql_context("Failed to get store")?;

    let mut entries = Vec::with_capacity(rows.len());
    for (bytes,) in rows {
        let entry: Entry =
            serde_ipld_dagcbor::from_slice(&bytes).map_err(|e| BackendError::SqlxError {
                reason: format!("CBOR deserialization failed: {e}"),
                source: None,
            })?;
        entries.push(entry);
    }

    sorting::sort_entries_by_store_height(store, &mut entries);

    Ok(entries)
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

// === Instance Secrets ===

/// Get instance secrets if they exist.
pub async fn get_instance_secrets(backend: &SqlxBackend) -> Result<Option<InstanceSecrets>> {
    let pool = backend.pool();

    let row: Option<(String,)> =
        sqlx::query_as("SELECT data FROM instance_secrets WHERE singleton = 1")
            .fetch_optional(pool)
            .await
            .sql_context("Failed to get instance secrets")?;

    match row {
        Some((json,)) => {
            let secrets: InstanceSecrets = serde_json::from_str(&json)
                .map_err(|e| BackendError::DeserializationFailed { source: e })?;
            Ok(Some(secrets))
        }
        None => Ok(None),
    }
}

/// Set instance secrets.
pub async fn set_instance_secrets(backend: &SqlxBackend, secrets: &InstanceSecrets) -> Result<()> {
    let pool = backend.pool();
    let json = serde_json::to_string(secrets)
        .map_err(|e| BackendError::SerializationFailed { source: e })?;

    if backend.is_sqlite() {
        sqlx::query("INSERT OR REPLACE INTO instance_secrets (singleton, data) VALUES (1, $1)")
            .bind(&json)
            .execute(pool)
            .await
            .sql_context("Failed to set instance secrets")?;
    } else {
        sqlx::query(
            "INSERT INTO instance_secrets (singleton, data) VALUES (1, $1)
             ON CONFLICT (singleton) DO UPDATE SET data = EXCLUDED.data",
        )
        .bind(&json)
        .execute(pool)
        .await
        .sql_context("Failed to set instance secrets")?;
    }

    Ok(())
}
