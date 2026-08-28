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

/// The inputs for materializing a multi-tip store state, resolved in one
/// call (see [`Backend::compute_merge_state`]).
///
/// When `merge_base` is `Some`, `path` holds every entry between the base
/// (exclusive) and the tips (inclusive), sorted by height then ID for the
/// CRDT fold. When it is `None` the tips share no common ancestor and the
/// caller materializes their full ancestry from the empty base — via a
/// batch entry fetch ([`Backend::store_at`]) rather than a path walk, so
/// `path` is empty.
#[derive(Debug, Clone)]
pub struct MergeSlice {
    /// The common dominator of the queried tips, or `None` for disjoint
    /// histories.
    pub merge_base: Option<ID>,
    /// Entries to fold on top of the base's state; empty when `merge_base`
    /// is `None`.
    pub path: Vec<ID>,
}

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

    /// Retrieve multiple entries by ID, preserving input order.
    ///
    /// Defaults to a [`Backend::get`] loop, which is the right shape for any
    /// in-process backend — the calls are local and there is nothing to
    /// batch. Backends that pay a per-call round-trip override this to fold
    /// the whole set into one request (see [`RemoteBackend`]).
    async fn get_entries(&self, ids: &[ID]) -> Result<Vec<Entry>> {
        let mut entries = Vec::with_capacity(ids.len());
        for id in ids {
            entries.push(self.get(id).await?);
        }
        Ok(entries)
    }

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

    /// The merge base of `entry_ids` within `store` and the path of entries
    /// from that base to them, resolved together.
    ///
    /// Base and path are one query on purpose: resolving them in separate
    /// calls lets the answers come from two different views of the store —
    /// on a remote backend, two RPCs a sync ingest can land between — and a
    /// path anchored at a base the caller never saw folds into a silently
    /// truncated state.
    async fn compute_merge_state(
        &self,
        tree: &ID,
        store: &str,
        entry_ids: &[ID],
    ) -> Result<MergeSlice>;

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
