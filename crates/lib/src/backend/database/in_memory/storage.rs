//! Core storage operations for InMemory database

use super::{InMemory, TreeHeightsCache};
use crate::{
    Result,
    backend::{VerificationStatus, errors::BackendError},
    entry::{Entry, ID},
};

/// Retrieves an entry by ID from the internal `HashMap`.
/// Used internally by traversal functions.
pub(crate) fn get(backend: &InMemory, id: &ID) -> Result<Entry> {
    let entries = backend.entries.read().unwrap();
    entries
        .get(id)
        .cloned()
        .ok_or_else(|| BackendError::EntryNotFound { id: id.clone() }.into())
}

/// Stores an entry in the database with the specified verification status.
pub(crate) fn put(
    backend: &InMemory,
    verification_status: VerificationStatus,
    entry: Entry,
) -> Result<()> {
    let entry_id = entry.id();
    let tree_id = entry.root();

    // SPECIAL CASE: For root entries (entry.root() == ""), we also need to update
    // tips for the tree whose ID is the entry's ID itself, since the root entry
    // becomes the root of a new tree.
    let additional_tree_id = if tree_id.is_empty() {
        Some(entry_id.clone())
    } else {
        None
    };

    // Store the entry
    {
        let mut entries = backend.entries.write().unwrap();
        entries.insert(entry_id.clone(), entry.clone());
    }

    // Store the verification status
    {
        let mut verification_status_map = backend.verification_status.write().unwrap();
        verification_status_map.insert(entry_id.clone(), verification_status);
    }

    // Smart cache update for heights
    {
        let mut heights_cache = backend.heights.write().unwrap();
        if let Some(cache) = heights_cache.get_mut(&tree_id) {
            update_cached_heights(cache, &entry, &entry_id);
        }
    }

    // FIXME: The tip tracking logic below assumes entries arrive in correct order
    // (parents before children). However, during sync operations, entries may arrive
    // out of order (children before parents). This causes incorrect tip tracking where:
    // 1. A child entry arrives first and is marked as a tip
    // 2. Its parent arrives later and is also marked as a tip
    // 3. The child should retroactively remove the parent from tips
    //
    // We need to handle this by:
    // - Checking if any existing tips are actually parents of the new entry
    // - Checking if the new entry is a parent of any existing tips
    // - Properly updating the tip set in both cases

    // Helper function to update tips for a given tree ID
    let update_tips_for_tree =
        |tips_cache: &mut std::collections::HashMap<ID, super::TreeTipsCache>,
         target_tree_id: &ID| {
            let cache = tips_cache.entry(target_tree_id.clone()).or_default();

            // Update tree tips
            if let Ok(parents) = entry.parents() {
                if parents.is_empty() {
                    // Root entry is also a tip initially
                    cache.tree_tips.insert(entry_id.clone());
                } else {
                    // Remove parents from tips if they exist (they're no longer tips)
                    for parent in &parents {
                        let was_tip = cache.tree_tips.remove(parent);
                        if was_tip {}
                    }
                    // Add the new entry as a tip (it has no children yet)
                    cache.tree_tips.insert(entry_id.clone());
                }
            }

            // IMPORTANT: Check if the new entry is actually a parent of any existing tips
            // This handles out-of-order arrival where children arrive before parents
            let tips_to_check: Vec<ID> = cache.tree_tips.iter().cloned().collect();
            for existing_tip_id in tips_to_check {
                // Skip if it's the entry we just added
                if existing_tip_id == entry_id {
                    continue;
                }

                // Get the existing tip entry to check its parents
                if let Ok(existing_tip) = get(backend, &existing_tip_id) {
                    if let Ok(tip_parents) = existing_tip.parents() {
                        // If the new entry is a parent of this tip, remove the new entry from tips
                        if tip_parents.contains(&entry_id) {
                            cache.tree_tips.remove(&entry_id);
                            break; // An entry can only be removed once
                        }
                    }
                }
            }
        };

    // Smart cache update for tips - ALWAYS update, creating cache if needed
    {
        let mut tips_cache = backend.tips.write().unwrap();

        // Update tips for the entry's declared tree
        update_tips_for_tree(&mut tips_cache, &tree_id);

        // SPECIAL CASE: For root entries, also update tips for the tree named after the entry ID
        if let Some(ref additional_tree) = additional_tree_id {
            update_tips_for_tree(&mut tips_cache, additional_tree);
        }

        // Update subtree tips for each subtree (for the main tree only)
        let cache = tips_cache.entry(tree_id.clone()).or_default();
        for subtree_name in entry.subtrees() {
            let subtree_tips = cache.subtree_tips.entry(subtree_name.clone()).or_default();
            if let Ok(store_parents) = entry.subtree_parents(&subtree_name) {
                if store_parents.is_empty() {
                    // Root subtree entry is also a tip initially
                    subtree_tips.insert(entry_id.clone());
                } else {
                    // Remove parents from tips if they exist (they're no longer tips)
                    for parent in &store_parents {
                        subtree_tips.remove(parent);
                    }
                    // Add the new entry as a tip (it has no children yet)
                    subtree_tips.insert(entry_id.clone());
                }
            }
        }
    }

    Ok(())
}

