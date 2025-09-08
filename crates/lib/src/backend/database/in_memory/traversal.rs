//! Database traversal and pathfinding for InMemory database
//!
//! This module handles navigation through the DAG structure of trees,
//! including path building, LCA (Lowest Common Ancestor) algorithms,
//! parent-child relationships, and tip finding.

use std::collections::{HashMap, HashSet, VecDeque};

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
pub(crate) fn build_path_from_root(
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
        let entry = super::storage::get(backend, &current)?;

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
pub(crate) fn collect_root_to_target(
    backend: &InMemory,
    tree: &ID,
    subtree: &str,
    target_entry: &ID,
) -> Result<Vec<ID>> {
    build_path_from_root(backend, tree, subtree, target_entry)
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
pub(crate) fn get_path_from_to(
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
        let parents = get_sorted_store_parents(backend, tree_id, &current, subtree)?;

        // Add all parents to be processed
        for parent in parents {
            if !processed.contains(&parent) {
                to_process.push_back(parent);
            }
        }
    }

    // Deduplicate and sort result by height then ID for deterministic ordering
    result.sort();
    result.dedup();

    if !result.is_empty() {
        let heights = super::cache::calculate_heights(backend, tree_id, Some(subtree))?;
        result.sort_by(|a, b| {
            let a_height = *heights.get(a).unwrap_or(&0);
            let b_height = *heights.get(b).unwrap_or(&0);
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
pub(crate) fn get_sorted_store_parents(
    backend: &InMemory,
    tree_id: &ID,
    entry_id: &ID,
    subtree: &str,
) -> Result<Vec<ID>> {
    let entries = backend.entries.read().unwrap();
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
    drop(entries);

    // Sort parents by height (ascending), then by ID for determinism
    if !parents.is_empty() {
        let heights = super::cache::calculate_heights(backend, tree_id, Some(subtree))?;
        parents.sort_by(|a, b| {
            let a_height = *heights.get(a).unwrap_or(&0);
            let b_height = *heights.get(b).unwrap_or(&0);
            a_height.cmp(&b_height).then_with(|| a.cmp(b))
        });
    }

    Ok(parents)
}

/// Find the Lowest Common Ancestor (LCA) of multiple entries within a tree/subtree
///
/// This function uses breadth-first search to find the first common ancestor
/// that is reachable from all the specified entry IDs.
///
/// # Arguments
/// * `backend` - The InMemory database
/// * `tree` - The ID of the tree containing all entries
/// * `subtree` - The name of the subtree to search within
/// * `entry_ids` - The entry IDs to find the LCA for
///
/// # Returns
/// A `Result` containing the ID of the lowest common ancestor, or an error if no LCA is found.
pub(crate) fn find_lca(
    backend: &InMemory,
    tree: &ID,
    subtree: &str,
    entry_ids: &[ID],
) -> Result<ID> {
    if entry_ids.is_empty() {
        return Err(BackendError::EmptyEntryList {
            operation: "LCA".to_string(),
        }
        .into());
    }

    if entry_ids.len() == 1 {
        return Ok(entry_ids[0].clone());
    }

    // Verify that all entries exist and belong to the specified tree
    for entry_id in entry_ids {
        match super::storage::get(backend, entry_id) {
            Ok(entry) => {
                if !entry.in_tree(tree) {
                    return Err(BackendError::EntryNotInTree {
                        entry_id: entry_id.clone(),
                        tree_id: tree.clone(),
                    }
                    .into());
                }
            }
            Err(_) => {
                return Err(BackendError::EntryNotFound {
                    id: entry_id.clone(),
                }
                .into());
            }
        }
    }

    // Track which entries can reach each ancestor
    let mut ancestors: HashMap<ID, HashSet<usize>> = HashMap::new();
    let mut queues: Vec<VecDeque<ID>> = Vec::new();

    // Initialize BFS from each entry
    for (idx, entry_id) in entry_ids.iter().enumerate() {
        let mut queue = VecDeque::new();
        queue.push_back(entry_id.clone());
        ancestors.entry(entry_id.clone()).or_default().insert(idx);
        queues.push(queue);
    }

    // BFS upward until we find common ancestor
    loop {
        let mut any_progress = false;

        for (idx, queue) in queues.iter_mut().enumerate() {
            if let Some(current) = queue.pop_front() {
                any_progress = true;

                // Check if this ancestor is reachable by all entries
                let reachable_by = ancestors.entry(current.clone()).or_default();
                reachable_by.insert(idx);

                if reachable_by.len() == entry_ids.len() {
                    // Found LCA!
                    return Ok(current);
                }

                // Add parents to queue
                if let Ok(entry) = super::storage::get(backend, &current) {
                    let parents = if let Ok(parents) = entry.subtree_parents(subtree) {
                        parents
                    } else {
                        entry.parents()?
                    };

                    for parent in parents {
                        queue.push_back(parent);
                    }
                }
            }
        }

        if !any_progress {
            break;
        }
    }

    Err(BackendError::NoCommonAncestor {
        entry_ids: entry_ids.to_vec(),
    }
    .into())
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
pub(crate) fn get_tips(backend: &InMemory, tree: &ID) -> Result<Vec<ID>> {
    // Check if we have cached tree tips
    let tips_cache = backend.tips.read().unwrap();
    if let Some(cache) = tips_cache.get(tree) {
        let cached_tips: Vec<ID> = cache.tree_tips.iter().cloned().collect();
        return Ok(cached_tips);
    }
    drop(tips_cache);

    // Compute tips lazily
    let mut tips = Vec::new();
    let entries = backend.entries.read().unwrap();

    for (id, entry) in entries.iter() {
        if entry.root() == tree && super::storage::is_tip(backend, tree, id) {
            tips.push(id.clone());
        } else if entry.is_root()
            && entry.id() == *tree
            && super::storage::is_tip(backend, tree, id)
        {
            // Handle the special case of the root entry
            tips.push(id.clone());
        }
    }
    drop(entries);

    // Cache the result
    let tips_set: HashSet<ID> = tips.iter().cloned().collect();
    let mut tips_cache = backend.tips.write().unwrap();
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
pub(crate) fn get_store_tips(backend: &InMemory, tree: &ID, subtree: &str) -> Result<Vec<ID>> {
    // Check if we have cached subtree tips
    let tips_cache = backend.tips.read().unwrap();
    if let Some(cache) = tips_cache.get(tree)
        && let Some(subtree_tips) = cache.subtree_tips.get(subtree)
    {
        return Ok(subtree_tips.iter().cloned().collect());
    }
    drop(tips_cache);

    // Compute subtree tips lazily
    let tree_tips = get_tips(backend, tree)?;
    let subtree_tips = get_store_tips_up_to_entries(backend, tree, subtree, &tree_tips)?;

    // Cache the result
    let tips_set: HashSet<ID> = subtree_tips.iter().cloned().collect();
    let mut tips_cache = backend.tips.write().unwrap();
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
pub(crate) fn get_store_tips_up_to_entries(
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
    let current_tree_tips = get_tips(backend, tree)?;
    if main_entries == current_tree_tips {
        // Use original algorithm for current tips case
        let mut tips = Vec::new();
        let entries = backend.entries.read().unwrap();
        for (id, entry) in entries.iter() {
            if entry.in_tree(tree)
                && entry.in_subtree(subtree)
                && super::storage::is_subtree_tip(backend, tree, subtree, id)
            {
                tips.push(id.clone());
            }
        }
        return Ok(tips);
    }

    // For custom tips: Get all tree entries reachable from the main entries,
    // then filter to those that are in the specified subtree
    let all_tree_entries = super::storage::get_tree_from_tips(backend, tree, main_entries)?;
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
