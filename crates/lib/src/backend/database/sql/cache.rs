//! Height calculation and caching for SQL backends.
//!
//! This module handles computation and storage of tree/store heights in the database.
//! Heights are immutable per entry once computed, so we persist them for efficient retrieval.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::Result;
use crate::backend::errors::BackendError;
use crate::entry::ID;

use super::SqlxBackend;

/// Calculate heights for all entries in a tree or store context.
///
/// This uses topological sort (Kahn's algorithm) to compute the longest path
/// from root for each entry.
///
/// # Arguments
/// * `backend` - The sqlx backend
/// * `tree_id` - The tree to calculate heights for
/// * `store` - Optional store name to limit calculation to a specific store
///
/// # Returns
/// A HashMap mapping entry IDs to their heights.
pub async fn calculate_heights(
    backend: &SqlxBackend,
    tree_id: &ID,
    store: Option<&str>,
) -> Result<HashMap<ID, usize>> {
    // First, check if we have cached heights
    let cached = get_cached_heights(backend, tree_id, store).await?;
    if !cached.is_empty() {
        // Verify we have heights for all entries in context
        let entry_count = count_entries_in_context(backend, tree_id, store).await?;
        if cached.len() == entry_count {
            return Ok(cached);
        }
    }

    // Compute heights using topological sort
    let heights = compute_heights_topological(backend, tree_id, store).await?;

    // Cache the computed heights
    cache_heights(backend, tree_id, store, &heights).await?;

    Ok(heights)
}

/// Get cached heights from the database.
async fn get_cached_heights(
    backend: &SqlxBackend,
    tree_id: &ID,
    store: Option<&str>,
) -> Result<HashMap<ID, usize>> {
    let pool = backend.pool();

    let store_name = store.unwrap_or("");

    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT entry_id, height FROM heights WHERE tree_id = $1 AND store_name = $2",
    )
    .bind(tree_id.to_string())
    .bind(store_name)
    .fetch_all(pool)
    .await
    .map_err(|e| BackendError::SqlxError {
        reason: format!("Failed to get cached heights: {e}"),
        source: Some(e),
    })?;

    let mut heights = HashMap::with_capacity(rows.len());
    for (entry_id, height) in rows {
        heights.insert(ID::from(entry_id), height as usize);
    }

    Ok(heights)
}

/// Count entries in the given context.
async fn count_entries_in_context(
    backend: &SqlxBackend,
    tree_id: &ID,
    store: Option<&str>,
) -> Result<usize> {
    let pool = backend.pool();

    let count: i64 = match store {
        Some(store_name) => {
            let row: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM entries e
                 JOIN store_memberships sm ON sm.entry_id = e.id
                 WHERE e.tree_id = $1 AND sm.store_name = $2",
            )
            .bind(tree_id.to_string())
            .bind(store_name)
            .fetch_one(pool)
            .await
            .map_err(|e| BackendError::SqlxError {
                reason: format!("Failed to count entries in store: {e}"),
                source: Some(e),
            })?;
            row.0
        }
        None => {
            let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM entries WHERE tree_id = $1")
                .bind(tree_id.to_string())
                .fetch_one(pool)
                .await
                .map_err(|e| BackendError::SqlxError {
                    reason: format!("Failed to count entries in tree: {e}"),
                    source: Some(e),
                })?;
            row.0
        }
    };

    Ok(count as usize)
}

