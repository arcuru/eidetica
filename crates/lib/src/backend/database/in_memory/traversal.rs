//! Database traversal and pathfinding for InMemory database
//!
//! This module handles navigation through the DAG structure of trees,
//! including path building, merge base computation, parent-child
//! relationships, and tip finding.

use std::collections::{HashSet, VecDeque};

use super::InMemoryInner;
use crate::{Result, backend::errors::BackendError, entry::ID};

/// Build the complete path from tree/subtree root to a target entry
///
/// This function traverses backwards through parent references to construct
/// the complete path from the root to the specified target entry.
///
/// # Arguments
/// * `inner` - Reference to the core data
/// * `tree` - The ID of the tree to search in
/// * `subtree` - The name of the subtree to search in (empty string for tree-level search)
/// * `target_entry` - The ID of the target entry to build a path to
///
/// # Returns
/// A `Result` containing a vector of entry IDs forming the path from root to target.
pub(crate) fn build_path_from_root(
    inner: &InMemoryInner,
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
        let entry = super::storage::get(inner, &current)?;

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
/// * `inner` - Reference to the core data
/// * `tree` - The ID of the tree to search in
/// * `subtree` - The name of the subtree to search in
/// * `target_entry` - The ID of the target entry
///
/// # Returns
/// A `Result` containing a vector of entry IDs from root to target.
pub(crate) fn collect_root_to_target(
    inner: &InMemoryInner,
    tree: &ID,
    subtree: &str,
    target_entry: &ID,
) -> Result<Vec<ID>> {
    build_path_from_root(inner, tree, subtree, target_entry)
}

/// Get all entry IDs on paths from a specific entry to multiple target entries
///
/// This function performs a breadth-first search backwards from the target entries
/// to find all entries that lie on paths between the `from_id` and any of the `to_ids`.
///
/// # Arguments
/// * `inner` - Reference to the core data
/// * `tree_id` - The ID of the tree containing all entries
/// * `subtree` - The name of the subtree to search within
/// * `from_id` - The starting entry ID
/// * `to_ids` - The target entry IDs to find paths to
///
/// # Returns
/// A `Result` containing a vector of entry IDs on paths from `from_id` to any `to_ids`,
/// sorted by height then ID for deterministic ordering.
pub(crate) fn get_path_from_to(
    inner: &InMemoryInner,
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
        let parents = get_sorted_store_parents(inner, tree_id, &current, subtree)?;

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
        result.sort_by(|a, b| {
            let a_height = inner
                .entries
                .get(a)
                .and_then(|e| e.subtree_height(subtree).ok())
                .unwrap_or(0);
            let b_height = inner
                .entries
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
/// * `inner` - Reference to the core data
/// * `tree_id` - The ID of the tree containing the entry
/// * `entry_id` - The ID of the entry to get parents for
/// * `subtree` - The name of the subtree context
///
/// # Returns
/// A `Result` containing a vector of parent entry IDs, sorted by height then ID.
pub(crate) fn get_sorted_store_parents(
    inner: &InMemoryInner,
    tree_id: &ID,
    entry_id: &ID,
    subtree: &str,
) -> Result<Vec<ID>> {
    let entry = inner
        .entries
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
            let a_height = inner
                .entries
                .get(a)
                .and_then(|e| e.subtree_height(subtree).ok())
                .unwrap_or(0);
            let b_height = inner
                .entries
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
/// * `inner` - Reference to the core data
/// * `tree` - The ID of the tree containing all entries
/// * `subtree` - The name of the subtree to search within
/// * `entry_ids` - The entry IDs to find the merge base for
///
/// # Returns
/// A `Result` containing the ID of the merge base, or an error if none is found.
pub(crate) fn find_merge_base(
    inner: &InMemoryInner,
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
        match super::storage::get(inner, entry_id) {
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
        let ancestors = collect_ancestors(inner, subtree, entry_id)?;
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
        let height = get_subtree_height(inner, subtree, &id)?;
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
            if !all_paths_pass_through(inner, subtree, entry_id, &candidate)? {
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
fn collect_ancestors(inner: &InMemoryInner, subtree: &str, entry: &ID) -> Result<HashSet<ID>> {
    let mut ancestors: HashSet<ID> = HashSet::new();
    let mut queue: VecDeque<ID> = VecDeque::new();
    queue.push_back(entry.clone());

    while let Some(current) = queue.pop_front() {
        if ancestors.contains(&current) {
            continue;
        }
        ancestors.insert(current.clone());

        if let Ok(entry_data) = super::storage::get(inner, &current)
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
fn get_subtree_height(inner: &InMemoryInner, subtree: &str, entry: &ID) -> Result<u64> {
    if let Ok(entry_data) = super::storage::get(inner, entry) {
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
fn all_paths_pass_through(
    inner: &InMemoryInner,
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
        if let Ok(entry_data) = super::storage::get(inner, &current) {
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
/// Takes `&mut InMemoryInner` because it may update the tips cache.
///
/// # Arguments
/// * `inner` - Mutable reference to the core data (for cache updates)
/// * `tree` - The ID of the tree to find tips for
///
/// # Returns
/// A `Result` containing a vector of entry IDs that are tips in the tree.
pub(crate) fn get_tips(inner: &mut InMemoryInner, tree: &ID) -> Result<Vec<ID>> {
    // Check if we have cached tree tips
    if let Some(cache) = inner.tips.get(tree) {
        return Ok(cache.tree_tips.iter().cloned().collect());
    }

    // Compute tips lazily
    let entry_info: Vec<_> = inner
        .entries
        .iter()
        .filter(|(_, entry)| entry.root() == tree || (entry.is_root() && entry.id() == *tree))
        .map(|(id, entry)| (id.clone(), entry.is_root(), entry.id()))
        .collect();

    let mut tips = Vec::new();
    for (id, is_root, entry_id) in entry_info {
        if super::storage::is_tip(&inner.entries, tree, &id) && (!is_root || entry_id == *tree) {
            tips.push(id);
        }
    }

    // Cache the result
    let tips_set: HashSet<ID> = tips.iter().cloned().collect();
    let cache = inner.tips.entry(tree.clone()).or_default();
    cache.tree_tips = tips_set;

    Ok(tips)
}

/// Compute store tips by scanning all entries in the tree.
///
/// This is a helper function that avoids recursion between get_store_tips
/// and get_store_tips_up_to_entries.
fn compute_store_tips_from_all_entries(
    inner: &InMemoryInner,
    tree: &ID,
    subtree: &str,
) -> Result<Vec<ID>> {
    let entry_info: Vec<_> = inner
        .entries
        .iter()
        .filter(|(_, entry)| entry.in_tree(tree) && entry.in_subtree(subtree))
        .map(|(id, _)| id.clone())
        .collect();

    let mut tips = Vec::new();
    for id in entry_info {
        if super::storage::is_subtree_tip(&inner.entries, tree, subtree, &id) {
            tips.push(id);
        }
    }
    Ok(tips)
}

/// Find the tip entries for the specified subtree
///
/// Uses lazy cached subtree tips for O(1) performance after first computation.
/// A subtree tip is an entry that has no children within the specific subtree.
///
/// # Arguments
/// * `inner` - Mutable reference to the core data (for cache updates)
/// * `tree` - The ID of the tree containing the subtree
/// * `subtree` - The name of the subtree to find tips for
///
/// # Returns
/// A `Result` containing a vector of entry IDs that are tips in the subtree.
pub(crate) fn get_store_tips(
    inner: &mut InMemoryInner,
    tree: &ID,
    subtree: &str,
) -> Result<Vec<ID>> {
    // Check if we have cached subtree tips
    if let Some(cache) = inner.tips.get(tree)
        && let Some(subtree_tips) = cache.subtree_tips.get(subtree)
    {
        return Ok(subtree_tips.iter().cloned().collect());
    }

    // Compute subtree tips (using helper to avoid recursion)
    let subtree_tips = compute_store_tips_from_all_entries(&*inner, tree, subtree)?;

    // Cache the result
    let tips_set: HashSet<ID> = subtree_tips.iter().cloned().collect();
    let cache = inner.tips.entry(tree.clone()).or_default();
    cache.subtree_tips.insert(subtree.to_string(), tips_set);

    Ok(subtree_tips)
}

/// Find subtree tips up to specific main tree entries
///
/// This function finds all entries that are tips within a subtree scope, considering
/// only entries reachable from the specified main tree entries.
///
/// # Arguments
/// * `inner` - Mutable reference to the core data (for cache updates)
/// * `tree` - The ID of the tree containing the subtree
/// * `subtree` - The name of the subtree to search within
/// * `main_entries` - The main tree entries to consider as the scope
///
/// # Returns
/// A `Result` containing a vector of entry IDs that are subtree tips within the scope.
pub(crate) fn get_store_tips_up_to_entries(
    inner: &mut InMemoryInner,
    tree: &ID,
    subtree: &str,
    main_entries: &[ID],
) -> Result<Vec<ID>> {
    if main_entries.is_empty() {
        return Ok(Vec::new());
    }

    // Fast path: if main_entries represents current tree tips, use cached subtree tips
    let current_tree_tips = get_tips(inner, tree)?;
    if main_entries == current_tree_tips {
        // Check cache first - O(1) lookup
        if let Some(cache) = inner.tips.get(tree)
            && let Some(subtree_tips) = cache.subtree_tips.get(subtree)
        {
            return Ok(subtree_tips.iter().cloned().collect());
        }

        // Cache miss - compute tips and cache them
        let computed_tips = compute_store_tips_from_all_entries(&*inner, tree, subtree)?;

        // Cache the result
        let tips_set: HashSet<ID> = computed_tips.iter().cloned().collect();
        let cache = inner.tips.entry(tree.clone()).or_default();
        cache.subtree_tips.insert(subtree.to_string(), tips_set);

        return Ok(computed_tips);
    }

    // For custom tips: Get all tree entries reachable from the main entries,
    // then filter to those that are in the specified subtree
    let all_tree_entries = super::storage::get_tree_from_tips(&*inner, tree, main_entries)?;
    let subtree_entries: Vec<_> = all_tree_entries
        .into_iter()
        .filter(|entry| entry.in_subtree(subtree))
        .collect();

    // If no subtree entries found, return empty
    if subtree_entries.is_empty() {
        return Ok(Vec::new());
    }

    // Find which of these are tips within the subtree scope
    // O(n) algorithm: collect all parents first, then filter
    let subtree_entry_ids: HashSet<ID> = subtree_entries.iter().map(|e| e.id()).collect();

    // Step 1: Collect all IDs that are parents of any entry in the scope
    let mut all_parents: HashSet<ID> = HashSet::new();
    for entry in &subtree_entries {
        if let Ok(parents) = entry.subtree_parents(subtree) {
            for parent in parents {
                if subtree_entry_ids.contains(&parent) {
                    all_parents.insert(parent);
                }
            }
        }
    }

    // Step 2: Tips = entries that are NOT parents of anything in the scope
    let tips: Vec<ID> = subtree_entry_ids
        .into_iter()
        .filter(|id| !all_parents.contains(id))
        .collect();

    Ok(tips)
}
