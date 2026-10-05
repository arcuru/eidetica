//! DAG traversal operations for SQL backends.
//!
//! This module implements graph traversal operations like finding tips,
//! computing merge bases, and collecting paths through the DAG using sqlx.

use std::collections::HashSet;

use crate::Result;
use crate::backend::errors::BackendError;
use crate::entry::{Entry, ID};

use super::{SqlxBackend, SqlxResultExt};
use crate::backend::database::sorting;

#[cfg(test)]
type SnapshotPause = (
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);
#[cfg(test)]
static SNAPSHOT_PAUSE: std::sync::OnceLock<std::sync::Mutex<Option<(ID, SnapshotPause)>>> =
    std::sync::OnceLock::new();

#[cfg(test)]
async fn pause_before_store_snapshot(tree: &ID) {
    let pause = SNAPSHOT_PAUSE
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .take_if(|(paused_tree, _)| paused_tree == tree);
    if let Some((_, (entered, resume))) = pause {
        let _ = entered.send(());
        let _ = resume.await;
    }
}

#[cfg(test)]
static AFTER_TIPS_PAUSE: std::sync::OnceLock<std::sync::Mutex<Option<(ID, SnapshotPause)>>> =
    std::sync::OnceLock::new();

#[cfg(test)]
async fn pause_after_tips_query(tree: &ID) {
    let pause = AFTER_TIPS_PAUSE
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .take_if(|(paused_tree, _)| paused_tree == tree);
    if let Some((_, (entered, resume))) = pause {
        let _ = entered.send(());
        let _ = resume.await;
    }
}

/// Get tree tips (entries with no children in the main tree).
pub async fn snapshot(backend: &SqlxBackend, tree: &ID) -> Result<Vec<ID>> {
    // Find a store with empty string name, used for tree-level tips
    store_snapshot(backend, tree, "").await
}

/// Get store tips (entries with no children in a specific store).
pub async fn store_snapshot(backend: &SqlxBackend, tree: &ID, store: &str) -> Result<Vec<ID>> {
    let pool = backend.pool();

    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT entry_id FROM tips WHERE tree_id = $1 AND store_name = $2")
            .bind(tree.to_string())
            .bind(store)
            .fetch_all(pool)
            .await
            .sql_context("Failed to get store tips")?;

    rows.into_iter().map(|(id,)| ID::parse(&id)).collect()
}

