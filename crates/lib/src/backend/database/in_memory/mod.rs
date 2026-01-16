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
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::{
    Result,
    backend::{BackendImpl, InstanceMetadata, VerificationStatus, errors::BackendError},
    entry::{Entry, ID},
};

/// Grouped tree tips cache: (tree_tips, subtree_name -> subtree_tips)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct TreeTipsCache {
    pub(crate) tree_tips: HashSet<ID>,
    pub(crate) subtree_tips: HashMap<String, HashSet<ID>>,
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
    /// Entries storage with read-write lock for concurrent access
    pub(crate) entries: RwLock<HashMap<ID, Entry>>,
    /// Verification status for each entry
    pub(crate) verification_status: RwLock<HashMap<ID, VerificationStatus>>,
    /// Instance metadata containing device key and system database IDs.
    ///
    /// When `None`, the backend is uninitialized. When `Some`, contains the
    /// device key and root IDs for system databases.
    ///
    /// **Security Warning**: The device key is stored in memory without encryption.
    /// This is suitable for development/testing only. Production systems should use
    /// proper key management with encryption at rest.
    pub(crate) instance_metadata: RwLock<Option<InstanceMetadata>>,
    /// Generic key-value cache for frequently computed results
    pub(crate) cache: RwLock<HashMap<String, String>>,
    /// Cached tips grouped by tree: tree_id -> (tree_tips, subtree_name -> subtree_tips)
    pub(crate) tips: RwLock<HashMap<ID, TreeTipsCache>>,
}

