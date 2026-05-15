//! Per-user CRDT-state cache for the service daemon.
//!
//! Service-mode clients each see their own slice of the cache, namespaced by
//! the session's `user_uuid` (set when a `TrustedLogin*` exchange completes).
//! Identical bytes uploaded by different users are stored once and
//! reference-counted — a write that matches an existing blob just bumps the
//! refcount. This bounds memory growth while preventing the cross-user
//! poisoning vector where one user's `CacheCrdtState` would be visible to
//! another user's `GetCachedCrdtState` against the same `(entry_id, store)`.
//!
//! ## Scope
//!
//! This cache lives entirely in the service layer and is daemon-local in
//! memory. Local (non-service) flows continue to use the backend's own
//! `cache_crdt_state` machinery, which has no per-user scoping (the local
//! caller is the device, not a wire client). The two caches are independent
//! by design: a service client's `GetCachedCrdtState` will not see anything
//! written by the daemon's own background work, and vice versa. Process
//! restart clears this cache — it is performance state, not durable storage.
//!
//! ## Information disclosure
//!
//! Content-addressable storage with refcounting means an attacker who can
//! upload arbitrary cache values can probe for existence of a known value
//! (their upload either dedupes against an existing blob or stands alone).
//! The wire surface doesn't expose dedup status, so this reduces to "guess
//! the exact ciphertext / plaintext and observe nothing different." For
//! AES-GCM-encrypted store cache state this is intractable (nonces are
//! random); for unencrypted store cache state the attacker would still need
//! to guess the exact serialized CRDT state. We accept this residual leak
//! in exchange for bounded memory growth.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Mutex;

use crate::entry::ID;

/// Content-addressable hash of a cached value. Computed via `ID::from_bytes`
/// (BLAKE3 multihash), matching the algorithm used for entry IDs elsewhere
/// in eidetica.
type BlobHash = ID;

/// Composite cache key. Cloning a `(String, ID, String)` per lookup is cheap
/// at the scales the cache operates on — switching to a typed wrapper or
/// borrowed key is a future optimisation if profiling demands it.
type CacheKey = (String, ID, String);

#[derive(Default)]
struct Inner {
    /// Per-user membership: which `(user_uuid, entry_id, store)` slots point
    /// at which content-hash. A user's read sees only the hashes registered
    /// under their `user_uuid`.
    membership: HashMap<CacheKey, BlobHash>,
    /// Shared, refcounted blob store. Two users uploading identical bytes
    /// share a single entry here; the refcount tracks how many membership
    /// entries reference the blob, so it can be dropped when the last user
    /// clears or overwrites their slot.
    blobs: HashMap<BlobHash, RefcountedBlob>,
}

struct RefcountedBlob {
    refcount: usize,
    bytes: Vec<u8>,
}

/// Daemon-wide, in-memory per-user CRDT-state cache.
///
/// `Arc<ServiceCache>` is shared across connection handlers in
/// `ServiceServer`. The single internal `Mutex` is fine for the access
/// pattern (cache hits don't fan out further work); contention can be
/// revisited if profiling shows it.
///
/// TODO (before multi-user / untrusted clients): the `lock().unwrap()` in
/// `put`/`get`/`clear_user` panics on poisoning. Because this mutex is
/// shared across every connection handler, a panic in one handler while
/// holding it poisons the lock and cascades a panic into every other
/// connection's cache access — a single bad request becomes a daemon-wide
/// cache outage. Acceptable for the v1 single trusted client. Fix by
/// switching to a non-poisoning lock or recovering the guard on poison
/// (the cache is rebuildable, so a poisoned cache can be cleared rather
/// than crash the daemon).
#[derive(Default)]
pub(crate) struct ServiceCache {
    inner: Mutex<Inner>,
}

impl ServiceCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace the cache slot for `(user_uuid, entry_id, store)`.
    /// Bytes are content-hashed; identical bytes share a single stored blob
    /// across users (refcount bump rather than duplicate copy).
    pub fn put(&self, user_uuid: &str, entry_id: &ID, store: &str, bytes: Vec<u8>) {
        let hash = ID::from_bytes(&bytes);
        let key: CacheKey = (user_uuid.to_string(), entry_id.clone(), store.to_string());
        let mut inner = self.inner.lock().unwrap();

        // If the user already cached something at this slot, decrement the
        // outgoing blob's refcount before installing the new pointer.
        if let Some(prev_hash) = inner.membership.insert(key, hash.clone()) {
            decrement_refcount(&mut inner.blobs, &prev_hash);
        }

        match inner.blobs.entry(hash) {
            Entry::Occupied(mut existing) => {
                existing.get_mut().refcount += 1;
            }
            Entry::Vacant(slot) => {
                slot.insert(RefcountedBlob { refcount: 1, bytes });
            }
        }
    }

    /// Look up the cached state for `(user_uuid, entry_id, store)`. Returns
    /// `None` if this user hasn't cached anything at this slot, even if
    /// another user has.
    pub fn get(&self, user_uuid: &str, entry_id: &ID, store: &str) -> Option<Vec<u8>> {
        let inner = self.inner.lock().unwrap();
        let key: CacheKey = (user_uuid.to_string(), entry_id.clone(), store.to_string());
        let hash = inner.membership.get(&key)?;
        inner.blobs.get(hash).map(|b| b.bytes.clone())
    }

    /// Drop every cache entry owned by `user_uuid`. Blobs whose refcount
    /// falls to zero are removed.
    pub fn clear_user(&self, user_uuid: &str) {
        let mut inner = self.inner.lock().unwrap();
        let to_remove: Vec<CacheKey> = inner
            .membership
            .keys()
            .filter(|(u, _, _)| u == user_uuid)
            .cloned()
            .collect();
        for key in to_remove {
            if let Some(hash) = inner.membership.remove(&key) {
                decrement_refcount(&mut inner.blobs, &hash);
            }
        }
    }

    #[cfg(test)]
    pub fn blob_count(&self) -> usize {
        self.inner.lock().unwrap().blobs.len()
    }

    #[cfg(test)]
    pub fn membership_count(&self) -> usize {
        self.inner.lock().unwrap().membership.len()
    }
}

