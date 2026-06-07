//! In-memory database backend implementation
//!
//! This module provides an in-memory implementation of the Database trait,
//! suitable for testing, development, or scenarios where data persistence
//! is not strictly required or is handled externally.

mod cache;
mod persistence;
mod storage;
mod traversal;

use std::{
    any::Any,
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Mutex, RwLock},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    Result,
    backend::{
        BackendImpl, CacheScope, InstanceMetadata, InstanceSecrets, VerificationStatus,
        errors::BackendError,
    },
    entry::{Entry, ID},
    snapshot::Snapshot,
};

use cache::InMemoryCrdtCache;

/// Grouped tree tips cache: (tree_tips, subtree_name -> subtree_tips)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct TreeTipsCache {
    pub(crate) tree_tips: HashSet<ID>,
    pub(crate) subtree_tips: HashMap<String, HashSet<ID>>,
}

/// Core data protected by a single lock.
///
/// All fields that participate in entry storage and tip tracking are grouped
/// together to eliminate lock ordering concerns. A single `RwLock` on the
/// outer `InMemory` struct protects all fields atomically.
#[derive(Debug)]
pub(crate) struct InMemoryInner {
    pub(crate) entries: HashMap<ID, Entry>,
    pub(crate) verification_status: HashMap<ID, VerificationStatus>,
    /// Instance metadata containing device public key and system database IDs.
    ///
    /// When `None`, the backend is uninitialized. When `Some`, contains the
    /// device public key and root IDs for system databases.
    pub(crate) instance_metadata: Option<InstanceMetadata>,
    /// Instance secrets containing the device signing key.
    ///
    /// **Security Warning**: The signing key is stored in memory without encryption.
    /// This is suitable for development/testing only. Production systems should use
    /// proper key management with encryption at rest.
    pub(crate) instance_secrets: Option<InstanceSecrets>,
    /// Cached tips grouped by tree: tree_id -> (tree_tips, subtree_name -> subtree_tips)
    pub(crate) tips: HashMap<ID, TreeTipsCache>,
}

/// A simple in-memory database implementation using a `HashMap` for storage.
///
/// This database is suitable for testing, development, or scenarios where
/// data persistence is not strictly required or is handled externally
/// (e.g., by saving/loading the entire state to/from a file).
///
/// It provides basic persistence capabilities via `save_to_file` and
/// `load_from_file`, serializing the `HashMap` to JSON.
///
/// **Security Note**: The device key is stored in memory in plaintext in this implementation.
/// This is acceptable for development and testing but should not be used in production
/// without proper encryption or hardware security module integration.
#[derive(Debug)]
pub struct InMemory {
    /// Core data protected by a single lock for atomic access and
    /// to eliminate lock ordering concerns between entries, verification
    /// status, and tips.
    pub(crate) inner: RwLock<InMemoryInner>,
    /// Scope-keyed, byte-bounded LRU cache for materialized CRDT state.
    /// `Mutex` (not `RwLock`) because `LruCache::get` mutates ordering, so
    /// every read is effectively a write under LRU semantics. Hosts both
    /// `Shared` (daemon-trusted) and `User` (client-attested) entries.
    pub(crate) crdt_cache: Mutex<InMemoryCrdtCache>,
}

