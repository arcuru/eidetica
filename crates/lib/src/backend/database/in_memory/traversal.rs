//! Database traversal and pathfinding for InMemory database
//!
//! This module handles navigation through the DAG structure of trees,
//! including path building, merge base computation, parent-child
//! relationships, and tip finding.

use std::collections::{HashSet, VecDeque};

use super::InMemory;
use crate::{Result, backend::errors::BackendError, entry::ID};

/// Build the complete path from tree/subtree root to a target entry
///
/// This function traverses backwards through parent references to construct
/// the complete path from the root to the specified target entry.
///
/// # Arguments
/// * `backend` - The InMemory database
/// * `tree` - The ID of the tree to search in
/// * `subtree` - The name of the subtree to search in (empty string for tree-level search)
/// * `target_entry` - The ID of the target entry to build a path to
///
/// # Returns
/// A `Result` containing a vector of entry IDs forming the path from root to target.
pub(crate) async fn build_path_from_root(
    backend: &InMemory,
    tree: &ID,
    subtree: &str,
    target_entry: &ID,
) -> Result<Vec<ID>> {
    let mut path = Vec::new();
    let mut current = target_entry.clone();
    let mut visited = HashSet::new();

    // Build path by following parents back to root
    loop {
        if visited.contains(&current) {
            return Err(BackendError::CycleDetected { entry_id: current }.into());
        }
        visited.insert(current.clone());
        path.push(current.clone());

        // Get the entry
        let entry = super::storage::get(backend, &current).await?;

        // Check if we've reached the tree root
        if current == *tree || entry.is_root() {
            break;
        }

        // Get subtree parents for this entry
        let parents = if subtree.is_empty() || entry.subtree_parents(subtree).is_err() {
            // If no subtree specified or no subtree parents, follow main parents
            entry.parents()?
        } else {
            entry.subtree_parents(subtree)?
        };

        if parents.is_empty() {
            // No parents - this must be a root entry
            break;
        } else {
            // Follow the first parent (in height/ID sorted order)
            current = parents[0].clone();
        }
    }

    // Reverse to get root-to-target order
    path.reverse();

    Ok(path)
}

/// Collect all entry IDs from root to a target entry
///
/// This is a convenience wrapper around `build_path_from_root`.
///
/// # Arguments
/// * `backend` - The InMemory database
/// * `tree` - The ID of the tree to search in
/// * `subtree` - The name of the subtree to search in
/// * `target_entry` - The ID of the target entry
///
/// # Returns
/// A `Result` containing a vector of entry IDs from root to target.
pub(crate) async fn collect_root_to_target(
    backend: &InMemory,
    tree: &ID,
    subtree: &str,
    target_entry: &ID,
) -> Result<Vec<ID>> {
    build_path_from_root(backend, tree, subtree, target_entry).await
}

/// Get all entry IDs on paths from a specific entry to multiple target entries
///
/// This function performs a breadth-first search backwards from the target entries
/// to find all entries that lie on paths between the `from_id` and any of the `to_ids`.
///
/// # Arguments
/// * `backend` - The InMemory database
/// * `tree_id` - The ID of the tree containing all entries
/// * `subtree` - The name of the subtree to search within
/// * `from_id` - The starting entry ID
/// * `to_ids` - The target entry IDs to find paths to
///
/// # Returns
/// A `Result` containing a vector of entry IDs on paths from `from_id` to any `to_ids`,
/// sorted by height then ID for deterministic ordering.
pub(crate) async fn get_path_from_to(
    backend: &InMemory,
    tree_id: &ID,
    subtree: &str,
    from_id: &ID,
    to_ids: &[ID],
) -> Result<Vec<ID>> {
    if to_ids.is_empty() {
        return Ok(vec![]);
    }

    // If any target is the same as from_id, we still process others
    // Use breadth-first search to find ALL entries between from_id and any of the to_ids
    let mut result = Vec::new();
    let mut to_process = VecDeque::new();
    let mut processed = HashSet::new();

    // Start from all to_ids
    for to_id in to_ids {
        if to_id != from_id {
            to_process.push_back(to_id.clone());
        }
    }

    while let Some(current) = to_process.pop_front() {
        // Skip if already processed
        if processed.contains(&current) {
            continue;
        }

        // If we've reached the from_id, stop processing this path
        if current == *from_id {
            processed.insert(current);
            continue;
        }

        // Add current to result (unless it's the from_id)
        result.push(current.clone());
        processed.insert(current.clone());

        // Get parents in the subtree
        let parents = get_sorted_store_parents(backend, tree_id, &current, subtree).await?;

        // Add all parents to be processed
        for parent in parents {
            if !processed.contains(&parent) {
                to_process.push_back(parent);
            }
        }
    }

    // Deduplicate result
    result.sort();
    result.dedup();

    // Sort by subtree height then ID for deterministic ordering
    // Fetch entries to get their embedded heights
    if !result.is_empty() {
        let entries = backend.entries.read().await;
        result.sort_by(|a, b| {
            let a_height = entries
                .get(a)
                .and_then(|e| e.subtree_height(subtree).ok())
                .unwrap_or(0);
            let b_height = entries
                .get(b)
                .and_then(|e| e.subtree_height(subtree).ok())
                .unwrap_or(0);
            a_height.cmp(&b_height).then_with(|| a.cmp(b))
        });
    }

    Ok(result)
}