/// Get store tips that are reachable from the given main tree entries.
pub async fn store_snapshot_at(
    backend: &SqlxBackend,
    tree: &ID,
    store: &str,
    main_entries: &[ID],
) -> Result<Vec<ID>> {
    if main_entries.is_empty() {
        return Ok(Vec::new());
    }

    #[cfg(test)]
    pause_before_store_snapshot(tree).await;
    let pool = backend.pool();

    // The comparison and both tip sets must come from one statement snapshot.
    // A separate store_snapshot after checking current tips can observe a newer
    // grant than the main_entries boundary and break signed settings pins.
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT store_name, entry_id FROM tips
         WHERE tree_id = $1 AND store_name IN ('', $2)",
    )
    .bind(tree.to_string())
    .bind(store)
    .fetch_all(pool)
    .await
    .sql_context("Failed to get current tree and store tips")?;
    let mut current_tree_tips = HashSet::new();
    let mut current_store_tips = Vec::new();
    for (name, entry_id) in rows {
        let id = ID::parse(&entry_id)?;
        if name.is_empty() {
            current_tree_tips.insert(id.clone());
        }
        if name == store {
            current_store_tips.push(id);
        }
    }
    #[cfg(test)]
    pause_after_tips_query(tree).await;
    if current_tree_tips == main_entries.iter().cloned().collect() {
        // This is a raw frontier index, not a proof of complete/Verified
        // ancestry. Auth validation independently walks main/delegated history
        // before promotion; manual status injection or legacy labels without
        // an offline trust reset violate that boundary.
        return Ok(current_store_tips);
    }

    // The empty store name denotes the main tree. Complete ancestry must be
    // present before deriving its historical frontier.
    if store.is_empty() {
        let entries = get_tree_from_tips(backend, tree, main_entries).await?;
        return sorting::tree_tips_from_entries(&entries);
    }

    // Historical boundaries traverse immutable ancestry. Report a missing or
    // foreign ancestor instead of silently materializing partial settings.
    let start_selects: Vec<String> = (1..=main_entries.len())
        .map(|i| format!("SELECT ${} AS id", i + 2)) // $1 store, $2 tree
        .collect();
    let starts_union = start_selects.join(" UNION ALL ");

    // CTE query that:
    // 1. Recursively traverses ancestors via tree_parents
    // 2. Joins with subtrees to find entries in the store
    // 3. Finds tips by excluding entries that are parents of other reachable entries
    let sql = format!(
        "WITH RECURSIVE reachable AS (
            -- Start from main_entries
            {starts_union}

            UNION

            -- Follow tree parents
            SELECT tp.parent_id AS id
            FROM reachable r
            JOIN tree_parents tp ON tp.child_id = r.id
        ),
        -- Filter to entries that are in the store
        store_entries AS (
            SELECT DISTINCT r.id
            FROM reachable r
            JOIN subtrees s ON s.entry_id = r.id AND s.store_name = $1
        ),
        -- Find entries that are parents of other store entries
        non_tips AS (
            SELECT DISTINCT sp.parent_id AS id
            FROM store_entries se
            JOIN store_parents sp ON sp.child_id = se.id AND sp.store_name = $1
            WHERE sp.parent_id IN (SELECT id FROM store_entries)
        )
        -- Return both tips and any incomplete or foreign ancestry. A single
        -- statement snapshot makes the completeness check and tip result agree.
        SELECT se.id, CAST(NULL AS TEXT), CAST(NULL AS INTEGER)
        FROM store_entries se
        WHERE se.id NOT IN (SELECT id FROM non_tips)

        UNION ALL

        SELECT CAST(NULL AS TEXT), r.id,
               CASE WHEN e.id IS NULL THEN 0 ELSE 1 END
        FROM reachable r
        LEFT JOIN entries e ON e.id = r.id
        WHERE e.id IS NULL OR e.tree_id <> $2"
    );

    // SAFETY: the only generated fragment is a sequence of numbered bind
    // placeholders derived from `main_entries.len()`; all values remain bound.
    let mut query = sqlx::query_as::<_, (Option<String>, Option<String>, Option<i32>)>(
        sqlx::AssertSqlSafe(sql),
    )
    .bind(store)
    .bind(tree.to_string());
    for entry in main_entries {
        query = query.bind(entry.to_string());
    }

    let rows = query
        .fetch_all(pool)
        .await
        .sql_context("Failed to get store tips up to entries")?;

    let mut tips = Vec::new();
    for (tip, invalid_id, wrong_tree) in rows {
        if let Some(id) = invalid_id {
            let id = ID::parse(&id)?;
            return if wrong_tree == Some(1) {
                Err(BackendError::EntryNotInTree {
                    entry_id: id,
                    tree_id: tree.clone(),
                }
                .into())
            } else {
                Err(BackendError::EntryNotFound { id }.into())
            };
        }
        if let Some(id) = tip {
            tips.push(ID::parse(&id)?);
        }
    }
    Ok(tips)
}

/// Depth limit for ancestor traversal in find_merge_base.
/// For typical shallow divergence (< 100 commits), this captures the merge base.
const MERGE_BASE_DEPTH_LIMIT: usize = 100;

