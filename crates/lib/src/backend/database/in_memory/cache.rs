//! Height-based sorting and CRDT caching for InMemory database
//!
//! Heights are stored directly in entries, so sorting is trivial.
//! This module also handles CRDT state caching for improved performance.

use super::InMemory;
use crate::Result;
use crate::entry::{Entry, ID};

/// Sort entries by tree height, with ID as tiebreaker.
///
/// Heights are stored in each entry, so this just reads the embedded heights
/// and sorts accordingly.
pub(crate) fn sort_entries_by_height(_backend: &InMemory, _tree: &ID, entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        a.height()
            .cmp(&b.height())
            .then_with(|| a.id().cmp(&b.id()))
    });
}

/// Sort entries by subtree height, with ID as tiebreaker.
///
/// Heights are stored in each entry's subtree data, so this just reads the
/// embedded heights and sorts accordingly.
pub(crate) fn sort_entries_by_subtree_height(
    _backend: &InMemory,
    _tree: &ID,
    subtree: &str,
    entries: &mut [Entry],
) {
    entries.sort_by(|a, b| {
        let a_height = a.subtree_height(subtree).unwrap_or(0);
        let b_height = b.subtree_height(subtree).unwrap_or(0);
        a_height.cmp(&b_height).then_with(|| a.id().cmp(&b.id()))
    });
}

/// Creates a cache key for CRDT state from entry ID and subtree.
pub(crate) fn create_crdt_cache_key(entry_id: &ID, subtree: &str) -> String {
    let mut key = String::with_capacity(5 + entry_id.as_str().len() + 1 + subtree.len());
    key.push_str("crdt:");
    key.push_str(entry_id.as_str());
    key.push(':');
    key.push_str(subtree);
    key
}

/// Get cached CRDT state for a subtree at a specific entry.
pub(crate) async fn get_cached_crdt_state(
    backend: &InMemory,
    entry_id: &ID,
    subtree: &str,
) -> Result<Option<String>> {
    let key = create_crdt_cache_key(entry_id, subtree);
    let cache = backend.cache.read().await;
    Ok(cache.get(&key).cloned())
}

/// Cache CRDT state for a subtree at a specific entry.
pub(crate) async fn cache_crdt_state(
    backend: &InMemory,
    entry_id: &ID,
    subtree: &str,
    state: String,
) -> Result<()> {
    let key = create_crdt_cache_key(entry_id, subtree);
    let mut cache = backend.cache.write().await;
    cache.insert(key, state);
    Ok(())
}

/// Clear all cached CRDT states.
pub(crate) async fn clear_crdt_cache(backend: &InMemory) -> Result<()> {
    let mut cache = backend.cache.write().await;
    cache.clear();
    Ok(())
}
