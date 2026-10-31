//! Retained verified tips, atomically published with ancestor-closed statuses.

use super::{SqlxBackend, SqlxResultExt};
use crate::{
    Result,
    backend::{VerificationRepair, VerificationStatus, errors::BackendError},
    entry::{Entry, ID},
    snapshot::Snapshot,
};

/// Begin a write transaction, upgrading SQLite to `BEGIN IMMEDIATE` so the
/// write lock is taken up-front (see `storage::put` for why DEFERRED races).
async fn begin_write_tx(backend: &SqlxBackend) -> Result<sqlx::Transaction<'_, sqlx::Any>> {
    let pool = backend.pool();
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
    Ok(tx)
}

/// Serialize status/frontier decisions within one tree on PostgreSQL. SQLite
/// already holds its write lock from `BEGIN IMMEDIATE`. Include the schema in
/// the advisory key so independent backend namespaces do not share tree locks.
/// The lock is released on commit or rollback, including error paths.
async fn lock_tree(
    backend: &SqlxBackend,
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    tree: &ID,
) -> Result<()> {
    if !backend.is_sqlite() {
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended(\
             format('eidetica-verified-v1:%s:%s', current_schema(), $1::text), 0))",
        )
        .bind(tree.to_string())
        .execute(&mut **tx)
        .await
        .sql_context("Failed to lock verified tree")?;
    }
    Ok(())
}

/// Read only the retained frontier; ordinary opens and reads never repair history.
pub async fn verified_snapshot(backend: &SqlxBackend, tree: &ID) -> Result<Snapshot> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT entry_id FROM verified_tips WHERE tree_id = $1")
            .bind(tree.to_string())
            .fetch_all(backend.pool())
            .await
            .sql_context("Failed to read verified frontier")?;
    Ok(Snapshot::new(
        rows.into_iter()
            .map(|(id,)| ID::parse(&id))
            .collect::<Result<_>>()?,
    ))
}

pub async fn update_verification_status(
    backend: &SqlxBackend,
    id: &ID,
    status: VerificationStatus,
) -> Result<()> {
    let mut tx = begin_write_tx(backend).await?;
    // Immutable routing information may be read before the tree lock. Mutable
    // status and all eligibility decisions must be read after acquiring it.
    let (tree, _) = entry_tree_and_status(&mut tx, id).await?;
    let tree = tree.ok_or_else(|| BackendError::EntryNotFound { id: id.clone() })?;
    lock_tree(backend, &mut tx, &tree).await?;
    let (_, current) = entry_tree_and_status(&mut tx, id).await?;
    let current = current.ok_or_else(|| BackendError::EntryNotFound { id: id.clone() })?;
    if current == status {
        tx.commit()
            .await
            .sql_context("Failed to commit status transaction")?;
        return Ok(());
    }
    if current == VerificationStatus::Failed {
        return Err(BackendError::EntryValidationFailed {
            entry_id: id.clone(),
            reason: "Failed verification is terminal; use an offline trust reset".into(),
        }
        .into());
    }
    if status == VerificationStatus::Verified {
        let (bytes,): (Vec<u8>,) = sqlx::query_as("SELECT entry_cbor FROM entries WHERE id = $1")
            .bind(id.to_string())
            .fetch_one(&mut *tx)
            .await
            .sql_context("Failed to read promotion entry")?;
        let entry = decode_entry(id, &tree, &bytes)?;
        let parents = entry.parents()?;
        for parent in &parents {
            let (parent_tree, parent_status) = entry_tree_and_status(&mut tx, parent).await?;
            if parent_tree.as_ref() != Some(&tree)
                || parent_status != Some(VerificationStatus::Verified)
            {
                return Err(BackendError::EntryValidationFailed {
                    entry_id: id.clone(),
                    reason: format!("parent {parent} must exist in the same tree and be Verified"),
                }
                .into());
            }
        }
        set_status(&mut tx, id, status).await?;
        for parent in parents {
            sqlx::query("DELETE FROM verified_tips WHERE tree_id = $1 AND entry_id = $2")
                .bind(tree.to_string())
                .bind(parent.to_string())
                .execute(&mut *tx)
                .await
                .sql_context("Failed to remove verified parent tip")?;
        }
        insert_tip(&mut tx, &tree, id).await?;
    } else {
        set_status(&mut tx, id, status).await?;
        // One repair of this tree cascades loss of Verified ancestry and
        // rebuilds tips once. Any decoding/storage failure rolls back everything.
        if current == VerificationStatus::Verified {
            repair_in_tx(&mut tx, &tree).await?;
        }
    }
    tx.commit()
        .await
        .sql_context("Failed to commit status transaction")?;
    Ok(())
}

