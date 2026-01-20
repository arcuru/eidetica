//! DAG traversal operations for SQL backends.
//!
//! This module implements graph traversal operations like finding tips,
//! computing merge bases, and collecting paths through the DAG using sqlx.

use std::collections::{HashSet, VecDeque};

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

    // Get all entries in the store that are reachable from main_entries
    // This is a complex query - for now we'll do it iteratively

    // First, get all store entries reachable from the main entries
    let mut reachable: HashSet<ID> = HashSet::new();
    let mut to_visit: VecDeque<ID> = main_entries.iter().cloned().collect();

    while let Some(entry_id) = to_visit.pop_front() {
        if reachable.contains(&entry_id) {
            continue;
        }

        // Check if this entry is in the store
        let in_store: Option<(i32,)> =
            sqlx::query_as("SELECT 1 FROM subtrees WHERE entry_id = $1 AND store_name = $2")
                .bind(entry_id.to_string())
                .bind(store)
                .fetch_optional(pool)
                .await
                .sql_context("Failed to check store membership")?;

        if in_store.is_some() {
            reachable.insert(entry_id.clone());
        }

        // Add tree parents to visit
        let parent_rows: Vec<(String,)> =
            sqlx::query_as("SELECT parent_id FROM tree_parents WHERE child_id = $1")
                .bind(entry_id.to_string())
                .fetch_all(pool)
                .await
                .sql_context("Failed to get tree parents")?;

        for (parent_id,) in parent_rows {
            let parent_id = ID::from(parent_id);
            if !reachable.contains(&parent_id) {
                to_visit.push_back(parent_id);
            }
        }
    }

    // Now find which of the reachable entries are tips (have no children in the store)
    // O(n) algorithm: collect all parents in one query, then filter
    if reachable.is_empty() {
        return Ok(Vec::new());
    }

    // Build IN clause for reachable IDs
    let reachable_vec: Vec<String> = reachable.iter().map(|id| id.to_string()).collect();
    let placeholders: Vec<String> = (1..=reachable_vec.len())
        .map(|i| format!("${}", i + 1)) // +1 because $1 is store_name
        .collect();
    let in_clause = placeholders.join(", ");

    // Single query to get all parent IDs within the reachable set
    let sql = format!(
        "SELECT DISTINCT parent_id FROM store_parents
         WHERE store_name = $1
         AND child_id IN ({in_clause})
         AND parent_id IN ({in_clause})"
    );

    let mut query = sqlx::query_as::<_, (String,)>(&sql).bind(store);
    // Bind child_id IN clause
    for id in &reachable_vec {
        query = query.bind(id);
    }
    // Bind parent_id IN clause
    for id in &reachable_vec {
        query = query.bind(id);
    }

    let parent_rows = query
        .fetch_all(pool)
        .await
        .sql_context("Failed to get store parents for tips")?;

    let parents_set: HashSet<ID> = parent_rows.into_iter().map(|(id,)| ID::from(id)).collect();

    // Tips = reachable entries that are NOT parents of anything
    let tips: Vec<ID> = reachable
        .into_iter()
        .filter(|id| !parents_set.contains(id))
        .collect();

    Ok(tips)
}

/// Find the merge base (common dominator) of the given entries in a store.
///
/// The merge base is the lowest ancestor that ALL paths from ALL entries must pass through.
///
/// This implementation uses recursive CTEs for efficient single-query DAG traversal
/// and batch height lookups from the `subtrees` table.
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

    // Step 1: Collect ancestors for each entry using recursive CTE
    let mut ancestor_sets: Vec<HashSet<ID>> = Vec::with_capacity(entry_ids.len());
    for entry_id in entry_ids {
        let ancestors = collect_ancestors_cte(backend, store, entry_id).await?;
        ancestor_sets.push(ancestors);
    }

    // Step 2: Find common ancestors (intersection of all ancestor sets)
    let mut common_ancestors: HashSet<ID> = ancestor_sets[0].clone();
    for ancestor_set in &ancestor_sets[1..] {
        common_ancestors.retain(|a| ancestor_set.contains(a));
    }

    if common_ancestors.is_empty() {
        return Err(BackendError::NoCommonAncestor {
            entry_ids: entry_ids.to_vec(),
        }
        .into());
    }

    // Step 3: Batch-fetch heights from subtrees table, sorted descending
    let candidates = get_heights_batch(backend, store, &common_ancestors).await?;

    // Step 4: Find the first candidate where ALL paths from ALL entries pass through it
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

    // Should not reach here if there's a root, but handle gracefully
    Err(BackendError::NoCommonAncestor {
        entry_ids: entry_ids.to_vec(),
    }
    .into())
}