/// Compute heights using topological sort (Kahn's algorithm).
async fn compute_heights_topological(
    backend: &SqlxBackend,
    tree_id: &ID,
    store: Option<&str>,
) -> Result<HashMap<ID, usize>> {
    // Build graph structure from database
    let (nodes, parent_map) = build_graph_from_db(backend, tree_id, store).await?;

    if nodes.is_empty() {
        return Ok(HashMap::new());
    }

    // Build in-degree map and children map
    let mut in_degree: HashMap<ID, usize> = HashMap::new();
    let mut children_map: HashMap<ID, Vec<ID>> = HashMap::new();

    for node_id in &nodes {
        in_degree.insert(node_id.clone(), 0);
    }

    for (child_id, parents) in &parent_map {
        // Only count parents that are in our context
        let valid_parent_count = parents.iter().filter(|p| nodes.contains(*p)).count();
        in_degree.insert(child_id.clone(), valid_parent_count);

        for parent_id in parents {
            if nodes.contains(parent_id) {
                children_map
                    .entry(parent_id.clone())
                    .or_default()
                    .push(child_id.clone());
            }
        }
    }

    // Initialize heights and queue with roots (in-degree 0)
    let mut heights: HashMap<ID, usize> = HashMap::new();
    let mut queue: VecDeque<ID> = VecDeque::new();

    for node_id in &nodes {
        heights.insert(node_id.clone(), 0);
        if *in_degree.get(node_id).unwrap_or(&0) == 0 {
            queue.push_back(node_id.clone());
        }
    }

    // Process nodes in topological order
    let mut processed = 0;
    while let Some(current_id) = queue.pop_front() {
        processed += 1;
        let current_height = *heights.get(&current_id).unwrap_or(&0);

        if let Some(children) = children_map.get(&current_id) {
            for child_id in children {
                // Update child height (longest path)
                let new_height = current_height + 1;
                let child_height = heights.entry(child_id.clone()).or_insert(0);
                *child_height = (*child_height).max(new_height);

                // Decrement in-degree
                if let Some(degree) = in_degree.get_mut(child_id) {
                    *degree = degree.saturating_sub(1);
                    if *degree == 0 {
                        queue.push_back(child_id.clone());
                    }
                }
            }
        }
    }

    // Check for cycles
    if processed != nodes.len() {
        return Err(BackendError::HeightCalculationCorruption {
            reason: format!(
                "Processed {} nodes but {} in context - possible cycle",
                processed,
                nodes.len()
            ),
        }
        .into());
    }

    Ok(heights)
}

/// Graph data structure: nodes and their parent relationships
type GraphData = (HashSet<ID>, HashMap<ID, Vec<ID>>);

/// Build graph structure from database queries.
async fn build_graph_from_db(
    backend: &SqlxBackend,
    tree_id: &ID,
    store: Option<&str>,
) -> Result<GraphData> {
    let pool = backend.pool();
    let mut nodes = HashSet::new();
    let mut parent_map: HashMap<ID, Vec<ID>> = HashMap::new();

    match store {
        Some(store_name) => {
            // Get entries in this store
            let entry_rows: Vec<(String,)> = sqlx::query_as(
                "SELECT e.id FROM entries e
                 JOIN store_memberships sm ON sm.entry_id = e.id
                 WHERE e.tree_id = $1 AND sm.store_name = $2",
            )
            .bind(tree_id.to_string())
            .bind(store_name)
            .fetch_all(pool)
            .await
            .map_err(|e| BackendError::SqlxError {
                reason: format!("Failed to get entries in store: {e}"),
                source: Some(e),
            })?;

            for (entry_id,) in entry_rows {
                let entry_id = ID::from(entry_id);
                nodes.insert(entry_id.clone());

                // Get store parents for this entry
                let parent_rows: Vec<(String,)> = sqlx::query_as(
                    "SELECT parent_id FROM store_parents WHERE child_id = $1 AND store_name = $2",
                )
                .bind(entry_id.to_string())
                .bind(store_name)
                .fetch_all(pool)
                .await
                .map_err(|e| BackendError::SqlxError {
                    reason: format!("Failed to get store parents: {e}"),
                    source: Some(e),
                })?;

                let parents: Vec<ID> = parent_rows.into_iter().map(|(id,)| ID::from(id)).collect();

                parent_map.insert(entry_id, parents);
            }
        }
        None => {
            // Get all entries in tree
            let entry_rows: Vec<(String,)> =
                sqlx::query_as("SELECT id FROM entries WHERE tree_id = $1")
                    .bind(tree_id.to_string())
                    .fetch_all(pool)
                    .await
                    .map_err(|e| BackendError::SqlxError {
                        reason: format!("Failed to get entries in tree: {e}"),
                        source: Some(e),
                    })?;

            for (entry_id,) in entry_rows {
                let entry_id = ID::from(entry_id);
                nodes.insert(entry_id.clone());

                // Get tree parents for this entry
                let parent_rows: Vec<(String,)> =
                    sqlx::query_as("SELECT parent_id FROM tree_parents WHERE child_id = $1")
                        .bind(entry_id.to_string())
                        .fetch_all(pool)
                        .await
                        .map_err(|e| BackendError::SqlxError {
                            reason: format!("Failed to get tree parents: {e}"),
                            source: Some(e),
                        })?;

                let parents: Vec<ID> = parent_rows.into_iter().map(|(id,)| ID::from(id)).collect();

                parent_map.insert(entry_id, parents);
            }
        }
    }

    Ok((nodes, parent_map))
}