/// Find the merge base (common dominator) of the given entries in a store.
///
/// The merge base is the lowest ancestor that ALL paths from ALL entries must pass through.
///
/// This implementation uses depth-bounded ancestor collection with multi-batch continuation
/// to avoid pulling entire history for deep DAGs. For typical shallow divergence, the merge
/// base is found in a single batch. For deeper divergence, additional batches are pulled
/// until a common ancestor is found or roots are reached.
/// Returns `Some(id)` for the merge base, or `None` when the entries share no
/// common ancestor and must merge from the empty base.
pub async fn find_merge_base(
    backend: &SqlxBackend,
    tree: &ID,
    store: &str,
    entry_ids: &[ID],
) -> Result<Option<ID>> {
    if entry_ids.is_empty() {
        return Err(BackendError::EmptyEntryList {
            operation: "find_merge_base".to_string(),
        }
        .into());
    }

    if entry_ids.len() == 1 {
        return Ok(Some(entry_ids[0].clone()));
    }

    // An unknown or foreign tip must error like the in-memory backend does,
    // not fall out of the frontier JOINs as a silent partial merge.
    validate_tips_in_tree(backend, tree, entry_ids).await?;

    // Track all known ancestors per tip and their frontiers for continuation
    let mut ancestor_sets: Vec<HashSet<ID>> = vec![HashSet::new(); entry_ids.len()];
    let mut frontiers: Vec<Vec<ID>> = entry_ids.iter().map(|id| vec![id.clone()]).collect();
    let mut all_with_heights: Vec<(ID, i64)> = Vec::new();
    // Store roots within the queried ancestry only; collected with the existing walk query.
    let mut walked_roots: HashSet<ID> = HashSet::new();
    let trace_topology = tracing::enabled!(tracing::Level::DEBUG);
    // Common ancestors seen on the most recent intersection. The frontiers can drain
    // either because no common ancestor exists or because none of them dominates every
    // path, and the empty-base event below distinguishes the two.
    let mut common_ancestor_count = 0usize;

    loop {
        // Check if all frontiers are exhausted (reached roots without finding a usable base)
        if frontiers.iter().all(|f| f.is_empty()) {
            if trace_topology {
                tracing::debug!(
                    store = store,
                    common_ancestor_count,
                    // Each tip's ancestor membership counts, including shared entries per tip.
                    walked_entry_count = all_with_heights.len(),
                    multiple_roots = walked_roots.len() > 1,
                    "Frontiers reached the store roots with no dominating common ancestor; \
                     merging from the empty base"
                );
            }
            return Ok(None);
        }

        // Pull next batch from each non-empty frontier
        for (i, frontier) in frontiers.iter_mut().enumerate() {
            if frontier.is_empty() {
                continue;
            }

            let (ancestors, new_frontier, roots) = collect_ancestors_from_frontier(
                backend,
                store,
                frontier,
                MERGE_BASE_DEPTH_LIMIT,
                trace_topology,
            )
            .await?;
            walked_roots.extend(roots);

            // Add to known ancestors (filtering duplicates)
            for (id, height) in ancestors {
                if ancestor_sets[i].insert(id.clone()) {
                    all_with_heights.push((id, height));
                }
            }

            // Update frontier for next batch - boundary entries are the starting points
            // Note: We don't filter against ancestor_sets because boundary entries
            // were just added to ancestors in this batch, but we need them as
            // starting points for the next batch. The SQL UNION handles deduplication.
            *frontier = new_frontier;
        }

        // Intersect to find common ancestors
        let mut common = ancestor_sets[0].clone();
        for set in &ancestor_sets[1..] {
            common.retain(|id| set.contains(id));
        }
        common_ancestor_count = common.len();

        if common.is_empty() {
            // No common ancestor yet, continue with next batch
            continue;
        }

        // Get heights for common ancestors, sorted DESC (highest first)
        let mut candidates: Vec<(ID, i64)> = all_with_heights
            .iter()
            .filter(|(id, _)| common.contains(id))
            .cloned()
            .collect();
        candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        candidates.dedup_by(|a, b| a.0 == b.0);

        // Find the first candidate where ALL paths from ALL entries pass through it
        for (candidate, _height) in candidates {
            let mut all_paths_pass = true;
            for entry_id in entry_ids {
                if !is_dominator_cte(backend, store, entry_id, &candidate).await? {
                    all_paths_pass = false;
                    break;
                }
            }
            if all_paths_pass {
                return Ok(Some(candidate));
            }
        }

        // Found common ancestors but none were dominators, continue searching
    }
}

