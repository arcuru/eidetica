//! Core storage operations for InMemory database

use std::collections::{HashMap, HashSet, VecDeque};

use super::{InMemoryInner, VerifiedState};
use crate::{
    Result,
    backend::{VerificationStatus, errors::BackendError},
    entry::{Entry, ID},
    snapshot::Snapshot,
};

use crate::backend::database::sorting;

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
/// A *new* entry is stored as [`VerificationStatus::Unverified`]; the storage
/// path never accepts a caller-chosen status. Promotion to `Verified` is done
/// separately by the local validation pass via `update_verification_status`.
/// An entry already held is left untouched (content and status): a re-`put`
/// never demotes a prior local promotion.
///
/// # Arguments
/// * `inner` - Mutable reference to the core data
/// * `entry` - The entry to store (must already be validated)
///
/// # Returns
/// * `Ok(())` on successful storage
pub(crate) fn put(inner: &mut InMemoryInner, entry: Entry) -> Result<()> {
    let entry_id = entry.id();

    // Content-addressed and immutable: if we already hold this entry, a
    // re-`put` is a no-op. We do NOT reset `verification_status` —
    // re-receiving an entry on overlapping/bootstrap sync must not demote a
    // prior local `Verified` promotion. Status is owned by the local
    // validation pass. Tips already account for this entry, so skipping the
    // recalculation below is correct (mirrors the SQL backend).
    if inner.entries.contains_key(&entry_id) {
        return Ok(());
    }

    // For root entries, root() returns None, so tree_id is the entry's own ID.
    // For non-root entries, tree_id is the root entry's ID.
    let tree_id = entry.root().unwrap_or_else(|| entry_id.clone());

    // SPECIAL CASE: For root entries (entry.root() is None), we also need to update
    // tips for the tree whose ID is the entry's ID itself, since the root entry
    // becomes the root of a new tree.
    let additional_tree_id = if entry.is_root() {
        Some(entry_id.clone())
    } else {
        None
    };

    // New entry: store it Unverified. (An already-held entry returned early
    // above; its status is never touched by a re-`put`.)
    inner.entries.insert(entry_id.clone(), entry.clone());
    inner
        .verification_status
        .insert(entry_id.clone(), VerificationStatus::Unverified);

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
            .filter(|e| e.in_tree(&tree_id) && e.subtrees().contains(&subtree_name))
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

/// Updates the verification status of an entry, maintaining the retained
/// verified prefix/frontier for its tree.
///
/// Supported transitions are monotonic (`Unverified -> Verified`,
/// `Unverified -> Failed`) plus idempotent rewrites. A promotion is admitted
/// incrementally and recursively activates already-`Verified` descendants
/// that were waiting on it, so child-before-parent promotion converges
/// without a read-time scan. Any demotion out of `Verified` funnels into a
/// per-tree [`rebuild_verified_state`]: removing a prefix member can
/// re-expose an arbitrarily large subgraph, which the incremental path
/// cannot cheaply recompute.
pub(crate) fn update_verification_status(
    inner: &mut InMemoryInner,
    id: &ID,
    status: VerificationStatus,
) -> Result<()> {
    let entry = inner
        .entries
        .get(id)
        .cloned()
        .ok_or_else(|| BackendError::EntryNotFound { id: id.clone() })?;
    let current = inner
        .verification_status
        .get(id)
        .copied()
        .unwrap_or(VerificationStatus::Unverified);
    if current == status {
        return Ok(());
    }
    inner.verification_status.insert(id.clone(), status);

    if status == VerificationStatus::Verified {
        admit_cascade(inner, id);
    } else if current == VerificationStatus::Verified {
        let tree = entry.root().unwrap_or_else(|| id.clone());
        rebuild_verified_state(inner, &tree)?;
    }
    Ok(())
}

/// Admit `start` into the verified prefix if eligible, then recursively admit
/// already-`Verified` descendants that were waiting on it.
///
/// An entry enters the prefix only when its own status is `Verified` and
/// every parent is already in the prefix (a root has no parents and is
/// immediately eligible). A promoted-before-its-parent entry simply waits:
/// promoting the last missing parent enqueues the children and activates it.
fn admit_cascade(inner: &mut InMemoryInner, start: &ID) {
    let mut queue: VecDeque<ID> = VecDeque::from([start.clone()]);
    while let Some(candidate) = queue.pop_front() {
        let tree = tree_of(inner, &candidate);
        {
            let state = inner.verified.entry(tree).or_default();
            if state.prefix.contains(&candidate) {
                continue;
            }
            if inner
                .verification_status
                .get(&candidate)
                .copied()
                .unwrap_or(VerificationStatus::Unverified)
                != VerificationStatus::Verified
            {
                continue;
            }
            let parents = inner
                .entries
                .get(&candidate)
                .map(|e| e.parents().unwrap_or_default())
                .unwrap_or_default();
            if !parents.iter().all(|p| state.prefix.contains(p)) {
                continue;
            }
            state.prefix.insert(candidate.clone());
            for p in &parents {
                state.frontier.remove(p);
            }
            state.frontier.insert(candidate.clone());
        }
        for child in children_of(inner, &candidate) {
            queue.push_back(child);
        }
    }
}

/// Tree owning `id`: the entry's declared root, or itself for roots/missing.
fn tree_of(inner: &InMemoryInner, id: &ID) -> ID {
    inner
        .entries
        .get(id)
        .and_then(|e| e.root())
        .unwrap_or_else(|| id.clone())
}

/// Every held entry listing `parent` as a tree parent.
fn children_of(inner: &InMemoryInner, parent: &ID) -> Vec<ID> {
    inner
        .entries
        .values()
        .filter(|e| e.parents().unwrap_or_default().contains(parent))
        .map(|e| e.id())
        .collect()
}

/// Returns the retained verified frontier of `tree` without scanning history.
///
/// Unknown trees (or trees whose root is not `Verified`) report empty,
/// matching the reconstruction oracle.
pub(crate) fn verified_snapshot(inner: &InMemoryInner, tree: &ID) -> Snapshot {
    inner
        .verified
        .get(tree)
        .map(|state| Snapshot::new(state.frontier.iter().cloned().collect()))
        .unwrap_or(Snapshot::EMPTY)
}

/// Rebuild one tree's verified prefix/frontier from entries and statuses in
/// deterministic topological order, replacing the retained state.
///
/// This is the migration/repair/demotion path and the oracle the incremental
/// promotion path must equal after every insertion/promotion order.
pub(crate) fn rebuild_verified_state(inner: &mut InMemoryInner, tree: &ID) -> Result<Snapshot> {
    // Kahn's topological order, not stored-height order: heights are
    // commit-time metadata and cannot be trusted on a rebuild path that
    // must handle whatever history holds. Ready set deterministic in
    // height-then-ID. In-degree counts only edges inside the held set.
    let held: Vec<Entry> = inner
        .entries
        .values()
        .filter(|e| e.in_tree(tree))
        .cloned()
        .collect();
    let held_ids: HashSet<ID> = held.iter().map(|e| e.id()).collect();
    let mut parents_of: HashMap<ID, Vec<ID>> = HashMap::with_capacity(held.len());
    let mut children_of: HashMap<ID, Vec<ID>> = HashMap::with_capacity(held.len());
    let mut in_degree: HashMap<ID, usize> = HashMap::with_capacity(held.len());
    let mut height_of: HashMap<ID, u64> = HashMap::with_capacity(held.len());
    for e in &held {
        let id = e.id();
        height_of.insert(id.clone(), e.height());
        let parents = e.parents().unwrap_or_default();
        let mut degree = 0usize;
        for p in &parents {
            children_of.entry(p.clone()).or_default().push(id.clone());
            if held_ids.contains(p) {
                degree += 1;
            }
        }
        parents_of.insert(id.clone(), parents);
        in_degree.insert(id, degree);
    }
    let mut ready: std::collections::BTreeSet<(u64, ID)> = in_degree
        .iter()
        .filter(|&(_, &d)| d == 0)
        .map(|(id, _)| (height_of[id], id.clone()))
        .collect();

    let mut rebuilt = VerifiedState::default();
    while let Some((_, id)) = ready.pop_first() {
        if let Some(kids) = children_of.get(&id) {
            for kid in kids.clone() {
                let d = in_degree.get_mut(&kid).expect("in_degree entry exists");
                *d -= 1;
                if *d == 0 {
                    ready.insert((height_of[&kid], kid));
                }
            }
        }
        if inner
            .verification_status
            .get(&id)
            .copied()
            .unwrap_or(VerificationStatus::Unverified)
            != VerificationStatus::Verified
        {
            continue;
        }
        let empty = Vec::new();
        let parents = parents_of.get(&id).unwrap_or(&empty);
        if parents.iter().all(|p| rebuilt.prefix.contains(p)) {
            rebuilt.prefix.insert(id.clone());
            for p in parents {
                rebuilt.frontier.remove(p);
            }
            rebuilt.frontier.insert(id);
        }
    }
    let snapshot = Snapshot::new(rebuilt.frontier.iter().cloned().collect());
    inner.verified.insert(tree.clone(), rebuilt);
    Ok(snapshot)
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
        .filter(|e| e.in_tree(target_tree_id))
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
        if other_entry.in_tree(tree) && other_entry.parents().unwrap_or_default().contains(entry_id)
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
        if other_entry.in_tree(tree)
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

    sorting::sort_entries_by_height(&mut tree_entries);
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

    sorting::sort_entries_by_store_height(subtree, &mut subtree_entries);
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
    sorting::sort_entries_by_height(&mut result);

    Ok(result)
}

/// Retrieves all entries belonging to a specific subtree within a tree up to the given tips, sorted topologically.
pub(crate) fn store_at(
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
    sorting::sort_entries_by_store_height(subtree, &mut result);

    Ok(result)
}
