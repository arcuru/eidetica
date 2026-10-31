//! Derived cache admission/eviction is part of publication's transaction.
//! Reads copy dirty recency batches and acknowledge only committed ticks, so
//! failures, cancellation and out-of-order flushes cannot discard newer hits.
//! Evicted and clear-unlinked generations retain their separate lifetimes.

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
/// Flushes only when `RECENCY_FLUSH_THRESHOLD` distinct namespaces are dirty.
pub(crate) async fn touch(backend: &SqlxBackend, namespace_id: &str) -> Result<()> {
    ensure_tick_seeded(backend).await?;
    let need_flush = backend.cache_recency.lock().unwrap().touch(namespace_id).1;
    if need_flush {
        // Optional metadata maintenance must not turn a successful read into
        // failure. Pending ticks remain retryable (including after cancellation).
        if let Err(error) = flush_dirty(backend).await {
            tracing::warn!(%error, "Deferred Store-state recency flush");
        }
    }
    Ok(())
}

/// Assign a tick for a namespace about to be published, without marking it
/// dirty: the publish transaction writes the tick itself, and the caller
/// records it in memory after commit via [`record_published`].
pub(crate) async fn assign_publish_tick(backend: &SqlxBackend) -> Result<u64> {
    ensure_tick_seeded(backend).await?;
    let mut recency = backend.cache_recency.lock().unwrap();
    let tick = recency.now().saturating_add(1).min(i64::MAX as u64);
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
    let batch = backend.cache_recency.lock().unwrap().pending();
    for chunk in batch.chunks(BATCH_CHUNK) {
        flush_chunk(backend.pool(), chunk).await?;
        backend.cache_recency.lock().unwrap().acknowledge(chunk);
    }
    if !batch.is_empty() {
        #[cfg(feature = "testing")]
        backend
            .recency_flushes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

/// Flush the pending tail inside the eviction transaction. Other flushes may
/// still be in flight, so eviction also consults the live order.
async fn flush_dirty_in_tx(
    backend: &SqlxBackend,
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
) -> Result<Vec<(String, u64)>> {
    let batch = backend.cache_recency.lock().unwrap().pending();
    for chunk in batch.chunks(BATCH_CHUNK) {
        flush_chunk(&mut **tx, chunk).await?;
    }
    Ok(batch)
}

async fn flush_chunk<'e, E>(executor: E, chunk: &[(String, u64)]) -> Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Any>,
{
    if chunk.is_empty() {
        return Ok(());
    }
    // Copied batches can finish out of order. Compare in the UPDATE itself
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

/// Global admission lock precedes target/namespace locks on PostgreSQL.
/// SQLite's immediate writer transaction already serializes maintenance.
pub(crate) async fn lock_maintenance(
    backend: &SqlxBackend,
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
) -> Result<()> {
    if !backend.is_sqlite() {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(EVICT_LOCK_KEY)
            .execute(&mut **tx)
            .await
            .sql_context("Failed to lock Store-state admission")?;
    }
    Ok(())
}

/// Copied recency and reclaimed IDs to acknowledge only after commit.
pub(crate) struct Maintenance {
    batch: Vec<(String, u64)>,
    reclaimed: Vec<(String,)>,
}

pub(crate) fn acknowledge(backend: &SqlxBackend, maintenance: Maintenance) {
    #[cfg(feature = "testing")]
    if !maintenance.batch.is_empty() {
        backend
            .recency_flushes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    let mut recency = backend.cache_recency.lock().unwrap();
    recency.acknowledge(&maintenance.batch);
    for (id,) in maintenance.reclaimed {
        recency.remove(&id);
    }
}

/// Trim inside the caller's locked publication/startup transaction.
pub(crate) async fn evict_in_tx(
    backend: &SqlxBackend,
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    protect: Option<&str>,
) -> Result<Maintenance> {
    let policy = backend.cache_policy();
    let batch = flush_dirty_in_tx(backend, tx).await?;
    let reclaimed = sqlx::query_as(
        "SELECT namespace_id FROM store_state_namespaces WHERE lifecycle = $1 AND status = 3",
    )
    .bind(StoreStateLifecycle::Derived.as_db_int())
    .fetch_all(&mut **tx)
    .await
    .sql_context("Failed to list reclaimed Store-state namespaces")?;
    // Reclaim the generation evicted by the previous pass (records
    // cascade). Rows unlinked by `clear_derived_store_state` (`status = 2`)
    // are never reclaimed here: the clear contract keeps them readable
    // until the following clear.
    sqlx::query("DELETE FROM store_state_namespaces WHERE lifecycle = $1 AND status = 3")
        .bind(StoreStateLifecycle::Derived.as_db_int())
        .execute(&mut **tx)
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
        .fetch_one(&mut **tx)
        .await
        .sql_context("Failed to measure Store-state cache")?;
    let live_bytes = live_bytes.unwrap_or(0).max(0) as u64;

    let over_rows = (live_namespaces.max(0) as usize).saturating_sub(policy.max_namespaces);
    let over_bytes = live_bytes.saturating_sub(policy.max_bytes);
    if over_rows > 0 || over_bytes > 0 {
        let mut candidates: Vec<(String, i64, i64)> = sqlx::query_as(
            "SELECT namespace_id, size_bytes, last_access_tick
             FROM store_state_namespaces
             WHERE lifecycle = $1 AND status = 1",
        )
        .bind(StoreStateLifecycle::Derived.as_db_int())
        .fetch_all(&mut **tx)
        .await
        .sql_context("Failed to list Store-state eviction candidates")?;
        {
            // Another reader may have copied a newer hit into a flush that
            // has not reached SQL yet. The live order still contains that hit.
            let recency = backend.cache_recency.lock().unwrap();
            for (id, _, tick) in &mut candidates {
                *tick = (*tick).max(recency.live_tick(id).unwrap_or(0) as i64);
            }
        }
        candidates.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
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
                .execute(&mut **tx)
                .await
                .sql_context("Failed to unlink Store-state eviction victims")?;
        }
    }

    Ok(Maintenance { batch, reclaimed })
}

/// Exclusive startup: no views or builders survive from the previous owner.
/// A lowered policy is enforced before exposing the backend; failure refuses open.
pub(crate) async fn startup(backend: &SqlxBackend) -> Result<()> {
    let invalid_query = if backend.is_sqlite() {
        "SELECT COUNT(*) FROM store_state_namespaces WHERE size_bytes < 0 OR last_access_tick < 0 OR typeof(size_bytes) <> 'integer' OR typeof(last_access_tick) <> 'integer'"
    } else {
        "SELECT COUNT(*) FROM store_state_namespaces WHERE size_bytes < 0 OR last_access_tick < 0 OR size_bytes IS NULL OR last_access_tick IS NULL"
    };
    let (invalid,): (i64,) = sqlx::query_as(invalid_query)
        .fetch_one(backend.pool())
        .await
        .sql_context("Failed to validate cache accounting")?;
    if invalid != 0 {
        return Err(crate::backend::BackendError::StateInconsistency {
            reason: "malformed Store-state cache counters".into(),
        }
        .into());
    }
    ensure_tick_seeded(backend).await?;
    let mut tx = backend
        .pool()
        .begin()
        .await
        .sql_context("Failed to begin cache startup")?;
    if backend.is_sqlite() {
        sqlx::query("COMMIT; BEGIN IMMEDIATE")
            .execute(&mut *tx)
            .await
            .sql_context("Failed to lock cache startup")?;
    }
    lock_maintenance(backend, &mut tx).await?;
    sqlx::query("DELETE FROM store_state_namespaces WHERE lifecycle = $1 OR (lifecycle = $2 AND status IN (0, 2, 3))")
        .bind(StoreStateLifecycle::Staging.as_db_int())
        .bind(StoreStateLifecycle::Derived.as_db_int())
        .execute(&mut *tx).await.sql_context("Failed to reclaim abandoned Store state")?;
    let maintenance = evict_in_tx(backend, &mut tx, None).await?;
    sqlx::query("DELETE FROM store_state_namespaces WHERE lifecycle = $1 AND status = 3")
        .bind(StoreStateLifecycle::Derived.as_db_int())
        .execute(&mut *tx)
        .await
        .sql_context("Failed to reclaim startup cache victims")?;
    tx.commit()
        .await
        .sql_context("Failed to commit cache startup")?;
    acknowledge(backend, maintenance);
    Ok(())
}

#[cfg(all(test, feature = "testing"))]
async fn evict_derived(backend: &SqlxBackend, protect: Option<&str>) -> Result<()> {
    let mut tx = backend.pool().begin().await.unwrap();
    if backend.is_sqlite() {
        sqlx::query("COMMIT; BEGIN IMMEDIATE")
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    lock_maintenance(backend, &mut tx).await?;
    let maintenance = evict_in_tx(backend, &mut tx, protect).await?;
    tx.commit()
        .await
        .sql_context("Failed to commit cache trim")?;
    acknowledge(backend, maintenance);
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
        // A flush copies under the mutex, then can yield before its SQL write.
        let older = backend.cache_recency.lock().unwrap().pending();
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
    async fn eviction_preserves_hits_in_an_in_flight_flush() {
        let Some(backend) = backend().await else {
            return;
        };
        let hot = publish(&backend, "hot").await;
        let cold = publish(&backend, "cold").await;
        touch(&backend, &hot.namespace_id).await.unwrap();
        // A reader copied the batch and yielded before its SQL write. Eviction
        // must still see this hit in the live order, not just durable ticks.
        let in_flight = backend.cache_recency.lock().unwrap().pending();
        assert_hot_survives(&backend, &hot, &cold).await;
        flush_chunk(backend.pool(), &in_flight).await.unwrap();
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

    #[tokio::test]
    async fn authoritative_reads_do_not_enter_derived_recency() {
        let Some(backend) = backend().await else {
            return;
        };
        let before = backend.testing_recency_flush_count();
        for i in 0..64 {
            let request = StoreStateRequest {
                database: ID::from_bytes("authoritative-db"),
                store: format!("store-{i}"),
                lifecycle: StoreStateLifecycle::Authoritative,
                scope: CacheScope::Shared,
                projection: ProjectionDescriptor {
                    name: "test/recency".into(),
                    version: 1,
                },
                source_key: vec![1],
            };
            let token = backend
                .begin_store_state_staging(request.clone())
                .await
                .unwrap();
            let view = backend.publish_store_state(token).await.unwrap();
            assert_eq!(
                backend.resolve_store_state(&request).await.unwrap(),
                Some(view.clone())
            );
            assert_eq!(
                backend
                    .store_state_record_get(&view, b"absent")
                    .await
                    .unwrap(),
                None
            );
            assert!(
                backend
                    .store_state_record_scan(
                        &view,
                        &crate::backend::RecordRange::default(),
                        None,
                        1
                    )
                    .await
                    .unwrap()
                    .records
                    .is_empty()
            );
            assert_eq!(
                backend
                    .cache_recency
                    .lock()
                    .unwrap()
                    .live_tick(&view.namespace_id),
                None
            );
        }
        assert_eq!(backend.testing_recency_flush_count(), before);
    }
    #[tokio::test]
    async fn wide_frontier_publication_round_trips() {
        let Some(backend) = backend().await else {
            return;
        };
        let snapshot = crate::Snapshot::new(
            (0..128)
                .map(|i: u64| ID::from_bytes(i.to_le_bytes()))
                .collect(),
        );
        let request = StoreStateRequest {
            database: ID::from_bytes("wide-db"),
            store: "wide".into(),
            lifecycle: StoreStateLifecycle::Derived,
            scope: CacheScope::Shared,
            projection: ProjectionDescriptor {
                name: "test/wide".into(),
                version: 1,
            },
            source_key: snapshot.cache_key_bytes(),
        };
        let token = backend
            .begin_store_state_staging(request.clone())
            .await
            .unwrap();
        backend
            .stage_store_state_records(&token, [(b"key".to_vec(), Some(b"value".to_vec()))].into())
            .await
            .unwrap();
        let view = backend.publish_store_state(token).await.unwrap();
        assert_eq!(
            backend.resolve_store_state(&request).await.unwrap(),
            Some(view.clone())
        );
        assert_eq!(
            backend.store_state_record_get(&view, b"key").await.unwrap(),
            Some(b"value".to_vec())
        );
    }

    async fn fail_tick_writes(backend: &SqlxBackend) {
        use sqlx::Executor;
        if backend.is_sqlite() {
            backend.pool().execute("CREATE TRIGGER fail_tick BEFORE UPDATE OF last_access_tick ON store_state_namespaces BEGIN SELECT RAISE(ABORT, 'injected tick failure'); END").await.unwrap();
        } else {
            backend.pool().execute("CREATE FUNCTION fail_tick() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected tick failure'; END $$").await.unwrap();
            backend.pool().execute("CREATE TRIGGER fail_tick BEFORE UPDATE OF last_access_tick ON store_state_namespaces FOR EACH ROW EXECUTE FUNCTION fail_tick()").await.unwrap();
        }
    }

    async fn drop_tick_failure(backend: &SqlxBackend) {
        let ddl = if backend.is_sqlite() {
            "DROP TRIGGER fail_tick"
        } else {
            "DROP TRIGGER fail_tick ON store_state_namespaces"
        };
        sqlx::query(ddl).execute(backend.pool()).await.unwrap();
    }

    #[tokio::test]
    async fn failed_recency_batch_retries_without_failing_data_reads() {
        let Some(backend) = backend().await else {
            return;
        };
        let views = futures_util::future::join_all((0..64).map(|i| {
            let backend = &backend;
            async move { publish(backend, &format!("pending-{i}")).await }
        }))
        .await;
        fail_tick_writes(&backend).await;
        for view in &views {
            assert_eq!(
                backend
                    .store_state_record_get(view, b"absent")
                    .await
                    .unwrap(),
                None
            );
        }
        let batch = backend.cache_recency.lock().unwrap().pending();
        assert_eq!(batch.len(), 64);
        assert!(flush_dirty(&backend).await.is_err());
        assert_eq!(backend.cache_recency.lock().unwrap().pending().len(), 64);
        drop_tick_failure(&backend).await;
        flush_dirty(&backend).await.unwrap();
        assert!(backend.cache_recency.lock().unwrap().pending().is_empty());
        for (id, tick) in batch {
            assert_eq!(
                persisted_tick(&backend, &RecordView { namespace_id: id }).await,
                tick
            );
        }
    }

    #[tokio::test]
    async fn cancelled_recency_batch_remains_pending_and_retries() {
        let Some(backend) = backend().await else {
            return;
        };
        let view = publish(&backend, "cancel-flush").await;
        let old = persisted_tick(&backend, &view).await;
        touch(&backend, &view.namespace_id).await.unwrap();
        // Hold the row (Postgres) / sole connection (SQLite). Poll the real
        // flush until it has copied the batch and yielded, then cancel it.
        let mut blocker = backend.pool().begin().await.unwrap();
        sqlx::query("UPDATE store_state_namespaces SET last_access_tick = last_access_tick WHERE namespace_id = $1")
            .bind(&view.namespace_id).execute(&mut *blocker).await.unwrap();
        let mut flush = Box::pin(flush_dirty(&backend));
        assert!(futures_util::poll!(flush.as_mut()).is_pending());
        drop(flush);
        let copied = backend.cache_recency.lock().unwrap().pending();
        assert_eq!(copied.len(), 1, "cancellation discarded pending recency");
        blocker.rollback().await.unwrap();
        assert_eq!(persisted_tick(&backend, &view).await, old);
        flush_dirty(&backend).await.unwrap();
        assert_eq!(persisted_tick(&backend, &view).await, copied[0].1);
        assert!(backend.cache_recency.lock().unwrap().pending().is_empty());
    }

    #[tokio::test]
    async fn rolled_back_recency_and_older_ack_preserve_newer_hits() {
        let Some(backend) = backend().await else {
            return;
        };
        let view = publish(&backend, "ack-race").await;
        touch(&backend, &view.namespace_id).await.unwrap();
        let mut tx = backend.pool().begin().await.unwrap();
        let older = flush_dirty_in_tx(&backend, &mut tx).await.unwrap();
        // Cancellation of the surrounding publication rolls back this write.
        tx.rollback().await.unwrap();
        assert_eq!(backend.cache_recency.lock().unwrap().pending(), older);
        touch(&backend, &view.namespace_id).await.unwrap();
        let newer = backend.cache_recency.lock().unwrap().pending();
        backend.cache_recency.lock().unwrap().acknowledge(&older);
        assert_eq!(backend.cache_recency.lock().unwrap().pending(), newer);
        flush_dirty(&backend).await.unwrap();
        assert_eq!(persisted_tick(&backend, &view).await, newer[0].1);
    }

    #[tokio::test]
    async fn reclaim_failure_rolls_back_publication_not_post_commit_success() {
        use sqlx::Executor;
        let Some(backend) = backend().await else {
            return;
        };
        backend.testing_set_derived_cache_policy(DerivedCachePolicy {
            max_namespaces: 1,
            max_bytes: 1024,
        });
        let retired = publish(&backend, "retired").await;
        let current = publish(&backend, "current").await;
        // This fault used to happen after publication's successful commit.
        if backend.is_sqlite() {
            backend.pool().execute("CREATE TRIGGER fail_reclaim BEFORE DELETE ON store_state_namespaces WHEN OLD.lifecycle = 0 BEGIN SELECT RAISE(ABORT, 'injected reclaim failure'); END").await.unwrap();
        } else {
            backend.pool().execute("CREATE FUNCTION fail_reclaim() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF OLD.lifecycle = 0 THEN RAISE EXCEPTION 'injected reclaim failure'; END IF; RETURN OLD; END $$").await.unwrap();
            backend.pool().execute("CREATE TRIGGER fail_reclaim BEFORE DELETE ON store_state_namespaces FOR EACH ROW EXECUTE FUNCTION fail_reclaim()").await.unwrap();
        }
        let mut request = StoreStateRequest {
            database: ID::from_bytes("recency-db"),
            store: "failed".into(),
            lifecycle: StoreStateLifecycle::Derived,
            scope: CacheScope::Shared,
            projection: ProjectionDescriptor {
                name: "test/recency".into(),
                version: 1,
            },
            source_key: vec![1],
        };
        let token = backend
            .begin_store_state_staging(request.clone())
            .await
            .unwrap();
        let error = backend.publish_store_state(token).await.unwrap_err();
        assert!(error.is_store_state_not_retained(), "{error}");
        assert_eq!(
            backend.resolve_store_state(&request).await.unwrap(),
            None,
            "Err must not hide a committed publication"
        );
        request.store = "current".into();
        assert_eq!(
            backend.resolve_store_state(&request).await.unwrap(),
            Some(current.clone())
        );
        assert_eq!(
            backend
                .store_state_record_get(&retired, b"absent")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            backend
                .store_state_record_get(&current, b"absent")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn commit_failure_is_not_optional_non_retention() {
        use sqlx::Executor;
        let Some(backend) = backend().await else {
            return;
        };
        backend.pool().execute("CREATE TABLE commit_guard (parent TEXT REFERENCES store_state_namespaces(namespace_id) DEFERRABLE INITIALLY DEFERRED)").await.unwrap();
        if backend.is_sqlite() {
            backend.pool().execute("CREATE TRIGGER fail_commit AFTER UPDATE OF status ON store_state_namespaces WHEN NEW.status = 1 BEGIN INSERT INTO commit_guard (parent) VALUES ('missing'); END").await.unwrap();
        } else {
            backend.pool().execute("CREATE FUNCTION fail_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO commit_guard (parent) VALUES ('missing'); RETURN NEW; END $$").await.unwrap();
            backend.pool().execute("CREATE TRIGGER fail_commit AFTER UPDATE OF status ON store_state_namespaces FOR EACH ROW WHEN (NEW.status = 1) EXECUTE FUNCTION fail_commit()").await.unwrap();
        }
        let request = StoreStateRequest {
            database: ID::from_bytes("commit-db"),
            store: "uncertain".into(),
            lifecycle: StoreStateLifecycle::Derived,
            scope: CacheScope::Shared,
            projection: ProjectionDescriptor {
                name: "test/commit".into(),
                version: 1,
            },
            source_key: vec![1],
        };
        let token = backend
            .begin_store_state_staging(request.clone())
            .await
            .unwrap();
        let error = backend.publish_store_state(token).await.unwrap_err();
        assert!(
            !error.is_optional_store_state_unavailable(),
            "commit failure misclassified: {error}"
        );
        assert!(
            error.to_string().contains("commit outcome uncertain"),
            "{error}"
        );
        assert_eq!(backend.resolve_store_state(&request).await.unwrap(), None);
    }
    #[tokio::test]
    async fn cancelled_publication_rolls_back_admission_and_recency_together() {
        let Some(backend) = backend().await else {
            return;
        };
        backend.testing_set_derived_cache_policy(DerivedCachePolicy {
            max_namespaces: 1,
            max_bytes: 1024,
        });
        let keep = publish(&backend, "keep-on-cancel").await;
        touch(&backend, &keep.namespace_id).await.unwrap();
        let pending = backend.cache_recency.lock().unwrap().pending();
        let target = StoreStateRequest {
            database: ID::from_bytes("recency-db"),
            store: "cancelled".into(),
            lifecycle: StoreStateLifecycle::Derived,
            scope: CacheScope::Shared,
            projection: ProjectionDescriptor {
                name: "test/recency".into(),
                version: 1,
            },
            source_key: vec![1],
        };
        let token = backend
            .begin_store_state_staging(target.clone())
            .await
            .unwrap();
        let mut tx = backend.pool().begin().await.unwrap();
        if backend.is_sqlite() {
            sqlx::query("COMMIT; BEGIN IMMEDIATE")
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        lock_maintenance(&backend, &mut tx).await.unwrap();
        let (view, _, published) =
            super::super::storage::prepare_publication(&backend, &mut tx, &token)
                .await
                .unwrap();
        assert!(published);
        let _unacknowledged = evict_in_tx(&backend, &mut tx, Some(&view.namespace_id))
            .await
            .unwrap();
        // Dropping at this await boundary is the cancellation path of the
        // real publisher: neither its publication nor eviction can survive.
        drop(tx);
        assert_eq!(backend.resolve_store_state(&target).await.unwrap(), None);
        let status_sql = if backend.is_sqlite() {
            "SELECT status FROM store_state_namespaces WHERE namespace_id = $1"
        } else {
            "SELECT status FROM store_state_namespaces WHERE namespace_id = $1 FOR UPDATE"
        };
        let (status,): (i64,) = sqlx::query_as(status_sql)
            .bind(&keep.namespace_id)
            .fetch_one(backend.pool())
            .await
            .unwrap();
        assert_eq!(
            status, 1,
            "cancelled publication must not evict the prior live state"
        );
        assert_eq!(backend.cache_recency.lock().unwrap().pending(), pending);
        backend.abort_store_state(token).await.unwrap();
    }
}
