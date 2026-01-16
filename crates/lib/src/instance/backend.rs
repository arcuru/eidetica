//! Backend wrapper for Instance-level operations
//!
//! This module provides the `Backend` struct which wraps `BackendImpl` and provides
//! a layer for Instance-level operations. Currently it's a thin passthrough,
//! but will be extended to support both local and remote (RPC) backends in the future.

use std::{any::Any, sync::Arc};

use handle_trait::Handle;

use crate::{
    Result,
    backend::{BackendImpl, InstanceMetadata, VerificationStatus},
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
    pub async fn get(&self, id: &ID) -> Result<Entry> {
        self.backend_impl.get(id).await
    }

    /// Get verification status of an entry
    pub async fn get_verification_status(&self, id: &ID) -> Result<VerificationStatus> {
        self.backend_impl.get_verification_status(id).await
    }

    /// Put an entry into the backend with verification status
    pub async fn put(&self, verification: VerificationStatus, entry: Entry) -> Result<()> {
        self.backend_impl.put(verification, entry).await
    }

    /// Put a verified entry (convenience method)
    pub async fn put_verified(&self, entry: Entry) -> Result<()> {
        self.backend_impl.put_verified(entry).await
    }

    /// Put an unverified entry (convenience method)
    pub async fn put_unverified(&self, entry: Entry) -> Result<()> {
        self.backend_impl.put_unverified(entry).await
    }

    /// Update verification status of an entry
    pub async fn update_verification_status(
        &self,
        id: &ID,
        status: VerificationStatus,
    ) -> Result<()> {
        self.backend_impl
            .update_verification_status(id, status)
            .await
    }

    /// Get entries by verification status
    pub async fn get_entries_by_verification_status(
        &self,
        status: VerificationStatus,
    ) -> Result<Vec<ID>> {
        self.backend_impl
            .get_entries_by_verification_status(status)
            .await
    }

    /// Get tips for a tree
    pub async fn get_tips(&self, tree: &ID) -> Result<Vec<ID>> {
        self.backend_impl.get_tips(tree).await
    }

    /// Get tips for a specific store within a tree
    pub async fn get_store_tips(&self, tree: &ID, store: &str) -> Result<Vec<ID>> {
        self.backend_impl.get_store_tips(tree, store).await
    }

    /// Get store tips up to specific entries
    pub async fn get_store_tips_up_to_entries(
        &self,
        tree: &ID,
        store: &str,
        up_to: &[ID],
    ) -> Result<Vec<ID>> {
        self.backend_impl
            .get_store_tips_up_to_entries(tree, store, up_to)
            .await
    }

    /// Get all root entries
    pub async fn all_roots(&self) -> Result<Vec<ID>> {
        self.backend_impl.all_roots().await
    }

    /// Find merge base (common dominator) of entries
    pub async fn find_merge_base(&self, tree: &ID, store: &str, entry_ids: &[ID]) -> Result<ID> {
        self.backend_impl
            .find_merge_base(tree, store, entry_ids)
            .await
    }

    /// Collect root to target path
    pub async fn collect_root_to_target(
        &self,
        tree: &ID,
        store: &str,
        target: &ID,
    ) -> Result<Vec<ID>> {
        self.backend_impl
            .collect_root_to_target(tree, store, target)
            .await
    }

    /// Get all entries in a tree
    pub async fn get_tree(&self, tree: &ID) -> Result<Vec<Entry>> {
        self.backend_impl.get_tree(tree).await
    }

    /// Get all entries in a store
    pub async fn get_store(&self, tree: &ID, store: &str) -> Result<Vec<Entry>> {
        self.backend_impl.get_store(tree, store).await
    }

    /// Get tree entries from tips
    pub async fn get_tree_from_tips(&self, tree: &ID, tips: &[ID]) -> Result<Vec<Entry>> {
        self.backend_impl.get_tree_from_tips(tree, tips).await
    }

    /// Get store entries from tips
    pub async fn get_store_from_tips(
        &self,
        tree: &ID,
        store: &str,
        tips: &[ID],
    ) -> Result<Vec<Entry>> {
        self.backend_impl
            .get_store_from_tips(tree, store, tips)
            .await
    }

    /// Get cached CRDT state
    pub async fn get_cached_crdt_state(
        &self,
        entry_id: &ID,
        store: &str,
    ) -> Result<Option<String>> {
        self.backend_impl
            .get_cached_crdt_state(entry_id, store)
            .await
    }

    /// Cache CRDT state
    pub async fn cache_crdt_state(&self, entry_id: &ID, store: &str, state: String) -> Result<()> {
        self.backend_impl
            .cache_crdt_state(entry_id, store, state)
            .await
    }

    /// Clear CRDT cache
    pub async fn clear_crdt_cache(&self) -> Result<()> {
        self.backend_impl.clear_crdt_cache().await
    }

    /// Get sorted store parents
    pub async fn get_sorted_store_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        store: &str,
    ) -> Result<Vec<ID>> {
        self.backend_impl
            .get_sorted_store_parents(tree_id, entry_id, store)
            .await
    }

    /// Get path from one entry to others
    pub async fn get_path_from_to(
        &self,
        tree_id: &ID,
        store: &str,
        from_id: &ID,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        self.backend_impl
            .get_path_from_to(tree_id, store, from_id, to_ids)
            .await
    }

    /// Get instance metadata
    pub async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>> {
        self.backend_impl.get_instance_metadata().await
    }

    /// Set instance metadata
    pub async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()> {
        self.backend_impl.set_instance_metadata(metadata).await
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
