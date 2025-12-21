//! Sync-specific utility functions.
//!
//! This module contains DAG traversal algorithms and other utilities
//! specific to the synchronization protocol.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::{
    Result,
    backend::BackendImpl,
    entry::{Entry, ID},
    sync::error::SyncError,
};

/// Collect all missing ancestors for given entry IDs using DAG traversal.
///
/// This function performs a breadth-first traversal of the DAG to find
/// all entries that are referenced but not present locally. It's used
/// when receiving tips from a peer to determine what entries need to be fetched.
///
/// # Algorithm Complexity
/// * **Time**: O(V + E) where V is the number of visited entries and E is the number of parent relationships
/// * **Space**: O(V) for the visited set and queue storage
///
/// # Arguments
/// * `backend` - Database backend to check for entry existence
/// * `entry_ids` - Starting entry IDs to traverse from
///
/// # Returns
/// Vec of entry IDs that are missing from the local database
///
/// # Example
/// ```rust,ignore
/// use eidetica::sync::utils::collect_missing_ancestors;
/// use eidetica::backend::database::InMemory;
///
/// let backend = InMemory::new();
/// let missing_ids = vec![ID::from("tip1"), ID::from("tip2")];
/// let missing = collect_missing_ancestors(&backend, &missing_ids).await?;
/// // Returns IDs of entries that need to be fetched from peer
/// ```
pub async fn collect_missing_ancestors(backend: &dyn BackendImpl, entry_ids: &[ID]) -> Result<Vec<ID>> {
    let mut missing = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    // Start with the given entry IDs
    for id in entry_ids {
        queue.push_back(id.clone());
    }

    while let Some(entry_id) = queue.pop_front() {
        if visited.contains(&entry_id) {
            continue;
        }
        visited.insert(entry_id.clone());

        match backend.get(&entry_id).await {
            Ok(entry) => {
                // We have this entry, but we need to check its parents
                if let Ok(parents) = entry.parents() {
                    for parent_id in parents {
                        if !visited.contains(&parent_id) {
                            queue.push_back(parent_id);
                        }
                    }
                }
            }
            Err(e) if e.is_not_found() => {
                // We don't have this entry - mark as missing
                missing.push(entry_id);
                // Note: We can't traverse parents of missing entries
                // The peer will need to send parent info separately
            }
            Err(e) => {
                return Err(SyncError::BackendError(format!(
                    "Failed to check for entry {entry_id}: {e}"
                ))
                .into());
            }
        }
    }

    Ok(missing)
}

/// Collect ancestors that need to be sent with the given entries.
///
/// This function performs DAG traversal to find all entries that need to be
/// sent along with the given entry IDs, excluding entries that the peer
/// already has (based on their tips). The algorithm ensures that all necessary
/// parent entries are included for proper DAG reconstruction on the peer.
///
/// # Algorithm Complexity
/// * **Time**: O(V + E) where V is the number of visited entries and E is the number of parent relationships
/// * **Space**: O(V) for the visited set, queue storage, and result entries
///
/// # Arguments
/// * `backend` - Database backend to retrieve entries
/// * `entry_ids` - Starting entry IDs to collect ancestors for
/// * `their_tips` - Entry IDs that the peer already has
///
/// # Returns
/// Vec of entries that need to be sent (including ancestors)
///
/// # Example
/// ```rust,ignore
/// use eidetica::sync::utils::collect_ancestors_to_send;
/// use eidetica::backend::database::InMemory;
///
/// let backend = InMemory::new();
/// let our_tips = vec![ID::from("tip1"), ID::from("tip2")];
/// let their_tips = vec![ID::from("common_ancestor")];
/// let to_send = collect_ancestors_to_send(&backend, &our_tips, &their_tips).await?;
/// // Returns entries that peer needs, excluding what they already have
/// ```
pub async fn collect_ancestors_to_send(
    backend: &dyn BackendImpl,
    entry_ids: &[ID],
    their_tips: &[ID],
) -> Result<Vec<Entry>> {
    let mut entries_to_send = HashMap::new();
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let their_tips_set: HashSet<&ID> = their_tips.iter().collect();

    // Start with the given entry IDs
    for id in entry_ids {
        queue.push_back(id.clone());
    }

    while let Some(entry_id) = queue.pop_front() {
        if visited.contains(&entry_id) || their_tips_set.contains(&entry_id) {
            continue; // Skip already visited or entries peer already has
        }
        visited.insert(entry_id.clone());

        match backend.get(&entry_id).await {
            Ok(entry) => {
                entries_to_send.insert(entry_id.clone(), entry.clone());

                // Add parents to queue if peer might not have them
                if let Ok(parents) = entry.parents() {
                    for parent_id in parents {
                        if !their_tips_set.contains(&parent_id) && !visited.contains(&parent_id) {
                            queue.push_back(parent_id);
                        }
                    }
                }
            }
            Err(e) => {
                return Err(SyncError::BackendError(format!(
                    "Failed to get entry {entry_id} to send: {e}"
                ))
                .into());
            }
        }
    }

    // Return entries without height sorting for now
    // Height-based ordering should be done by the sync_tree_with_peer method
    // when it has the tree context needed for height calculation
    let entries: Vec<Entry> = entries_to_send.into_values().collect();

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{Entry, backend::database::InMemory};

    fn create_test_backend() -> Arc<InMemory> {
        Arc::new(InMemory::new())
    }

    #[tokio::test]
    async fn test_collect_missing_ancestors_empty() {
        let backend = create_test_backend();
        let result = collect_missing_ancestors(backend.as_ref(), &[]).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_collect_missing_ancestors_not_found() {
        let backend = create_test_backend();
        let missing_id = ID::from("missing123");

        let result =
            collect_missing_ancestors(backend.as_ref(), std::slice::from_ref(&missing_id)).await.unwrap();
        assert_eq!(result, vec![missing_id]);
    }

    #[tokio::test]
    async fn test_collect_missing_ancestors_present() {
        let backend = create_test_backend();
        let entry = Entry::root_builder()
            .build()
            .expect("Root entry should build successfully");
        let entry_id = entry.id();

        // Store the entry
        backend.put_verified(entry).await.unwrap();

        let result = collect_missing_ancestors(backend.as_ref(), &[entry_id]).await.unwrap();
        assert!(result.is_empty()); // Entry exists, so nothing missing
    }

    #[tokio::test]
    async fn test_collect_ancestors_to_send_empty() {
        let backend = create_test_backend();
        let result = collect_ancestors_to_send(backend.as_ref(), &[], &[]).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_collect_ancestors_to_send_single_entry() {
        let backend = create_test_backend();
        let entry = Entry::root_builder()
            .build()
            .expect("Root entry should build successfully");
        let entry_id = entry.id();

        // Store the entry
        backend.put_verified(entry.clone()).await.unwrap();

        let result = collect_ancestors_to_send(backend.as_ref(), &[entry_id], &[]).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id(), entry.id());
    }

    #[tokio::test]
    async fn test_collect_ancestors_to_send_peer_already_has() {
        let backend = create_test_backend();
        let entry = Entry::root_builder()
            .build()
            .expect("Root entry should build successfully");
        let entry_id = entry.id();

        // Store the entry
        backend.put_verified(entry).await.unwrap();

        // Peer already has this entry
        let result = collect_ancestors_to_send(
            backend.as_ref(),
            std::slice::from_ref(&entry_id),
            std::slice::from_ref(&entry_id),
        )
        .await
        .unwrap();
        assert!(result.is_empty());
    }
}