pub async fn repair_verification(backend: &SqlxBackend, tree: &ID) -> Result<VerificationRepair> {
    let mut tx = begin_write_tx(backend).await?;
    lock_tree(backend, &mut tx, tree).await?;
    let result = repair_in_tx(&mut tx, tree).await?;
    tx.commit()
        .await
        .sql_context("Failed to commit verification repair")?;
    Ok(result)
}

/// Shared by migration, explicit repair, and demotion. Caller holds the tree
/// lock or exclusive offline migration ownership. Ancestry comes from immutable
/// Entries, not stored heights or the disposable frontier.
pub(crate) async fn repair_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    tree: &ID,
) -> Result<VerificationRepair> {
    let rows: Vec<(String, Vec<u8>, i64)> = sqlx::query_as(
        "SELECT id, entry_cbor, verification_status FROM entries WHERE tree_id = $1",
    )
    .bind(tree.to_string())
    .fetch_all(&mut **tx)
    .await
    .sql_context("Failed to load tree for repair")?;
    let mut entries = Vec::with_capacity(rows.len());
    for (id, bytes, status) in rows {
        entries.push((
            decode_entry(&ID::parse(&id)?, tree, &bytes)?,
            VerificationStatus::from_db_int(status)?,
        ));
    }
    let plan = super::super::verification::repair_plan(tree, &entries)?;
    for id in &plan.demoted {
        set_status(tx, id, VerificationStatus::Unverified).await?;
    }
    sqlx::query("DELETE FROM verified_tips WHERE tree_id = $1")
        .bind(tree.to_string())
        .execute(&mut **tx)
        .await
        .sql_context("Failed to clear verified tips")?;
    for id in plan.snapshot.tips() {
        insert_tip(tx, tree, id).await?;
    }
    Ok(VerificationRepair {
        demoted: plan.demoted.len(),
        snapshot: plan.snapshot,
    })
}

fn decode_entry(id: &ID, tree: &ID, bytes: &[u8]) -> Result<Entry> {
    let entry: Entry =
        serde_ipld_dagcbor::from_slice(bytes).map_err(|error| BackendError::SqlxError {
            reason: format!("Invalid immutable entry during verification maintenance: {error}"),
            source: None,
        })?;
    entry.validate()?;
    if entry.id() != *id || !entry.in_tree(tree) {
        return Err(BackendError::EntryValidationFailed {
            entry_id: id.clone(),
            reason: "stored entry identity/tree does not match immutable data".into(),
        }
        .into());
    }
    Ok(entry)
}

async fn set_status(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    id: &ID,
    status: VerificationStatus,
) -> Result<()> {
    sqlx::query("UPDATE entries SET verification_status = $1 WHERE id = $2")
        .bind(status.as_db_int())
        .bind(id.to_string())
        .execute(&mut **tx)
        .await
        .sql_context("Failed to update verification status")?;
    Ok(())
}

async fn entry_tree_and_status(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    id: &ID,
) -> Result<(Option<ID>, Option<VerificationStatus>)> {
    let row: Option<(String, i64)> =
        sqlx::query_as("SELECT tree_id, verification_status FROM entries WHERE id = $1")
            .bind(id.to_string())
            .fetch_optional(&mut **tx)
            .await
            .sql_context("Failed to read entry status")?;
    match row {
        Some((tree, status)) => Ok((
            Some(ID::parse(&tree)?),
            Some(VerificationStatus::from_db_int(status)?),
        )),
        None => Ok((None, None)),
    }
}

async fn insert_tip(tx: &mut sqlx::Transaction<'_, sqlx::Any>, tree: &ID, id: &ID) -> Result<()> {
    sqlx::query("INSERT INTO verified_tips (tree_id, entry_id) VALUES ($1, $2)")
        .bind(tree.to_string())
        .bind(id.to_string())
        .execute(&mut **tx)
        .await
        .sql_context("Failed to insert verified tip")?;
    Ok(())
}

#[cfg(all(test, feature = "postgres"))]
mod tests;