/// Collect ancestors starting from a frontier of entries, up to a depth limit.
///
/// Returns (ancestors with heights, new frontier entries, walked store roots).
/// The new frontier contains entries at exactly `depth_limit` depth whose parents
/// were not included - these can be used to continue traversal in the next batch.
/// Root detection is part of this query and is evaluated only when tracing is enabled.
async fn collect_ancestors_from_frontier(
    backend: &SqlxBackend,
    store: &str,
    frontier: &[ID],
    depth_limit: usize,
    trace_topology: bool,
) -> Result<(Vec<(ID, i64)>, Vec<ID>, HashSet<ID>)> {
    if frontier.is_empty() {
        return Ok((Vec::new(), Vec::new(), HashSet::new()));
    }

    let pool = backend.pool();

    // Build UNION ALL clause for starting entries
    let start_selects: Vec<String> = (1..=frontier.len())
        .map(|i| format!("SELECT ${} AS id, 0 AS depth", i + 1)) // +1 because $1 is store_name
        .collect();
    let starts_union = start_selects.join(" UNION ALL ");

    // Recursive CTE that tracks depth and collects ancestors
    // We return both the ancestors and which entries are at max depth (new frontier)
    let sql = format!(
        "WITH RECURSIVE ancestors AS (
            {starts_union}
            UNION
            SELECT sp.parent_id AS id, a.depth + 1
            FROM ancestors a
            JOIN store_parents sp ON sp.child_id = a.id AND sp.store_name = $1
            WHERE a.depth < ${depth_param}
        )
        SELECT a.id, s.height, a.depth,
               CASE WHEN ${trace_param} = 1 AND NOT EXISTS (
                   SELECT 1 FROM store_parents sp
                   WHERE sp.child_id = a.id AND sp.store_name = $1
               ) THEN 1 ELSE 0 END AS is_root
        FROM ancestors a
        JOIN subtrees s ON s.entry_id = a.id AND s.store_name = $1
        ORDER BY s.height DESC",
        depth_param = frontier.len() + 2, // +1 for store_name, +1 for 1-indexed
        trace_param = frontier.len() + 3
    );

    // SAFETY: generated fragments contain only numbered bind placeholders whose
    // positions derive from slice lengths; no values are interpolated.
    let mut query =
        sqlx::query_as::<_, (String, i64, i64, i64)>(sqlx::AssertSqlSafe(sql)).bind(store);
    for id in frontier {
        query = query.bind(id.to_string());
    }
    query = query
        .bind(depth_limit as i64)
        .bind(i64::from(trace_topology));

    let rows = query
        .fetch_all(pool)
        .await
        .sql_context("Failed to collect ancestors from frontier")?;

    // Separate ancestors and identify new frontier (entries at max depth with parents)
    let mut ancestors: Vec<(ID, i64)> = Vec::with_capacity(rows.len());
    let mut at_boundary: HashSet<ID> = HashSet::new();
    let mut roots: HashSet<ID> = HashSet::new();

    for (id_str, height, depth, is_root) in rows {
        let id = ID::parse(&id_str)?;
        if is_root != 0 {
            roots.insert(id.clone());
        }
        ancestors.push((id.clone(), height));

        // Entries at max depth are candidates for the new frontier
        if depth as usize == depth_limit {
            at_boundary.insert(id);
        }
    }

    // The new frontier is entries at the boundary that have parents not yet visited
    // For simplicity, we just return all boundary entries - the caller will filter
    // based on what's already been visited
    let new_frontier: Vec<ID> = at_boundary.into_iter().collect();

    Ok((ancestors, new_frontier, roots))
}

/// Check if candidate is a dominator (all paths pass through it) using recursive CTE.
///
/// Returns true if ALL paths from entry to root pass through candidate.
/// This works by trying to reach a root while avoiding the candidate using a CTE.
async fn is_dominator_cte(
    backend: &SqlxBackend,
    store: &str,
    entry: &ID,
    candidate: &ID,
) -> Result<bool> {
    // Trivial case: entry is the candidate
    if entry == candidate {
        return Ok(true);
    }

    let pool = backend.pool();

    // Recursive CTE that tries to reach a root while avoiding the candidate.
    // If we can reach any root (entry with no parents), there's a bypass path.
    // Use CASE to return integer (1/0) for cross-database compatibility
    // (SQLite returns int for EXISTS, PostgreSQL returns boolean).
    let row: (i64,) = sqlx::query_as(
        "WITH RECURSIVE bypass AS (
            -- Start from entry, but only if it's not the candidate
            SELECT $1 AS id
            WHERE $1 != $2

            UNION

            -- Follow parents, blocking the candidate
            SELECT sp.parent_id AS id
            FROM bypass b
            JOIN store_parents sp ON sp.child_id = b.id AND sp.store_name = $3
            WHERE sp.parent_id != $2
        )
        -- Check if any node in bypass has no parents (is a root)
        -- Use CASE to normalize boolean/int difference between SQLite and PostgreSQL
        SELECT CASE WHEN EXISTS(
            SELECT 1 FROM bypass b
            WHERE NOT EXISTS (
                SELECT 1 FROM store_parents sp
                WHERE sp.child_id = b.id AND sp.store_name = $3
            )
        ) THEN 1 ELSE 0 END AS has_bypass",
    )
    .bind(entry.to_string())
    .bind(candidate.to_string())
    .bind(store)
    .fetch_one(pool)
    .await
    .sql_context("Failed to check dominator")?;

    // If there's a bypass path (1), candidate is NOT a dominator
    Ok(row.0 == 0)
}