/// Collect all ancestors of an entry in a store using a recursive CTE.
///
/// This performs the traversal in a single query instead of N queries.
async fn collect_ancestors_cte(
    backend: &SqlxBackend,
    store: &str,
    entry: &ID,
) -> Result<HashSet<ID>> {
    let pool = backend.pool();

    // Recursive CTE to collect all ancestors including the entry itself
    let rows: Vec<(String,)> = sqlx::query_as(
        "WITH RECURSIVE ancestors AS (
            SELECT $1 AS id
            UNION
            SELECT sp.parent_id AS id
            FROM ancestors a
            JOIN store_parents sp ON sp.child_id = a.id AND sp.store_name = $2
        )
        SELECT id FROM ancestors",
    )
    .bind(entry.to_string())
    .bind(store)
    .fetch_all(pool)
    .await
    .sql_context("Failed to collect ancestors")?;

    Ok(rows.into_iter().map(|(id,)| ID::from(id)).collect())
}

/// Get heights for multiple entries from the subtrees table in a single query.
///
/// Returns entries sorted by height descending (highest first = closest to tips).
async fn get_heights_batch(
    backend: &SqlxBackend,
    store: &str,
    entry_ids: &HashSet<ID>,
) -> Result<Vec<(ID, u64)>> {
    if entry_ids.is_empty() {
        return Ok(Vec::new());
    }

    let pool = backend.pool();

    // Build IN clause - both SQLite and PostgreSQL support this syntax
    let placeholders: Vec<String> = (1..=entry_ids.len())
        .map(|i| format!("${}", i + 1)) // +1 because $1 is store_name
        .collect();
    let in_clause = placeholders.join(", ");

    let sql = format!(
        "SELECT entry_id, height FROM subtrees
         WHERE store_name = $1 AND entry_id IN ({in_clause})
         ORDER BY height DESC, entry_id ASC"
    );

    let mut query = sqlx::query_as::<_, (String, i64)>(&sql).bind(store);
    for entry_id in entry_ids {
        query = query.bind(entry_id.to_string());
    }

    let rows = query
        .fetch_all(pool)
        .await
        .sql_context("Failed to get heights batch")?;

    Ok(rows
        .into_iter()
        .map(|(id, height)| (ID::from(id), height as u64))
        .collect())
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
    // TODO: Optimize with recursive CTE to do traversal in a single query
    // instead of multiple round-trips.
    if tips.is_empty() {
        return Ok(Vec::new());
    }

    let pool = backend.pool();

    // BFS from tips to collect all ancestors, but only include entries in the specified tree
    let mut collected: HashSet<ID> = HashSet::new();
    let mut to_visit: VecDeque<ID> = VecDeque::new();

    // Initialize with tips that belong to the tree
    for tip in tips {
        // Check if this tip belongs to the specified tree
        let in_tree: Option<(i32,)> =
            sqlx::query_as("SELECT 1 FROM entries WHERE id = $1 AND tree_id = $2")
                .bind(tip.to_string())
                .bind(tree.to_string())
                .fetch_optional(pool)
                .await
                .sql_context("Failed to check tree membership")?;

        if in_tree.is_some() {
            to_visit.push_back(tip.clone());
        } else {
            // Entry not in tree - check if it exists at all to give a better error
            let exists: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM entries WHERE id = $1")
                .bind(tip.to_string())
                .fetch_optional(pool)
                .await
                .sql_context("Failed to check entry existence")?;

            if exists.is_some() {
                return Err(BackendError::EntryNotInTree {
                    entry_id: tip.clone(),
                    tree_id: tree.clone(),
                }
                .into());
            } else {
                return Err(BackendError::EntryNotFound { id: tip.clone() }.into());
            }
        }
    }

    while let Some(entry_id) = to_visit.pop_front() {
        if collected.contains(&entry_id) {
            continue;
        }
        collected.insert(entry_id.clone());

        // Add tree parents to visit
        let parent_rows: Vec<(String,)> =
            sqlx::query_as("SELECT parent_id FROM tree_parents WHERE child_id = $1")
                .bind(entry_id.to_string())
                .fetch_all(pool)
                .await
                .sql_context("Failed to get tree parents")?;

        for (parent_id,) in parent_rows {
            let parent_id = ID::from(parent_id);
            if !collected.contains(&parent_id) {
                to_visit.push_back(parent_id);
            }
        }
    }

    // Fetch the collected entries
    let mut entries = Vec::with_capacity(collected.len());
    for id in &collected {
        let row: Option<(String,)> = sqlx::query_as("SELECT entry_json FROM entries WHERE id = $1")
            .bind(id.to_string())
            .fetch_optional(pool)
            .await
            .sql_context("Failed to get entry")?;

        if let Some((json,)) = row {
            let entry: Entry = serde_json::from_str(&json)
                .map_err(|e| BackendError::DeserializationFailed { source: e })?;
            entries.push(entry);
        }
    }

    // Sort by height (stored in entries)
    super::cache::sort_entries_by_height(&mut entries);

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