impl InMemory {
    /// Creates a new, empty `InMemory` database.
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(InMemoryInner {
                entries: HashMap::new(),
                verification_status: HashMap::new(),
                instance_metadata: None,
                instance_secrets: None,
                tips: HashMap::new(),
            }),
            crdt_cache: Mutex::new(InMemoryCrdtCache::new()),
        }
    }

    /// Returns a vector containing the IDs of all entries currently stored in the database.
    pub async fn all_ids(&self) -> Vec<ID> {
        let inner = self.inner.read().unwrap();
        inner.entries.keys().cloned().collect()
    }

    /// Saves the entire database state (all entries) to a specified file as JSON.
    ///
    /// The write is atomic on POSIX (writes to `<path>.tmp` then renames
    /// into place). On Windows the final rename is not atomic when the
    /// destination already exists.
    ///
    /// This is synchronous — the body is just `std::fs::write` + `rename`
    /// with no await points — so it's safe to call from `Drop` impls and
    /// other non-async contexts. Callers on a tokio runtime should be
    /// aware that the write briefly blocks the calling worker thread.
    ///
    /// # Arguments
    /// * `path` - The path to the file where the state should be saved.
    ///
    /// # Returns
    /// A `Result` indicating success or an I/O or serialization error.
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        persistence::save_to_file(self, path)
    }

    /// Loads the database state from a specified JSON file.
    ///
    /// If the file does not exist, a new, empty `InMemory` database is returned.
    /// Callers that need to tell "missing" apart from "loaded empty" should
    /// use [`Self::try_load_from_file`].
    ///
    /// # Arguments
    /// * `path` - The path to the file from which to load the state.
    ///
    /// # Returns
    /// A `Result` containing the loaded `InMemory` database or an I/O or deserialization error.
    pub async fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        persistence::load_from_file(path)
    }

    /// Like [`Self::load_from_file`], but returns `Ok(None)` when the file
    /// does not exist instead of falling back to an empty backend. Lets
    /// callers distinguish "no snapshot yet" from "snapshot loaded as
    /// empty" without a separate `path.exists()` round-trip (and the TOCTOU
    /// window that comes with it).
    pub async fn try_load_from_file<P: AsRef<Path>>(path: P) -> Result<Option<Self>> {
        persistence::try_load_from_file(path)
    }

    /// Sort entries by their height within a tree (exposed for testing)
    ///
    /// Heights are stored directly in entries, so this just reads and sorts.
    ///
    /// # Arguments
    /// * `_tree` - The ID of the tree context (unused, kept for API compatibility)
    /// * `entries` - The vector of entries to be sorted in place
    pub fn sort_entries_by_height(&self, _tree: &ID, entries: &mut [Entry]) {
        cache::sort_entries_by_height(entries)
    }

    /// Sort entries by their height within a subtree (exposed for testing)
    ///
    /// Heights are stored directly in entries, so this just reads and sorts.
    ///
    /// # Arguments
    /// * `_tree` - The ID of the tree context (unused, kept for API compatibility)
    /// * `subtree` - The name of the subtree context
    /// * `entries` - The vector of entries to be sorted in place
    pub fn sort_entries_by_subtree_height(&self, _tree: &ID, subtree: &str, entries: &mut [Entry]) {
        cache::sort_entries_by_subtree_height(subtree, entries)
    }

    /// Check if an entry is a tip within its tree (exposed for benchmarks)
    ///
    /// An entry is a tip if no other entry in the same tree lists it as a parent.
    ///
    /// # Arguments
    /// * `tree` - The ID of the tree to check within
    /// * `entry_id` - The ID of the entry to check
    ///
    /// # Returns
    /// `true` if the entry is a tip, `false` otherwise
    pub async fn is_tip(&self, tree: &ID, entry_id: &ID) -> bool {
        let inner = self.inner.read().unwrap();
        storage::is_tip(&inner.entries, tree, entry_id)
    }
}

impl Default for InMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BackendImpl for InMemory {
    /// Retrieves an entry by its unique content-addressable ID.
    ///
    /// # Arguments
    /// * `id` - The ID of the entry to retrieve.
    ///
    /// # Returns
    /// A `Result` containing the `Entry` if found, or a `DatabaseError::EntryNotFound` otherwise.
    /// Returns an owned copy to support concurrent access with internal synchronization.
    async fn get(&self, id: &ID) -> Result<Entry> {
        let inner = self.inner.read().unwrap();
        storage::get(&inner, id)
    }