/// Validate that every ID exists and belongs to `tree`, in one batch query.
///
/// Returns `EntryNotFound` for an ID with no entry row at all, and
/// `EntryNotInTree` for one that exists under a different tree.
async fn validate_tips_in_tree(backend: &SqlxBackend, tree: &ID, tips: &[ID]) -> Result<()> {
    if tips.is_empty() {
        return Ok(());
    }

    let pool = backend.pool();

    // Build UNION ALL clause for tip IDs (works in both SQLite and PostgreSQL)
    let start_selects: Vec<String> = (1..=tips.len())
        .map(|i| format!("SELECT ${} AS id", i + 1)) // +1 because $1 is tree_id
        .collect();
    let starts_union = start_selects.join(" UNION ALL ");

    // For each tip, check: does it exist? is it in the right tree?
    // Uses CASE expressions returning 1/0 for SQLite compatibility (no native bool)
    let validation_sql = format!(
        "SELECT s.id,
                CASE WHEN e_any.id IS NOT NULL THEN 1 ELSE 0 END AS exists_at_all,
                CASE WHEN e_tree.id IS NOT NULL THEN 1 ELSE 0 END AS in_tree
         FROM ({starts_union}) AS s
         LEFT JOIN entries e_any ON e_any.id = s.id
         LEFT JOIN entries e_tree ON e_tree.id = s.id AND e_tree.tree_id = $1"
    );

    // SAFETY: `starts_union` contains only numbered bind placeholders generated
    // from `tips.len()`; tree and tip identifiers remain bind parameters.
    let mut validation_query =
        sqlx::query_as::<_, (String, i32, i32)>(sqlx::AssertSqlSafe(validation_sql))
            .bind(tree.to_string());

    for tip in tips {
        validation_query = validation_query.bind(tip.to_string());
    }

    let validation_rows = validation_query
        .fetch_all(pool)
        .await
        .sql_context("Failed to validate tips")?;

    for (tip_id_str, exists_at_all, in_tree) in &validation_rows {
        if *exists_at_all == 0 {
            return Err(BackendError::EntryNotFound {
                id: ID::parse(tip_id_str)?,
            }
            .into());
        }
        if *in_tree == 0 {
            return Err(BackendError::EntryNotInTree {
                entry_id: ID::parse(tip_id_str)?,
                tree_id: tree.clone(),
            }
            .into());
        }
    }

    Ok(())
}

