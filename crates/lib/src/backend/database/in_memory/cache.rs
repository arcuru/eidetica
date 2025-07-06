//! Height calculation and CRDT caching for InMemory database
//!
//! This module handles computation and caching of tree/subtree heights,
//! as well as CRDT state caching for improved performance.

use super::InMemory;
use crate::Result;
use crate::backend::errors::DatabaseError;
use crate::entry::{Entry, ID};
use std::collections::{HashMap, HashSet, VecDeque};

/// Calculate the heights of all entries within a tree or subtree.
///
/// This function computes the height (longest path from root) for each entry
/// within the specified context using breadth-first search.
///
/// # Arguments
/// * `backend` - The InMemory database
/// * `tree` - The ID of the tree to calculate heights for
/// * `subtree` - Optional subtree name to limit calculation to a specific subtree
///
/// # Returns
/// A `Result` containing a `HashMap` mapping entry IDs to their heights.
pub(crate) fn calculate_heights(
    backend: &InMemory,
    tree: &ID,
    subtree: Option<&str>,
) -> Result<HashMap<ID, usize>> {
    match subtree {
        None => {
            // Tree height calculation with caching
            let heights_cache = backend.heights.read().unwrap();
            if let Some(tree_cache) = heights_cache.get(tree) {
                // Try to serve from cache
                let entries = backend.entries.read().unwrap();
                let tree_entries: Vec<_> = entries
                    .keys()
                    .filter(|&id| entries.get(id).is_some_and(|entry| entry.in_tree(tree)))
                    .cloned()
                    .collect();
                drop(entries);

                let mut result = HashMap::new();
                let mut all_cached = true;

                for id in tree_entries {
                    if let Some((height, _)) = tree_cache.get(&id) {
                        result.insert(id, *height);
                    } else {
                        all_cached = false;
                        break;
                    }
                }

                if all_cached {
                    return Ok(result);
                }
            }
            drop(heights_cache);

            // Compute heights and cache them
            let computed_heights = calculate_heights_original(backend, tree, None)?;

            // Update cache
            let mut heights_cache = backend.heights.write().unwrap();
            let tree_cache = heights_cache.entry(tree.clone()).or_default();

            // Update tree heights for each entry
            for (id, height) in &computed_heights {
                tree_cache
                    .entry(id.clone())
                    .and_modify(|(th, _)| *th = *height)
                    .or_insert((*height, HashMap::new()));
            }

            Ok(computed_heights)
        }
        Some(subtree_name) => {
            // Subtree height calculation with caching
            let heights_cache = backend.heights.read().unwrap();
            if let Some(tree_cache) = heights_cache.get(tree) {
                // Try to serve from cache
                let entries = backend.entries.read().unwrap();
                let subtree_entries: Vec<_> = entries
                    .keys()
                    .filter(|&id| {
                        entries.get(id).is_some_and(|entry| {
                            entry.in_tree(tree) && entry.in_subtree(subtree_name)
                        })
                    })
                    .cloned()
                    .collect();
                drop(entries);

                let mut result = HashMap::new();
                let mut all_cached = true;

                for id in subtree_entries {
                    if let Some((_, subtree_heights)) = tree_cache.get(&id) {
                        if let Some(height) = subtree_heights.get(subtree_name) {
                            result.insert(id, *height);
                        } else {
                            all_cached = false;
                            break;
                        }
                    } else {
                        all_cached = false;
                        break;
                    }
                }

                if all_cached {
                    return Ok(result);
                }
            }
            drop(heights_cache);

            // Compute heights and cache them
            let computed_heights = calculate_heights_original(backend, tree, Some(subtree_name))?;

            // Update cache
            let mut heights_cache = backend.heights.write().unwrap();
            let tree_cache = heights_cache.entry(tree.clone()).or_default();

            // Update subtree heights for each entry
            for (id, height) in &computed_heights {
                tree_cache
                    .entry(id.clone())
                    .and_modify(|(_, sh)| {
                        sh.insert(subtree_name.to_string(), *height);
                    })
                    .or_insert((0, [(subtree_name.to_string(), *height)].into()));
            }

            Ok(computed_heights)
        }
    }
}