/// Helper function to check if an entry is a tip within its tree.
///
/// An entry is a tip if no other entry in the same tree lists it as a parent.
pub(crate) fn is_tip(backend: &InMemory, tree: &ID, entry_id: &ID) -> bool {
    // Check if any other entry has this entry as its parent
    let entries = backend.entries.read().unwrap();
    for other_entry in entries.values() {
        if other_entry.root() == tree
            && other_entry.parents().unwrap_or_default().contains(entry_id)
        {
            return false;
        }
    }
    true
}

/// Helper function to check if an entry is a tip within a specific subtree.
///
/// An entry is a subtree tip if no other entry in the same subtree lists it as a subtree parent.
pub(crate) fn is_subtree_tip(backend: &InMemory, tree: &ID, subtree: &str, entry_id: &ID) -> bool {
    let entries = backend.entries.read().unwrap();
    for other_entry in entries.values() {
        if other_entry.root() == tree
            && other_entry.subtrees().contains(&subtree.to_string())
            && let Ok(store_parents) = other_entry.subtree_parents(subtree)
            && store_parents.contains(entry_id)
        {
            return false;
        }
    }
    true
}

/// Helper function to update cached heights
fn update_cached_heights(cache: &mut TreeHeightsCache, entry: &Entry, entry_id: &ID) {
    // Calculate height based on parents
    let tree_height = if let Ok(parents) = entry.parents() {
        if parents.is_empty() {
            0 // Root has height 0
        } else {
            // Height is max parent height + 1
            parents
                .iter()
                .filter_map(|parent_id| cache.get(parent_id).map(|(h, _)| h))
                .max()
                .unwrap_or(&0)
                + 1
        }
    } else {
        0 // If parents() fails, assume it's a root
    };

    // Calculate subtree heights
    let mut subtree_heights = std::collections::HashMap::new();
    for subtree_name in entry.subtrees() {
        let subtree_height = if let Ok(store_parents) = entry.subtree_parents(&subtree_name) {
            if store_parents.is_empty() {
                0 // Subtree root has height 0
            } else {
                // Height is max subtree parent height + 1
                store_parents
                    .iter()
                    .filter_map(|parent_id| {
                        cache
                            .get(parent_id)
                            .and_then(|(_, subtree_map)| subtree_map.get(&subtree_name))
                    })
                    .max()
                    .unwrap_or(&0)
                    + 1
            }
        } else {
            0 // If store_parents() fails, assume it's a subtree root
        };
        subtree_heights.insert(subtree_name, subtree_height);
    }

    cache.insert(entry_id.clone(), (tree_height, subtree_heights));
}

/// Retrieves all entries belonging to a specific tree, sorted topologically.
pub(crate) fn get_tree(backend: &InMemory, tree: &ID) -> Result<Vec<Entry>> {
    let entries = backend.entries.read().unwrap();
    let mut tree_entries: Vec<Entry> = entries
        .values()
        .filter(|entry| entry.in_tree(tree))
        .cloned()
        .collect();

    drop(entries); // Release the lock before calling sort_entries_by_height

    // Sort by height using the cache module function
    super::cache::sort_entries_by_height(backend, tree, &mut tree_entries)?;
    Ok(tree_entries)
}