/// Get the subtree parent IDs for a specific entry and subtree, sorted by height then ID
///
/// This function retrieves the parent entries of a given entry within a specific subtree
/// context and sorts them by their height (ascending) and then by ID for determinism.
///
/// # Arguments
/// * `backend` - The InMemory database
/// * `tree_id` - The ID of the tree containing the entry
/// * `entry_id` - The ID of the entry to get parents for
/// * `subtree` - The name of the subtree context
///
/// # Returns
/// A `Result` containing a vector of parent entry IDs, sorted by height then ID.
pub(crate) async fn get_sorted_store_parents(
    backend: &InMemory,
    tree_id: &ID,
    entry_id: &ID,
    subtree: &str,
) -> Result<Vec<ID>> {
    let entries = backend.entries.read().await;
    let entry = entries
        .get(entry_id)
        .ok_or_else(|| BackendError::EntryNotFound {
            id: entry_id.clone(),
        })?;

    if !entry.in_tree(tree_id) || !entry.in_subtree(subtree) {
        return Ok(Vec::new());
    }

    let mut parents = match entry.subtree_parents(subtree) {
        Ok(parents) => parents,
        Err(_) => return Ok(Vec::new()),
    };

    // Sort parents by height (ascending), then by ID for determinism
    // Heights are embedded in entries, so we read them directly
    if !parents.is_empty() {
        parents.sort_by(|a, b| {
            let a_height = entries
                .get(a)
                .and_then(|e| e.subtree_height(subtree).ok())
                .unwrap_or(0);
            let b_height = entries
                .get(b)
                .and_then(|e| e.subtree_height(subtree).ok())
                .unwrap_or(0);
            a_height.cmp(&b_height).then_with(|| a.cmp(b))
        });
    }

    Ok(parents)
}

/// Find the merge base (common dominator) of multiple entries within a tree/subtree.
///
/// The merge base is the lowest ancestor that ALL paths from ALL tips must pass through.
///
/// # Arguments
/// * `backend` - The InMemory database
/// * `tree` - The ID of the tree containing all entries
/// * `subtree` - The name of the subtree to search within
/// * `entry_ids` - The entry IDs to find the merge base for
///
/// # Returns
/// A `Result` containing the ID of the merge base, or an error if none is found.
pub(crate) async fn find_merge_base(
    backend: &InMemory,
    tree: &ID,
    subtree: &str,
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

    tracing::debug!(
        tree_id = %tree,
        subtree = subtree,
        entry_count = entry_ids.len(),
        entry_ids = ?entry_ids,
        "Starting merge base algorithm"
    );

    // Verify that all entries exist and belong to the specified tree
    for entry_id in entry_ids {
        match super::storage::get(backend, entry_id).await {
            Ok(entry) => {
                if let Err(validation_error) = entry.validate() {
                    tracing::error!(
                        entry_id = %entry_id,
                        error = %validation_error,
                        "Entry failed validation in merge base algorithm"
                    );
                    return Err(BackendError::EntryValidationFailed {
                        entry_id: entry_id.clone(),
                        reason: validation_error.to_string(),
                    }
                    .into());
                }

                if !entry.in_tree(tree) {
                    tracing::warn!(
                        entry_id = %entry_id,
                        tree_id = %tree,
                        actual_tree = %entry.root(),
                        "Entry is not in the expected tree"
                    );
                    return Err(BackendError::EntryNotInTree {
                        entry_id: entry_id.clone(),
                        tree_id: tree.clone(),
                    }
                    .into());
                }
            }
            Err(_) => {
                tracing::error!(entry_id = %entry_id, "Entry not found");
                return Err(BackendError::EntryNotFound {
                    id: entry_id.clone(),
                }
                .into());
            }
        }
    }

    // Step 1: Collect all ancestors for each entry
    // FIXME(perf): Improve this, it's correct but leaves optimizations on the table.
    let mut ancestor_sets: Vec<HashSet<ID>> = Vec::with_capacity(entry_ids.len());
    for entry_id in entry_ids {
        let ancestors = collect_ancestors(backend, subtree, entry_id).await?;
        ancestor_sets.push(ancestors);
    }

    // Step 2: Find common ancestors (intersection of all ancestor sets)
    let mut common_ancestors: HashSet<ID> = ancestor_sets[0].clone();
    for ancestor_set in &ancestor_sets[1..] {
        common_ancestors.retain(|a| ancestor_set.contains(a));
    }

    if common_ancestors.is_empty() {
        tracing::debug!(subtree = subtree, "No common ancestors found");
        return Err(BackendError::NoCommonAncestor {
            entry_ids: entry_ids.to_vec(),
        }
        .into());
    }

    // Step 3: Get heights for sorting (we want highest height first = closest to tips)
    let mut candidates: Vec<(ID, u64)> = Vec::with_capacity(common_ancestors.len());
    for id in common_ancestors {
        let height = get_subtree_height(backend, subtree, &id).await?;
        candidates.push((id, height));
    }
    // Sort by height descending (highest first = closest to tips)
    candidates.sort_by(|a, b| b.1.cmp(&a.1));

    tracing::trace!(
        candidate_count = candidates.len(),
        "Checking candidates for merge base"
    );

    // Step 4: Find the first candidate where ALL paths from ALL entries pass through it
    for (candidate, height) in candidates {
        let mut all_paths_pass = true;
        for entry_id in entry_ids {
            if !all_paths_pass_through(backend, subtree, entry_id, &candidate).await? {
                all_paths_pass = false;
                break;
            }
        }
        if all_paths_pass {
            tracing::debug!(
                merge_base = %candidate,
                height = height,
                "Found merge base"
            );
            return Ok(candidate);
        }
    }

    // Should not reach here if there's a root
    Err(BackendError::NoCommonAncestor {
        entry_ids: entry_ids.to_vec(),
    }
    .into())
}

