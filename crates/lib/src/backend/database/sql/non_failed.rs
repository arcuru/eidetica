//! Retained selected-vertex frontier, independent of Verified ancestry.
//!
//! Callers serialize put/status decisions with `verified::lock_tree`. All
//! online maintenance uses indexed direct edges; reconstruction is reserved for
//! migration, explicit repair and offline reset.

use super::{SqlxBackend, SqlxResultExt};
use crate::{
    Result,
    backend::VerificationStatus,
    entry::{Entry, ID},
    snapshot::Snapshot,
};

pub(super) async fn snapshot(backend: &SqlxBackend, tree: &ID) -> Result<Snapshot> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT entry_id FROM non_failed_tips WHERE tree_id = $1")
            .bind(tree.to_string())
            .fetch_all(backend.pool())
            .await
            .sql_context("Failed to read non-Failed frontier")?;
    Ok(Snapshot::new(
        rows.into_iter()
            .map(|(id,)| ID::parse(&id))
            .collect::<Result<_>>()?,
    ))
}

pub(super) async fn inserted(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    tree: &ID,
    entry: &Entry,
) -> Result<()> {
    for parent in entry.parents()? {
        remove_tip(tx, tree, &parent).await?;
    }
    // A non-Failed child may already reference this newly arriving parent.
    insert_if_tip(tx, tree, &entry.id()).await
}

pub(super) async fn failed(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    tree: &ID,
    id: &ID,
) -> Result<()> {
    remove_tip(tx, tree, id).await?;
    let parents: Vec<(String,)> =
        sqlx::query_as("SELECT parent_id FROM tree_parents WHERE child_id = $1")
            .bind(id.to_string())
            .fetch_all(&mut **tx)
            .await
            .sql_context("Failed to read rejected entry's parents")?;
    for (parent,) in parents {
        insert_if_tip(tx, tree, &ID::parse(&parent)?).await?;
    }
    Ok(())
}

async fn remove_tip(tx: &mut sqlx::Transaction<'_, sqlx::Any>, tree: &ID, id: &ID) -> Result<()> {
    sqlx::query("DELETE FROM non_failed_tips WHERE tree_id = $1 AND entry_id = $2")
        .bind(tree.to_string())
        .bind(id.to_string())
        .execute(&mut **tx)
        .await
        .sql_context("Failed to remove non-Failed tip")?;
    Ok(())
}

// Shared with query-plan tests so they exercise the actual mutation statement.
pub(super) const INSERT_IF_TIP: &str = "INSERT INTO non_failed_tips (tree_id, entry_id)
    SELECT e.tree_id, e.id FROM entries e
    WHERE e.tree_id = $1 AND e.id = $2 AND e.verification_status != $3
      AND NOT EXISTS (
          SELECT 1 FROM tree_parents p JOIN entries child ON child.id = p.child_id
          WHERE p.parent_id = e.id AND child.tree_id = e.tree_id
            AND child.verification_status != $3)
    ON CONFLICT (tree_id, entry_id) DO NOTHING";

async fn insert_if_tip(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    tree: &ID,
    id: &ID,
) -> Result<()> {
    sqlx::query(INSERT_IF_TIP)
        .bind(tree.to_string())
        .bind(id.to_string())
        .bind(VerificationStatus::Failed.as_db_int())
        .execute(&mut **tx)
        .await
        .sql_context("Failed to restore non-Failed tip")?;
    Ok(())
}

pub(super) async fn rebuild_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    tree: &ID,
) -> Result<()> {
    sqlx::query("DELETE FROM non_failed_tips WHERE tree_id = $1")
        .bind(tree.to_string())
        .execute(&mut **tx)
        .await
        .sql_context("Failed to clear non-Failed frontier")?;
    sqlx::query(
        "INSERT INTO non_failed_tips (tree_id, entry_id)
         SELECT e.tree_id, e.id FROM entries e
         WHERE e.tree_id = $1 AND e.verification_status != $2
           AND NOT EXISTS (
               SELECT 1 FROM tree_parents p JOIN entries child ON child.id = p.child_id
               WHERE p.parent_id = e.id AND child.tree_id = e.tree_id
                 AND child.verification_status != $2)",
    )
    .bind(tree.to_string())
    .bind(VerificationStatus::Failed.as_db_int())
    .execute(&mut **tx)
    .await
    .sql_context("Failed to reconstruct non-Failed frontier")?;
    Ok(())
}

pub(super) async fn rebuild_all_in_tx(tx: &mut sqlx::Transaction<'_, sqlx::Any>) -> Result<()> {
    let trees: Vec<(String,)> = sqlx::query_as("SELECT DISTINCT tree_id FROM entries")
        .fetch_all(&mut **tx)
        .await
        .sql_context("Failed to list non-Failed reconstruction trees")?;
    for (tree,) in trees {
        rebuild_in_tx(tx, &ID::parse(&tree)?).await?;
    }
    Ok(())
}