/// Get entries in a tree reachable from the given tips.
///
/// Returns an error if any tip doesn't exist locally (`EntryNotFound`) or
/// belongs to a different tree (`EntryNotInTree`).
pub async fn get_tree_from_tips(
    backend: &SqlxBackend,
    tree: &ID,
    tips: &[ID],
) -> Result<Vec<Entry>> {
    if tips.is_empty() {
        return Ok(Vec::new());
    }

    let pool = backend.pool();

    // Step 1: Validate all tips
    validate_tips_in_tree(backend, tree, tips).await?;

    // Build UNION ALL clause for tip IDs (works in both SQLite and PostgreSQL)
    let start_selects: Vec<String> = (1..=tips.len())
        .map(|i| format!("SELECT ${} AS id", i + 1)) // +1 because $1 is tree_id
        .collect();
    let starts_union = start_selects.join(" UNION ALL ");

    // Step 2: Single recursive CTE query to traverse tree and fetch entries
    let sql = format!(
        "WITH RECURSIVE ancestors AS (
            -- Start from tips that are in this tree
            SELECT s.id
            FROM ({starts_union}) AS s
            JOIN entries e ON e.id = s.id AND e.tree_id = $1

            UNION

            -- Follow tree parents
            SELECT tp.parent_id AS id
            FROM ancestors a
            JOIN tree_parents tp ON tp.child_id = a.id
        )
        SELECT a.id, e.entry_cbor, e.height
        FROM ancestors a
        LEFT JOIN entries e ON e.id = a.id"
    );

    // SAFETY: `starts_union` contains only numbered bind placeholders generated
    // from `tips.len()`; entry identifiers remain bind parameters.
    let mut query =
        sqlx::query_as::<_, (String, Option<Vec<u8>>, Option<i64>)>(sqlx::AssertSqlSafe(sql))
            .bind(tree.to_string());

    for tip in tips {
        query = query.bind(tip.to_string());
    }

    let rows: Vec<(String, Option<Vec<u8>>, Option<i64>)> = query
        .fetch_all(pool)
        .await
        .sql_context("Failed to get tree entries from tips")?;

    let mut entries = Vec::with_capacity(rows.len());
    for (id, bytes, _height) in rows {
        let Some(bytes) = bytes else {
            return Err(BackendError::EntryNotFound {
                id: ID::parse(&id)?,
            }
            .into());
        };
        let entry: Entry =
            serde_ipld_dagcbor::from_slice(&bytes).map_err(|e| BackendError::SqlxError {
                reason: format!("CBOR deserialization failed: {e}"),
                source: None,
            })?;
        if !entry.in_tree(tree) {
            return Err(BackendError::EntryNotInTree {
                entry_id: entry.id(),
                tree_id: tree.clone(),
            }
            .into());
        }
        entries.push(entry);
    }

    sorting::sort_entries_by_height(&mut entries);

    Ok(entries)
}

/// Get entries in a store reachable from the given tips.
///
/// Only includes entries that belong to the specified tree and store. Tips that don't
/// belong to the tree or store are ignored.
pub async fn store_at(
    backend: &SqlxBackend,
    tree: &ID,
    store: &str,
    tips: &[ID],
) -> Result<Vec<Entry>> {
    if tips.is_empty() {
        return Ok(Vec::new());
    }

    let pool = backend.pool();

    // Build UNION ALL clause for starting entries (works in both SQLite and PostgreSQL)
    let start_selects: Vec<String> = (1..=tips.len())
        .map(|i| format!("SELECT ${} AS id", i + 2)) // +2 because $1 is tree_id, $2 is store_name
        .collect();
    let starts_union = start_selects.join(" UNION ALL ");

    // Single query using recursive CTE to:
    // 1. Collect all ancestors from tips
    // 2. Join with entries to get full entry CBOR
    // 3. Join with subtrees to get height for sorting
    // 4. Filter by tree_id
    let sql = format!(
        "WITH RECURSIVE ancestors AS (
            -- Start from tips that are in this tree and store
            SELECT s.id
            FROM ({starts_union}) AS s
            JOIN entries e ON e.id = s.id AND e.tree_id = $1
            JOIN subtrees st ON st.entry_id = s.id AND st.store_name = $2

            UNION

            -- Follow store parents
            SELECT sp.parent_id AS id
            FROM ancestors a
            JOIN store_parents sp ON sp.child_id = a.id AND sp.store_name = $2
        )
        SELECT e.entry_cbor, st.height
        FROM ancestors a
        JOIN entries e ON e.id = a.id
        JOIN subtrees st ON st.entry_id = a.id AND st.store_name = $2"
    );

    // SAFETY: `starts_union` contains only numbered bind placeholders generated
    // from `tips.len()`; tree, store, and tip values remain bind parameters.
    let mut query = sqlx::query_as::<_, (Vec<u8>, i64)>(sqlx::AssertSqlSafe(sql))
        .bind(tree.to_string())
        .bind(store);

    for tip in tips {
        query = query.bind(tip.to_string());
    }

    let rows = query
        .fetch_all(pool)
        .await
        .sql_context("Failed to get store entries from tips")?;

    let mut entries = Vec::with_capacity(rows.len());
    for (bytes, _height) in rows {
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

/// Get parents of an entry in a store, sorted by height then ID.
///
/// Uses a single query joining store_parents with subtrees to get heights efficiently.
pub async fn get_sorted_store_parents(
    backend: &SqlxBackend,
    _tree_id: &ID,
    entry_id: &ID,
    store: &str,
) -> Result<Vec<ID>> {
    let pool = backend.pool();

    // Single query that joins store_parents with subtrees to get parents with heights
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT sp.parent_id, s.height
         FROM store_parents sp
         JOIN subtrees s ON s.entry_id = sp.parent_id AND s.store_name = sp.store_name
         WHERE sp.child_id = $1 AND sp.store_name = $2",
    )
    .bind(entry_id.to_string())
    .bind(store)
    .fetch_all(pool)
    .await
    .sql_context("Failed to get sorted store parents")?;

    let mut parents: Vec<(ID, i64)> = rows
        .into_iter()
        .map(|(id, height)| ID::parse(&id).map(|id| (id, height)))
        .collect::<Result<_>>()?;

    sorting::sort_ids_by_height(&mut parents);

    Ok(parents.into_iter().map(|(id, _)| id).collect())
}