/// Collect all ancestors of an entry in a subtree (including the entry itself).
async fn collect_ancestors(backend: &InMemory, subtree: &str, entry: &ID) -> Result<HashSet<ID>> {
    let mut ancestors: HashSet<ID> = HashSet::new();
    let mut queue: VecDeque<ID> = VecDeque::new();
    queue.push_back(entry.clone());

    while let Some(current) = queue.pop_front() {
        if ancestors.contains(&current) {
            continue;
        }
        ancestors.insert(current.clone());

        if let Ok(entry_data) = super::storage::get(backend, &current).await
            && let Ok(parents) = entry_data.subtree_parents(subtree)
        {
            for parent in parents {
                queue.push_back(parent);
            }
        }
    }

    Ok(ancestors)
}

/// Get the subtree height for an entry.
async fn get_subtree_height(backend: &InMemory, subtree: &str, entry: &ID) -> Result<u64> {
    if let Ok(entry_data) = super::storage::get(backend, entry).await {
        // Try to get subtree-specific height, fall back to tree height
        Ok(entry_data
            .subtree_height(subtree)
            .unwrap_or_else(|_| entry_data.height()))
    } else {
        Ok(0)
    }
}

/// Check if ALL paths from entry to root pass through candidate.
///
/// This works by trying to reach a root (entry with no parents) while avoiding
/// the candidate. If we can reach a root, there's a bypass path and the candidate
/// is not on all paths.
async fn all_paths_pass_through(
    backend: &InMemory,
    subtree: &str,
    entry: &ID,
    candidate: &ID,
) -> Result<bool> {
    // Trivial case: entry is the candidate
    if entry == candidate {
        return Ok(true);
    }

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

        // Get parents in subtree
        if let Ok(entry_data) = super::storage::get(backend, &current).await {
            match entry_data.subtree_parents(subtree) {
                Ok(parents) => {
                    if parents.is_empty() {
                        // Reached a root while avoiding the candidate - there's a bypass path!
                        return Ok(false);
                    }
                    for parent in parents {
                        queue.push_back(parent);
                    }
                }
                Err(_) => {
                    // Entry doesn't have this subtree - treat as root
                    return Ok(false);
                }
            }
        }
    }

    // Couldn't reach any root without going through the candidate
    Ok(true)
}

/// Find the tip entries for the specified tree
///
/// Uses lazy cached tips for O(1) performance after first computation.
/// A tip is an entry that has no children within the tree.
///
/// # Arguments
/// * `backend` - The InMemory database
/// * `tree` - The ID of the tree to find tips for
///
/// # Returns
/// A `Result` containing a vector of entry IDs that are tips in the tree.
pub(crate) async fn get_tips(backend: &InMemory, tree: &ID) -> Result<Vec<ID>> {
    // Check if we have cached tree tips
    let tips_cache = backend.tips.read().await;
    if let Some(cache) = tips_cache.get(tree) {
        let cached_tips: Vec<ID> = cache.tree_tips.iter().cloned().collect();
        return Ok(cached_tips);
    }
    drop(tips_cache);

    // Compute tips lazily
    let mut tips = Vec::new();
    let entries = backend.entries.read().await;

    // Collect entry info before async calls
    let entry_info: Vec<_> = entries
        .iter()
        .filter(|(_, entry)| entry.root() == tree || (entry.is_root() && entry.id() == *tree))
        .map(|(id, entry)| (id.clone(), entry.is_root(), entry.id()))
        .collect();
    drop(entries);

    for (id, is_root, entry_id) in entry_info {
        let is_tip_result = super::storage::is_tip(backend, tree, &id).await;
        if is_tip_result && (!is_root || entry_id == *tree) {
            tips.push(id);
        }
    }

    // Cache the result
    let tips_set: HashSet<ID> = tips.iter().cloned().collect();
    let mut tips_cache = backend.tips.write().await;
    let cache = tips_cache.entry(tree.clone()).or_default();
    cache.tree_tips = tips_set;

    Ok(tips)
}