/// Original height calculation implementation (fallback)
fn calculate_heights_original(
    backend: &InMemory,
    tree: &ID,
    subtree: Option<&str>,
) -> Result<HashMap<ID, usize>> {
    let mut heights: HashMap<ID, usize> = HashMap::new();
    let mut in_degree: HashMap<ID, usize> = HashMap::new();
    // Map: parent_id -> list of child_ids *within the context*
    let mut children_map: HashMap<ID, Vec<ID>> = HashMap::new();
    // Keep track of all nodes actually in the context
    let mut nodes_in_context: HashSet<ID> = HashSet::new();

    // 1. Build graph structure (children_map, in_degree) for the context
    let entries = backend.entries.read().unwrap();
    for (id, entry) in entries.iter() {
        // Check if entry is in the context (tree or tree+subtree)
        let in_context = match subtree {
            Some(subtree_name) => entry.in_tree(tree) && entry.in_subtree(subtree_name),
            None => entry.in_tree(tree),
        };
        if !in_context {
            continue;
        }

        nodes_in_context.insert(id.clone()); // Track node

        // Get the relevant parents for this context
        let parents = match subtree {
            Some(subtree_name) => entry.subtree_parents(subtree_name)?,
            None => entry.parents()?,
        };

        // Initialize in_degree for this node. It might be adjusted if parents are outside the context.
        in_degree.insert(id.clone(), parents.len());

        // Populate children_map and adjust in_degree based on parent context
        for parent_id in parents {
            // Check if the parent is ALSO in the context
            let parent_in_context = entries
                .get(&parent_id)
                .is_some_and(|p_entry| match subtree {
                    Some(subtree_name) => p_entry.in_tree(tree) && p_entry.in_subtree(subtree_name),
                    None => p_entry.in_tree(tree),
                });

            if parent_in_context {
                // Parent is in context, add edge to children_map
                children_map
                    .entry(parent_id.clone())
                    .or_default()
                    .push(id.clone());
            } else {
                // Parent is outside context, this edge doesn't count for in-degree *within* the context
                if let Some(d) = in_degree.get_mut(id) {
                    *d = d.saturating_sub(1);
                }
            }
        }
    }

    // 2. Initialize queue with root nodes (in-degree 0 within the context)
    let mut queue: VecDeque<ID> = VecDeque::new();
    for id in &nodes_in_context {
        // Initialize all heights to 0, roots will start the propagation
        heights.insert(id.clone(), 0);
        let degree = in_degree.get(id).cloned().unwrap_or(0); // Get degree for this node
        if degree == 0 {
            // Nodes with 0 in-degree *within the context* are the roots for this calculation
            queue.push_back(id.clone());
            // Height is already set to 0
        }
    }

    // 3. Process nodes using BFS (topological sort order)
    let mut processed_nodes_count = 0;
    while let Some(current_id) = queue.pop_front() {
        processed_nodes_count += 1;
        let current_height = *heights.get(&current_id).ok_or_else(|| {
            DatabaseError::HeightCalculationCorruption {
                reason: format!("Height missing for node {current_id}"),
            }
        })?;

        // Process children within the context
        if let Some(children) = children_map.get(&current_id) {
            for child_id in children {
                // Child must be in context (redundant check if children_map built correctly, but safe)
                if !nodes_in_context.contains(child_id) {
                    continue;
                }

                // Update child height: longest path = max(current paths)
                let new_height = current_height + 1;
                let child_current_height = heights.entry(child_id.clone()).or_insert(0); // Should exist, default 0
                *child_current_height = (*child_current_height).max(new_height);

                // Decrement in-degree and enqueue if it becomes 0
                if let Some(degree) = in_degree.get_mut(child_id) {
                    // Only decrement degree if it's > 0
                    if *degree > 0 {
                        *degree -= 1;
                        if *degree == 0 {
                            queue.push_back(child_id.clone());
                        }
                    } else {
                        // This indicates an issue: degree already 0 but node is being processed as child.
                        return Err(DatabaseError::HeightCalculationCorruption {
                            reason: format!("Negative in-degree detected for child {child_id}"),
                        }
                        .into());
                    }
                } else {
                    // This indicates an inconsistency: child_id was in children_map but not in_degree map
                    return Err(DatabaseError::HeightCalculationCorruption {
                        reason: format!("In-degree missing for child {child_id}"),
                    }
                    .into());
                }
            }
        }
    }

    // 4. Check for cycles (if not all nodes were processed) - Assumes DAG
    if processed_nodes_count != nodes_in_context.len() {
        panic!(
            "calculate_heights processed {} nodes, but found {} nodes in context. Potential cycle or disconnected graph portion detected.",
            processed_nodes_count,
            nodes_in_context.len()
        );
    }

    // Ensure the final map only contains heights for nodes within the specified context
    heights.retain(|id, _| nodes_in_context.contains(id));

    Ok(heights)
}

