//! Bounded derived Store-state materialization cache for SQL backends.
//!
//! The cache substrate is the `store_state_namespaces` table itself:
//! every derived (`Derived`, live) namespace is one retained historical
//! Snapshot materialization, keyed by its full canonical `source_key`
//! bytes (equality-compared, never truncated — there is no digest
//! collision class). Bounds come from the one [`DerivedCachePolicy`]
//! surface; eviction is performance-only and never changes semantics
//! (a miss recomputes from history, and readers already retry an
//! invalidated view once — see `store::state::load_cached`).
//!
//! # Recency without a write per hit
//!
//! Reads touch only the in-process [`RecencyState`]. Dirty ticks flush as
//! one batched `CASE` statement: piggybacked in the eviction transaction,
//! or on a read path once `RECENCY_FLUSH_THRESHOLD` touches are dirty
//! (amortized — never one write per hit). A crash loses only recent
//! recency metadata, never cached values.
//!
//! # Eviction safety
//!
//! Eviction runs after a derived publish commits, in its own transaction:
//! `BEGIN IMMEDIATE` on SQLite serializes writers, and PostgreSQL takes a
//! transaction-scoped advisory lock on a fixed eviction key so concurrent
//! publish-triggered passes cannot over-evict each other. Victims are
//! unlinked to a dedicated evicted generation (`status = 3`), never
//! hard-deleted, so a reader holding a resolved view keeps reading through
//! it; the next pass reclaims the previous evicted generation.
//! `clear_derived_store_state` owns the other unlinked generation
//! (`status = 2`): eviction never reclaims it, so the clear contract —
//! unlinked stays readable until the following clear — is preserved
//! exactly. The just-published namespace is always protected, and the
//! current raw/verified frontier materializations are protected by recency
//! itself: every current read re-touches them, so oldest-first eviction
//! reaches cold history long before it reaches a hot frontier. (A separate
//! pin set would need a signal for "no longer current" that the backend
//! does not have; recency is the honest mechanism.)

use crate::Result;
use crate::backend::StoreStateLifecycle;

use super::{SqlxBackend, SqlxResultExt};

/// Advisory-lock key serializing derived-cache eviction passes on PostgreSQL.
const EVICT_LOCK_KEY: &str = "eidetica-derived-cache-evict-v1";

/// Largest single flush/evict batch: 256 namespaces × 2 binds stays far
/// under SQLite's legacy 999-variable limit.
const BATCH_CHUNK: usize = 256;

/// Ensure the tick clock starts above every persisted tick, once per backend.
///
/// Concurrent first-touches may both run the query; both advance to the
/// same maximum, so the race is benign.
pub(crate) async fn ensure_tick_seeded(backend: &SqlxBackend) -> Result<()> {
    if backend
        .cache_tick_seeded
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Ok(());
    }
    let row: (i64,) =
        sqlx::query_as("SELECT COALESCE(MAX(last_access_tick), 0) FROM store_state_namespaces")
            .fetch_one(backend.pool())
            .await
            .sql_context("Failed to seed Store-state recency clock")?;
    backend
        .cache_recency
        .lock()
        .unwrap()
        .advance_to(row.0.max(0) as u64);
    backend
        .cache_tick_seeded
        .store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// Record a cache hit in memory, flushing once the dirty set is full.
///
/// Never issues more than one write per `RECENCY_FLUSH_THRESHOLD` hits.
pub(crate) async fn touch(backend: &SqlxBackend, namespace_id: &str) -> Result<()> {
    ensure_tick_seeded(backend).await?;
    let need_flush = backend.cache_recency.lock().unwrap().touch(namespace_id).1;
    if need_flush {
        flush_dirty(backend).await?;
    }
    Ok(())
}

/// Assign a tick for a namespace about to be published, without marking it
/// dirty: the publish transaction writes the tick itself, and the caller
/// records it in memory after commit via [`record_published`].
pub(crate) async fn assign_publish_tick(backend: &SqlxBackend) -> Result<u64> {
    ensure_tick_seeded(backend).await?;
    let mut recency = backend.cache_recency.lock().unwrap();
    let tick = recency.now().saturating_add(1);
    recency.advance_to(tick);
    Ok(tick)
}

/// Record a committed publish tick in memory (already durable — not dirty).
pub(crate) fn record_published(backend: &SqlxBackend, namespace_id: &str, tick: u64) {
    backend
        .cache_recency
        .lock()
        .unwrap()
        .touch_with_tick(namespace_id, tick);
}

