//! Backend wrapper for Instance-level operations
//!
//! This module provides the `Backend` struct which wraps `BackendImpl` and provides
//! a layer for Instance-level operations. Currently it's a thin passthrough,
//! but will be extended to support both local and remote (RPC) backends in the future.

use std::{any::Any, sync::Arc};

use ed25519_dalek::SigningKey;
use handle_trait::Handle;

use crate::{
    Result,
    backend::{BackendImpl, VerificationStatus},
    entry::{Entry, ID},
};

/// Backend wrapper for Instance operations
///
/// This struct wraps a `BackendImpl` and provides methods for backend operations.
/// Currently it's a thin wrapper that delegates all calls to the underlying BackendImpl.
///
/// In the future, this will be converted to an enum to support both local and remote
/// (RPC-based) backends, allowing for transparent local/remote dispatch.
#[derive(Clone, Handle)]
pub struct Backend {
    backend_impl: Arc<dyn BackendImpl>,
}

impl Backend {
    /// Create a new Backend wrapping a BackendImpl
    pub fn new(backend_impl: Arc<dyn BackendImpl>) -> Self {
        Self { backend_impl }
    }

    /// Get an entry from the backend
    pub fn get(&self, id: &ID) -> Result<Entry> {
        self.backend_impl.get(id)
    }

    /// Get verification status of an entry
    pub fn get_verification_status(&self, id: &ID) -> Result<VerificationStatus> {
        self.backend_impl.get_verification_status(id)
    }

    /// Put an entry into the backend with verification status
    pub fn put(&self, verification: VerificationStatus, entry: Entry) -> Result<()> {
        self.backend_impl.put(verification, entry)
    }

    /// Put a verified entry (convenience method)
    pub fn put_verified(&self, entry: Entry) -> Result<()> {
        self.backend_impl.put_verified(entry)
    }

    /// Put an unverified entry (convenience method)
    pub fn put_unverified(&self, entry: Entry) -> Result<()> {
        self.backend_impl.put_unverified(entry)
    }

    /// Update verification status of an entry
    pub fn update_verification_status(&self, id: &ID, status: VerificationStatus) -> Result<()> {
        self.backend_impl.update_verification_status(id, status)
    }

    /// Get entries by verification status
    pub fn get_entries_by_verification_status(
        &self,
        status: VerificationStatus,
    ) -> Result<Vec<ID>> {
        self.backend_impl.get_entries_by_verification_status(status)
    }

    /// Get tips for a tree
    pub fn get_tips(&self, tree: &ID) -> Result<Vec<ID>> {
        self.backend_impl.get_tips(tree)
    }

    /// Get tips for a specific store within a tree
    pub fn get_store_tips(&self, tree: &ID, store: &str) -> Result<Vec<ID>> {
        self.backend_impl.get_store_tips(tree, store)
    }

    /// Get store tips up to specific entries
    pub fn get_store_tips_up_to_entries(
        &self,
        tree: &ID,
        store: &str,
        up_to: &[ID],
    ) -> Result<Vec<ID>> {
        self.backend_impl
            .get_store_tips_up_to_entries(tree, store, up_to)
    }

    /// Get all root entries
    pub fn all_roots(&self) -> Result<Vec<ID>> {
        self.backend_impl.all_roots()
    }

    /// Find lowest common ancestor
    pub fn find_lca(&self, tree: &ID, store: &str, entry_ids: &[ID]) -> Result<ID> {
        self.backend_impl.find_lca(tree, store, entry_ids)
    }

    /// Collect root to target path
    pub fn collect_root_to_target(&self, tree: &ID, store: &str, target: &ID) -> Result<Vec<ID>> {
        self.backend_impl
            .collect_root_to_target(tree, store, target)
    }

    /// Get all entries in a tree
    pub fn get_tree(&self, tree: &ID) -> Result<Vec<Entry>> {
        self.backend_impl.get_tree(tree)
    }

    /// Get all entries in a store
    pub fn get_store(&self, tree: &ID, store: &str) -> Result<Vec<Entry>> {
        self.backend_impl.get_store(tree, store)
    }

    /// Get tree entries from tips
    pub fn get_tree_from_tips(&self, tree: &ID, tips: &[ID]) -> Result<Vec<Entry>> {
        self.backend_impl.get_tree_from_tips(tree, tips)
    }

    /// Get store entries from tips
    pub fn get_store_from_tips(&self, tree: &ID, store: &str, tips: &[ID]) -> Result<Vec<Entry>> {
        self.backend_impl.get_store_from_tips(tree, store, tips)
    }

    /// Store a private key
    pub fn store_private_key(&self, key_name: &str, private_key: SigningKey) -> Result<()> {
        self.backend_impl.store_private_key(key_name, private_key)
    }

    /// Get a private key
    pub fn get_private_key(&self, key_name: &str) -> Result<Option<SigningKey>> {
        self.backend_impl.get_private_key(key_name)
    }

    /// List all private keys
    pub fn list_private_keys(&self) -> Result<Vec<String>> {
        self.backend_impl.list_private_keys()
    }

    /// Remove a private key
    pub fn remove_private_key(&self, key_name: &str) -> Result<()> {
        self.backend_impl.remove_private_key(key_name)
    }

    /// Get cached CRDT state
    pub fn get_cached_crdt_state(&self, entry_id: &ID, store: &str) -> Result<Option<String>> {
        self.backend_impl.get_cached_crdt_state(entry_id, store)
    }

    /// Cache CRDT state
    pub fn cache_crdt_state(&self, entry_id: &ID, store: &str, state: String) -> Result<()> {
        self.backend_impl.cache_crdt_state(entry_id, store, state)
    }

    /// Clear CRDT cache
    pub fn clear_crdt_cache(&self) -> Result<()> {
        self.backend_impl.clear_crdt_cache()
    }

    /// Get sorted store parents
    pub fn get_sorted_store_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        store: &str,
    ) -> Result<Vec<ID>> {
        self.backend_impl
            .get_sorted_store_parents(tree_id, entry_id, store)
    }

    /// Get path from one entry to others
    pub fn get_path_from_to(
        &self,
        tree_id: &ID,
        store: &str,
        from_id: &ID,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        self.backend_impl
            .get_path_from_to(tree_id, store, from_id, to_ids)
    }

    /// Get access to the underlying BackendImpl
    ///
    /// This is provided for special operations like downcasting to concrete
    /// backend types (e.g., for save/load operations on InMemory).
    /// Use with caution.
    pub fn as_backend_impl(&self) -> &dyn BackendImpl {
        &*self.backend_impl
    }

    /// Get access to the underlying `Arc<dyn BackendImpl>`
    ///
    /// This is needed for validation functions and other code that expects
    /// the Arc wrapper. Returns a reference to the Arc.
    pub fn as_arc_backend_impl(&self) -> &Arc<dyn BackendImpl> {
        &self.backend_impl
    }

    /// Downcast to Any for concrete backend type access
    ///
    /// This is primarily used for downcasting to concrete backend types
    /// (e.g., InMemory) for save/load operations or testing.
    pub fn as_any(&self) -> &dyn Any {
        self.backend_impl.as_any()
    }
}