/// Sorts entries by their height (longest path from a root) within a tree.
///
/// Entries with lower height (closer to a root) appear before entries with higher height.
/// Entries with the same height are then sorted by their ID for determinism.
/// Entries without any parents (root nodes) have a height of 0 and appear first.
///
/// # Arguments
/// * `backend` - The InMemory database
/// * `tree` - The ID of the tree context.
/// * `entries` - The vector of entries to be sorted in place.
///
/// # Returns
/// A `Result` indicating success or an error if height calculation fails.
pub(crate) fn sort_entries_by_height(
    backend: &InMemory,
    tree: &ID,
    entries: &mut [Entry],
) -> Result<()> {
    let heights = calculate_heights(backend, tree, None)?;

    entries.sort_by(|a, b| {
        let a_height = *heights.get(&a.id()).unwrap_or(&0);
        let b_height = *heights.get(&b.id()).unwrap_or(&0);
        a_height.cmp(&b_height).then_with(|| a.id().cmp(&b.id()))
    });
    Ok(())
}

/// Sorts entries by their height within a specific subtree context.
///
/// Entries with lower height (closer to a root) appear before entries with higher height.
/// Entries with the same height are then sorted by their ID for determinism.
/// Entries without any subtree parents have a height of 0 and appear first.
///
/// # Arguments
/// * `backend` - The InMemory database
/// * `tree` - The ID of the tree context.
/// * `subtree` - The name of the subtree context.
/// * `entries` - The vector of entries to be sorted in place.
///
/// # Returns
/// A `Result` indicating success or an error if height calculation fails.
pub(crate) fn sort_entries_by_subtree_height(
    backend: &InMemory,
    tree: &ID,
    subtree: &str,
    entries: &mut [Entry],
) -> Result<()> {
    let heights = calculate_heights(backend, tree, Some(subtree))?;
    entries.sort_by(|a, b| {
        let a_height = *heights.get(&a.id()).unwrap_or(&0);
        let b_height = *heights.get(&b.id()).unwrap_or(&0);
        a_height.cmp(&b_height).then_with(|| a.id().cmp(&b.id()))
    });
    Ok(())
}

/// Creates a cache key for CRDT state from entry ID and subtree.
pub(crate) fn create_crdt_cache_key(entry_id: &ID, subtree: &str) -> String {
    format!("crdt:{entry_id}:{subtree}")
}

/// Get cached CRDT state for a subtree at a specific entry.
pub(crate) fn get_cached_crdt_state(
    backend: &InMemory,
    entry_id: &ID,
    subtree: &str,
) -> Result<Option<String>> {
    let key = create_crdt_cache_key(entry_id, subtree);
    let cache = backend.cache.read().unwrap();
    Ok(cache.get(&key).cloned())
}

/// Cache CRDT state for a subtree at a specific entry.
pub(crate) fn cache_crdt_state(
    backend: &InMemory,
    entry_id: &ID,
    subtree: &str,
    state: String,
) -> Result<()> {
    let key = create_crdt_cache_key(entry_id, subtree);
    let mut cache = backend.cache.write().unwrap();
    cache.insert(key, state);
    Ok(())
}

/// Clear all cached CRDT states.
pub(crate) fn clear_crdt_cache(backend: &InMemory) -> Result<()> {
    let mut cache = backend.cache.write().unwrap();
    cache.clear();
    Ok(())
}
