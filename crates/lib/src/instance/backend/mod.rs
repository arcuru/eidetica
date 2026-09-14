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

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;

#[cfg(all(unix, feature = "service"))]
use crate::service::client::RemoteConnection;
use crate::{
    Result,
    backend::{
        BackendError, BackendImpl, HistorylessOwner, HistorylessReadSnapshot,
        HistorylessStoreMutation, InstanceMetadata, LegacyHistorylessSnapshot, RecordMutations,
        RecordPage, RecordRange, RecordView, StagingToken, StoreStateRequest, VerificationStatus,
    },
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

/// Storage operations shared by transactions, Stores, databases, and instances,
/// whether storage is local or served by a daemon.
///
/// Tree-scoped methods take the tree explicitly; the remote implementation uses
/// the argument directly (callers already pass the owning database's root), so
/// no per-handle root needs to be bound. The only per-handle state a remote
/// backend carries is its acting identity (see [`RemoteBackend`]).
#[async_trait]
pub trait Backend: Send + Sync + std::fmt::Debug {
    async fn resolve_store_state(
        &self,
        _request: &StoreStateRequest,
    ) -> Result<Option<RecordView>> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }
    async fn begin_store_state_staging(&self, _request: StoreStateRequest) -> Result<StagingToken> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }
    async fn stage_store_state_records(
        &self,
        _token: &StagingToken,
        _records: RecordMutations,
    ) -> Result<()> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }
    async fn publish_store_state(&self, _token: StagingToken) -> Result<RecordView> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }
    async fn abort_store_state(&self, _token: StagingToken) -> Result<()> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }
    async fn store_state_record_get(
        &self,
        _view: &RecordView,
        _key: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }
    async fn store_state_record_scan(
        &self,
        _view: &RecordView,
        _range: &RecordRange,
        _after: Option<&[u8]>,
        _limit: usize,
    ) -> Result<RecordPage> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }
    async fn clear_derived_store_state(&self) -> Result<()> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }
    /// Create an authoritative historyless database at revision zero.
    async fn create_historyless(
        &self,
        _id: &ID,
        _owner: HistorylessOwner,
        _stores: BTreeMap<String, HistorylessStoreMutation>,
    ) -> Result<()> {
        Err(BackendError::HistorylessStorageUnsupported.into())
    }

    async fn create_historyless_initialized(
        &self,
        id: &ID,
        owner: HistorylessOwner,
        stores: BTreeMap<String, HistorylessStoreMutation>,
    ) -> Result<()> {
        self.create_historyless(id, owner, stores).await
    }

    async fn begin_historyless_read(&self, _id: &ID) -> Result<HistorylessReadSnapshot> {
        Err(BackendError::HistorylessStorageUnsupported.into())
    }

    async fn historyless_record_get(
        &self,
        _snapshot: &HistorylessReadSnapshot,
        _store: &str,
        _key: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        Err(BackendError::HistorylessStorageUnsupported.into())
    }

    async fn historyless_record_scan(
        &self,
        _snapshot: &HistorylessReadSnapshot,
        _store: &str,
        _range: &RecordRange,
        _after: Option<&[u8]>,
        _limit: usize,
    ) -> Result<RecordPage> {
        Err(BackendError::HistorylessStorageUnsupported.into())
    }

    async fn commit_historyless(
        &self,
        _id: &ID,
        _expected_revision: u64,
        _stores: BTreeMap<String, HistorylessStoreMutation>,
    ) -> Result<u64> {
        Err(BackendError::HistorylessStorageUnsupported.into())
    }

    async fn release_historyless_read(&self, _snapshot: HistorylessReadSnapshot) -> Result<()> {
        Ok(())
    }

    async fn read_historyless_compat(&self, _id: &ID) -> Result<LegacyHistorylessSnapshot> {
        Err(BackendError::HistorylessStorageUnsupported.into())
    }

    async fn replace_historyless_compat(
        &self,
        _id: &ID,
        _expected_revision: u64,
        _stores: BTreeMap<String, Vec<u8>>,
    ) -> Result<u64> {
        Err(BackendError::HistorylessStorageUnsupported.into())
    }

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
    /// mutation, and `all_roots`/`get_tree` raw dumps) are
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
