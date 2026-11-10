//! Entry storage operations for SQL backends.
//!
//! This module implements the core CRUD operations for entries using sqlx.

use std::collections::HashSet;

use crate::Result;
use crate::backend::errors::BackendError;
use crate::backend::{
    BlobMeta, CacheScope, InstanceMetadata, InstanceSecrets, RecordMutations, RecordPage,
    RecordRange, RecordView, StagingToken, StoreStateLifecycle, StoreStateRequest,
    VerificationStatus,
};
use crate::entry::{Entry, ID};

use super::{SqlxBackend, SqlxResultExt};
use crate::backend::database::sorting;

fn store_state_scope(scope: &CacheScope) -> &str {
    match scope {
        CacheScope::Shared => "",
        CacheScope::Login(user) => user,
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
    let view = row.map(|(namespace_id,)| RecordView { namespace_id });
    if let Some(view) = &view
        && request.lifecycle == StoreStateLifecycle::Derived
    {
        super::cache::touch(backend, &view.namespace_id).await?;
    }
    Ok(view)
}

pub async fn begin_store_state_staging(
    backend: &SqlxBackend,
    request: StoreStateRequest,
) -> Result<StagingToken> {
    if request.lifecycle == StoreStateLifecycle::Staging {
        return Err(BackendError::InvalidStoreStateStagingToken.into());
    }
    if request.lifecycle == StoreStateLifecycle::Derived && !backend.cache_policy().admits(0) {
        return Err(BackendError::StoreStateNotRetained {
            reason: "derived retention disabled".into(),
        }
        .into());
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

/// Publish, admission and eviction commit together. No fallible maintenance
/// runs after commit, and arbitrary errors never turn into winner adoption.
pub async fn publish_store_state(backend: &SqlxBackend, token: StagingToken) -> Result<RecordView> {
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
    super::cache::lock_maintenance(backend, &mut tx).await?;
    lock_store_state_namespace(backend, &mut tx, &token.target, &token.namespace_id).await?;
    let prepared = prepare_publication(backend, &mut tx, &token).await;
    let (view, tick, newly_published) = match prepared {
        Ok(result) => result,
        Err(error) => {
            tx.rollback()
                .await
                .sql_context("Store-state rollback outcome uncertain")?;
            let _ = discard_staging_namespace(backend, &token.namespace_id).await;
            return Err(error);
        }
    };
    let maintenance = if newly_published && token.target.lifecycle == StoreStateLifecycle::Derived {
        match super::cache::evict_in_tx(backend, &mut tx, Some(&view.namespace_id)).await {
            Ok(maintenance) => Some(maintenance),
            Err(error) => {
                // Only a confirmed rollback establishes optional non-retention.
                // Commit/rollback acknowledgement failures are never downgraded.
                tx.rollback()
                    .await
                    .sql_context("Store-state rollback outcome uncertain")?;
                let _ = discard_staging_namespace(backend, &token.namespace_id).await;
                return Err(BackendError::StoreStateNotRetained {
                    reason: format!("admission rolled back: {error}"),
                }
                .into());
            }
        }
    } else {
        None
    };
    tx.commit()
        .await
        .sql_context("Store-state publish commit outcome uncertain")?;
    if let Some(maintenance) = maintenance {
        super::cache::acknowledge(backend, maintenance);
    }
    if newly_published && token.target.lifecycle == StoreStateLifecycle::Derived {
        super::cache::record_published(backend, &view.namespace_id, tick);
    }
    Ok(view)
}

/// Total published record bytes (key plus value) for one namespace.
///
/// Ready namespaces hold no NULL values (publish rejects them), so plain
/// `SUM` lengths need only a `COALESCE` for the empty-record case.
fn published_size_expr(backend: &SqlxBackend, namespace_param: &'static str) -> String {
    let (key_len, value_len) = if backend.is_sqlite() {
        ("length(record_key)", "length(record_value)")
    } else {
        ("octet_length(record_key)", "octet_length(record_value)")
    };
    format!(
        "(SELECT COALESCE(SUM({key_len} + {value_len}), 0) FROM store_state_records \
         WHERE namespace_id = {namespace_param})"
    )
}

pub(super) async fn prepare_publication(
    backend: &SqlxBackend,
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    token: &StagingToken,
) -> Result<(RecordView, u64, bool)> {
    // The begin-time identity must agree before same-target adoption. A
    // malformed clone cannot publish another build or destroy a ready view.
    let valid: Option<(String,)> = sqlx::query_as(
        "SELECT namespace_id FROM store_state_namespaces
         WHERE namespace_id = $1 AND database_id = $2 AND store_name = $3
           AND scope_user_uuid = $4 AND projection_name = $5 AND projection_version = $6
           AND ((status = 0 AND lifecycle = $7) OR (status = 1 AND lifecycle = $8 AND source_key = $9))")
        .bind(&token.namespace_id).bind(token.target.database.to_string()).bind(&token.target.store)
        .bind(store_state_scope(&token.target.scope)).bind(&token.target.projection.name)
        .bind(i64::from(token.target.projection.version)).bind(StoreStateLifecycle::Staging.as_db_int())
        .bind(token.target.lifecycle.as_db_int()).bind(&token.target.source_key)
        .fetch_optional(&mut **tx).await.sql_context("Failed to validate publication identity")?;
    if valid.is_none() {
        return Err(BackendError::InvalidStoreStateStagingToken.into());
    }
    let winner: Option<(String,)> = sqlx::query_as(
        "SELECT namespace_id FROM store_state_namespaces
         WHERE database_id = $1 AND store_name = $2 AND lifecycle = $3 AND status = 1
           AND scope_user_uuid = $4 AND projection_name = $5
           AND projection_version = $6 AND source_key = $7",
    )
    .bind(token.target.database.to_string())
    .bind(&token.target.store)
    .bind(token.target.lifecycle.as_db_int())
    .bind(store_state_scope(&token.target.scope))
    .bind(&token.target.projection.name)
    .bind(i64::from(token.target.projection.version))
    .bind(&token.target.source_key)
    .fetch_optional(&mut **tx)
    .await
    .sql_context("Failed to inspect Store-state publication winner")?;
    if let Some((namespace_id,)) = winner {
        sqlx::query("DELETE FROM store_state_namespaces WHERE namespace_id = $1 AND status = 0")
            .bind(&token.namespace_id)
            .execute(&mut **tx)
            .await
            .sql_context("Failed to discard losing Store-state build")?;
        return Ok((RecordView { namespace_id }, 0, false));
    }
    let staging: Option<(i64,)> = sqlx::query_as(
        "SELECT lifecycle FROM store_state_namespaces WHERE namespace_id = $1 AND status = 0",
    )
    .bind(&token.namespace_id)
    .fetch_optional(&mut **tx)
    .await
    .sql_context("Failed to validate Store-state publication token")?;
    if staging != Some((StoreStateLifecycle::Staging.as_db_int(),)) {
        return Err(BackendError::InvalidStoreStateStagingToken.into());
    }
    let deletes: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM store_state_records WHERE namespace_id = $1 AND record_value IS NULL",
    )
    .bind(&token.namespace_id)
    .fetch_one(&mut **tx)
    .await
    .sql_context("Failed to validate Store-state records")?;
    if deletes.0 != 0 {
        return Err(BackendError::InvalidStoreStateStagingToken.into());
    }
    let size_expr = published_size_expr(backend, "$1");
    // SAFETY: the size expression and cast are dialect constants.
    let (size,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT CAST({size_expr} AS BIGINT)"
    )))
    .bind(&token.namespace_id)
    .fetch_one(&mut **tx)
    .await
    .sql_context("Failed to measure Store-state publication")?;
    if token.target.lifecycle == StoreStateLifecycle::Derived
        && !backend.cache_policy().admits(size as u64)
    {
        return Err(BackendError::StoreStateNotRetained {
            reason: "disabled or oversized materialization".into(),
        }
        .into());
    }
    let tick = if token.target.lifecycle == StoreStateLifecycle::Derived {
        super::cache::assign_publish_tick(backend).await?
    } else {
        0
    };
    let result = sqlx::query(
        "UPDATE store_state_namespaces SET lifecycle = $1, status = 1, source_key = $2,
         size_bytes = $4, last_access_tick = $5
         WHERE namespace_id = $3 AND lifecycle = $6 AND status = 0",
    )
    .bind(token.target.lifecycle.as_db_int())
    .bind(&token.target.source_key)
    .bind(&token.namespace_id)
    .bind(size)
    .bind(tick as i64)
    .bind(StoreStateLifecycle::Staging.as_db_int())
    .execute(&mut **tx)
    .await
    .sql_context("Failed to publish Store-state namespace")?;
    if result.rows_affected() != 1 {
        return Err(BackendError::InvalidStoreStateStagingToken.into());
    }
    Ok((
        RecordView {
            namespace_id: token.namespace_id.clone(),
        },
        tick,
        true,
    ))
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
    let row: Option<(i64, i64, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT namespaces.status, namespaces.lifecycle, records.record_value
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
    let Some((status, lifecycle, value)) = row else {
        return Err(BackendError::InvalidStoreStateView.into());
    };
    // Live (1), clear-unlinked (2), and evicted (3) namespaces all stay
    // readable through resolved views; only eviction reclaims generation 3,
    // and only a clear reclaims generation 2.
    if !matches!(status, 1..=3) {
        return Err(BackendError::InvalidStoreStateView.into());
    }
    if lifecycle == StoreStateLifecycle::Derived.as_db_int() {
        super::cache::touch(backend, &view.namespace_id).await?;
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
        "SELECT status, lifecycle FROM store_state_namespaces WHERE namespace_id = $1"
    } else {
        "SELECT status, lifecycle FROM store_state_namespaces WHERE namespace_id = $1 FOR SHARE"
    };
    let status: Option<(i64, i64)> = sqlx::query_as(validation)
        .bind(&view.namespace_id)
        .fetch_optional(&mut *tx)
        .await
        .sql_context("Failed to validate Store-state view")?;
    let Some((1..=3, lifecycle)) = status else {
        return Err(BackendError::InvalidStoreStateView.into());
    };
    if limit == 0 {
        return Ok(RecordPage::default());
    }
    let sql_limit = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
    let rows: Vec<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "SELECT r.record_key, r.record_value FROM store_state_records r
         JOIN store_state_namespaces n ON n.namespace_id = r.namespace_id
         WHERE r.namespace_id = $1 AND n.status IN (1, 2, 3)
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
    if lifecycle == StoreStateLifecycle::Derived.as_db_int() {
        super::cache::touch(backend, &view.namespace_id).await?;
    }
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

/// Unlink every live derived namespace and reclaim the previously
/// unlinked generation (clear-unlinked and evicted alike).
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
    super::cache::lock_maintenance(backend, &mut tx).await?;
    sqlx::query("DELETE FROM store_state_namespaces WHERE lifecycle = $1 AND status IN (2, 3)")
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

/// Offline trust reset: status, retained verified frontier and cache
/// invalidation commit together.
/// Unlike the online two-generation derived clear, there must be no live readers.
pub async fn reset_local_verification(backend: &SqlxBackend) -> Result<()> {
    let mut tx = backend
        .pool()
        .begin()
        .await
        .sql_context("Failed to begin trust reset")?;
    if backend.is_sqlite() {
        sqlx::query("COMMIT; BEGIN IMMEDIATE")
            .execute(&mut *tx)
            .await
            .sql_context("Failed to lock trust reset")?;
    }
    super::cache::lock_maintenance(backend, &mut tx).await?;
    sqlx::query("DELETE FROM store_state_namespaces WHERE lifecycle = $1 OR lifecycle = $2")
        .bind(StoreStateLifecycle::Derived.as_db_int())
        .bind(StoreStateLifecycle::Staging.as_db_int())
        .execute(&mut *tx)
        .await
        .sql_context("Failed to discard disposable Store state")?;
    sqlx::query("UPDATE entries SET verification_status = $1")
        .bind(VerificationStatus::Unverified.as_db_int())
        .execute(&mut *tx)
        .await
        .sql_context("Failed to reset verification statuses")?;
    // With every status Unverified the retained verified frontier is empty in
    // every tree; clearing it here publishes the reset atomically with the
    // status write, so no reader observes a stale retained frontier.
    sqlx::query("DELETE FROM verified_tips")
        .execute(&mut *tx)
        .await
        .sql_context("Failed to clear retained verified frontier")?;
    tx.commit()
        .await
        .sql_context("Failed to commit trust reset")
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
    table: &'static str,
    columns: &[&'static str],
    values: &[String],
) -> Result<()> {
    let cols = columns.join(", ");
    let placeholders: Vec<String> = (1..=columns.len()).map(|i| format!("${i}")).collect();
    let placeholders = placeholders.join(", ");

    // SAFETY: `table` and `columns` are fixed identifiers supplied only by this
    // module's call sites, and the generated placeholders contain only integers.
    // All row values remain bind parameters.
    let sql = if backend.is_sqlite() {
        format!("INSERT OR IGNORE INTO {table} ({cols}) VALUES ({placeholders})")
    } else {
        format!("INSERT INTO {table} ({cols}) VALUES ({placeholders}) ON CONFLICT DO NOTHING")
    };

    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
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
// === Blob Storage (content-addressed, durable) ===

/// Blobs up to 16 KiB stay inline in SQL. Larger blobs use a content-addressed
/// file when a blob directory is configured. This internal threshold affects
/// new writes only; reads follow each row's recorded location.
const INLINE_BLOB_THRESHOLD: u64 = 16 * 1024;

/// Where a blob's bytes live — the meaning of the `blobs.location` column. This
/// is the single place that integer mapping is defined; every read decodes
/// through [`from_db`](Self::from_db) so an unrecognized value is a hard error
/// rather than silently reading from the wrong tier.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlobLocation {
    /// Bytes inline in the SQL `data` column (small blobs).
    Inline,
    /// Bytes in a content-addressed file under the backend's blob dir.
    OnDisk,
}

impl BlobLocation {
    fn to_db(self) -> i64 {
        match self {
            Self::Inline => 0,
            Self::OnDisk => 1,
        }
    }

    fn from_db(value: i64) -> Result<Self> {
        match value {
            0 => Ok(Self::Inline),
            1 => Ok(Self::OnDisk),
            other => Err(BackendError::StateInconsistency {
                reason: format!("unknown blob location {other} (expected 0=inline, 1=on-disk)"),
            }
            .into()),
        }
    }
}

/// Store a blob under its content address. Idempotent: a row already present
/// for `cid` is left untouched. Disk bytes are atomically republished so a
/// successful write does not rely on an earlier file's durability. The caller
/// ([`super::SqlxBackend::put_blob`]) verifies `cid` matches the bytes before
/// this runs.
///
/// Hybrid tier: a blob larger than [`INLINE_BLOB_THRESHOLD`] is written
/// to a file on disk (`location = 1`, `data` NULL) when the backend has a blob
/// dir; everything else stays inline (`location = 0`). The file is written
/// *before* the row so a crash never leaves a row pointing at a missing file —
/// an orphan file is inert. Row-based GC does not reclaim orphan files.
pub async fn put_blob(backend: &SqlxBackend, cid: &ID, data: Vec<u8>) -> Result<()> {
    let pool = backend.pool();
    let size = data.len() as i64;
    // Persist the bao outboard alongside the metadata so a later range
    // serve reads only the requested window plus this ~0.4% sidecar instead of
    // whole-loading and re-hashing. Computed once, here, on the write path. The
    // outboard stays in SQL for both tiers (it is small; the data is what's big).
    let outboard = crate::blob::bao::compute_outboard(&data);

    // Large blobs go to disk when this backend has a blob dir; otherwise inline.
    let location = match backend.blob_dir() {
        Some(dir) if data.len() as u64 > INLINE_BLOB_THRESHOLD => {
            super::blob_disk::write_atomic(dir, cid, &data).await?;
            BlobLocation::OnDisk
        }
        _ => BlobLocation::Inline,
    };
    // Inline blobs carry their bytes in `data`; disk blobs store NULL there.
    let inline_data: Option<&[u8]> = match location {
        BlobLocation::Inline => Some(&data),
        BlobLocation::OnDisk => None,
    };

    // `last_accessed` defaults to 0 here; the caller stamps it to *now* via
    // `touch_blob_accessed` (see `Instance::persist_blob`) so LRU/GC see a fresh
    // blob and the grace window protects it.
    let sql = if backend.is_sqlite() {
        "INSERT OR IGNORE INTO blobs (cid, size, location, data, outboard) VALUES ($1, $2, $3, $4, $5)"
    } else {
        "INSERT INTO blobs (cid, size, location, data, outboard) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (cid) DO NOTHING"
    };

    sqlx::query(sql)
        .bind(cid.to_string())
        .bind(size)
        .bind(location.to_db())
        .bind(inline_data)
        .bind(&outboard)
        .execute(pool)
        .await
        .sql_context("Failed to store blob")?;

    Ok(())
}

/// Read a byte range of a blob's `data` without materializing the whole blob,
/// using the backend's native 1-indexed substring (SQLite `substr(b, from,
/// for)`, Postgres `substring(b FROM from FOR for)`). `range` is clamped to the
/// stored bytes: an over-long `end` yields the available tail and an
/// empty/past-the-end range yields empty bytes. Returns `Ok(None)` only if the
/// blob is not held.
///
/// Offsets are bound as `i32`, the native width of both dialects' substring
/// arguments — the `DEFAULT_MAX_BLOB_BYTES` cap keeps every offset well inside
/// 32 bits, so the saturating casts never actually clamp. Raising the cap past
/// ~2 GiB would require widening this (and moving to a disk tier for the data
/// anyway, since a >2 GiB SQL BLOB column is its own problem).
pub async fn get_blob_range(
    backend: &SqlxBackend,
    cid: &ID,
    range: std::ops::Range<u64>,
) -> Result<Option<Vec<u8>>> {
    let pool = backend.pool();
    // 1-indexed start for both dialects; length is the half-open span. Bound as
    // i32 so each dialect hits its native substring(_, int, int) signature.
    let start = range.start.saturating_add(1).min(i32::MAX as u64) as i32;
    let len = range.end.saturating_sub(range.start).min(i32::MAX as u64) as i32;

    // Fetch the tier alongside the inline substring in one round trip. For a
    // disk blob (`location = 1`) `data` is NULL, so the substring is NULL and we
    // then read the window from the file via `pread`.
    let sql = if backend.is_sqlite() {
        "SELECT location, substr(data, $2, $3) FROM blobs WHERE cid = $1"
    } else {
        "SELECT location, substring(data FROM $2 FOR $3) FROM blobs WHERE cid = $1"
    };

    let row: Option<(i64, Option<Vec<u8>>)> = sqlx::query_as(sql)
        .bind(cid.to_string())
        .bind(start)
        .bind(len)
        .fetch_optional(pool)
        .await
        .sql_context("Failed to read blob range")?;

    match row {
        None => Ok(None),
        Some((loc, bytes)) => match BlobLocation::from_db(loc)? {
            BlobLocation::Inline => Ok(Some(bytes.unwrap_or_default())),
            // On-disk tier: read only the requested window from the file.
            BlobLocation::OnDisk => {
                let dir = backend.blob_dir().ok_or_else(disk_tier_unavailable)?;
                // As in `get_blob`: a row recorded on disk with no file is an
                // inconsistency. (An empty/past-EOF range over a present file
                // still reads back as `Some(empty)`, so only a truly absent file
                // reaches this arm.)
                match super::blob_disk::read_range(dir, cid, range).await? {
                    Some(bytes) => Ok(Some(bytes)),
                    None => Err(blob_file_missing(cid)),
                }
            }
        },
    }
}

/// Error for an on-disk blob (`location = 1`) whose backend has no blob dir —
/// i.e. the database was opened without the blob tier that originally wrote it.
/// The bytes exist on disk but we can't locate them. Reopen the backend with
/// the same `with_blob_dir` (or `open_sqlite`) directory it was created with.
fn disk_tier_unavailable() -> crate::Error {
    BackendError::StateInconsistency {
        reason: "blob stored on disk (location=1) but this backend has no blob directory; \
                 reopen the backend with its on-disk blob tier (with_blob_dir / open_sqlite)"
            .to_string(),
    }
    .into()
}

/// Error for an on-disk blob (`location = 1`) whose backing file is gone — the
/// metadata row says we hold it but the bytes are absent (a lost rename after a
/// crash, an external deletion, or bit-rot). Distinct from "blob not held" so a
/// vanished blob does not read back as if it were never stored.
fn blob_file_missing(cid: &ID) -> crate::Error {
    BackendError::BlobFileMissing { cid: cid.clone() }.into()
}

/// Fetch a blob's `(size, pre-order outboard)` without materializing its data —
/// the input a verified range serve needs before reading the window.
/// Returns `Ok(None)` if the blob is not held.
pub async fn get_blob_header(backend: &SqlxBackend, cid: &ID) -> Result<Option<(u64, Vec<u8>)>> {
    let pool = backend.pool();
    let row: Option<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT size, outboard FROM blobs WHERE cid = $1")
            .bind(cid.to_string())
            .fetch_optional(pool)
            .await
            .sql_context("Failed to read blob header")?;
    row.map(|(size, outboard)| {
        let size = u64::try_from(size).map_err(|_| BackendError::StateInconsistency {
            reason: format!("negative size for blob {cid}"),
        })?;
        Ok((size, outboard))
    })
    .transpose()
}

/// Fetch a blob's bytes by content address, or `None` if not held locally.
/// Reads inline bytes from SQL (`location = 0`) or the whole file from the
/// on-disk tier (`location = 1`).
pub async fn get_blob(backend: &SqlxBackend, cid: &ID) -> Result<Option<Vec<u8>>> {
    let pool = backend.pool();

    let row: Option<(i64, Option<Vec<u8>>)> =
        sqlx::query_as("SELECT location, data FROM blobs WHERE cid = $1")
            .bind(cid.to_string())
            .fetch_optional(pool)
            .await
            .sql_context("Failed to get blob")?;

    match row {
        None => Ok(None),
        Some((loc, data)) => match BlobLocation::from_db(loc)? {
            BlobLocation::Inline => Ok(Some(data.unwrap_or_default())),
            BlobLocation::OnDisk => {
                let dir = backend.blob_dir().ok_or_else(disk_tier_unavailable)?;
                // The row says we hold these bytes on disk, so an absent file is
                // an inconsistency, not "not held" — surface it rather than
                // letting a vanished (possibly pinned) blob read back as None.
                match super::blob_disk::read_whole(dir, cid).await? {
                    Some(bytes) => Ok(Some(bytes)),
                    None => Err(blob_file_missing(cid)),
                }
            }
        },
    }
}

/// Cheap existence check for a blob, without materializing its bytes.
pub async fn has_blob(backend: &SqlxBackend, cid: &ID) -> Result<bool> {
    let pool = backend.pool();

    let row: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM blobs WHERE cid = $1")
        .bind(cid.to_string())
        .fetch_optional(pool)
        .await
        .sql_context("Failed to check blob existence")?;

    Ok(row.is_some())
}

/// Stamp a blob's `last_accessed` to `now_ms` (epoch ms). No-op if the blob is
/// not held locally. Called on every put and every local read hit so LRU
/// eviction reflects recency.
pub async fn touch_blob_accessed(backend: &SqlxBackend, cid: &ID, now_ms: i64) -> Result<()> {
    let pool = backend.pool();
    sqlx::query("UPDATE blobs SET last_accessed = $1 WHERE cid = $2")
        .bind(now_ms)
        .bind(cid.to_string())
        .execute(pool)
        .await
        .sql_context("Failed to touch blob access time")?;
    Ok(())
}

/// Delete a blob's bytes and row outright (complete-only deletion, §5.5).
/// Returns whether a row existed. Used by GC eviction; does not touch
/// `blob_pins`. For an on-disk blob, the file is unlinked after the row is
/// removed (row first: a dangling row — a `location = 1` entry whose bytes were
/// already unlinked — would read back as a hard error via `get_blob`, whereas
/// an orphan file is inert). Note an orphan file is *not* currently reclaimed by
/// GC, which sweeps rows only (`all_blob_meta`); a crash between the row delete
/// and the unlink can leave an orphan file.
pub async fn delete_blob(backend: &SqlxBackend, cid: &ID) -> Result<bool> {
    let pool = backend.pool();
    // Learn the tier before deleting so we know whether a file backs this blob.
    let location: Option<i64> = sqlx::query_as("SELECT location FROM blobs WHERE cid = $1")
        .bind(cid.to_string())
        .fetch_optional(pool)
        .await
        .sql_context("Failed to read blob location for delete")?
        .map(|(l,): (i64,)| l);

    let result = sqlx::query("DELETE FROM blobs WHERE cid = $1")
        .bind(cid.to_string())
        .execute(pool)
        .await
        .sql_context("Failed to delete blob")?;
    let existed = result.rows_affected() > 0;

    let on_disk = matches!(location, Some(l) if BlobLocation::from_db(l)? == BlobLocation::OnDisk);
    if existed
        && on_disk
        && let Some(dir) = backend.blob_dir()
    {
        super::blob_disk::delete(dir, cid).await?;
    }

    Ok(existed)
}

/// Enumerate every locally-held blob's `(cid, size, last_accessed)` — the GC
/// sweep input. This trusted engine substrate is not exposed as an Instance
/// method or service inventory operation.
pub async fn all_blob_meta(backend: &SqlxBackend) -> Result<Vec<BlobMeta>> {
    let pool = backend.pool();
    let rows: Vec<(String, i64, i64)> =
        sqlx::query_as("SELECT cid, size, last_accessed FROM blobs")
            .fetch_all(pool)
            .await
            .sql_context("Failed to list blob metadata")?;
    rows.into_iter()
        .map(|(cid, size, last_accessed)| {
            Ok(BlobMeta {
                cid: ID::parse(&cid)?,
                size: size.max(0) as u64,
                last_accessed,
            })
        })
        .collect()
}

/// Pin a blob for `(user, database)`. Idempotent. A pin keeps the blob from
/// being GC'd while it exists.
pub async fn pin_blob(
    backend: &SqlxBackend,
    user_id: &str,
    database_id: &str,
    blob_cid: &ID,
) -> Result<()> {
    let pool = backend.pool();
    let sql = if backend.is_sqlite() {
        "INSERT OR IGNORE INTO blob_pins (user_id, database_id, blob_cid) VALUES ($1, $2, $3)"
    } else {
        "INSERT INTO blob_pins (user_id, database_id, blob_cid) VALUES ($1, $2, $3)
         ON CONFLICT (user_id, database_id, blob_cid) DO NOTHING"
    };
    sqlx::query(sql)
        .bind(user_id)
        .bind(database_id)
        .bind(blob_cid.to_string())
        .execute(pool)
        .await
        .sql_context("Failed to pin blob")?;
    Ok(())
}

/// Remove a `(user, database, blob)` pin. Returns whether a pin existed.
pub async fn unpin_blob(
    backend: &SqlxBackend,
    user_id: &str,
    database_id: &str,
    blob_cid: &ID,
) -> Result<bool> {
    let pool = backend.pool();
    let result = sqlx::query(
        "DELETE FROM blob_pins WHERE user_id = $1 AND database_id = $2 AND blob_cid = $3",
    )
    .bind(user_id)
    .bind(database_id)
    .bind(blob_cid.to_string())
    .execute(pool)
    .await
    .sql_context("Failed to unpin blob")?;
    Ok(result.rows_affected() > 0)
}

/// The GC root set: every distinct blob CID with at least one live pin.
pub async fn pinned_cids(backend: &SqlxBackend) -> Result<HashSet<ID>> {
    let pool = backend.pool();
    let rows: Vec<(String,)> = sqlx::query_as("SELECT DISTINCT blob_cid FROM blob_pins")
        .fetch_all(pool)
        .await
        .sql_context("Failed to list pinned blobs")?;
    rows.into_iter().map(|(cid,)| ID::parse(&cid)).collect()
}

/// Every `(user_id, database_id, blob_cid)` pin row.
pub async fn all_blob_pins(backend: &SqlxBackend) -> Result<Vec<(String, String, ID)>> {
    let pool = backend.pool();
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT user_id, database_id, blob_cid FROM blob_pins")
            .fetch_all(pool)
            .await
            .sql_context("Failed to list blob pins")?;
    rows.into_iter()
        .map(|(user, database, cid)| Ok((user, database, ID::parse(&cid)?)))
        .collect()
}

/// Total bytes of the distinct blobs pinned by `user_id` (data-provenance /
/// quota accounting). A blob pinned under several `(user, database)` rows for
/// the same user counts once.
pub async fn pinned_size_by_user(backend: &SqlxBackend, user_id: &str) -> Result<u64> {
    let pool = backend.pool();
    // Postgres SUM(bigint) returns NUMERIC, which sqlx's Any driver cannot
    // decode. Keep the aggregate in the same integer domain as stored sizes.
    let row: Option<(Option<i64>,)> = sqlx::query_as(
        "SELECT CAST(COALESCE(SUM(b.size), 0) AS BIGINT) FROM (
             SELECT DISTINCT blob_cid FROM blob_pins WHERE user_id = $1
         ) p JOIN blobs b ON b.cid = p.blob_cid",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .sql_context("Failed to sum pinned size for user")?;
    Ok(row.and_then(|(s,)| s).unwrap_or(0).max(0) as u64)
}
