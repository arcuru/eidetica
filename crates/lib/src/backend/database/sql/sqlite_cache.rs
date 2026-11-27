//! SQLite's separately owned, disposable Derived record store.
//!
//! Identity is the existing database/Store/projection/version/trust-scope and
//! canonical Snapshot key. Unrelated history writes do not invalidate it.

use sqlx::Executor;

use super::{SqlxBackend, SqlxResultExt};
use crate::{Result, backend::errors::BackendError};

pub(super) const NAMESPACE_PREFIX: &str = "sqlite-cache-v1:";

fn invalid(reason: &str) -> crate::Error {
    BackendError::StateInconsistency {
        reason: format!("SQLite disposable storage: {reason}"),
    }
    .into()
}

/// Refuse unrelated, unsupported or corrupt files; never erase them on open.
/// Returns false only for a genuinely empty SQLite database.
pub(super) async fn validate(backend: &SqlxBackend) -> Result<bool> {
    let pool = backend.cache_pool();
    let tables: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    ).fetch_all(pool).await.sql_context("Failed to inspect SQLite cache file")?;
    if tables.is_empty() {
        let (objects,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sqlite_master")
            .fetch_one(pool)
            .await
            .sql_context("Failed to inspect empty SQLite cache")?;
        if objects != 0 {
            return Err(invalid("unrelated nonempty cache file"));
        }
        return Ok(false);
    }
    if tables
        != [
            ("cache_format".into(),),
            ("store_state_namespaces".into(),),
            ("store_state_records".into(),),
        ]
    {
        return Err(invalid(
            "unrecognized cache tables; preserve the file and correct its path (unreleased cache_binding formats are unsupported)",
        ));
    }
    let versions: Vec<(i64, i64)> = sqlx::query_as("SELECT singleton, version FROM cache_format")
        .fetch_all(pool)
        .await
        .sql_context("Failed to read SQLite cache format")?;
    if versions != [(1, 1)] {
        return Err(invalid("unsupported or malformed cache format"));
    }
    // A recognized marker is not permission to repair missing tables/columns.
    for (table, required) in [
        (
            "store_state_namespaces",
            &[
                "namespace_id",
                "database_id",
                "store_name",
                "lifecycle",
                "status",
                "scope_user_uuid",
                "projection_name",
                "projection_version",
                "source_key",
                "created_revision",
                "size_bytes",
                "last_access_tick",
            ][..],
        ),
        (
            "store_state_records",
            &["namespace_id", "record_key", "record_value"][..],
        ),
    ] {
        let columns: Vec<(String,)> = sqlx::query_as("SELECT name FROM pragma_table_info($1)")
            .bind(table)
            .fetch_all(pool)
            .await
            .sql_context("Failed to inspect SQLite cache columns")?;
        if required
            .iter()
            .any(|column| !columns.iter().any(|(name,)| name == column))
        {
            return Err(invalid("missing cache columns"));
        }
    }
    let (invalid_rows,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM store_state_namespaces WHERE lifecycle NOT IN (0, 2)
         OR status NOT IN (0, 1, 2, 3) OR (lifecycle = 2 AND status <> 0)
         OR namespace_id NOT GLOB 'sqlite-cache-v1:*'
         OR size_bytes < 0 OR last_access_tick < 0
         OR typeof(size_bytes) <> 'integer' OR typeof(last_access_tick) <> 'integer'",
    )
    .fetch_one(pool)
    .await
    .sql_context("Failed to validate SQLite cache rows")?;
    if invalid_rows != 0 {
        return Err(invalid("malformed cache namespaces/accounting"));
    }
    let (invalid_records,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM store_state_records r
         LEFT JOIN store_state_namespaces n USING(namespace_id)
         WHERE n.namespace_id IS NULL OR typeof(r.record_key) <> 'blob'
         OR (r.record_value IS NOT NULL AND typeof(r.record_value) <> 'blob')
         OR (n.status IN (1, 2, 3) AND r.record_value IS NULL)",
    )
    .fetch_one(pool)
    .await
    .sql_context("Failed to validate SQLite cache records")?;
    if invalid_records != 0 {
        return Err(invalid("orphaned or malformed cache records"));
    }
    Ok(true)
}

pub(super) async fn initialize(backend: &SqlxBackend) -> Result<()> {
    if !validate(backend).await? {
        let mut tx = backend
            .cache_pool()
            .begin()
            .await
            .sql_context("Failed to begin SQLite cache initialization")?;
        tx.execute("CREATE TABLE cache_format (singleton INTEGER PRIMARY KEY CHECK (singleton = 1), version INTEGER NOT NULL)")
            .await.sql_context("Failed to create SQLite cache format")?;
        for statement in super::schema::CREATE_STORE_STATE_TABLES {
            // SAFETY: schema and the lifecycle constraint are compile-time SQL.
            let statement = statement.replace(
                "lifecycle BIGINT NOT NULL,",
                "lifecycle BIGINT NOT NULL CHECK (lifecycle IN (0, 2)),",
            );
            tx.execute(sqlx::AssertSqlSafe(statement))
                .await
                .sql_context("Failed to create SQLite cache schema")?;
        }
        tx.execute("INSERT INTO cache_format VALUES (1, 1)")
            .await
            .sql_context("Failed to record SQLite cache format")?;
        tx.commit()
            .await
            .sql_context("SQLite cache initialization commit outcome uncertain")?;
    }
    // Legacy single-file Derived values are reconstructible. Keep the H tables
    // and version so supported v4 readers remain compatible; retain every
    // Authoritative value, credential and verification row.
    sqlx::query("DELETE FROM store_state_namespaces WHERE lifecycle IN (0, 2)")
        .execute(backend.pool())
        .await
        .sql_context("Failed to reclaim legacy disposable state")?;
    let (invalid_rows,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM store_state_namespaces WHERE lifecycle <> 1 OR status <> 1
         OR namespace_id LIKE 'sqlite-cache-%' OR size_bytes < 0 OR last_access_tick < 0
         OR typeof(size_bytes) <> 'integer' OR typeof(last_access_tick) <> 'integer'",
    )
    .fetch_one(backend.pool())
    .await
    .sql_context("Failed to validate authoritative Store state")?;
    if invalid_rows != 0 {
        return Err(invalid(
            "malformed Store-state cache counters or authoritative namespaces",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
