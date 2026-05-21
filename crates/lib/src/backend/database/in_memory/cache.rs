//! Height-based sorting and CRDT caching for InMemory database
//!
//! Heights are stored directly in entries, so sorting is trivial.
//! This module also handles CRDT state caching with byte-bounded LRU
//! eviction, scoped by [`CacheScope`] so daemon-trusted bytes (Shared)
//! and client-uploaded bytes (User) coexist in one storage substrate.

use lru::LruCache;

use super::InMemory;
use crate::Result;
use crate::backend::CacheScope;
use crate::entry::{Entry, ID};

/// Default cap on total cached blob bytes for the in-memory backend.
/// Covers both `Shared` and `User`-scoped entries — they share the budget.
pub(crate) const DEFAULT_CAPACITY_BYTES: usize = 256 * 1024 * 1024;

type CacheKey = (CacheScope, ID, String);

/// Bytes-bounded LRU cache for CRDT state, shared across all scopes.
#[derive(Debug)]
pub(crate) struct InMemoryCrdtCache {
    lru: LruCache<CacheKey, Vec<u8>>,
    current_bytes: usize,
    capacity_bytes: usize,
}

impl InMemoryCrdtCache {
    pub fn new() -> Self {
        Self::with_capacity_bytes(DEFAULT_CAPACITY_BYTES)
    }

    pub fn with_capacity_bytes(capacity_bytes: usize) -> Self {
        Self {
            lru: LruCache::unbounded(),
            current_bytes: 0,
            capacity_bytes,
        }
    }

    pub fn get(&mut self, scope: &CacheScope, entry_id: &ID, subtree: &str) -> Option<Vec<u8>> {
        // `LruCache::get` promotes the entry to most-recently-used.
        self.lru
            .get(&(scope.clone(), entry_id.clone(), subtree.to_string()))
            .cloned()
    }

    pub fn put(&mut self, scope: CacheScope, entry_id: ID, subtree: String, state: Vec<u8>) {
        let blob_size = state.len();
        let cache_key: CacheKey = (scope, entry_id, subtree);
        if let Some(prev) = self.lru.put(cache_key.clone(), state) {
            self.current_bytes = self.current_bytes.saturating_sub(prev.len());
        }
        self.current_bytes = self.current_bytes.saturating_add(blob_size);
        // Evict LRU entries until under cap. Soft cap: a single oversized
        // blob is kept rather than thrashing.
        while self.current_bytes > self.capacity_bytes {
            let Some((k, v)) = self.lru.pop_lru() else {
                break;
            };
            if k == cache_key {
                self.lru.put(k, v);
                break;
            }
            self.current_bytes = self.current_bytes.saturating_sub(v.len());
        }
    }

    pub fn clear(&mut self) {
        self.lru.clear();
        self.current_bytes = 0;
    }

    #[cfg(test)]
    pub fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lru.len()
    }
}

impl Default for InMemoryCrdtCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Sort entries by tree height, with ID as tiebreaker.
///
/// Heights are stored in each entry, so this just reads the embedded heights
/// and sorts accordingly.
pub(crate) fn sort_entries_by_height(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        a.height()
            .cmp(&b.height())
            .then_with(|| a.id().cmp(&b.id()))
    });
}

/// Sort entries by subtree height, with ID as tiebreaker.
pub(crate) fn sort_entries_by_subtree_height(subtree: &str, entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        let a_height = a.subtree_height(subtree).unwrap_or(0);
        let b_height = b.subtree_height(subtree).unwrap_or(0);
        a_height.cmp(&b_height).then_with(|| a.id().cmp(&b.id()))
    });
}

/// Get cached CRDT state for a subtree at a specific entry within a scope.
pub(crate) fn get_cached_crdt_state(
    backend: &InMemory,
    scope: &CacheScope,
    entry_id: &ID,
    subtree: &str,
) -> Result<Option<Vec<u8>>> {
    let mut cache = backend.crdt_cache.lock().unwrap();
    Ok(cache.get(scope, entry_id, subtree))
}

