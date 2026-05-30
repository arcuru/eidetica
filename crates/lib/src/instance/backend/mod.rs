//! The `Backend` seam: the single storage abstraction higher-level code
//! (`Transaction`, `Store`, `Database`, `Instance`) operates through, with no
//! branching on local vs remote.
//!
//! [`LocalBackend`] wraps a concrete in-process storage engine
//! ([`BackendImpl`](crate::backend::BackendImpl)); [`RemoteBackend`] wraps a
//! [`RemoteConnection`](crate::service::client::RemoteConnection) and
//! translates each method to a wire RPC. The trait is the *intersection* of
//! what both can honor with the same meaning — storage primitives that have no
//! authorisable remote shape (secrets, verification-status mutation, raw tree
//! dumps, scope-keyed cache) are deliberately **not** on the trait. They live
//! on the concrete local engine, reached only where one exists via
//! [`Backend::local_engine`].

mod local;
#[cfg(all(unix, feature = "service"))]
mod remote;

pub use local::LocalBackend;
#[cfg(all(unix, feature = "service"))]
pub use remote::RemoteBackend;

use std::sync::Arc;

use async_trait::async_trait;

#[cfg(all(unix, feature = "service"))]
use crate::service::client::RemoteConnection;
use crate::{
    Result,
    backend::{BackendImpl, InstanceMetadata, VerificationStatus},
    entry::{Entry, ID},
    instance::WriteSource,
    snapshot::Snapshot,
};

/// The storage operations `Transaction`/`Store`/`Database`/`Instance` perform,
/// independent of whether storage is in-process or served by a daemon.
///
/// Tree-scoped methods take the tree explicitly; the remote implementation uses
/// the argument directly (callers already pass the owning database's root), so
/// no per-handle root needs to be bound. The only per-handle state a remote
/// backend carries is its acting identity (see [`RemoteBackend`]).
#[async_trait]
pub trait Backend: Send + Sync + std::fmt::Debug {
    /// Retrieve an entry by ID.
    async fn get(&self, id: &ID) -> Result<Entry>;

    /// Raw [`Snapshot`] of `tree` (no Verified-frontier filtering — that stays
    /// in `Database`).
    async fn snapshot(&self, tree: &ID) -> Result<Snapshot>;

    /// Raw [`Snapshot`] of `store` within `tree`.
    async fn store_snapshot(&self, tree: &ID, store: &str) -> Result<Snapshot>;

    /// Store snapshot reachable as of a specific main-tree snapshot.
    async fn store_snapshot_at(
        &self,
        tree: &ID,
        store: &str,
        main_snapshot: &Snapshot,
    ) -> Result<Snapshot>;

    /// Every entry of `store` reachable from `snapshot`.
    async fn store_at(&self, tree: &ID, store: &str, snapshot: &Snapshot) -> Result<Vec<Entry>>;

    /// Lowest common ancestor of `entry_ids` within `store`.
    async fn find_merge_base(&self, tree: &ID, store: &str, entry_ids: &[ID]) -> Result<ID>;

    /// Every entry on the path from `from_id` to each of `to_ids` within
    /// `store`.
    async fn get_path_from_to(
        &self,
        tree: &ID,
        store: &str,
        from_id: &ID,
        to_ids: &[ID],
    ) -> Result<Vec<ID>>;

    /// Cached materialized CRDT state for `(entry_id, store)` within `tree`, if
    /// present. `tree` keys the daemon-side cache and gates the wire RPC; the
    /// local engine ignores it (it serves the trusted shared scope).
    async fn get_cached_crdt_state(
        &self,
        tree: &ID,
        entry_id: &ID,
        store: &str,
    ) -> Result<Option<Vec<u8>>>;

    /// Cache materialized CRDT state for `(entry_id, store)` within `tree`.
    async fn cache_crdt_state(
        &self,
        tree: &ID,
        entry_id: &ID,
        store: &str,
        state: Vec<u8>,
    ) -> Result<()>;

    /// Persist an entry. Local stores it directly; remote submits it via
    /// `DatabaseOp::SubmitSignedEntry` (stored `Unverified`, server-verified).
    async fn put(&self, entry: Entry) -> Result<()>;

    /// Durably persist a signed entry, applying `verification` locally or
    /// submitting it over the wire. `source` informs local callback dispatch
    /// (handled by `Instance::put_entry`) and is unused on remote.
    async fn write_entry(
        &self,
        verification: VerificationStatus,
        entry: Entry,
        source: WriteSource,
    ) -> Result<()>;

    /// Public instance metadata (device identity, system database IDs).
    async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>>;

    /// Persist public instance metadata.
    async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()>;

    /// The concrete in-process storage engine, if this is a local backend.
    ///
    /// Off-seam local-only operations (instance secrets, verification-status
    /// mutation, `all_roots`/`get_tree` raw dumps, scope-keyed cache) are
    /// reached through this accessor, so they are usable only where a concrete
    /// local backend exists. Returns `None` for remote backends.
    fn local_engine(&self) -> Option<Arc<dyn BackendImpl>> {
        None
    }

    /// The remote connection, if this is a remote backend. Returns `None` for
    /// local backends.
    #[cfg(all(unix, feature = "service"))]
    fn remote_connection(&self) -> Option<RemoteConnection> {
        None
    }
}