/// Flush accumulated recency touches in chunks of one batched statement.
pub(crate) async fn flush_dirty(backend: &SqlxBackend) -> Result<()> {
    let batch = backend.cache_recency.lock().unwrap().drain_dirty();
    for chunk in batch.chunks(BATCH_CHUNK) {
        flush_chunk(backend.pool(), chunk).await?;
    }
    if !batch.is_empty() {
        #[cfg(feature = "testing")]
        backend
            .recency_flushes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

/// Flush inside an already-open write transaction (eviction): the following
/// candidate `SELECT` in the same transaction must see the live order.
async fn flush_dirty_in_tx(
    backend: &SqlxBackend,
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
) -> Result<()> {
    let batch = backend.cache_recency.lock().unwrap().drain_dirty();
    for chunk in batch.chunks(BATCH_CHUNK) {
        flush_chunk(&mut **tx, chunk).await?;
    }
    if !batch.is_empty() {
        #[cfg(feature = "testing")]
        backend
            .recency_flushes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

async fn flush_chunk<'e, E>(executor: E, chunk: &[(String, u64)]) -> Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    if chunk.is_empty() {
        return Ok(());
    }
    // Drained batches can finish out of order. Compare in the UPDATE itself
    // so an older in-flight flush cannot lower a newer durable tick. CASE
    // works on both SQLite and PostgreSQL (unlike scalar MAX/GREATEST).
    let mut sql = String::from("UPDATE store_state_namespaces SET last_access_tick = CASE ");
    for i in 0..chunk.len() {
        sql.push_str(&format!(
            "WHEN namespace_id = ${} AND last_access_tick < ${} THEN ${} ",
            2 * i + 1,
            2 * i + 2,
            2 * i + 2
        ));
    }
    sql.push_str("ELSE last_access_tick END WHERE namespace_id IN (");
    for i in 0..chunk.len() {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&format!("${}", 2 * i + 1));
    }
    sql.push(')');
    // SAFETY: the statement is built only from constant SQL and numbered
    // placeholders; every namespace ID and tick stays a bound parameter.
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for (id, tick) in chunk {
        query = query.bind(id).bind(*tick as i64);
    }
    query
        .execute(executor)
        .await
        .sql_context("Failed to flush Store-state recency")?;
    Ok(())
}

/// Opportunistically trim live derived namespaces to the cache policy.
///
/// Runs after a derived publish commits. Reclaims the generation evicted by
/// the previous pass first, then unlinks the oldest live namespaces (persisted
/// recency just flushed, so the order is the best durable approximation of
/// LRU) until both the row and byte bounds hold. `protect` (the namespace
/// just published) is never a victim. Within budget the pass is a flush
/// plus two cheap aggregate queries.
pub(crate) async fn evict_derived(backend: &SqlxBackend, protect: Option<&str>) -> Result<()> {
    let policy = backend.cache_policy();
    let mut tx = backend
        .pool()
        .begin()
        .await
        .sql_context("Failed to begin Store-state eviction")?;
    if backend.is_sqlite() {
        sqlx::query("COMMIT; BEGIN IMMEDIATE")
            .execute(&mut *tx)
            .await
            .sql_context("Failed to lock Store-state eviction transaction")?;
    } else {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(EVICT_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .sql_context("Failed to lock Store-state eviction")?;
    }
    flush_dirty_in_tx(backend, &mut tx).await?;

    // Reclaim the generation evicted by the previous pass (records
    // cascade). Rows unlinked by `clear_derived_store_state` (`status = 2`)
    // are never reclaimed here: the clear contract keeps them readable
    // until the following clear.
    sqlx::query("DELETE FROM store_state_namespaces WHERE lifecycle = $1 AND status = 3")
        .bind(StoreStateLifecycle::Derived.as_db_int())
        .execute(&mut *tx)
        .await
        .sql_context("Failed to reclaim evicted Store-state namespaces")?;

    // `SUM(bigint)` is `numeric` on PostgreSQL, which the Any driver
    // cannot decode to `i64`; cast it back. SQLite `SUM` over integers
    // already yields an integer.
    let measure = if backend.is_sqlite() {
        "SELECT COUNT(*), SUM(size_bytes) FROM store_state_namespaces
         WHERE lifecycle = $1 AND status = 1"
    } else {
        "SELECT COUNT(*), SUM(size_bytes)::BIGINT FROM store_state_namespaces
         WHERE lifecycle = $1 AND status = 1"
    };
    let (live_namespaces, live_bytes): (i64, Option<i64>) = sqlx::query_as(measure)
        .bind(StoreStateLifecycle::Derived.as_db_int())
        .fetch_one(&mut *tx)
        .await
        .sql_context("Failed to measure Store-state cache")?;
    let live_bytes = live_bytes.unwrap_or(0).max(0) as u64;

    let over_rows = (live_namespaces.max(0) as usize).saturating_sub(policy.max_namespaces);
    let over_bytes = live_bytes.saturating_sub(policy.max_bytes);
    if over_rows > 0 || over_bytes > 0 {
        let candidates: Vec<(String, i64, i64)> = sqlx::query_as(
            "SELECT namespace_id, size_bytes, last_access_tick
             FROM store_state_namespaces
             WHERE lifecycle = $1 AND status = 1
             ORDER BY last_access_tick ASC, namespace_id ASC",
        )
        .bind(StoreStateLifecycle::Derived.as_db_int())
        .fetch_all(&mut *tx)
        .await
        .sql_context("Failed to list Store-state eviction candidates")?;
        let mut victims: Vec<String> = Vec::new();
        let mut freed_bytes: u64 = 0;
        for (id, size, _) in &candidates {
            if victims.len() >= over_rows && freed_bytes >= over_bytes {
                break;
            }
            if protect == Some(id.as_str()) {
                continue;
            }
            victims.push(id.clone());
            freed_bytes += (*size).max(0) as u64;
        }
        for chunk in victims.chunks(BATCH_CHUNK) {
            // Unlink to the evicted generation (`status = 3`), readable
            // like a clear-unlinked row and reclaimed by the next pass.
            let mut sql = String::from(
                "UPDATE store_state_namespaces SET status = 3 WHERE namespace_id IN (",
            );
            for i in 0..chunk.len() {
                if i > 0 {
                    sql.push_str(", ");
                }
                sql.push_str(&format!("${}", i + 1));
            }
            sql.push(')');
            // SAFETY: constant SQL plus numbered placeholders; every
            // namespace ID stays a bound parameter.
            let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
            for id in chunk {
                query = query.bind(id);
            }
            query
                .execute(&mut *tx)
                .await
                .sql_context("Failed to unlink Store-state eviction victims")?;
        }
    }

    // Prune touches for namespaces that no longer resolve. Evicted rows
    // stay tracked while readable; the reclaim above already dropped the
    // previous evicted generation.
    let live_ids: Vec<(String,)> = sqlx::query_as(
        "SELECT namespace_id FROM store_state_namespaces WHERE lifecycle = $1 AND status IN (1, 2, 3)",
    )
    .bind(StoreStateLifecycle::Derived.as_db_int())
    .fetch_all(&mut *tx)
    .await
    .sql_context("Failed to list live Store-state namespaces")?;
    backend.cache_recency.lock().unwrap().retain_existing(
        &live_ids
            .into_iter()
            .map(|(id,)| id)
            .collect::<std::collections::HashSet<_>>(),
    );

    tx.commit()
        .await
        .sql_context("Failed to commit Store-state eviction")?;
    Ok(())
}

#[cfg(all(test, feature = "testing"))]
mod tests {
    use super::*;
    use crate::backend::{
        BackendImpl, CacheScope, DerivedCachePolicy, ProjectionDescriptor, RecordView,
        StoreStateRequest,
    };
    use crate::entry::ID;

    async fn backend() -> Option<SqlxBackend> {
        #[cfg(feature = "postgres")]
        if std::env::var("TEST_BACKEND").as_deref() == Ok("postgres") {
            let url = std::env::var("TEST_POSTGRES_URL")
                .unwrap_or_else(|_| "postgres://localhost/eidetica_test".to_string());
            return Some(SqlxBackend::connect_postgres_isolated(&url).await.unwrap());
        }
        #[cfg(feature = "sqlite")]
        if std::env::var("TEST_BACKEND").as_deref() == Ok("sqlite") {
            return Some(
                SqlxBackend::connect_sqlite("sqlite::memory:")
                    .await
                    .unwrap(),
            );
        }
        None
    }

    async fn publish(backend: &SqlxBackend, store: &str) -> RecordView {
        let request = StoreStateRequest {
            database: ID::from_bytes("recency-db"),
            store: store.to_string(),
            lifecycle: StoreStateLifecycle::Derived,
            scope: CacheScope::Shared,
            projection: ProjectionDescriptor {
                name: "test/recency".to_string(),
                version: 1,
            },
            source_key: vec![1],
        };
        let token = backend.begin_store_state_staging(request).await.unwrap();
        backend.publish_store_state(token).await.unwrap()
    }

    async fn persisted_tick(backend: &SqlxBackend, view: &RecordView) -> u64 {
        let (tick,): (i64,) = sqlx::query_as(
            "SELECT last_access_tick FROM store_state_namespaces WHERE namespace_id = $1",
        )
        .bind(&view.namespace_id)
        .fetch_one(backend.pool())
        .await
        .unwrap();
        tick as u64
    }

    async fn assert_hot_survives(backend: &SqlxBackend, hot: &RecordView, cold: &RecordView) {
        backend.testing_set_derived_cache_policy(DerivedCachePolicy {
            max_namespaces: 1,
            max_bytes: u64::MAX,
        });
        evict_derived(backend, None).await.unwrap();
        // Query statuses directly: probing the cache API would itself touch recency.
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT namespace_id, status FROM store_state_namespaces ORDER BY namespace_id",
        )
        .fetch_all(backend.pool())
        .await
        .unwrap();
        assert!(
            rows.contains(&(hot.namespace_id.clone(), 1)),
            "hot entry evicted: {rows:?}"
        );
        assert!(
            rows.contains(&(cold.namespace_id.clone(), 3)),
            "cold entry retained: {rows:?}"
        );
    }

    #[tokio::test]
    async fn delayed_publication_hit_reaches_durable_recency() {
        let Some(backend) = backend().await else {
            return;
        };
        let view = publish(&backend, "published").await;
        let publish_tick = persisted_tick(&backend, &view).await;
        touch(&backend, &view.namespace_id).await.unwrap();
        let hit = backend
            .cache_recency
            .lock()
            .unwrap()
            .live_tick(&view.namespace_id)
            .unwrap();
        assert!(hit > publish_tick);
        // Replay the committed publisher's bookkeeping after a hit. Its first
        // recording is immaterial: this is the same state as a delayed resume.
        record_published(&backend, &view.namespace_id, publish_tick);
        flush_dirty(&backend).await.unwrap();
        assert_eq!(persisted_tick(&backend, &view).await, hit);
    }

    #[tokio::test]
    async fn delayed_publication_preserves_eviction_order() {
        let Some(backend) = backend().await else {
            return;
        };
        let hot = publish(&backend, "hot").await;
        let publish_tick = persisted_tick(&backend, &hot).await;
        let cold = publish(&backend, "cold").await;
        touch(&backend, &hot.namespace_id).await.unwrap();
        record_published(&backend, &hot.namespace_id, publish_tick);
        assert_hot_survives(&backend, &hot, &cold).await;
    }

    async fn flush_out_of_order(backend: &SqlxBackend, hot: &RecordView, cold: &RecordView) -> u64 {
        touch(backend, &hot.namespace_id).await.unwrap();
        touch(backend, &cold.namespace_id).await.unwrap();
        // A flush drains under the mutex, then can yield before its SQL write.
        let older = backend.cache_recency.lock().unwrap().drain_dirty();
        touch(backend, &cold.namespace_id).await.unwrap();
        touch(backend, &hot.namespace_id).await.unwrap();
        let newest = backend
            .cache_recency
            .lock()
            .unwrap()
            .live_tick(&hot.namespace_id)
            .unwrap();
        flush_dirty(backend).await.unwrap();
        assert_eq!(persisted_tick(backend, hot).await, newest);
        // Resume the old batch through the same SQL helper used by both flush
        // paths. Test transactional execution too, not just the pool executor.
        let mut tx = backend.pool().begin().await.unwrap();
        flush_chunk(&mut *tx, &older).await.unwrap();
        tx.commit().await.unwrap();
        newest
    }

    #[tokio::test]
    async fn stale_flush_cannot_lower_durable_recency() {
        let Some(backend) = backend().await else {
            return;
        };
        let hot = publish(&backend, "hot").await;
        let cold = publish(&backend, "cold").await;
        let newest = flush_out_of_order(&backend, &hot, &cold).await;
        assert_eq!(persisted_tick(&backend, &hot).await, newest);
        assert_eq!(persisted_tick(&backend, &cold).await, newest - 1);
        // Equal/repeated batches and missing rows are harmless as well.
        flush_chunk(
            backend.pool(),
            &[
                (hot.namespace_id.clone(), newest),
                ("missing".to_string(), newest + 1),
            ],
        )
        .await
        .unwrap();
        assert_eq!(persisted_tick(&backend, &hot).await, newest);
        flush_chunk(backend.pool(), &[]).await.unwrap();
    }

    #[tokio::test]
    async fn stale_flush_preserves_eviction_order() {
        let Some(backend) = backend().await else {
            return;
        };
        let hot = publish(&backend, "hot").await;
        let cold = publish(&backend, "cold").await;
        flush_out_of_order(&backend, &hot, &cold).await;
        assert_hot_survives(&backend, &hot, &cold).await;
    }
}
