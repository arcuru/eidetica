//! Backend wrapper for Instance-level operations
//!
//! This module provides the `Backend` enum which dispatches storage operations
//! to either a local `BackendImpl` or a remote service connection.

use std::any::Any;
use std::sync::Arc;

use crate::{
    Result,
    backend::{BackendImpl, InstanceMetadata, InstanceSecrets, VerificationStatus},
    entry::{Entry, ID},
    instance::WriteSource,
};

/// Backend for Instance operations.
///
/// Dispatches storage operations to either a local `BackendImpl` implementation
/// or a remote service connection over a Unix domain socket.
pub enum Backend {
    /// Local backend with direct access to storage.
    Local(Arc<dyn BackendImpl>),
    /// Remote backend connected to a service daemon.
    #[cfg(all(unix, feature = "service"))]
    Remote(crate::service::client::RemoteConnection),
}

impl Clone for Backend {
    fn clone(&self) -> Self {
        match self {
            Backend::Local(b) => Backend::Local(Arc::clone(b)),
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(c) => Backend::Remote(c.clone()),
        }
    }
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Backend::Local(_) => f.debug_tuple("Backend::Local").finish(),
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(c) => f.debug_tuple("Backend::Remote").field(c).finish(),
        }
    }
}

/// Dispatch a method call to the appropriate backend variant.
///
/// For methods where Local delegates to `BackendImpl` and Remote delegates
/// to `RemoteConnection` with the same method name and signature.
macro_rules! dispatch {
    ($self:expr, $method:ident ( $($arg:expr),* $(,)? )) => {
        match $self {
            Backend::Local(b) => b.$method($($arg),*).await,
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(c) => c.$method($($arg),*).await,
        }
    };
}

impl Backend {
    /// Create a new local Backend wrapping a BackendImpl.
    pub fn new(backend_impl: Arc<dyn BackendImpl>) -> Self {
        Self::Local(backend_impl)
    }

    // === Entry operations ===

    pub async fn get(&self, id: &ID) -> Result<Entry> {
        dispatch!(self, get(id))
    }

    pub async fn get_verification_status(&self, id: &ID) -> Result<VerificationStatus> {
        dispatch!(self, get_verification_status(id))
    }

    pub async fn put(&self, verification: VerificationStatus, entry: Entry) -> Result<()> {
        dispatch!(self, put(verification, entry))
    }

    pub async fn put_verified(&self, entry: Entry) -> Result<()> {
        self.put(VerificationStatus::Verified, entry).await
    }

    pub async fn put_unverified(&self, entry: Entry) -> Result<()> {
        self.put(VerificationStatus::Failed, entry).await
    }

    pub async fn update_verification_status(
        &self,
        id: &ID,
        status: VerificationStatus,
    ) -> Result<()> {
        dispatch!(self, update_verification_status(id, status))
    }

    pub async fn get_entries_by_verification_status(
        &self,
        status: VerificationStatus,
    ) -> Result<Vec<ID>> {
        dispatch!(self, get_entries_by_verification_status(status))
    }

    // === Tips ===

    pub async fn get_tips(&self, tree: &ID) -> Result<Vec<ID>> {
        dispatch!(self, get_tips(tree))
    }

    pub async fn get_store_tips(&self, tree: &ID, store: &str) -> Result<Vec<ID>> {
        dispatch!(self, get_store_tips(tree, store))
    }

    pub async fn get_store_tips_up_to_entries(
        &self,
        tree: &ID,
        store: &str,
        up_to: &[ID],
    ) -> Result<Vec<ID>> {
        dispatch!(self, get_store_tips_up_to_entries(tree, store, up_to))
    }

    // === Tree/Store traversal ===

    pub async fn all_roots(&self) -> Result<Vec<ID>> {
        dispatch!(self, all_roots())
    }

    pub async fn find_merge_base(&self, tree: &ID, store: &str, entry_ids: &[ID]) -> Result<ID> {
        dispatch!(self, find_merge_base(tree, store, entry_ids))
    }

    pub async fn collect_root_to_target(
        &self,
        tree: &ID,
        store: &str,
        target: &ID,
    ) -> Result<Vec<ID>> {
        dispatch!(self, collect_root_to_target(tree, store, target))
    }

    pub async fn get_tree(&self, tree: &ID) -> Result<Vec<Entry>> {
        dispatch!(self, get_tree(tree))
    }

    pub async fn get_store(&self, tree: &ID, store: &str) -> Result<Vec<Entry>> {
        dispatch!(self, get_store(tree, store))
    }

    pub async fn get_tree_from_tips(&self, tree: &ID, tips: &[ID]) -> Result<Vec<Entry>> {
        dispatch!(self, get_tree_from_tips(tree, tips))
    }

    pub async fn get_store_from_tips(
        &self,
        tree: &ID,
        store: &str,
        tips: &[ID],
    ) -> Result<Vec<Entry>> {
        dispatch!(self, get_store_from_tips(tree, store, tips))
    }

