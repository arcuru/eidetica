//! In-memory database backend implementation
//!
//! This module provides an in-memory implementation of the Database trait,
//! suitable for testing, development, or scenarios where data persistence
//! is not strictly required or is handled externally.

mod cache;
mod persistence;
mod storage;
mod traversal;

use crate::Result;
use crate::backend::errors::DatabaseError;
use crate::backend::{Database, VerificationStatus};
use crate::entry::{Entry, ID};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::RwLock;

/// Heights cache: entry_id -> (tree_height, subtree_name -> subtree_height)
pub(crate) type TreeHeightsCache = HashMap<ID, (usize, HashMap<String, usize>)>;

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
/// **Security Note**: Private keys are stored in memory in plaintext in this implementation.
/// This is acceptable for development and testing but should not be used in production
/// without proper encryption or hardware security module integration.
#[derive(Debug)]
pub struct InMemory {
    /// Entries storage with read-write lock for concurrent access
    pub(crate) entries: RwLock<HashMap<ID, Entry>>,
    /// Verification status for each entry
    pub(crate) verification_status: RwLock<HashMap<ID, VerificationStatus>>,
    /// Private key storage for authentication
    ///
    /// **Security Warning**: Keys are stored in memory without encryption.
    /// This is suitable for development/testing only. Production systems should use
    /// proper key management with encryption at rest.
    pub(crate) private_keys: RwLock<HashMap<String, SigningKey>>,
    /// Generic key-value cache for frequently computed results
    pub(crate) cache: RwLock<HashMap<String, String>>,
    /// Cached heights grouped by tree: tree_id -> (entry_id -> (tree_height, subtree_name -> subtree_height))
    pub(crate) heights: RwLock<HashMap<ID, TreeHeightsCache>>,
    /// Cached tips grouped by tree: tree_id -> (tree_tips, subtree_name -> subtree_tips)
    pub(crate) tips: RwLock<HashMap<ID, TreeTipsCache>>,
}

impl InMemory {
    /// Creates a new, empty `InMemory` database.
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            verification_status: RwLock::new(HashMap::new()),
            private_keys: RwLock::new(HashMap::new()),
            cache: RwLock::new(HashMap::new()),
            heights: RwLock::new(HashMap::new()),
            tips: RwLock::new(HashMap::new()),
        }
    }

    /// Returns a vector containing the IDs of all entries currently stored in the database.
    pub fn all_ids(&self) -> Vec<ID> {
        let entries = self.entries.read().unwrap();
        entries.keys().cloned().collect()
    }

    /// Saves the entire database state (all entries) to a specified file as JSON.
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
    ///
    /// # Arguments
    /// * `path` - The path to the file from which to load the state.
    ///
    /// # Returns
    /// A `Result` containing the loaded `InMemory` database or an I/O or deserialization error.
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        persistence::load_from_file(path)
    }

    /// Calculate heights for entries in a tree or subtree (exposed for testing)
    ///
    /// # Arguments
    /// * `tree` - The ID of the tree to calculate heights for
    /// * `subtree` - Optional subtree name to limit calculation to a specific subtree
    ///
    /// # Returns
    /// A `Result` containing a `HashMap` mapping entry IDs to their heights.
    pub fn calculate_heights(
        &self,
        tree: &ID,
        subtree: Option<&str>,
    ) -> Result<std::collections::HashMap<ID, usize>> {
        cache::calculate_heights(self, tree, subtree)
    }

    /// Sort entries by their height within a tree (exposed for testing)
    ///
    /// # Arguments
    /// * `tree` - The ID of the tree context
    /// * `entries` - The vector of entries to be sorted in place
    ///
    /// # Returns
    /// A `Result` indicating success or an error if height calculation fails.
    pub fn sort_entries_by_height(&self, tree: &ID, entries: &mut [Entry]) -> Result<()> {
        cache::sort_entries_by_height(self, tree, entries)
    }

    /// Sort entries by their height within a subtree (exposed for testing)
    ///
    /// # Arguments
    /// * `tree` - The ID of the tree context
    /// * `subtree` - The name of the subtree context
    /// * `entries` - The vector of entries to be sorted in place
    ///
    /// # Returns
    /// A `Result` indicating success or an error if height calculation fails.
    pub fn sort_entries_by_subtree_height(
        &self,
        tree: &ID,
        subtree: &str,
        entries: &mut [Entry],
    ) -> Result<()> {
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
    pub fn is_tip(&self, tree: &ID, entry_id: &ID) -> bool {
        storage::is_tip(self, tree, entry_id)
    }
}

