//! DAG traversal operations for SQL backends.
//!
//! This module implements graph traversal operations like finding tips,
//! computing merge bases, and collecting paths through the DAG using sqlx.

use std::collections::HashSet;

use crate::Result;
use crate::backend::errors::BackendError;
use crate::entry::{Entry, ID};

use super::{SqlxBackend, SqlxResultExt};

/// Get tree tips (entries with no children in the main tree).
pub async fn get_tips(backend: &SqlxBackend, tree: &ID) -> Result<Vec<ID>> {
    // Find a store with empty string name, used for tree-level tips
    get_store_tips(backend, tree, "").await
}

/// Get store tips (entries with no children in a specific store).
pub async fn get_store_tips(backend: &SqlxBackend, tree: &ID, store: &str) -> Result<Vec<ID>> {
    let pool = backend.pool();

    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT entry_id FROM tips WHERE tree_id = $1 AND store_name = $2")
            .bind(tree.to_string())
            .bind(store)
            .fetch_all(pool)
            .await
            .sql_context("Failed to get store tips")?;

    Ok(rows.into_iter().map(|(id,)| ID::from(id)).collect())
}

/// Get store tips that are reachable from the given main tree entries.
pub async fn get_store_tips_up_to_entries(
    backend: &SqlxBackend,
    tree: &ID,
    store: &str,
    main_entries: &[ID],
) -> Result<Vec<ID>> {
    if main_entries.is_empty() {
        return Ok(Vec::new());
    }

    // Fast path: if main_entries are current tree tips, use tips table directly
    let current_tree_tips = get_tips(backend, tree).await?;
    let main_entries_set: HashSet<_> = main_entries.iter().collect();
    let current_tips_set: HashSet<_> = current_tree_tips.iter().collect();
    if main_entries_set == current_tips_set {
        return get_store_tips(backend, tree, store).await;
    }

    let pool = backend.pool();

    // Use a single CTE to find all store entries reachable from main_entries
    // This replaces the O(N²) iterative BFS with a single query

    // Build UNION ALL clause for starting entries
    let start_selects: Vec<String> = (1..=main_entries.len())
        .map(|i| format!("SELECT ${} AS id", i + 1)) // +1 because $1 is store_name
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
        -- Tips are store entries that are not parents
        SELECT se.id FROM store_entries se
        WHERE se.id NOT IN (SELECT id FROM non_tips)"
    );

    let mut query = sqlx::query_as::<_, (String,)>(&sql).bind(store);
    for entry in main_entries {
        query = query.bind(entry.to_string());
    }

    let rows = query
        .fetch_all(pool)
        .await
        .sql_context("Failed to get store tips up to entries")?;

    Ok(rows.into_iter().map(|(id,)| ID::from(id)).collect())
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
pub async fn find_merge_base(
    backend: &SqlxBackend,
    _tree: &ID,
    store: &str,
    entry_ids: &[ID],
) -> Result<ID> {
    if entry_ids.is_empty() {
        return Err(BackendError::EmptyEntryList {
            operation: "find_merge_base".to_string(),
        }
        .into());
    }

    if entry_ids.len() == 1 {
        return Ok(entry_ids[0].clone());
    }

    // Track all known ancestors per tip and their frontiers for continuation
    let mut ancestor_sets: Vec<HashSet<ID>> = vec![HashSet::new(); entry_ids.len()];
    let mut frontiers: Vec<Vec<ID>> = entry_ids.iter().map(|id| vec![id.clone()]).collect();
    let mut all_with_heights: Vec<(ID, i64)> = Vec::new();

    loop {
        // Check if all frontiers are exhausted (reached roots without finding common ancestor)
        if frontiers.iter().all(|f| f.is_empty()) {
            return Err(BackendError::NoCommonAncestor {
                entry_ids: entry_ids.to_vec(),
            }
            .into());
        }

        // Pull next batch from each non-empty frontier
        for (i, frontier) in frontiers.iter_mut().enumerate() {
            if frontier.is_empty() {
                continue;
            }

            let (ancestors, new_frontier) =
                collect_ancestors_from_frontier(backend, store, frontier, MERGE_BASE_DEPTH_LIMIT)
                    .await?;

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
        let mut common: HashSet<ID> = ancestor_sets[0].clone();
        for set in &ancestor_sets[1..] {
            common.retain(|id| set.contains(id));
        }

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
                return Ok(candidate);
            }
        }

        // Found common ancestors but none were dominators, continue searching
    }
}