impl InMemory {
    /// Creates a new, empty `InMemory` database.
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            verification_status: RwLock::new(HashMap::new()),
            instance_metadata: RwLock::new(None),
            cache: RwLock::new(HashMap::new()),
            tips: RwLock::new(HashMap::new()),
        }
    }

    /// Returns a vector containing the IDs of all entries currently stored in the database.
    pub async fn all_ids(&self) -> Vec<ID> {
        let entries = self.entries.read().await;
        entries.keys().cloned().collect()
    }

    /// Saves the entire database state (all entries) to a specified file as JSON.
    ///
    /// # Arguments
    /// * `path` - The path to the file where the state should be saved.
    ///
    /// # Returns
    /// A `Result` indicating success or an I/O or serialization error.
    pub async fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        persistence::save_to_file(self, path).await
    }

    /// Loads the database state from a specified JSON file.
    ///
    /// If the file does not exist, a new, empty `InMemory` database is returned.
    ///
    /// # Arguments
    /// * `path` - The path to the file from which to load the state.
    ///
    /// # Returns
    /// A `Result` containing the loaded `InMemory` database or an I/O or deserialization error.
    pub async fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        persistence::load_from_file(path).await
    }

    /// Sort entries by their height within a tree (exposed for testing)
    ///
    /// Heights are stored directly in entries, so this just reads and sorts.
    ///
    /// # Arguments
    /// * `tree` - The ID of the tree context (unused, kept for API compatibility)
    /// * `entries` - The vector of entries to be sorted in place
    pub fn sort_entries_by_height(&self, tree: &ID, entries: &mut [Entry]) {
        cache::sort_entries_by_height(self, tree, entries)
    }

    /// Sort entries by their height within a subtree (exposed for testing)
    ///
    /// Heights are stored directly in entries, so this just reads and sorts.
    ///
    /// # Arguments
    /// * `tree` - The ID of the tree context (unused, kept for API compatibility)
    /// * `subtree` - The name of the subtree context
    /// * `entries` - The vector of entries to be sorted in place
    pub fn sort_entries_by_subtree_height(&self, tree: &ID, subtree: &str, entries: &mut [Entry]) {
        cache::sort_entries_by_subtree_height(self, tree, subtree, entries)
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
        storage::is_tip(self, tree, entry_id).await
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
        let entries = self.entries.read().await;
        entries
            .get(id)
            .cloned()
            .ok_or_else(|| BackendError::EntryNotFound { id: id.clone() }.into())
    }

    /// Gets the verification status of an entry.
    ///
    /// # Arguments
    /// * `id` - The ID of the entry to check.
    ///
    /// # Returns
    /// A `Result` containing the `VerificationStatus` if the entry exists, or a `DatabaseError::VerificationStatusNotFound` otherwise.
    async fn get_verification_status(&self, id: &ID) -> Result<VerificationStatus> {
        let verification_status_map = self.verification_status.read().await;
        verification_status_map
            .get(id)
            .copied()
            .ok_or_else(|| BackendError::VerificationStatusNotFound { id: id.clone() }.into())
    }

    async fn put(&self, verification_status: VerificationStatus, entry: Entry) -> Result<()> {
        storage::put(self, verification_status, entry).await
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
        let mut verification_status_map = self.verification_status.write().await;
        if verification_status_map.contains_key(id) {
            verification_status_map.insert(id.clone(), verification_status);
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
        let verification_status_map = self.verification_status.read().await;
        let ids = verification_status_map
            .iter()
            .filter(|&(_, entry_status)| *entry_status == status)
            .map(|(id, _)| id.clone())
            .collect();
        Ok(ids)
    }

    async fn get_tips(&self, tree: &ID) -> Result<Vec<ID>> {
        traversal::get_tips(self, tree).await
    }

    async fn get_store_tips(&self, tree: &ID, subtree: &str) -> Result<Vec<ID>> {
        traversal::get_store_tips(self, tree, subtree).await
    }

    async fn get_store_tips_up_to_entries(
        &self,
        tree: &ID,
        subtree: &str,
        main_entries: &[ID],
    ) -> Result<Vec<ID>> {
        traversal::get_store_tips_up_to_entries(self, tree, subtree, main_entries).await
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
        let entries = self.entries.read().await;
        let roots: Vec<ID> = entries
            .values()
            .filter(|entry| entry.is_root())
            .map(|entry| entry.id())
            .collect();
        Ok(roots)
    }

    async fn find_merge_base(&self, tree: &ID, subtree: &str, entry_ids: &[ID]) -> Result<ID> {
        traversal::find_merge_base(self, tree, subtree, entry_ids).await
    }

    async fn collect_root_to_target(
        &self,
        tree: &ID,
        subtree: &str,
        target_entry: &ID,
    ) -> Result<Vec<ID>> {
        traversal::collect_root_to_target(self, tree, subtree, target_entry).await
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    async fn get_tree(&self, tree: &ID) -> Result<Vec<Entry>> {
        storage::get_tree(self, tree).await
    }

    async fn get_store(&self, tree: &ID, subtree: &str) -> Result<Vec<Entry>> {
        storage::get_store(self, tree, subtree).await
    }

    async fn get_tree_from_tips(&self, tree: &ID, tips: &[ID]) -> Result<Vec<Entry>> {
        storage::get_tree_from_tips(self, tree, tips).await
    }

    async fn get_store_from_tips(
        &self,
        tree: &ID,
        subtree: &str,
        tips: &[ID],
    ) -> Result<Vec<Entry>> {
        storage::get_store_from_tips(self, tree, subtree, tips).await
    }

    async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>> {
        let metadata = self.instance_metadata.read().await;
        Ok(metadata.clone())
    }

    async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()> {
        let mut instance_metadata = self.instance_metadata.write().await;
        *instance_metadata = Some(metadata.clone());
        Ok(())
    }

    async fn get_cached_crdt_state(&self, entry_id: &ID, subtree: &str) -> Result<Option<String>> {
        cache::get_cached_crdt_state(self, entry_id, subtree).await
    }

    async fn cache_crdt_state(&self, entry_id: &ID, subtree: &str, state: String) -> Result<()> {
        cache::cache_crdt_state(self, entry_id, subtree, state).await
    }

    async fn clear_crdt_cache(&self) -> Result<()> {
        cache::clear_crdt_cache(self).await
    }

    async fn get_sorted_store_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        subtree: &str,
    ) -> Result<Vec<ID>> {
        traversal::get_sorted_store_parents(self, tree_id, entry_id, subtree).await
    }

    async fn get_path_from_to(
        &self,
        tree_id: &ID,
        subtree: &str,
        from_id: &ID,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        traversal::get_path_from_to(self, tree_id, subtree, from_id, to_ids).await
    }
}
