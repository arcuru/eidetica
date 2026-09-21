//! Retained verified prefix/frontier for SQL backends.
//!
//! The `verified_prefix` table holds, per tree, the ancestor-closed
//! all-`Verified` prefix (`is_frontier = 0`) and its maximal frontier
//! (`is_frontier = 1`). The raw frontier lives in `tips`; this table is its
//! verified counterpart, maintained incrementally at the
//! `update_verification_status` boundary and rebuilt wholesale on migration,
//! repair, and demotion.

use std::collections::{HashMap, HashSet, VecDeque};

use super::{SqlxBackend, SqlxResultExt};
use crate::{
    Result,
    backend::{VerificationStatus, errors::BackendError},
    entry::ID,
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

/// Read the retained verified frontier without scanning history.
pub async fn verified_snapshot(backend: &SqlxBackend, tree: &ID) -> Result<Snapshot> {
    let pool = backend.pool();
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT entry_id FROM verified_prefix WHERE tree_id = $1 AND is_frontier = 1",
    )
    .bind(tree.to_string())
    .fetch_all(pool)
    .await
    .sql_context("Failed to read verified frontier")?;

    let mut tips = Vec::with_capacity(rows.len());
    for (id,) in rows {
        tips.push(ID::parse(&id)?);
    }
    Ok(Snapshot::new(tips))
}

/// Update an entry's verification status, maintaining the retained verified
/// state atomically in the same transaction.
///
/// Monotonic promotion (`Unverified -> Verified`) is admitted incrementally
/// and recursively activates already-`Verified` descendants waiting on it.
/// Any demotion out of `Verified` funnels into a per-tree rebuild inside the
/// same transaction, so readers never observe a promoted status paired with
/// the previous frontier (or vice versa).
pub async fn update_verification_status(
    backend: &SqlxBackend,
    id: &ID,
    verification_status: VerificationStatus,
) -> Result<()> {
    let mut tx = begin_write_tx(backend).await?;

    let (tree_id, current) = entry_tree_and_status(&mut tx, id).await?;
    let Some((tree_id, current)) = tree_id.zip(current) else {
        return Err(BackendError::EntryNotFound { id: id.clone() }.into());
    };
    if current == verification_status {
        tx.commit()
            .await
            .sql_context("Failed to commit transaction")?;
        return Ok(());
    }

    sqlx::query("UPDATE entries SET verification_status = $1 WHERE id = $2")
        .bind(verification_status.as_db_int())
        .bind(id.to_string())
        .execute(&mut *tx)
        .await
        .sql_context("Failed to update verification status")?;

    if verification_status == VerificationStatus::Verified {
        admit_cascade(&mut tx, &tree_id, id).await?;
    } else if current == VerificationStatus::Verified {
        rebuild_in_tx(&mut tx, &tree_id).await?;
    }

    tx.commit()
        .await
        .sql_context("Failed to commit transaction")?;
    Ok(())
}

/// Rebuild one tree's retained verified state, replacing it wholesale, and
/// return the rebuilt verified [`Snapshot`].
///
/// Each tree rebuilds in deterministic topological (height, ID) order — the
/// same order as the [`BackendImpl`](crate::backend::BackendImpl) default
/// oracle, which this must equal after every insertion/promotion order.
pub async fn rebuild_verified_state(backend: &SqlxBackend, tree: &ID) -> Result<Snapshot> {
    let mut tx = begin_write_tx(backend).await?;
    let snapshot = rebuild_in_tx(&mut tx, tree).await?;
    tx.commit()
        .await
        .sql_context("Failed to commit transaction")?;
    Ok(snapshot)
}

/// Rebuild `tree` inside an open transaction: delete, recompute, insert.
pub(crate) async fn rebuild_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    tree: &ID,
) -> Result<Snapshot> {
    sqlx::query("DELETE FROM verified_prefix WHERE tree_id = $1")
        .bind(tree.to_string())
        .execute(&mut **tx)
        .await
        .sql_context("Failed to clear verified prefix")?;

    // Kahn's topological order, not stored-height order: heights are
    // commit-time metadata and cannot be trusted on a rebuild path that
    // must handle whatever history holds. Ready set deterministic in
    // height-then-ID. In-degree counts only edges inside the held set.
    let rows: Vec<(String, i64, i64)> =
        sqlx::query_as("SELECT id, verification_status, height FROM entries WHERE tree_id = $1")
            .bind(tree.to_string())
            .fetch_all(&mut **tx)
            .await
            .sql_context("Failed to load tree for verified rebuild")?;

    let parent_rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT p.child_id, p.parent_id FROM tree_parents p
         JOIN entries e ON e.id = p.child_id
         WHERE e.tree_id = $1",
    )
    .bind(tree.to_string())
    .fetch_all(&mut **tx)
    .await
    .sql_context("Failed to load parents for verified rebuild")?;
    let held: HashSet<String> = rows.iter().map(|(id, _, _)| id.clone()).collect();
    let mut status_of: HashMap<String, VerificationStatus> = HashMap::with_capacity(rows.len());
    let mut height_of: HashMap<String, i64> = HashMap::with_capacity(rows.len());
    for (id, status_int, height) in &rows {
        status_of.insert(id.clone(), VerificationStatus::from_db_int(*status_int)?);
        height_of.insert(id.clone(), *height);
    }
    let mut parents: HashMap<String, Vec<String>> = HashMap::new();
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    let mut in_degree: HashMap<String, usize> = HashMap::with_capacity(rows.len());
    for (id, _, _) in &rows {
        in_degree.insert(id.clone(), 0);
    }
    for (child, parent) in parent_rows {
        children
            .entry(parent.clone())
            .or_default()
            .push(child.clone());
        parents
            .entry(child.clone())
            .or_default()
            .push(parent.clone());
        if held.contains(&parent) {
            *in_degree.entry(child).or_default() += 1;
        }
    }
    let mut ready: std::collections::BTreeSet<(i64, String)> = in_degree
        .iter()
        .filter(|&(_, &d)| d == 0)
        .map(|(id, _)| (height_of[id], id.clone()))
        .collect();

    let mut prefix: HashSet<String> = HashSet::new();
    let mut frontier: HashSet<String> = HashSet::new();
    while let Some((_, id)) = ready.pop_first() {
        if let Some(kids) = children.get(&id) {
            for kid in kids.clone() {
                let d = in_degree.get_mut(&kid).expect("in_degree entry exists");
                *d -= 1;
                if *d == 0 {
                    ready.insert((height_of[&kid], kid));
                }
            }
        }
        if status_of.get(&id) != Some(&VerificationStatus::Verified) {
            continue;
        }
        let empty = Vec::new();
        let entry_parents = parents.get(&id).unwrap_or(&empty);
        if entry_parents.iter().all(|p| prefix.contains(p)) {
            prefix.insert(id.clone());
            for p in entry_parents {
                frontier.remove(p);
            }
            frontier.insert(id.clone());
        }
    }

    for id in &frontier {
        insert_prefix_row(tx, tree, id, true).await?;
    }
    for id in prefix.iter().filter(|id| !frontier.contains(*id)) {
        insert_prefix_row(tx, tree, id, false).await?;
    }

    let mut tips = Vec::with_capacity(frontier.len());
    for id in &frontier {
        tips.push(ID::parse(id)?);
    }
    Ok(Snapshot::new(tips))
}