/// Collect ancestors starting from a frontier of entries, up to a depth limit.
///
/// Returns (ancestors with heights, new frontier entries at the boundary).
/// The new frontier contains entries at exactly `depth_limit` depth whose parents
/// were not included - these can be used to continue traversal in the next batch.
async fn collect_ancestors_from_frontier(
    backend: &SqlxBackend,
    store: &str,
    frontier: &[ID],
    depth_limit: usize,
) -> Result<(Vec<(ID, i64)>, Vec<ID>)> {
    if frontier.is_empty() {
        return Ok((Vec::new(), Vec::new()));
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
        SELECT a.id, s.height, a.depth
        FROM ancestors a
        JOIN subtrees s ON s.entry_id = a.id AND s.store_name = $1
        ORDER BY s.height DESC",
        depth_param = frontier.len() + 2 // +1 for store_name, +1 for 1-indexed
    );

    let mut query = sqlx::query_as::<_, (String, i64, i64)>(&sql).bind(store);
    for id in frontier {
        query = query.bind(id.to_string());
    }
    query = query.bind(depth_limit as i64);

    let rows = query
        .fetch_all(pool)
        .await
        .sql_context("Failed to collect ancestors from frontier")?;

    // Separate ancestors and identify new frontier (entries at max depth with parents)
    let mut ancestors: Vec<(ID, i64)> = Vec::with_capacity(rows.len());
    let mut at_boundary: HashSet<ID> = HashSet::new();

    for (id_str, height, depth) in rows {
        let id = ID::from(id_str);
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

    Ok((ancestors, new_frontier))
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

/// Collect all entries from root to the target entry in a store.
pub async fn collect_root_to_target(
    backend: &SqlxBackend,
    _tree: &ID,
    store: &str,
    target_entry: &ID,
) -> Result<Vec<ID>> {
    let pool = backend.pool();

    // BFS from target back to root, then reverse
    let mut path = Vec::new();
    let mut current = target_entry.clone();
    let mut visited: HashSet<ID> = HashSet::new();

    loop {
        if visited.contains(&current) {
            return Err(BackendError::CycleDetected { entry_id: current }.into());
        }
        visited.insert(current.clone());
        path.push(current.clone());

        // Get parents in store
        let parent_rows: Vec<(String,)> = sqlx::query_as(
            "SELECT parent_id FROM store_parents WHERE child_id = $1 AND store_name = $2",
        )
        .bind(current.to_string())
        .bind(store)
        .fetch_all(pool)
        .await
        .sql_context("Failed to get store parents")?;

        if parent_rows.is_empty() {
            // Reached root
            break;
        }

        // Follow first parent (for a simple linear path)
        // For complex DAGs, this should collect all ancestors
        current = ID::from(parent_rows[0].0.clone());
    }

    path.reverse();
    Ok(path)
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

    // Build UNION ALL clause for tip IDs (works in both SQLite and PostgreSQL)
    let start_selects: Vec<String> = (1..=tips.len())
        .map(|i| format!("SELECT ${} AS id", i + 1)) // +1 because $1 is tree_id
        .collect();
    let starts_union = start_selects.join(" UNION ALL ");

    // Step 1: Validate all tips in a single batch query
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

    let mut validation_query =
        sqlx::query_as::<_, (String, i32, i32)>(&validation_sql).bind(tree.to_string());

    for tip in tips {
        validation_query = validation_query.bind(tip.to_string());
    }

    let validation_rows = validation_query
        .fetch_all(pool)
        .await
        .sql_context("Failed to validate tips")?;

    // Check for validation errors
    for (tip_id_str, exists_at_all, in_tree) in &validation_rows {
        if *exists_at_all == 0 {
            return Err(BackendError::EntryNotFound {
                id: ID::from(tip_id_str.clone()),
            }
            .into());
        }
        if *in_tree == 0 {
            return Err(BackendError::EntryNotInTree {
                entry_id: ID::from(tip_id_str.clone()),
                tree_id: tree.clone(),
            }
            .into());
        }
    }

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
        SELECT e.entry_json, e.height
        FROM ancestors a
        JOIN entries e ON e.id = a.id
        ORDER BY e.height ASC, a.id ASC"
    );

    let mut query = sqlx::query_as::<_, (String, i64)>(&sql).bind(tree.to_string());

    for tip in tips {
        query = query.bind(tip.to_string());
    }

    let rows = query
        .fetch_all(pool)
        .await
        .sql_context("Failed to get tree entries from tips")?;

    // Deserialize entries (already sorted by height)
    let mut entries = Vec::with_capacity(rows.len());
    for (json, _height) in rows {
        let entry: Entry = serde_json::from_str(&json)
            .map_err(|e| BackendError::DeserializationFailed { source: e })?;
        entries.push(entry);
    }

    Ok(entries)
}

/// Get entries in a store reachable from the given tips.
///
/// Only includes entries that belong to the specified tree and store. Tips that don't
/// belong to the tree or store are ignored.
pub async fn get_store_from_tips(
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
    // 2. Join with entries to get full entry JSON
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
        SELECT e.entry_json, st.height
        FROM ancestors a
        JOIN entries e ON e.id = a.id
        JOIN subtrees st ON st.entry_id = a.id AND st.store_name = $2
        ORDER BY st.height ASC, a.id ASC"
    );

    let mut query = sqlx::query_as::<_, (String, i64)>(&sql)
        .bind(tree.to_string())
        .bind(store);

    for tip in tips {
        query = query.bind(tip.to_string());
    }

    let rows = query
        .fetch_all(pool)
        .await
        .sql_context("Failed to get store entries from tips")?;

    // Deserialize entries (already sorted by height)
    let mut entries = Vec::with_capacity(rows.len());
    for (json, _height) in rows {
        let entry: Entry = serde_json::from_str(&json)
            .map_err(|e| BackendError::DeserializationFailed { source: e })?;
        entries.push(entry);
    }

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
         WHERE sp.child_id = $1 AND sp.store_name = $2
         ORDER BY s.height ASC, sp.parent_id ASC",
    )
    .bind(entry_id.to_string())
    .bind(store)
    .fetch_all(pool)
    .await
    .sql_context("Failed to get sorted store parents")?;

    Ok(rows.into_iter().map(|(id, _)| ID::from(id)).collect())
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
    from_id: &ID,
    to_ids: &[ID],
) -> Result<Vec<ID>> {
    if to_ids.is_empty() {
        return Ok(Vec::new());
    }

    let pool = backend.pool();

    // Build UNION ALL clause for starting entries (works in both SQLite and PostgreSQL)
    // Format: SELECT $3 AS id UNION ALL SELECT $4 AS id ...
    let start_selects: Vec<String> = (1..=to_ids.len())
        .map(|i| format!("SELECT ${} AS id", i + 2)) // +2 because $1 is store, $2 is from_id
        .collect();
    let starts_union = start_selects.join(" UNION ALL ");

    // Recursive CTE that traverses from to_ids back to from_id,
    // then joins with subtrees to get heights for sorting
    let sql = format!(
        "WITH RECURSIVE path_entries AS (
            -- Start from to_ids (excluding from_id)
            SELECT id FROM ({starts_union}) AS starts
            WHERE id != $2

            UNION

            -- Follow parents back, stopping at from_id
            SELECT sp.parent_id AS id
            FROM path_entries p
            JOIN store_parents sp ON sp.child_id = p.id AND sp.store_name = $1
            WHERE sp.parent_id != $2
        )
        SELECT p.id, s.height
        FROM path_entries p
        JOIN subtrees s ON s.entry_id = p.id AND s.store_name = $1
        ORDER BY s.height ASC, p.id ASC"
    );

    let mut query = sqlx::query_as::<_, (String, i64)>(&sql)
        .bind(store)
        .bind(from_id.to_string());

    for to_id in to_ids {
        query = query.bind(to_id.to_string());
    }

    let rows = query
        .fetch_all(pool)
        .await
        .sql_context("Failed to get path from to")?;

    Ok(rows.into_iter().map(|(id, _)| ID::from(id)).collect())
}