impl Default for InMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl Database for InMemory {
    /// Retrieves an entry by its unique content-addressable ID.
    ///
    /// # Arguments
    /// * `id` - The ID of the entry to retrieve.
    ///
    /// # Returns
    /// A `Result` containing the `Entry` if found, or a `DatabaseError::EntryNotFound` otherwise.
    /// Returns an owned copy to support concurrent access with internal synchronization.
    fn get(&self, id: &ID) -> Result<Entry> {
        let entries = self.entries.read().unwrap();
        entries
            .get(id)
            .cloned()
            .ok_or_else(|| DatabaseError::EntryNotFound { id: id.clone() }.into())
    }

    /// Gets the verification status of an entry.
    ///
    /// # Arguments
    /// * `id` - The ID of the entry to check.
    ///
    /// # Returns
    /// A `Result` containing the `VerificationStatus` if the entry exists, or a `DatabaseError::VerificationStatusNotFound` otherwise.
    fn get_verification_status(&self, id: &ID) -> Result<VerificationStatus> {
        let verification_status_map = self.verification_status.read().unwrap();
        verification_status_map
            .get(id)
            .copied()
            .ok_or_else(|| DatabaseError::VerificationStatusNotFound { id: id.clone() }.into())
    }

    fn put(&self, verification_status: VerificationStatus, entry: Entry) -> Result<()> {
        storage::put(self, verification_status, entry)
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
    fn update_verification_status(
        &self,
        id: &ID,
        verification_status: VerificationStatus,
    ) -> Result<()> {
        let mut verification_status_map = self.verification_status.write().unwrap();
        if verification_status_map.contains_key(id) {
            verification_status_map.insert(id.clone(), verification_status);
            Ok(())
        } else {
            Err(DatabaseError::EntryNotFound { id: id.clone() }.into())
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
    fn get_entries_by_verification_status(&self, status: VerificationStatus) -> Result<Vec<ID>> {
        let verification_status_map = self.verification_status.read().unwrap();
        let ids = verification_status_map
            .iter()
            .filter(|&(_, entry_status)| *entry_status == status)
            .map(|(id, _)| id.clone())
            .collect();
        Ok(ids)
    }

    fn get_tips(&self, tree: &ID) -> Result<Vec<ID>> {
        traversal::get_tips(self, tree)
    }

    fn get_subtree_tips(&self, tree: &ID, subtree: &str) -> Result<Vec<ID>> {
        traversal::get_subtree_tips(self, tree, subtree)
    }

    fn get_subtree_tips_up_to_entries(
        &self,
        tree: &ID,
        subtree: &str,
        main_entries: &[ID],
    ) -> Result<Vec<ID>> {
        traversal::get_subtree_tips_up_to_entries(self, tree, subtree, main_entries)
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
    fn all_roots(&self) -> Result<Vec<ID>> {
        let entries = self.entries.read().unwrap();
        let roots: Vec<ID> = entries
            .values()
            .filter(|entry| entry.is_root())
            .map(|entry| entry.id())
            .collect();
        Ok(roots)
    }

    fn find_lca(&self, tree: &ID, subtree: &str, entry_ids: &[ID]) -> Result<ID> {
        traversal::find_lca(self, tree, subtree, entry_ids)
    }

    fn collect_root_to_target(
        &self,
        tree: &ID,
        subtree: &str,
        target_entry: &ID,
    ) -> Result<Vec<ID>> {
        traversal::collect_root_to_target(self, tree, subtree, target_entry)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get_tree(&self, tree: &ID) -> Result<Vec<Entry>> {
        storage::get_tree(self, tree)
    }

    fn get_subtree(&self, tree: &ID, subtree: &str) -> Result<Vec<Entry>> {
        storage::get_subtree(self, tree, subtree)
    }

    fn get_tree_from_tips(&self, tree: &ID, tips: &[ID]) -> Result<Vec<Entry>> {
        storage::get_tree_from_tips(self, tree, tips)
    }

    fn get_subtree_from_tips(&self, tree: &ID, subtree: &str, tips: &[ID]) -> Result<Vec<Entry>> {
        storage::get_subtree_from_tips(self, tree, subtree, tips)
    }

    /// Store a private key in the database's local key storage.
    ///
    /// Private keys are stored separately from entries and are not part of the content-addressable
    /// database. They are used for signing new entries but are never shared or synchronized.
    ///
    /// # Arguments
    /// * `key_id` - A unique identifier for the private key (e.g., "KEY_LAPTOP")
    /// * `private_key` - The Ed25519 private key to store
    ///
    /// # Returns
    /// A `Result` indicating success or an error during storage.
    ///
    /// # Security Note
    /// This is a basic implementation suitable for development and testing.
    /// Production systems should consider encryption at rest and hardware security modules.
    fn store_private_key(&self, key_id: &str, private_key: SigningKey) -> Result<()> {
        let mut private_keys = self.private_keys.write().unwrap();
        private_keys.insert(key_id.to_string(), private_key);
        Ok(())
    }

    /// Retrieve a private key from the database's local key storage.
    ///
    /// # Arguments
    /// * `key_id` - The unique identifier of the private key to retrieve
    ///
    /// # Returns
    /// A `Result` containing an `Option<SigningKey>`. Returns `None` if the key is not found.
    fn get_private_key(&self, key_id: &str) -> Result<Option<SigningKey>> {
        let private_keys = self.private_keys.read().unwrap();
        Ok(private_keys.get(key_id).cloned())
    }

    /// List all private key identifiers stored in the database.
    ///
    /// # Returns
    /// A `Result` containing a vector of key identifiers, or an error.
    fn list_private_keys(&self) -> Result<Vec<String>> {
        let private_keys = self.private_keys.read().unwrap();
        Ok(private_keys.keys().cloned().collect())
    }

    /// Remove a private key from the database's local key storage.
    ///
    /// # Arguments
    /// * `key_id` - The unique identifier of the private key to remove
    ///
    /// # Returns
    /// A `Result` indicating success or an error. Succeeds even if the key doesn't exist.
    fn remove_private_key(&self, key_id: &str) -> Result<()> {
        let mut private_keys = self.private_keys.write().unwrap();
        private_keys.remove(key_id);
        Ok(())
    }

    fn get_cached_crdt_state(&self, entry_id: &ID, subtree: &str) -> Result<Option<String>> {
        cache::get_cached_crdt_state(self, entry_id, subtree)
    }

    fn cache_crdt_state(&self, entry_id: &ID, subtree: &str, state: String) -> Result<()> {
        cache::cache_crdt_state(self, entry_id, subtree, state)
    }

    fn clear_crdt_cache(&self) -> Result<()> {
        cache::clear_crdt_cache(self)
    }

    fn get_sorted_subtree_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        subtree: &str,
    ) -> Result<Vec<ID>> {
        traversal::get_sorted_subtree_parents(self, tree_id, entry_id, subtree)
    }

    fn get_path_from_to(
        &self,
        tree_id: &ID,
        subtree: &str,
        from_id: &ID,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        traversal::get_path_from_to(self, tree_id, subtree, from_id, to_ids)
    }
}