/// Find the tip entries for the specified subtree
///
/// Uses lazy cached subtree tips for O(1) performance after first computation.
/// A subtree tip is an entry that has no children within the specific subtree.
///
/// # Arguments
/// * `backend` - The InMemory database
/// * `tree` - The ID of the tree containing the subtree
/// * `subtree` - The name of the subtree to find tips for
///
/// # Returns
/// A `Result` containing a vector of entry IDs that are tips in the subtree.
pub(crate) async fn get_store_tips(
    backend: &InMemory,
    tree: &ID,
    subtree: &str,
) -> Result<Vec<ID>> {
    // Check if we have cached subtree tips
    let tips_cache = backend.tips.read().await;
    if let Some(cache) = tips_cache.get(tree)
        && let Some(subtree_tips) = cache.subtree_tips.get(subtree)
    {
        return Ok(subtree_tips.iter().cloned().collect());
    }
    drop(tips_cache);

    // Compute subtree tips lazily
    let tree_tips = get_tips(backend, tree).await?;
    let subtree_tips = get_store_tips_up_to_entries(backend, tree, subtree, &tree_tips).await?;

    // Cache the result
    let tips_set: HashSet<ID> = subtree_tips.iter().cloned().collect();
    let mut tips_cache = backend.tips.write().await;
    let cache = tips_cache.entry(tree.clone()).or_default();
    cache.subtree_tips.insert(subtree.to_string(), tips_set);

    Ok(subtree_tips)
}

/// Find subtree tips up to specific main tree entries
///
/// This function finds all entries that are tips within a subtree scope, considering
/// only entries reachable from the specified main tree entries.
///
/// # Arguments
/// * `backend` - The InMemory database
/// * `tree` - The ID of the tree containing the subtree
/// * `subtree` - The name of the subtree to search within
/// * `main_entries` - The main tree entries to consider as the scope
///
/// # Returns
/// A `Result` containing a vector of entry IDs that are subtree tips within the scope.
pub(crate) async fn get_store_tips_up_to_entries(
    backend: &InMemory,
    tree: &ID,
    subtree: &str,
    main_entries: &[ID],
) -> Result<Vec<ID>> {
    if main_entries.is_empty() {
        return Ok(Vec::new());
    }

    // Special case: if main_entries represents current tree tips (i.e., we want all subtree tips),
    // use the original algorithm that checks all entries
    let current_tree_tips = get_tips(backend, tree).await?;
    if main_entries == current_tree_tips {
        // Use original algorithm for current tips case
        let mut tips = Vec::new();
        let entries = backend.entries.read().await;

        // Collect entry info before async calls
        let entry_info: Vec<_> = entries
            .iter()
            .filter(|(_, entry)| entry.in_tree(tree) && entry.in_subtree(subtree))
            .map(|(id, _)| id.clone())
            .collect();
        drop(entries);

        for id in entry_info {
            if super::storage::is_subtree_tip(backend, tree, subtree, &id).await {
                tips.push(id);
            }
        }
        return Ok(tips);
    }

    // For custom tips: Get all tree entries reachable from the main entries,
    // then filter to those that are in the specified subtree
    let all_tree_entries = super::storage::get_tree_from_tips(backend, tree, main_entries).await?;
    let subtree_entries: Vec<_> = all_tree_entries
        .into_iter()
        .filter(|entry| entry.in_subtree(subtree))
        .collect();

    // If no subtree entries found, return empty
    if subtree_entries.is_empty() {
        return Ok(Vec::new());
    }

    // Find which of these are tips within the subtree scope
    let mut tips = Vec::new();
    for entry in &subtree_entries {
        let entry_id = entry.id();

        // Check if this entry is a tip by seeing if any other entry in our scope
        // has it as a subtree parent
        let is_tip = !subtree_entries.iter().any(|other_entry| {
            if let Ok(parents) = other_entry.subtree_parents(subtree) {
                parents.contains(&entry_id)
            } else {
                false
            }
        });

        if is_tip {
            tips.push(entry_id);
        }
    }

    Ok(tips)
}