/// Get all entries between from_id and to_ids in a store.
///
/// This correctly handles diamond patterns by finding ALL entries reachable
/// from to_ids by following parents back to from_id.
///
/// Uses a recursive CTE for efficient single-query traversal.
pub async fn get_path_from_to(
    backend: &SqlxBackend,
    _tree_id: &ID,
    store: &str,
    from_id: Option<&ID>,
    to_ids: &[ID],
) -> Result<Vec<ID>> {
    if to_ids.is_empty() {
        return Ok(Vec::new());
    }

    let pool = backend.pool();

    // $1 is store; $2 is from_id when there is one, so the to_ids start one
    // slot later in that case.
    let to_id_base = if from_id.is_some() { 2 } else { 1 };

    // Build UNION ALL clause for starting entries (works in both SQLite and PostgreSQL)
    let start_selects: Vec<String> = (1..=to_ids.len())
        .map(|i| format!("SELECT ${} AS id", i + to_id_base))
        .collect();
    let starts_union = start_selects.join(" UNION ALL ");

    // Without a base the traversal runs to the store roots, collecting the
    // full ancestry of the tips — the empty-base merge.
    let (start_filter, walk_filter) = if from_id.is_some() {
        ("WHERE id != $2", "WHERE sp.parent_id != $2")
    } else {
        ("", "")
    };

    // Recursive CTE that traverses from to_ids back to from_id,
    // then joins with subtrees to get heights for sorting
    let sql = format!(
        "WITH RECURSIVE path_entries AS (
            -- Start from to_ids (excluding from_id)
            SELECT id FROM ({starts_union}) AS starts
            {start_filter}

            UNION

            -- Follow parents back, stopping at from_id
            SELECT sp.parent_id AS id
            FROM path_entries p
            JOIN store_parents sp ON sp.child_id = p.id AND sp.store_name = $1
            {walk_filter}
        )
        SELECT p.id, s.height
        FROM path_entries p
        JOIN subtrees s ON s.entry_id = p.id AND s.store_name = $1"
    );

    // SAFETY: generated fragments are fixed optional predicates plus numbered
    // bind placeholders derived from `to_ids.len()`; all values remain bound.
    let mut query = sqlx::query_as::<_, (String, i64)>(sqlx::AssertSqlSafe(sql)).bind(store);

    if let Some(from_id) = from_id {
        query = query.bind(from_id.to_string());
    }

    for to_id in to_ids {
        query = query.bind(to_id.to_string());
    }

    let rows = query
        .fetch_all(pool)
        .await
        .sql_context("Failed to get path from to")?;

    let mut path: Vec<(ID, i64)> = rows
        .into_iter()
        .map(|(id, height)| ID::parse(&id).map(|id| (id, height)))
        .collect::<Result<_>>()?;

    sorting::sort_ids_by_height(&mut path);

    Ok(path.into_iter().map(|(id, _)| id).collect())
}