/// Retrieves all entries belonging to a specific subtree within a tree, sorted topologically.
pub(crate) fn get_store(backend: &InMemory, tree: &ID, subtree: &str) -> Result<Vec<Entry>> {
    let entries = backend.entries.read().unwrap();
    let mut subtree_entries: Vec<Entry> = entries
        .values()
        .filter(|entry| entry.in_tree(tree) && entry.in_subtree(subtree))
        .cloned()
        .collect();

    drop(entries); // Release the lock before calling sort_entries_by_subtree_height

    // Sort by subtree height using the cache module function
    super::cache::sort_entries_by_subtree_height(backend, tree, subtree, &mut subtree_entries)?;
    Ok(subtree_entries)
}

/// Retrieves all entries belonging to a specific tree up to the given tips, sorted topologically.
pub(crate) fn get_tree_from_tips(backend: &InMemory, tree: &ID, tips: &[ID]) -> Result<Vec<Entry>> {
    if tips.is_empty() {
        return Ok(vec![]);
    }

    // Use breadth-first search to collect all entries reachable from tips
    let mut result = Vec::new();
    let mut to_process = std::collections::VecDeque::new();
    let mut processed = std::collections::HashSet::new();

    // Initialize with tips
    let entries = backend.entries.read().unwrap();
    for tip in tips {
        if let Some(entry) = entries.get(tip) {
            // Only include entries that are part of the specified tree
            if entry.in_tree(tree) {
                to_process.push_back(tip.clone());
            }
        }
    }

    // Process entries in breadth-first order
    while let Some(current_id) = to_process.pop_front() {
        // Skip if already processed
        if processed.contains(&current_id) {
            continue;
        }

        if let Some(entry) = entries.get(&current_id) {
            // Entry must be in the specified tree to be included
            if entry.in_tree(tree) {
                // Add parents to be processed
                if let Ok(parents) = entry.parents() {
                    for parent in parents {
                        if !processed.contains(&parent) {
                            to_process.push_back(parent);
                        }
                    }
                }

                // Include this entry in the result
                result.push(entry.clone());
                processed.insert(current_id);
            }
        }
    }
    drop(entries);

    // Sort the result by height within the tree context
    if !result.is_empty() {
        super::cache::sort_entries_by_height(backend, tree, &mut result)?;
    }

    Ok(result)
}

/// Retrieves all entries belonging to a specific subtree within a tree up to the given tips, sorted topologically.
pub(crate) fn get_store_from_tips(
    backend: &InMemory,
    tree: &ID,
    subtree: &str,
    tips: &[ID],
) -> Result<Vec<Entry>> {
    if tips.is_empty() {
        return Ok(vec![]);
    }

    // Use breadth-first search to collect all entries reachable from tips within the subtree
    let mut result = Vec::new();
    let mut to_process = std::collections::VecDeque::new();
    let mut processed = std::collections::HashSet::new();

    // Initialize with tips
    let entries = backend.entries.read().unwrap();
    for tip in tips {
        if let Some(entry) = entries.get(tip) {
            // Only include entries that are part of both the tree and the subtree
            if entry.in_tree(tree) && entry.in_subtree(subtree) {
                to_process.push_back(tip.clone());
            }
        }
    }

    // Process entries in breadth-first order
    while let Some(current_id) = to_process.pop_front() {
        // Skip if already processed
        if processed.contains(&current_id) {
            continue;
        }

        if let Some(entry) = entries.get(&current_id) {
            // Entry must be in both the tree and subtree to be included
            if entry.in_tree(tree) && entry.in_subtree(subtree) {
                // Add subtree parents to be processed
                if let Ok(store_parents) = entry.subtree_parents(subtree) {
                    for parent in store_parents {
                        if !processed.contains(&parent) {
                            to_process.push_back(parent);
                        }
                    }
                }

                // Include this entry in the result
                result.push(entry.clone());
                processed.insert(current_id);
            }
        }
    }
    drop(entries);

    // Sort the result by subtree height
    if !result.is_empty() {
        super::cache::sort_entries_by_subtree_height(backend, tree, subtree, &mut result)?;
    }

    Ok(result)
}
