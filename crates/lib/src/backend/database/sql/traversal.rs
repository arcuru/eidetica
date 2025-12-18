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
    _tree: &ID,
    store: &str,
    main_entries: &[ID],
) -> Result<Vec<ID>> {
    if main_entries.is_empty() {
        return Ok(Vec::new());
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
        let in_store: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM store_memberships WHERE entry_id = $1 AND store_name = $2",
        )
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
    let mut tips = Vec::new();
    for entry_id in &reachable {
        // Check if any reachable entry has this as a parent
        let mut has_child = false;
        for potential_child in &reachable {
            if potential_child == entry_id {
                continue;
            }
            // Check store parents
            let result: Option<(i32,)> = sqlx::query_as(
                "SELECT 1 FROM store_parents WHERE child_id = $1 AND parent_id = $2 AND store_name = $3",
            )
            .bind(potential_child.to_string())
            .bind(entry_id.to_string())
            .bind(store)
            .fetch_optional(pool)
            .await
            .sql_context("Failed to check store parents")?;

            if result.is_some() {
                has_child = true;
                break;
            }
        }

        if !has_child {
            tips.push(entry_id.clone());
        }
    }

    Ok(tips)
}

/// Find the merge base (common dominator) of the given entries in a store.
///
/// The merge base is the lowest ancestor that ALL paths from ALL entries must pass through.
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

    // Step 1: Collect all ancestors for each entry
    // FIXME(perf): We should not load the full list of Entries here, it's unnecessary but correct for now
    let mut ancestor_sets: Vec<HashSet<ID>> = Vec::with_capacity(entry_ids.len());
    for entry_id in entry_ids {
        let ancestors = collect_ancestors_async(backend, store, entry_id).await?;
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

    // Step 3: Get heights for sorting (we want highest height first = closest to tips)
    let mut candidates: Vec<(ID, u64)> = Vec::with_capacity(common_ancestors.len());
    for id in common_ancestors {
        let height = get_store_height_async(backend, store, &id).await?;
        candidates.push((id, height));
    }
    // Sort by height descending (highest first = closest to tips)
    candidates.sort_by(|a, b| b.1.cmp(&a.1));

    // Step 4: Find the first candidate where ALL paths from ALL entries pass through it
    for (candidate, _height) in candidates {
        let mut all_paths_pass = true;
        for entry_id in entry_ids {
            if !all_paths_pass_through_async(backend, store, entry_id, &candidate).await? {
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

/// Collect all ancestors of an entry in a store (including the entry itself).
async fn collect_ancestors_async(
    backend: &SqlxBackend,
    store: &str,
    entry: &ID,
) -> Result<HashSet<ID>> {
    let pool = backend.pool();
    let mut ancestors: HashSet<ID> = HashSet::new();
    let mut queue: VecDeque<ID> = VecDeque::new();
    queue.push_back(entry.clone());

    while let Some(current) = queue.pop_front() {
        if ancestors.contains(&current) {
            continue;
        }
        ancestors.insert(current.clone());

        let parent_rows: Vec<(String,)> = sqlx::query_as(
            "SELECT parent_id FROM store_parents WHERE child_id = $1 AND store_name = $2",
        )
        .bind(current.to_string())
        .bind(store)
        .fetch_all(pool)
        .await
        .sql_context("Failed to get store parents")?;

        for (parent_id,) in parent_rows {
            queue.push_back(ID::from(parent_id));
        }
    }

    Ok(ancestors)
}

/// Get the store height for an entry.
async fn get_store_height_async(backend: &SqlxBackend, store: &str, entry: &ID) -> Result<u64> {
    let pool = backend.pool();

    // Get entry JSON and extract store height
    let row: Option<(String,)> = sqlx::query_as("SELECT entry_json FROM entries WHERE id = $1")
        .bind(entry.to_string())
        .fetch_optional(pool)
        .await
        .sql_context("Failed to get entry")?;

    if let Some((json,)) = row {
        let entry: Entry = serde_json::from_str(&json)
            .map_err(|e| BackendError::DeserializationFailed { source: e })?;
        // Try to get store-specific height, fall back to tree height
        Ok(entry
            .subtree_height(store)
            .unwrap_or_else(|_| entry.height()))
    } else {
        // Entry not found, use 0 as fallback
        Ok(0)
    }
}

/// Check if ALL paths from entry to root pass through candidate.
///
/// This works by trying to reach a root (entry with no parents) while avoiding
/// the candidate. If we can reach a root, there's a bypass path and the candidate
/// is not on all paths.
async fn all_paths_pass_through_async(
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

    // Try to reach a root while avoiding the candidate
    let mut visited: HashSet<ID> = HashSet::new();
    visited.insert(candidate.clone()); // Block the candidate
    let mut queue: VecDeque<ID> = VecDeque::new();
    queue.push_back(entry.clone());

    while let Some(current) = queue.pop_front() {
        if visited.contains(&current) {
            continue;
        }
        visited.insert(current.clone());

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
            // Reached a root while avoiding the candidate - there's a bypass path!
            return Ok(false);
        }

        for (parent_id,) in parent_rows {
            queue.push_back(ID::from(parent_id));
        }
    }

    // Couldn't reach any root without going through the candidate
    Ok(true)
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
    // TODO: Optimize with recursive CTE to do traversal in a single query
    // instead of multiple round-trips.
    if tips.is_empty() {
        return Ok(Vec::new());
    }

    let pool = backend.pool();

    // BFS from tips to collect all ancestors in this store
    let mut collected: HashSet<ID> = HashSet::new();
    let mut to_visit: VecDeque<ID> = VecDeque::new();

    // Initialize with tips that belong to the tree and store
    for tip in tips {
        // Check if this tip belongs to the specified tree and store
        let in_tree_and_store: Option<(i32,)> = sqlx::query_as(
            "SELECT 1 FROM entries e
             JOIN store_memberships sm ON sm.entry_id = e.id
             WHERE e.id = $1 AND e.tree_id = $2 AND sm.store_name = $3",
        )
        .bind(tip.to_string())
        .bind(tree.to_string())
        .bind(store)
        .fetch_optional(pool)
        .await
        .sql_context("Failed to check tree/store membership")?;

        if in_tree_and_store.is_some() {
            to_visit.push_back(tip.clone());
        }
    }

    while let Some(entry_id) = to_visit.pop_front() {
        if collected.contains(&entry_id) {
            continue;
        }
        collected.insert(entry_id.clone());

        // Add store parents to visit
        let parent_rows: Vec<(String,)> = sqlx::query_as(
            "SELECT parent_id FROM store_parents WHERE child_id = $1 AND store_name = $2",
        )
        .bind(entry_id.to_string())
        .bind(store)
        .fetch_all(pool)
        .await
        .sql_context("Failed to get store parents")?;

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

    // Sort by store height (stored in entries)
    super::cache::sort_entries_by_subtree_height(&mut entries, store)?;

    Ok(entries)
}

/// Get parents of an entry in a store, sorted by height then ID.
pub async fn get_sorted_store_parents(
    backend: &SqlxBackend,
    _tree_id: &ID,
    entry_id: &ID,
    store: &str,
) -> Result<Vec<ID>> {
    let pool = backend.pool();

    let parent_rows: Vec<(String,)> = sqlx::query_as(
        "SELECT parent_id FROM store_parents WHERE child_id = $1 AND store_name = $2",
    )
    .bind(entry_id.to_string())
    .bind(store)
    .fetch_all(pool)
    .await
    .sql_context("Failed to get store parents")?;

    let parent_ids: Vec<ID> = parent_rows.into_iter().map(|(id,)| ID::from(id)).collect();

    if parent_ids.is_empty() {
        return Ok(parent_ids);
    }

    // Fetch parent entries to get their heights
    let mut parent_entries: Vec<Entry> = Vec::with_capacity(parent_ids.len());
    for id in &parent_ids {
        let row: Option<(String,)> = sqlx::query_as("SELECT entry_json FROM entries WHERE id = $1")
            .bind(id.to_string())
            .fetch_optional(pool)
            .await
            .sql_context("Failed to get entry")?;

        if let Some((json,)) = row {
            let entry: Entry = serde_json::from_str(&json)
                .map_err(|e| BackendError::DeserializationFailed { source: e })?;
            parent_entries.push(entry);
        }
    }

    // Sort by subtree height (ascending) then ID
    super::cache::sort_entries_by_subtree_height(&mut parent_entries, store)?;

    Ok(parent_entries.into_iter().map(|e| e.id()).collect())
}

/// Get all entries between from_id and to_ids in a store.
///
/// This correctly handles diamond patterns by finding ALL entries reachable
/// from to_ids by following parents back to from_id.
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

    // BFS from to_ids backward, collecting everything until we hit from_id
    let mut collected: HashSet<ID> = HashSet::new();
    let mut to_visit: VecDeque<ID> = to_ids.iter().cloned().collect();

    while let Some(current) = to_visit.pop_front() {
        if current == *from_id {
            // Don't include from_id and don't traverse past it
            continue;
        }

        if collected.contains(&current) {
            continue;
        }
        collected.insert(current.clone());

        // Add store parents to visit
        let parent_rows: Vec<(String,)> = sqlx::query_as(
            "SELECT parent_id FROM store_parents WHERE child_id = $1 AND store_name = $2",
        )
        .bind(current.to_string())
        .bind(store)
        .fetch_all(pool)
        .await
        .sql_context("Failed to get store parents")?;

        for (parent_id,) in parent_rows {
            let parent_id = ID::from(parent_id);
            if !collected.contains(&parent_id) {
                to_visit.push_back(parent_id);
            }
        }
    }

    if collected.is_empty() {
        return Ok(Vec::new());
    }

    // Fetch entries to get their heights for sorting
    let mut entries: Vec<Entry> = Vec::with_capacity(collected.len());
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

    // Sort by subtree height (ascending) then ID
    super::cache::sort_entries_by_subtree_height(&mut entries, store)?;

    Ok(entries.into_iter().map(|e| e.id()).collect())
}