    /// Gets the verification status of an entry.
    ///
    /// # Arguments
    /// * `id` - The ID of the entry to check.
    ///
    /// # Returns
    /// A `Result` containing the `VerificationStatus` if the entry exists, or a `DatabaseError::VerificationStatusNotFound` otherwise.
    async fn get_verification_status(&self, id: &ID) -> Result<VerificationStatus> {
        let inner = self.inner.read().unwrap();
        inner
            .verification_status
            .get(id)
            .copied()
            .ok_or_else(|| BackendError::VerificationStatusNotFound { id: id.clone() }.into())
    }

    async fn put(&self, entry: Entry) -> Result<()> {
        // Validate before acquiring write lock to fail fast
        entry.validate()?;
        let mut inner = self.inner.write().unwrap();
        storage::put(&mut inner, entry)
    }

    /// Updates the verification status of an existing entry.
    ///
    /// This allows the authentication system to mark entries as verified or failed
    /// after they have been stored. Useful for batch verification operations.
    ///
    /// # Arguments
    /// * `id` - The ID of the entry to update
    /// * `verification_status` - The new verification status
    ///
    /// # Returns
    /// A `Result` indicating success or `DatabaseError::EntryNotFound` if the entry doesn't exist.
    async fn update_verification_status(
        &self,
        id: &ID,
        verification_status: VerificationStatus,
    ) -> Result<()> {
        let mut inner = self.inner.write().unwrap();
        if inner.verification_status.contains_key(id) {
            inner
                .verification_status
                .insert(id.clone(), verification_status);
            Ok(())
        } else {
            Err(BackendError::EntryNotFound { id: id.clone() }.into())
        }
    }

    /// Gets all entries with a specific verification status.
    ///
    /// This is useful for finding unverified entries that need authentication
    /// or for security audits.
    ///
    /// # Arguments
    /// * `status` - The verification status to filter by
    ///
    /// # Returns
    /// A `Result` containing a vector of entry IDs with the specified status.
    async fn get_entries_by_verification_status(
        &self,
        status: VerificationStatus,
    ) -> Result<Vec<ID>> {
        let inner = self.inner.read().unwrap();
        let ids = inner
            .verification_status
            .iter()
            .filter(|&(_, entry_status)| *entry_status == status)
            .map(|(id, _)| id.clone())
            .collect();
        Ok(ids)
    }

    async fn snapshot(&self, tree: &ID) -> Result<Snapshot> {
        // Fast path: check cache with read lock
        {
            let inner = self.inner.read().unwrap();
            if let Some(cache) = inner.tips.get(tree) {
                return Ok(Snapshot::new(cache.tree_tips.iter().cloned().collect()));
            }
        }
        // Slow path: compute and cache with write lock
        let mut inner = self.inner.write().unwrap();
        traversal::snapshot(&mut inner, tree).map(Snapshot::new)
    }

    async fn store_snapshot(&self, tree: &ID, subtree: &str) -> Result<Snapshot> {
        // Fast path: check cache with read lock
        {
            let inner = self.inner.read().unwrap();
            if let Some(cache) = inner.tips.get(tree)
                && let Some(subtree_tips) = cache.subtree_tips.get(subtree)
            {
                return Ok(Snapshot::new(subtree_tips.iter().cloned().collect()));
            }
        }
        // Slow path: compute and cache with write lock
        let mut inner = self.inner.write().unwrap();
        traversal::store_snapshot(&mut inner, tree, subtree).map(Snapshot::new)
    }

    async fn store_snapshot_at(
        &self,
        tree: &ID,
        subtree: &str,
        main_snapshot: &Snapshot,
    ) -> Result<Snapshot> {
        let mut inner = self.inner.write().unwrap();
        traversal::store_snapshot_at(&mut inner, tree, subtree, main_snapshot.tips())
            .map(Snapshot::new)
    }

