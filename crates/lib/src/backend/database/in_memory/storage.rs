//! Core storage operations for InMemory database

use std::collections::HashMap;

use super::InMemoryInner;
use crate::{
    Result,
    backend::{VerificationStatus, errors::BackendError},
    entry::{Entry, ID},
};

/// Retrieves an entry by ID from the internal `HashMap`.
/// Used internally by traversal functions.
pub(crate) fn get(inner: &InMemoryInner, id: &ID) -> Result<Entry> {
    inner
        .entries
        .get(id)
        .cloned()
        .ok_or_else(|| BackendError::EntryNotFound { id: id.clone() }.into())
}

/// Stores an entry in the database with the specified verification status.
///
/// IMPORTANT: `entry.validate()` must be called by the caller before this function.
/// Validation is separated to allow failing before acquiring the write lock.
///
/// # Storage Operations
/// 1. Stores the entry in the entries HashMap
/// 2. Records the verification status
/// 3. Updates tip tracking for efficient DAG traversal
///
/// # Tip Tracking
/// The function maintains tips (leaf nodes) for both the main tree and subtrees.
/// This is complicated by entries potentially arriving out of order during sync:
/// - A child entry might arrive before its parent
/// - The tip tracking is recalculated from scratch to handle this correctly
///
/// # Arguments
/// * `inner` - Mutable reference to the core data
/// * `verification_status` - Whether the entry has been cryptographically verified
/// * `entry` - The entry to store (must already be validated)
///
/// # Returns
/// * `Ok(())` on successful storage
pub(crate) fn put(
    inner: &mut InMemoryInner,
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

    // Store the entry and verification status
    inner.entries.insert(entry_id.clone(), entry.clone());
    inner
        .verification_status
        .insert(entry_id.clone(), verification_status);

    // Tip tracking uses full recalculation to handle out-of-order entry arrival during sync.
    // This ensures correctness when entries arrive in any order, which is common during
    // sync operations between peers.

    // Update tips for the entry's declared tree (split borrows: &entries + &mut tips)
    update_tips_for_tree(&inner.entries, &mut inner.tips, &tree_id);

    // SPECIAL CASE: For root entries, also update tips for the tree named after the entry ID
    if let Some(ref additional_tree) = additional_tree_id {
        update_tips_for_tree(&inner.entries, &mut inner.tips, additional_tree);
    }

    // Update subtree tips - recalculate from scratch to handle out-of-order arrival
    // This mirrors the tree-level tip recalculation above
    let cache = inner.tips.entry(tree_id.clone()).or_default();
    for subtree_name in entry.subtrees() {
        // Recalculate tips for this store from scratch
        let subtree_tips = cache.subtree_tips.entry(subtree_name.clone()).or_default();
        subtree_tips.clear();

        // Get all entries in this store (split borrow: &entries while &mut tips held via cache)
        let store_entries: Vec<&Entry> = inner
            .entries
            .values()
            .filter(|e| {
                (e.root() == tree_id || (e.is_root() && e.id() == tree_id))
                    && e.subtrees().contains(&subtree_name)
            })
            .collect();

        // An entry is a store tip if no other entry in the store has it as a store parent
        for store_entry in &store_entries {
            let store_entry_id = store_entry.id();
            let mut is_tip = true;

            for other_entry in &store_entries {
                if let Ok(parents) = other_entry.subtree_parents(&subtree_name)
                    && parents.contains(&store_entry_id)
                {
                    is_tip = false;
                    break;
                }
            }

            if is_tip {
                subtree_tips.insert(store_entry_id);
            }
        }
    }

    Ok(())
}

