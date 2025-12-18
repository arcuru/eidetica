//! Core storage operations for InMemory database

use super::InMemory;
use crate::{
    Result,
    backend::{VerificationStatus, errors::BackendError},
    entry::{Entry, ID},
};

/// Retrieves an entry by ID from the internal `HashMap`.
/// Used internally by traversal functions.
pub(crate) async fn get(backend: &InMemory, id: &ID) -> Result<Entry> {
    let entries = backend.entries.read().await;
    entries
        .get(id)
        .cloned()
        .ok_or_else(|| BackendError::EntryNotFound { id: id.clone() }.into())
}

/// Stores an entry in the database with the specified verification status.
///
/// This function is the **final validation gate** before entries are persisted.
/// It serves as the last line of defense against invalid entries that could
/// corrupt the DAG structure and cause sync failures.
///
/// # Critical Entry Validation
///
/// ## Why This Matters
/// Invalid entries with missing parent relationships break fundamental assumptions:
/// - LCA calculations fail with "no common ancestor" errors
/// - Tree traversal becomes impossible when nodes are unreachable
/// - Height calculations fail for orphaned entries
/// - Sync operations cannot determine proper merge points
///
/// ## Validation Enforcement
/// Calls `entry.validate()` which enforces:
/// - **Non-root entries MUST have main tree parents** (prevents orphaned nodes)
/// - Parent IDs cannot be empty strings
/// - Subtree structure integrity is maintained
///
/// This validation applies to ALL entries, whether:
/// - Created locally through transactions
/// - Received from remote peers during sync
/// - Loaded from disk during deserialization
///
/// # Storage Operations
/// 1. **Validates entry structure** via `entry.validate()` - HARD FAILURE on invalid
/// 2. Stores the entry in the entries HashMap
/// 3. Records the verification status
/// 4. Updates tip tracking for efficient DAG traversal
///
/// # Tip Tracking
/// The function maintains tips (leaf nodes) for both the main tree and subtrees.
/// This is complicated by entries potentially arriving out of order during sync:
/// - A child entry might arrive before its parent
/// - The tip tracking is recalculated from scratch to handle this correctly
///
/// # Arguments
/// * `backend` - The InMemory database instance
/// * `verification_status` - Whether the entry has been cryptographically verified
/// * `entry` - The entry to store
///
/// # Returns
/// * `Ok(())` on successful storage
/// * `Err` if validation fails or storage operations fail
pub(crate) async fn put(
    backend: &InMemory,
    verification_status: VerificationStatus,
    entry: Entry,
) -> Result<()> {
    // CRITICAL VALIDATION GATE: Final check before persistence
    // This is the last line of defense against invalid entries. The validate() call:
    // 1. Ensures non-root entries have main tree parents (prevents orphaned nodes)
    // 2. Rejects empty parent IDs that would break DAG traversal
    // 3. Validates subtree structural integrity
    //
    // Without this validation, invalid entries would cause:
    // - "No common ancestor" errors during sync LCA calculations
    // - Unreachable nodes breaking tree traversal algorithms
    // - Sync failures when peers cannot merge divergent histories
    //
    // This applies to ALL entries: local creations, sync receipts, and disk loads
    entry.validate()?;

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
        let mut entries = backend.entries.write().await;
        entries.insert(entry_id.clone(), entry.clone());
    }

    // Store the verification status
    {
        let mut verification_status_map = backend.verification_status.write().await;
        verification_status_map.insert(entry_id.clone(), verification_status);
    }

    // Tip tracking uses full recalculation to handle out-of-order entry arrival during sync.
    // This ensures correctness when entries arrive in any order, which is common during
    // sync operations between peers.

    // Smart cache update for tips - ALWAYS update, creating cache if needed
    {
        let mut tips_cache = backend.tips.write().await;

        // Update tips for the entry's declared tree
        update_tips_for_tree_async(backend, &mut tips_cache, &tree_id).await;

        // SPECIAL CASE: For root entries, also update tips for the tree named after the entry ID
        if let Some(ref additional_tree) = additional_tree_id {
            update_tips_for_tree_async(backend, &mut tips_cache, additional_tree).await;
        }

        // Update subtree tips - recalculate from scratch to handle out-of-order arrival
        // This mirrors the tree-level tip recalculation above
        let cache = tips_cache.entry(tree_id.clone()).or_default();
        for subtree_name in entry.subtrees() {
            // Recalculate tips for this store from scratch
            let subtree_tips = cache.subtree_tips.entry(subtree_name.clone()).or_default();
            subtree_tips.clear();

            // Get all entries in this store
            let entries = backend.entries.read().await;
            let store_entries: Vec<&Entry> = entries
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
            drop(entries);
        }
    }

    Ok(())
}

/// Helper function to update tips for a given tree ID (async version)
async fn update_tips_for_tree_async(
    backend: &InMemory,
    tips_cache: &mut std::collections::HashMap<ID, super::TreeTipsCache>,
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
    let entries = backend.entries.read().await;
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
pub(crate) async fn is_tip(backend: &InMemory, tree: &ID, entry_id: &ID) -> bool {
    // Check if any other entry has this entry as its parent
    let entries = backend.entries.read().await;
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
pub(crate) async fn is_subtree_tip(
    backend: &InMemory,
    tree: &ID,
    subtree: &str,
    entry_id: &ID,
) -> bool {
    let entries = backend.entries.read().await;
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
pub(crate) async fn get_tree(backend: &InMemory, tree: &ID) -> Result<Vec<Entry>> {
    let entries = backend.entries.read().await;
    let mut tree_entries: Vec<Entry> = entries
        .values()
        .filter(|entry| entry.in_tree(tree))
        .cloned()
        .collect();

    drop(entries); // Release the lock before calling sort_entries_by_height

    // Sort by height
    super::cache::sort_entries_by_height(backend, tree, &mut tree_entries);
    Ok(tree_entries)
}

/// Retrieves all entries belonging to a specific subtree within a tree, sorted topologically.
pub(crate) async fn get_store(backend: &InMemory, tree: &ID, subtree: &str) -> Result<Vec<Entry>> {
    let entries = backend.entries.read().await;
    let mut subtree_entries: Vec<Entry> = entries
        .values()
        .filter(|entry| entry.in_tree(tree) && entry.in_subtree(subtree))
        .cloned()
        .collect();

    drop(entries); // Release the lock before calling sort_entries_by_subtree_height

    // Sort by subtree height
    super::cache::sort_entries_by_subtree_height(backend, tree, subtree, &mut subtree_entries);
    Ok(subtree_entries)
}

/// Retrieves all entries belonging to a specific tree up to the given tips, sorted topologically.
pub(crate) async fn get_tree_from_tips(
    backend: &InMemory,
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
    let entries = backend.entries.read().await;
    for tip in tips {
        if let Some(entry) = entries.get(tip) {
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

    // Sort the result by height
    super::cache::sort_entries_by_height(backend, tree, &mut result);

    Ok(result)
}

/// Retrieves all entries belonging to a specific subtree within a tree up to the given tips, sorted topologically.
pub(crate) async fn get_store_from_tips(
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
    let entries = backend.entries.read().await;
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
    super::cache::sort_entries_by_subtree_height(backend, tree, subtree, &mut result);

    Ok(result)
}