#[cfg(test)]
async fn concurrent_snapshot_test_backend() -> std::sync::Arc<SqlxBackend> {
    #[cfg(feature = "postgres")]
    if std::env::var("TEST_BACKEND").as_deref() == Ok("postgres") {
        let url = std::env::var("TEST_POSTGRES_URL").expect("Postgres test URL is required");
        return std::sync::Arc::new(SqlxBackend::connect_postgres_isolated(&url).await.unwrap());
    }
    std::sync::Arc::new(SqlxBackend::sqlite_in_memory().await.unwrap())
}

#[cfg(test)]
mod snapshot_race_tests {
    use super::*;
    use crate::backend::{BackendImpl, VerificationStatus};
    use std::sync::Arc;

    #[tokio::test]
    async fn fixed_parent_store_snapshot_ignores_concurrent_live_write() {
        let backend = concurrent_snapshot_test_backend().await;
        let root = Entry::root_builder()
            .set_subtree_data("race_store", b"root")
            .build()
            .unwrap();
        let root_id = root.id();
        backend.put(root).await.unwrap();
        backend
            .update_verification_status(&root_id, VerificationStatus::Verified)
            .await
            .unwrap();

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
        *SNAPSHOT_PAUSE.get_or_init(Default::default).lock().unwrap() =
            Some((root_id.clone(), (entered_tx, resume_rx)));

        let reader_backend = Arc::clone(&backend);
        let reader_root = root_id.clone();
        let read_task = tokio::spawn(async move {
            store_snapshot_at(
                &reader_backend,
                &reader_root,
                "race_store",
                std::slice::from_ref(&reader_root),
            )
            .await
        });
        entered_rx.await.unwrap();

        let child = Entry::builder(root_id.clone())
            .add_parent(root_id.clone())
            .set_subtree_data("race_store", b"child")
            .add_subtree_parent("race_store", root_id.clone())
            .build()
            .unwrap();
        let child_id = child.id();
        backend.put(child).await.unwrap();
        backend
            .update_verification_status(&child_id, VerificationStatus::Verified)
            .await
            .unwrap();
        resume_tx.send(()).unwrap();

        assert_eq!(read_task.await.unwrap().unwrap(), vec![root_id]);
    }
}

#[cfg(test)]
mod atomic_tips_tests {
    use super::*;
    use crate::backend::{BackendImpl, VerificationStatus};
    use std::sync::Arc;

    #[tokio::test]
    async fn current_tips_query_returns_its_own_snapshot_after_concurrent_write() {
        let backend = concurrent_snapshot_test_backend().await;
        let store = "post_query_race";
        let root = Entry::root_builder()
            .set_subtree_data(store, b"post_root")
            .build()
            .unwrap();
        let root_id = root.id();
        backend.put(root).await.unwrap();
        backend
            .update_verification_status(&root_id, VerificationStatus::Verified)
            .await
            .unwrap();

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
        *AFTER_TIPS_PAUSE
            .get_or_init(Default::default)
            .lock()
            .unwrap() = Some((root_id.clone(), (entered_tx, resume_rx)));
        let reader_backend = Arc::clone(&backend);
        let reader_root = root_id.clone();
        let read_task = tokio::spawn(async move {
            store_snapshot_at(
                &reader_backend,
                &reader_root,
                store,
                std::slice::from_ref(&reader_root),
            )
            .await
        });
        entered_rx.await.unwrap();

        let child = Entry::builder(root_id.clone())
            .add_parent(root_id.clone())
            .set_subtree_data(store, b"post_child")
            .add_subtree_parent(store, root_id.clone())
            .build()
            .unwrap();
        let child_id = child.id();
        backend.put(child).await.unwrap();
        backend
            .update_verification_status(&child_id, VerificationStatus::Verified)
            .await
            .unwrap();
        resume_tx.send(()).unwrap();

        assert_eq!(read_task.await.unwrap().unwrap(), vec![root_id.clone()]);
        assert_eq!(
            store_snapshot(&backend, &root_id, store).await.unwrap(),
            vec![child_id]
        );
    }
}