/// Cache computed heights to the database.
async fn cache_heights(
    backend: &SqlxBackend,
    tree_id: &ID,
    store: Option<&str>,
    heights: &HashMap<ID, usize>,
) -> Result<()> {
    let pool = backend.pool();
    let store_name = store.unwrap_or("");

    for (entry_id, height) in heights {
        if backend.is_sqlite() {
            sqlx::query(
                "INSERT OR REPLACE INTO heights (entry_id, tree_id, store_name, height)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(entry_id.to_string())
            .bind(tree_id.to_string())
            .bind(store_name)
            .bind(*height as i64)
            .execute(pool)
            .await
            .map_err(|e| BackendError::SqlxError {
                reason: format!("Failed to cache height: {e}"),
                source: Some(e),
            })?;
        } else {
            sqlx::query(
                "INSERT INTO heights (entry_id, tree_id, store_name, height)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (entry_id, tree_id, store_name) DO UPDATE SET height = EXCLUDED.height",
            )
            .bind(entry_id.to_string())
            .bind(tree_id.to_string())
            .bind(store_name)
            .bind(*height as i64)
            .execute(pool)
            .await
            .map_err(|e| BackendError::SqlxError {
                reason: format!("Failed to cache height: {e}"),
                source: Some(e),
            })?;
        }
    }

    Ok(())
}

/// Get the height of a specific entry.
///
/// This will compute and cache heights if not already cached.
#[allow(dead_code)]
pub async fn get_entry_height(
    backend: &SqlxBackend,
    tree_id: &ID,
    entry_id: &ID,
    store: Option<&str>,
) -> Result<usize> {
    let pool = backend.pool();
    let store_name = store.unwrap_or("");

    // Try to get from cache first
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT height FROM heights WHERE entry_id = $1 AND tree_id = $2 AND store_name = $3",
    )
    .bind(entry_id.to_string())
    .bind(tree_id.to_string())
    .bind(store_name)
    .fetch_optional(pool)
    .await
    .map_err(|e| BackendError::SqlxError {
        reason: format!("Failed to get entry height: {e}"),
        source: Some(e),
    })?;

    if let Some((height,)) = row {
        return Ok(height as usize);
    }

    // Not cached, compute all heights for this context
    let heights = calculate_heights(backend, tree_id, store).await?;
    Ok(*heights.get(entry_id).unwrap_or(&0))
}

/// Sort entries by height, with ID as tiebreaker.
pub async fn sort_entries_by_height(
    backend: &SqlxBackend,
    tree_id: &ID,
    store: Option<&str>,
    entries: &mut [crate::entry::Entry],
) -> Result<()> {
    let heights = calculate_heights(backend, tree_id, store).await?;

    entries.sort_by(|a, b| {
        let a_height = *heights.get(&a.id()).unwrap_or(&0);
        let b_height = *heights.get(&b.id()).unwrap_or(&0);
        a_height.cmp(&b_height).then_with(|| a.id().cmp(&b.id()))
    });

    Ok(())
}