/// Admit `start` into the prefix if eligible, then recursively admit
/// already-`Verified` descendants waiting on it. Same eligibility rule as
/// the in-memory backend: own status `Verified`, every parent in the prefix.
async fn admit_cascade(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    tree: &ID,
    start: &ID,
) -> Result<()> {
    let mut queue: VecDeque<String> = VecDeque::from([start.to_string()]);
    while let Some(candidate) = queue.pop_front() {
        if prefix_contains(tx, tree, &candidate).await? {
            continue;
        }
        let (candidate_tree, status) = entry_tree_and_status_by_str(tx, &candidate).await?;
        if status != Some(VerificationStatus::Verified)
            || candidate_tree.as_deref() != Some(&tree.to_string())
        {
            continue;
        }
        let candidate_parents = parents_of(tx, &candidate).await?;
        let mut eligible = true;
        for p in &candidate_parents {
            if !prefix_contains(tx, tree, p).await? {
                eligible = false;
                break;
            }
        }
        if !eligible {
            continue;
        }
        insert_prefix_row(tx, tree, &candidate, true).await?;
        for p in &candidate_parents {
            sqlx::query(
                "UPDATE verified_prefix SET is_frontier = 0 WHERE tree_id = $1 AND entry_id = $2",
            )
            .bind(tree.to_string())
            .bind(p)
            .execute(&mut **tx)
            .await
            .sql_context("Failed to update verified frontier")?;
        }
        for child in children_of(tx, &candidate).await? {
            queue.push_back(child);
        }
    }
    Ok(())
}

/// Owning tree and verification status of one entry, if held.
async fn entry_tree_and_status(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    id: &ID,
) -> Result<(Option<ID>, Option<VerificationStatus>)> {
    entry_tree_and_status_by_str(tx, &id.to_string())
        .await
        .map(|(tree, status)| (tree.and_then(|t| ID::parse(&t).ok()), status))
}

async fn entry_tree_and_status_by_str(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    id: &str,
) -> Result<(Option<String>, Option<VerificationStatus>)> {
    let row: Option<(String, i64)> =
        sqlx::query_as("SELECT tree_id, verification_status FROM entries WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
            .sql_context("Failed to read entry status")?;
    match row {
        Some((tree, status_int)) => Ok((
            Some(tree),
            Some(VerificationStatus::from_db_int(status_int)?),
        )),
        None => Ok((None, None)),
    }
}

async fn prefix_contains(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    tree: &ID,
    entry: &str,
) -> Result<bool> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM verified_prefix WHERE tree_id = $1 AND entry_id = $2")
            .bind(tree.to_string())
            .bind(entry)
            .fetch_optional(&mut **tx)
            .await
            .sql_context("Failed to read verified prefix")?;
    Ok(row.is_some())
}

async fn parents_of(tx: &mut sqlx::Transaction<'_, sqlx::Any>, child: &str) -> Result<Vec<String>> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT parent_id FROM tree_parents WHERE child_id = $1")
            .bind(child)
            .fetch_all(&mut **tx)
            .await
            .sql_context("Failed to read entry parents")?;
    Ok(rows.into_iter().map(|(p,)| p).collect())
}

async fn children_of(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    parent: &str,
) -> Result<Vec<String>> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT child_id FROM tree_parents WHERE parent_id = $1")
            .bind(parent)
            .fetch_all(&mut **tx)
            .await
            .sql_context("Failed to read entry children")?;
    Ok(rows.into_iter().map(|(c,)| c).collect())
}

async fn insert_prefix_row(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    tree: &ID,
    entry: &str,
    is_frontier: bool,
) -> Result<()> {
    sqlx::query("INSERT INTO verified_prefix (tree_id, entry_id, is_frontier) VALUES ($1, $2, $3)")
        .bind(tree.to_string())
        .bind(entry)
        .bind(if is_frontier { 1i64 } else { 0i64 })
        .execute(&mut **tx)
        .await
        .sql_context("Failed to insert verified prefix row")?;
    Ok(())
}