fn decrement_refcount(blobs: &mut HashMap<BlobHash, RefcountedBlob>, hash: &BlobHash) {
    if let Some(blob) = blobs.get_mut(hash) {
        blob.refcount -= 1;
        if blob.refcount == 0 {
            blobs.remove(hash);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eid(s: &str) -> ID {
        ID::from_bytes(s)
    }

    #[test]
    fn put_get_round_trip() {
        let cache = ServiceCache::new();
        cache.put("alice", &eid("e1"), "store1", b"hello".to_vec());
        assert_eq!(
            cache.get("alice", &eid("e1"), "store1"),
            Some(b"hello".to_vec())
        );
    }

    #[test]
    fn user_namespaces_are_isolated() {
        let cache = ServiceCache::new();
        cache.put("alice", &eid("e1"), "store1", b"a-secret".to_vec());
        assert!(
            cache.get("bob", &eid("e1"), "store1").is_none(),
            "bob must not see alice's cache slot, even at the same (entry_id, store)"
        );
    }

    #[test]
    fn writes_dont_poison_other_users() {
        let cache = ServiceCache::new();
        cache.put("alice", &eid("e1"), "store1", b"a-value".to_vec());
        cache.put("bob", &eid("e1"), "store1", b"b-value".to_vec());
        assert_eq!(
            cache.get("alice", &eid("e1"), "store1"),
            Some(b"a-value".to_vec()),
            "bob's write must not overwrite alice's slot"
        );
        assert_eq!(
            cache.get("bob", &eid("e1"), "store1"),
            Some(b"b-value".to_vec())
        );
        // Distinct values → two stored blobs.
        assert_eq!(cache.blob_count(), 2);
    }

    #[test]
    fn identical_bytes_dedup_across_users() {
        let cache = ServiceCache::new();
        cache.put("alice", &eid("e1"), "store1", b"shared".to_vec());
        cache.put("bob", &eid("e1"), "store1", b"shared".to_vec());
        assert_eq!(
            cache.blob_count(),
            1,
            "identical bytes must share one stored blob"
        );
        assert_eq!(
            cache.membership_count(),
            2,
            "each user keeps their own membership entry into the shared blob"
        );
        assert_eq!(
            cache.get("alice", &eid("e1"), "store1"),
            Some(b"shared".to_vec())
        );
        assert_eq!(
            cache.get("bob", &eid("e1"), "store1"),
            Some(b"shared".to_vec())
        );
    }

    #[test]
    fn overwrite_evicts_old_blob() {
        let cache = ServiceCache::new();
        cache.put("alice", &eid("e1"), "store1", b"v1".to_vec());
        assert_eq!(cache.blob_count(), 1);
        cache.put("alice", &eid("e1"), "store1", b"v2".to_vec());
        assert_eq!(
            cache.blob_count(),
            1,
            "v1 must be evicted when alice overwrites her own slot"
        );
        assert_eq!(
            cache.get("alice", &eid("e1"), "store1"),
            Some(b"v2".to_vec())
        );
    }

    #[test]
    fn clear_user_removes_only_their_entries() {
        let cache = ServiceCache::new();
        cache.put("alice", &eid("e1"), "store1", b"a1".to_vec());
        cache.put("alice", &eid("e2"), "store1", b"a2".to_vec());
        cache.put("bob", &eid("e1"), "store1", b"b1".to_vec());
        cache.clear_user("alice");
        assert!(cache.get("alice", &eid("e1"), "store1").is_none());
        assert!(cache.get("alice", &eid("e2"), "store1").is_none());
        assert_eq!(
            cache.get("bob", &eid("e1"), "store1"),
            Some(b"b1".to_vec()),
            "clear_user(alice) must leave bob untouched"
        );
    }

    #[test]
    fn clear_decrements_dedup_refcount() {
        let cache = ServiceCache::new();
        cache.put("alice", &eid("e1"), "store1", b"shared".to_vec());
        cache.put("bob", &eid("e1"), "store1", b"shared".to_vec());
        assert_eq!(cache.blob_count(), 1);
        // After alice clears, bob still references the shared blob.
        cache.clear_user("alice");
        assert_eq!(cache.blob_count(), 1);
        assert_eq!(
            cache.get("bob", &eid("e1"), "store1"),
            Some(b"shared".to_vec())
        );
        // Once bob clears too, the refcount reaches zero and the blob is dropped.
        cache.clear_user("bob");
        assert_eq!(
            cache.blob_count(),
            0,
            "blob must be dropped when the last reference disappears"
        );
    }
}