/// Helper function to update tips for a given tree ID.
///
/// Takes split borrows on entries (read) and tips (write) to avoid
/// needing a mutable reference to the entire InMemoryInner.
fn update_tips_for_tree(
    entries: &HashMap<ID, Entry>,
    tips_cache: &mut HashMap<ID, super::TreeTipsCache>,
    target_tree_id: &ID,
) {
    let cache = tips_cache.entry(target_tree_id.clone()).or_default();

    // IMPORTANT: Recalculate tips from scratch after adding any entry
    //
    // Why full recalculation is necessary:
    // During sync operations, entries can arrive out of order. For example:
    // 1. A child entry arrives first and is marked as a tip
    // 2. Its parent arrives later
    // 3. The parent should not be a tip (it has a child)
    // 4. The child should remain a tip
    //
    // Incremental updates would miss removing the parent from tips in step 3.
    // Full recalculation ensures correctness at the cost of performance.
    //
    // TODO: Optimize with proper DAG-aware incremental updates that handle
    // out-of-order arrival by checking if new entries are parents of existing tips
    cache.tree_tips.clear();

    // Get all entries in this tree and recalculate which ones are actually tips
    let tree_entries: Vec<&Entry> = entries
        .values()
        .filter(|e| e.root() == target_tree_id || (e.is_root() && e.id() == *target_tree_id))
        .collect();

    // An entry is a tip if no other entry in the same tree has it as a parent
    for entry in &tree_entries {
        let entry_id = entry.id();
        let mut is_tip = true;

        for other_entry in &tree_entries {
            if let Ok(parents) = other_entry.parents()
                && parents.contains(&entry_id)
            {
                is_tip = false;
                break;
            }
        }

        if is_tip {
            cache.tree_tips.insert(entry_id);
        }
    }
}

/// Helper function to check if an entry is a tip within its tree.
///
/// An entry is a tip if no other entry in the same tree lists it as a parent.
pub(crate) fn is_tip(entries: &HashMap<ID, Entry>, tree: &ID, entry_id: &ID) -> bool {
    // Check if any other entry has this entry as its parent
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
pub(crate) fn is_subtree_tip(
    entries: &HashMap<ID, Entry>,
    tree: &ID,
    subtree: &str,
    entry_id: &ID,
) -> bool {
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

/// Retrieves all entries belonging to a specific tree, sorted topologically.
pub(crate) fn get_tree(inner: &InMemoryInner, tree: &ID) -> Result<Vec<Entry>> {
    let mut tree_entries: Vec<Entry> = inner
        .entries
        .values()
        .filter(|entry| entry.in_tree(tree))
        .cloned()
        .collect();

    super::cache::sort_entries_by_height(&mut tree_entries);
    Ok(tree_entries)
}

/// Retrieves all entries belonging to a specific subtree within a tree, sorted topologically.
pub(crate) fn get_store(inner: &InMemoryInner, tree: &ID, subtree: &str) -> Result<Vec<Entry>> {
    let mut subtree_entries: Vec<Entry> = inner
        .entries
        .values()
        .filter(|entry| entry.in_tree(tree) && entry.in_subtree(subtree))
        .cloned()
        .collect();

    super::cache::sort_entries_by_subtree_height(subtree, &mut subtree_entries);
    Ok(subtree_entries)
}

/// Retrieves all entries belonging to a specific tree up to the given tips, sorted topologically.
pub(crate) fn get_tree_from_tips(
    inner: &InMemoryInner,
    tree: &ID,
    tips: &[ID],
) -> Result<Vec<Entry>> {
    if tips.is_empty() {
        return Ok(vec![]);
    }

    // Use breadth-first search to collect all entries reachable from tips
    let mut result = Vec::new();
    let mut to_process = std::collections::VecDeque::new();
    let mut processed = std::collections::HashSet::new();

    // Initialize with tips
    for tip in tips {
        if let Some(entry) = inner.entries.get(tip) {
            // Only include entries that are part of the specified tree
            if entry.in_tree(tree) {
                to_process.push_back(tip.clone());
            } else {
                return Err(BackendError::EntryNotInTree {
                    entry_id: tip.clone(),
                    tree_id: tree.clone(),
                }
                .into());
            }
        } else {
            return Err(BackendError::EntryNotFound { id: tip.clone() }.into());
        }
    }

    // Process entries in breadth-first order
    while let Some(current_id) = to_process.pop_front() {
        // Skip if already processed
        if processed.contains(&current_id) {
            continue;
        }

        if let Some(entry) = inner.entries.get(&current_id) {
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

    // Sort the result by height
    super::cache::sort_entries_by_height(&mut result);

    Ok(result)
}

/// Retrieves all entries belonging to a specific subtree within a tree up to the given tips, sorted topologically.
pub(crate) fn get_store_from_tips(
    inner: &InMemoryInner,
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
    for tip in tips {
        if let Some(entry) = inner.entries.get(tip) {
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

        if let Some(entry) = inner.entries.get(&current_id) {
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

    // Sort the result by subtree height
    super::cache::sort_entries_by_subtree_height(subtree, &mut result);

    Ok(result)
}