    // === CRDT cache ===

    pub async fn get_cached_crdt_state(
        &self,
        entry_id: &ID,
        store: &str,
    ) -> Result<Option<Vec<u8>>> {
        dispatch!(self, get_cached_crdt_state(entry_id, store))
    }

    /// Cache CRDT state
    pub async fn cache_crdt_state(&self, entry_id: &ID, store: &str, state: Vec<u8>) -> Result<()> {
        dispatch!(self, cache_crdt_state(entry_id, store, state))
    }

    pub async fn clear_crdt_cache(&self) -> Result<()> {
        dispatch!(self, clear_crdt_cache())
    }

    // === Path operations ===

    pub async fn get_sorted_store_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        store: &str,
    ) -> Result<Vec<ID>> {
        dispatch!(self, get_sorted_store_parents(tree_id, entry_id, store))
    }

    pub async fn get_path_from_to(
        &self,
        tree_id: &ID,
        store: &str,
        from_id: &ID,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        dispatch!(self, get_path_from_to(tree_id, store, from_id, to_ids))
    }

    // === Instance metadata ===

    pub async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>> {
        dispatch!(self, get_instance_metadata())
    }

    pub async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()> {
        dispatch!(self, set_instance_metadata(metadata))
    }

    // === Instance secrets ===

    /// Get instance secrets. Remote backends always return `Ok(None)`.
    pub async fn get_instance_secrets(&self) -> Result<Option<InstanceSecrets>> {
        match self {
            Backend::Local(b) => b.get_instance_secrets().await,
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(_) => Ok(None),
        }
    }

    /// Set instance secrets. Returns an error on remote backends.
    pub async fn set_instance_secrets(&self, secrets: &InstanceSecrets) -> Result<()> {
        match self {
            Backend::Local(b) => b.set_instance_secrets(secrets).await,
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(_) => Err(crate::instance::InstanceError::OperationNotSupported {
                operation: "set_instance_secrets on remote backend".to_string(),
            }
            .into()),
        }
    }

    // === Write coordination ===

    /// Write an entry to storage and handle remote write coordination.
    ///
    /// For local backends, this persists the entry. For remote backends, this
    /// persists the entry via Put RPC and then notifies the server to dispatch
    /// write callbacks via NotifyEntryWritten RPC.
    ///
    /// Local callback dispatch is handled by `Instance::put_entry()`.
    pub async fn write_entry(
        &self,
        tree_id: &ID,
        verification: VerificationStatus,
        entry: Entry,
        source: WriteSource,
    ) -> Result<()> {
        match self {
            Backend::Local(b) => b.put(verification, entry).await,
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(c) => {
                let entry_id = entry.id();
                c.put(verification, entry).await?;
                c.notify_entry_written(tree_id, &entry_id, source).await
            }
        }
    }

    // === User management ===

    /// Create a user via RPC (remote only). Local backends use Instance methods directly.
    #[cfg(all(unix, feature = "service"))]
    pub async fn create_user(&self, username: &str, password: Option<&str>) -> Result<String> {
        match self {
            Backend::Local(_) => Err(crate::instance::InstanceError::OperationNotSupported {
                operation: "create_user RPC on local backend".to_string(),
            }
            .into()),
            Backend::Remote(c) => c.create_user(username, password).await,
        }
    }

    /// List users via RPC (remote only). Local backends use Instance methods directly.
    #[cfg(all(unix, feature = "service"))]
    pub async fn list_users(&self) -> Result<Vec<String>> {
        match self {
            Backend::Local(_) => Err(crate::instance::InstanceError::OperationNotSupported {
                operation: "list_users RPC on local backend".to_string(),
            }
            .into()),
            Backend::Remote(c) => c.list_users().await,
        }
    }

    // === Backend access ===

    /// Get access to the underlying `BackendImpl` (local only).
    ///
    /// Returns `None` for remote backends.
    pub fn as_backend_impl(&self) -> Option<&dyn BackendImpl> {
        match self {
            Backend::Local(b) => Some(&**b),
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(_) => None,
        }
    }

    /// Get access to the underlying `Arc<dyn BackendImpl>` (local only).
    ///
    /// Returns `None` for remote backends.
    pub fn as_arc_backend_impl(&self) -> Option<&Arc<dyn BackendImpl>> {
        match self {
            Backend::Local(b) => Some(b),
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(_) => None,
        }
    }

    /// Downcast to Any for concrete backend type access (local only).
    ///
    /// Panics on remote backends.
    pub fn as_any(&self) -> &dyn Any {
        match self {
            Backend::Local(b) => b.as_any(),
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(_) => panic!("as_any() not available on remote backend"),
        }
    }

    /// Check if this is a local backend.
    pub fn is_local(&self) -> bool {
        matches!(self, Backend::Local(_))
    }
}