    /// Retrieves the IDs of all top-level root entries stored in the database.
    ///
    /// Top-level roots are entries that are themselves roots of a tree
    /// (i.e., `entry.is_root()` is true) and are not part of a larger tree structure
    /// tracked by the backend. These represent the starting points
    /// of distinct trees managed by the database.
    ///
    /// # Returns
    /// A `Result` containing a vector of top-level root entry IDs or an error.
    async fn all_roots(&self) -> Result<Vec<ID>> {
        let inner = self.inner.read().unwrap();
        let roots: Vec<ID> = inner
            .entries
            .values()
            .filter(|entry| entry.is_root())
            .map(|entry| entry.id())
            .collect();
        Ok(roots)
    }

    async fn find_merge_base(&self, tree: &ID, subtree: &str, entry_ids: &[ID]) -> Result<ID> {
        let inner = self.inner.read().unwrap();
        traversal::find_merge_base(&inner, tree, subtree, entry_ids)
    }

    async fn collect_root_to_target(
        &self,
        tree: &ID,
        subtree: &str,
        target_entry: &ID,
    ) -> Result<Vec<ID>> {
        let inner = self.inner.read().unwrap();
        traversal::collect_root_to_target(&inner, tree, subtree, target_entry)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    async fn get_tree(&self, tree: &ID) -> Result<Vec<Entry>> {
        let inner = self.inner.read().unwrap();
        storage::get_tree(&inner, tree)
    }

    async fn get_store(&self, tree: &ID, subtree: &str) -> Result<Vec<Entry>> {
        let inner = self.inner.read().unwrap();
        storage::get_store(&inner, tree, subtree)
    }

    async fn get_tree_from_tips(&self, tree: &ID, tips: &[ID]) -> Result<Vec<Entry>> {
        let inner = self.inner.read().unwrap();
        storage::get_tree_from_tips(&inner, tree, tips)
    }

    async fn store_at(&self, tree: &ID, subtree: &str, snapshot: &Snapshot) -> Result<Vec<Entry>> {
        let inner = self.inner.read().unwrap();
        storage::store_at(&inner, tree, subtree, snapshot.tips())
    }

    async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>> {
        let inner = self.inner.read().unwrap();
        Ok(inner.instance_metadata.clone())
    }

    async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()> {
        let mut inner = self.inner.write().unwrap();
        inner.instance_metadata = Some(metadata.clone());
        Ok(())
    }

    async fn get_instance_secrets(&self) -> Result<Option<InstanceSecrets>> {
        let inner = self.inner.read().unwrap();
        Ok(inner.instance_secrets.clone())
    }

    async fn set_instance_secrets(&self, secrets: &InstanceSecrets) -> Result<()> {
        let mut inner = self.inner.write().unwrap();
        inner.instance_secrets = Some(secrets.clone());
        Ok(())
    }

    async fn get_cached_crdt_state(
        &self,
        scope: &CacheScope,
        entry_id: &ID,
        subtree: &str,
    ) -> Result<Option<Vec<u8>>> {
        cache::get_cached_crdt_state(self, scope, entry_id, subtree)
    }

    async fn cache_crdt_state(
        &self,
        scope: CacheScope,
        entry_id: &ID,
        subtree: &str,
        state: Vec<u8>,
    ) -> Result<()> {
        cache::cache_crdt_state(self, scope, entry_id, subtree, state)
    }

    async fn clear_crdt_cache(&self) -> Result<()> {
        cache::clear_crdt_cache(self)
    }

    async fn get_sorted_store_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        subtree: &str,
    ) -> Result<Vec<ID>> {
        let inner = self.inner.read().unwrap();
        traversal::get_sorted_store_parents(&inner, tree_id, entry_id, subtree)
    }

    async fn get_path_from_to(
        &self,
        tree_id: &ID,
        subtree: &str,
        from_id: &ID,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        let inner = self.inner.read().unwrap();
        traversal::get_path_from_to(&inner, tree_id, subtree, from_id, to_ids)
    }
}
