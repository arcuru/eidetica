//! Backend wrapper for Instance-level operations
//!
//! This module provides the `Backend` enum which dispatches storage operations
//! to either a local `BackendImpl` or a remote service connection.

use std::any::Any;
use std::sync::Arc;

use crate::{
    Result,
    backend::{BackendImpl, CacheScope, InstanceMetadata, InstanceSecrets, VerificationStatus},
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

/// For `BackendImpl` methods that are deliberately **not** exposed over the
/// service wire (backend-internal primitives, no production remote caller).
/// Local delegates as usual; Remote fails fast with `OperationNotSupported`
/// rather than silently mirroring an unauthorisable op onto the socket.
macro_rules! local_only {
    ($self:expr, $op:literal, $method:ident ( $($arg:expr),* $(,)? )) => {
        match $self {
            Backend::Local(b) => b.$method($($arg),*).await,
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(_) => Err(crate::instance::InstanceError::OperationNotSupported {
                operation: concat!($op, " on remote backend").to_string(),
            }
            .into()),
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
        local_only!(self, "get_verification_status", get_verification_status(id))
    }

    /// Store an entry. Local delegates to BackendImpl; remote uses
    /// `DatabaseOp::SubmitSignedEntry` via the connection.
    pub async fn put(&self, entry: Entry) -> Result<()> {
        match self {
            Backend::Local(b) => b.put(entry).await,
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(c) => {
                let identity = c.session_identity().unwrap_or_default();
                let root_id = entry.root().unwrap_or(entry.id());
                c.submit_signed_entry(root_id, identity, entry).await
            }
        }
    }

    /// Update the verification status of an entry.
    ///
    /// Local backends apply the change directly. Remote backends return
    /// `OperationNotSupported` — `update_verification_status` is not exposed
    /// over the wire because verification flags should be derived by each
    /// node from its own validation pass, not flipped by a peer.
    pub async fn update_verification_status(
        &self,
        id: &ID,
        status: VerificationStatus,
    ) -> Result<()> {
        match self {
            Backend::Local(b) => b.update_verification_status(id, status).await,
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(_) => Err(crate::instance::InstanceError::OperationNotSupported {
                operation: "update_verification_status on remote backend".to_string(),
            }
            .into()),
        }
    }

    pub async fn get_entries_by_verification_status(
        &self,
        status: VerificationStatus,
    ) -> Result<Vec<ID>> {
        local_only!(
            self,
            "get_entries_by_verification_status",
            get_entries_by_verification_status(status)
        )
    }

    // === Tips (local-only) ===

    /// Get tips for a tree. Local delegates to BackendImpl; remote uses
    /// `DatabaseOp::GetVerifiedTips` via the connection. Returns empty
    /// for not-yet-existing databases (needed by `Database::create`).
    pub async fn get_tips(&self, tree: &ID) -> Result<Vec<ID>> {
        match self {
            Backend::Local(b) => b.get_tips(tree).await,
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(c) => {
                let identity = c.session_identity().unwrap_or_default();
                match c.get_verified_tips(tree.clone(), identity).await {
                    Ok(tips) => Ok(tips),
                    Err(e) if e.is_not_found() => Ok(Vec::new()),
                    Err(e) => Err(e),
                }
            }
        }
    }

    /// Get store tips. Local delegates to BackendImpl; remote uses the
    /// store-specific `GetStoreTipsUpToEntries` wire RPC against the tree's
    /// current verified tips (the "store tips" semantic = store-specific
    /// tips reachable from the current tree tips).
    pub async fn get_store_tips(&self, tree: &ID, store: &str) -> Result<Vec<ID>> {
        match self {
            Backend::Local(b) => b.get_store_tips(tree, store).await,
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(c) => {
                let identity = c.session_identity().unwrap_or_default();
                let tree_tips = match c.get_verified_tips(tree.clone(), identity.clone()).await {
                    Ok(tips) => tips,
                    Err(e) if e.is_not_found() => return Ok(Vec::new()),
                    Err(e) => return Err(e),
                };
                if tree_tips.is_empty() {
                    return Ok(Vec::new());
                }
                match c
                    .get_store_tips_up_to_entries(
                        tree.clone(),
                        identity,
                        store.to_string(),
                        tree_tips,
                    )
                    .await
                {
                    Ok(tips) => Ok(tips),
                    Err(e) if e.is_not_found() => Ok(Vec::new()),
                    Err(e) => Err(e),
                }
            }
        }
    }

    /// Get store tips up to entries. Local delegates to BackendImpl; remote
    /// uses the store-specific `GetStoreTipsUpToEntries` wire RPC directly
    /// against `up_to` (the parameter passed in).
    pub async fn get_store_tips_up_to_entries(
        &self,
        tree: &ID,
        store: &str,
        up_to: &[ID],
    ) -> Result<Vec<ID>> {
        match self {
            Backend::Local(b) => b.get_store_tips_up_to_entries(tree, store, up_to).await,
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(c) => {
                let identity = c.session_identity().unwrap_or_default();
                match c
                    .get_store_tips_up_to_entries(
                        tree.clone(),
                        identity,
                        store.to_string(),
                        up_to.to_vec(),
                    )
                    .await
                {
                    Ok(tips) => Ok(tips),
                    Err(e) if e.is_not_found() => Ok(Vec::new()),
                    Err(e) => Err(e),
                }
            }
        }
    }

    // === Tree/Store traversal (local-only) ===

    /// Enumerate every database root in the backend. Local-only.
    pub async fn all_roots(&self) -> Result<Vec<ID>> {
        local_only!(self, "all_roots", all_roots())
    }

    /// All entries in the tree. Local-only.
    pub async fn get_tree(&self, tree: &ID) -> Result<Vec<Entry>> {
        local_only!(self, "get_tree", get_tree(tree))
    }

    /// All entries in `store` reachable from `tips`. Local delegates to
    /// BackendImpl; remote uses `DatabaseOp::GetStoreEntries`.
    pub async fn get_store_from_tips(
        &self,
        tree: &ID,
        store: &str,
        tips: &[ID],
    ) -> Result<Vec<Entry>> {
        match self {
            Backend::Local(b) => b.get_store_from_tips(tree, store, tips).await,
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(c) => {
                let identity = c.session_identity().unwrap_or_default();
                c.get_store_entries(
                    tree.clone(),
                    identity,
                    store.to_string(),
                    tips.to_vec(),
                    crate::service::protocol::ReadScope::Verified,
                )
                .await
            }
        }
    }

    /// Lowest common ancestor. Local delegates to BackendImpl; remote
    /// uses the `ComputeMergeState` wire RPC (which fuses LCA + path).
    pub async fn find_merge_base(&self, tree: &ID, store: &str, entry_ids: &[ID]) -> Result<ID> {
        match self {
            Backend::Local(b) => b.find_merge_base(tree, store, entry_ids).await,
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(c) => {
                let identity = c.session_identity().unwrap_or_default();
                let state = c
                    .compute_merge_state(
                        tree.clone(),
                        identity,
                        store.to_string(),
                        entry_ids.to_vec(),
                    )
                    .await?;
                Ok(state.merge_base)
            }
        }
    }

    /// Path from `from_id` to each `to_id`. Local delegates to BackendImpl;
    /// remote uses the `ComputeMergeState` wire RPC. The `from_id` argument
    /// is informational here — the server fuses LCA + path against `to_ids`
    /// in one round-trip, so a separately-supplied LCA isn't replayed.
    pub async fn get_path_from_to(
        &self,
        tree_id: &ID,
        store: &str,
        from_id: &ID,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        match self {
            Backend::Local(b) => b.get_path_from_to(tree_id, store, from_id, to_ids).await,
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(c) => {
                let _ = from_id;
                let identity = c.session_identity().unwrap_or_default();
                let state = c
                    .compute_merge_state(
                        tree_id.clone(),
                        identity,
                        store.to_string(),
                        to_ids.to_vec(),
                    )
                    .await?;
                Ok(state.path)
            }
        }
    }

    pub async fn collect_root_to_target(
        &self,
        tree: &ID,
        store: &str,
        target: &ID,
    ) -> Result<Vec<ID>> {
        local_only!(
            self,
            "collect_root_to_target",
            collect_root_to_target(tree, store, target)
        )
    }

    // === Internal primitives (local-only) ===

    pub async fn clear_crdt_cache(&self) -> Result<()> {
        match self {
            Backend::Local(b) => b.clear_crdt_cache().await,
            // No local cache exists on a connected instance — the daemon
            // owns the cache server-side. Cache-invalidation calls from
            // client-side test fixtures become no-ops.
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(_) => Ok(()),
        }
    }

    /// Get cached CRDT state within a scope. Local delegates to
    /// BackendImpl; remote returns None (remote handles never see the
    /// in-process backend cache — they go through the service wire).
    pub async fn get_cached_crdt_state(
        &self,
        scope: &CacheScope,
        entry_id: &ID,
        store: &str,
    ) -> Result<Option<Vec<u8>>> {
        match self {
            Backend::Local(b) => b.get_cached_crdt_state(scope, entry_id, store).await,
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(_) => Ok(None),
        }
    }

    /// Cache CRDT state within a scope. Local delegates to BackendImpl;
    /// remote is a no-op.
    pub async fn cache_crdt_state(
        &self,
        scope: CacheScope,
        entry_id: &ID,
        store: &str,
        state: Vec<u8>,
    ) -> Result<()> {
        match self {
            Backend::Local(b) => b.cache_crdt_state(scope, entry_id, store, state).await,
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(_) => {
                let _ = (scope, entry_id, store, state);
                Ok(())
            }
        }
    }

    pub async fn get_sorted_store_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        store: &str,
    ) -> Result<Vec<ID>> {
        local_only!(
            self,
            "get_sorted_store_parents",
            get_sorted_store_parents(tree_id, entry_id, store)
        )
    }

    // === Instance metadata ===

    pub async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>> {
        dispatch!(self, get_instance_metadata())
    }

    pub async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()> {
        dispatch!(self, set_instance_metadata(metadata))
    }

    // === Instance secrets ===

    /// Get instance secrets. Local-only.
    pub async fn get_instance_secrets(&self) -> Result<Option<InstanceSecrets>> {
        match self {
            Backend::Local(b) => b.get_instance_secrets().await,
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(_) => Err(crate::instance::InstanceError::OperationNotSupported {
                operation: "get_instance_secrets on remote backend".to_string(),
            }
            .into()),
        }
    }

    /// Set instance secrets. Local-only.
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
    /// For local backends, this persists the entry and promotes verification
    /// status if non-`Unverified`. For remote backends, this submits the
    /// entry via `DatabaseOp::SubmitSignedEntry`.
    ///
    /// Local callback dispatch is handled by `Instance::put_entry()`.
    pub async fn write_entry(
        &self,
        verification: VerificationStatus,
        entry: Entry,
        source: WriteSource,
    ) -> Result<()> {
        match self {
            Backend::Local(b) => {
                let entry_id = entry.id();
                b.put(entry).await?;
                if verification != VerificationStatus::Unverified {
                    b.update_verification_status(&entry_id, verification)
                        .await?;
                }
                Ok(())
            }
            #[cfg(all(unix, feature = "service"))]
            Backend::Remote(c) => {
                let identity = c.session_identity().unwrap_or_default();
                // Use the entry's own id as the tree root for genesis
                // entries (root=ID::default()), and the entry's claimed
                // root for non-genesis entries.
                // For genesis entries (root=None or root=zero-id), the
                // tree is the entry's own id. For non-root entries, use
                // the entry's claimed root.
                let tree_root = entry.root().unwrap_or(entry.id());
                c.submit_signed_entry(tree_root, identity, entry).await?;
                let _ = source;
                Ok(())
            }
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