/// Cache CRDT state for a subtree at a specific entry within a scope.
pub(crate) fn cache_crdt_state(
    backend: &InMemory,
    scope: CacheScope,
    entry_id: &ID,
    subtree: &str,
    state: Vec<u8>,
) -> Result<()> {
    let mut cache = backend.crdt_cache.lock().unwrap();
    cache.put(scope, entry_id.clone(), subtree.to_string(), state);
    Ok(())
}

/// Clear all cached CRDT states across every scope.
pub(crate) fn clear_crdt_cache(backend: &InMemory) -> Result<()> {
    let mut cache = backend.crdt_cache.lock().unwrap();
    cache.clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eid(s: &str) -> ID {
        ID::from_bytes(s)
    }

    #[test]
    fn shared_and_user_scopes_are_isolated() {
        let mut c = InMemoryCrdtCache::new();
        let k = eid("e1");
        c.put(CacheScope::Shared, k.clone(), "s".into(), b"shared".to_vec());
        c.put(
            CacheScope::User("alice".into()),
            k.clone(),
            "s".into(),
            b"alice-only".to_vec(),
        );
        assert_eq!(
            c.get(&CacheScope::Shared, &k, "s"),
            Some(b"shared".to_vec()),
            "Shared slot must not be clobbered by a User write at the same entry+store"
        );
        assert_eq!(
            c.get(&CacheScope::User("alice".into()), &k, "s"),
            Some(b"alice-only".to_vec())
        );
        assert!(
            c.get(&CacheScope::User("bob".into()), &k, "s").is_none(),
            "bob's slot must be empty even when alice and Shared both have entries"
        );
    }

    #[test]
    fn lru_evicts_under_byte_pressure() {
        let mut c = InMemoryCrdtCache::with_capacity_bytes(100);
        c.put(CacheScope::Shared, eid("e1"), "s".into(), vec![1u8; 50]);
        c.put(CacheScope::Shared, eid("e2"), "s".into(), vec![2u8; 50]);
        assert_eq!(c.current_bytes(), 100);
        c.put(CacheScope::Shared, eid("e3"), "s".into(), vec![3u8; 50]);
        // e1 (LRU) evicted.
        assert!(c.get(&CacheScope::Shared, &eid("e1"), "s").is_none());
        assert!(c.get(&CacheScope::Shared, &eid("e2"), "s").is_some());
        assert!(c.get(&CacheScope::Shared, &eid("e3"), "s").is_some());
        assert!(c.current_bytes() <= 100);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn get_promotes_to_most_recent() {
        let mut c = InMemoryCrdtCache::with_capacity_bytes(100);
        c.put(CacheScope::Shared, eid("e1"), "s".into(), vec![1u8; 50]);
        c.put(CacheScope::Shared, eid("e2"), "s".into(), vec![2u8; 50]);
        // Promote e1 → e2 is now LRU.
        let _ = c.get(&CacheScope::Shared, &eid("e1"), "s");
        c.put(CacheScope::Shared, eid("e3"), "s".into(), vec![3u8; 50]);
        assert!(c.get(&CacheScope::Shared, &eid("e1"), "s").is_some());
        assert!(c.get(&CacheScope::Shared, &eid("e2"), "s").is_none());
    }

    #[test]
    fn replace_in_place_keeps_byte_accounting() {
        let mut c = InMemoryCrdtCache::with_capacity_bytes(1024);
        c.put(CacheScope::Shared, eid("e1"), "s".into(), b"v1".to_vec());
        c.put(
            CacheScope::Shared,
            eid("e1"),
            "s".into(),
            b"v2-different-len".to_vec(),
        );
        assert_eq!(c.current_bytes(), b"v2-different-len".len());
        assert_eq!(
            c.get(&CacheScope::Shared, &eid("e1"), "s"),
            Some(b"v2-different-len".to_vec())
        );
    }

    #[test]
    fn clear_drops_every_scope() {
        let mut c = InMemoryCrdtCache::new();
        c.put(CacheScope::Shared, eid("e1"), "s".into(), b"a".to_vec());
        c.put(
            CacheScope::User("alice".into()),
            eid("e1"),
            "s".into(),
            b"b".to_vec(),
        );
        c.clear();
        assert_eq!(c.len(), 0);
        assert_eq!(c.current_bytes(), 0);
        assert!(c.get(&CacheScope::Shared, &eid("e1"), "s").is_none());
    }
}

