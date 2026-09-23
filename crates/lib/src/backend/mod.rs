//! Backend implementations for Eidetica storage
//!
//! This module provides the core `BackendImpl` trait and various backend implementations
//! organized by category (database, file, network, cloud).
//!
//! The `BackendImpl` trait defines the interface for storing and retrieving `Entry` objects.
//! This allows the core database logic (`Instance`, `Database`) to be independent of the specific storage mechanism.
//!
//! Instance wraps BackendImpl in a `Backend` struct that provides a layer for future development.

use std::{any::Any, collections::BTreeMap};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    Result,
    auth::crypto::{PrivateKey, PublicKey},
    entry::{Entry, ID},
    snapshot::Snapshot,
};

/// Trust/visibility scope for a cached CRDT state entry.
///
/// Cached state holds the same kind of data — opaque serialized
/// Store state — regardless of where they came from. They differ only in
/// *provenance*, which determines who is allowed to see them:
///
/// - **Shared**: state the daemon materialized itself via a local
///   Transaction. The daemon is the trusted computer, so these bytes are
///   good for any user with read permission on the database. Encrypted
///   stores never land here (the daemon has no encryptor key — see
///   [`crate::store::PasswordStore`]), so shared state is always plaintext.
///
/// - **User(uuid)**: state a specific user computed and published over the
///   service wire. The daemon cannot verify the merge result, so it is
///   scoped to that user only — alice's publication is invisible to bob.
///   This is where encrypted-store materializations live (the client
///   decrypts, merges, re-encrypts, and publishes the ciphertext).
///
/// On read, the wire handler resolves `User(session_user)` first and falls
/// back to `Shared` on miss — so a remote read of an unencrypted store
/// benefits from cross-user dedup via the shared scope, while encrypted
/// store reads only ever hit user-scoped cached state.
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheScope {
    /// Daemon-computed; visible to every user with database read permission.
    Shared,
    /// Client-uploaded; visible only to the named user.
    User(String),
}

/// Lifecycle of stored Store state.
///
/// Derived state is a disposable cached materialization of historical
/// Entries. Authoritative state is durable current state. Staging is a
/// private unpublished build and is never visible to record reads.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreStateLifecycle {
    Derived,
    Authoritative,
    Staging,
}

impl StoreStateLifecycle {
    pub(crate) fn as_db_int(self) -> i64 {
        match self {
            Self::Derived => 0,
            Self::Authoritative => 1,
            Self::Staging => 2,
        }
    }
}

/// Identifies a Store's cached record format.
///
/// Store implementations change the version when the encoding becomes
/// incompatible. Backends compare this value opaquely when resolving cached
/// state.
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionDescriptor {
    pub name: String,
    pub version: u32,
}

/// An immutable view onto a published record set.
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordView {
    pub(crate) namespace_id: String,
}

/// Metadata used to resolve or build cached state.
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreStateRequest {
    pub database: ID,
    pub store: String,
    pub lifecycle: StoreStateLifecycle,
    pub scope: CacheScope,
    pub projection: ProjectionDescriptor,
    /// Canonical historical source key. Empty only for authoritative state.
    pub source_key: Vec<u8>,
}

/// Opaque token for one private unpublished build.
///
/// Minted by [`BackendImpl::begin_store_state_staging`] and used to stage,
/// publish, or abort that build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagingToken {
    pub(crate) namespace_id: String,
    pub(crate) target: StoreStateRequest,
}

/// Backend-owned outcome of a staging token. Terminal outcomes remain queryable
/// even after the private records have been reclaimed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StagingStatus {
    Active,
    Published(RecordView),
    Adopted(RecordView),
    Aborted,
    Expired,
}

#[cfg(feature = "testing")]
impl StagingToken {
    /// Test-only accessor for the pause-gate registry key.
    ///
    /// Exists so integration tests can register a stage pause for a token
    /// they hold; compiled out of every production build.
    pub fn testing_namespace_id(&self) -> &str {
        &self.namespace_id
    }
}

/// Ordered record changes.
///
/// Legacy collapsed record overlay used by the current Doc-backed Table.
/// `None` is a tombstone in this overlay; the explicit staging API below
/// instead removes a physical row.
pub type RecordMutations = BTreeMap<Vec<u8>, Option<Vec<u8>>>;

/// One physical change to a private generation. Order is significant even for
/// repeated keys, and deletes remove the stored row rather than storing null.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordMutation {
    Put { key: Vec<u8>, value: Vec<u8> },
    Delete { key: Vec<u8> },
}

/// Half-open byte-key range `[start, end)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordRange {
    pub start: Option<Vec<u8>>,
    pub end: Option<Vec<u8>>,
}

/// One bounded ordered scan page. `next` is the last returned key and must be
/// supplied as the exclusive continuation on the next request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordPage {
    pub records: Vec<(Vec<u8>, Vec<u8>)>,
    pub next: Option<Vec<u8>>,
}

impl CacheScope {
    /// Storage key for the scope — `None` encodes [`Self::Shared`], `Some`
    /// encodes [`Self::User`]. Useful for backends that need a single
    /// nullable column or a uniform key prefix (e.g. SQL primary keys,
    /// Redis key formatting).
    pub fn storage_key(&self) -> Option<&str> {
        match self {
            CacheScope::Shared => None,
            CacheScope::User(uuid) => Some(uuid.as_str()),
        }
    }
}

/// Persistent public metadata for an Eidetica instance.
///
/// This struct consolidates all instance-level state that needs to persist across restarts:
/// - The device public key (cryptographic identity)
/// - System database root IDs
/// - Optional sync database root ID
///
/// The presence of `InstanceMetadata` in a backend indicates an initialized instance.
/// A backend without metadata is treated as uninitialized and may trigger instance creation.
///
/// This struct contains only public information and is safe to transmit over the wire
/// (e.g., to remote clients via RPC). Private key material is stored separately in
/// [`InstanceSecrets`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceMetadata {
    /// Device public key - the instance's cryptographic identity.
    ///
    /// This is the public half of the device signing key, generated once during instance
    /// creation and persisted for the lifetime of the instance. Used for identity
    /// verification and sync peer identification.
    pub id: PublicKey,

    /// Root ID of the _users system database.
    ///
    /// This database tracks user accounts and their associated data.
    pub users_db: ID,

    /// Root ID of the _databases system database.
    ///
    /// This database tracks metadata about all databases in the instance.
    pub databases_db: ID,

    /// Root ID of the _sync database (None until `enable_sync()` is called).
    ///
    /// This database stores all sync-related state.
    pub sync_db: Option<ID>,
}

/// Private secrets for an Eidetica instance.
///
/// This struct holds the device signing key, which must never be transmitted
/// over the wire or exposed to remote clients. It is stored separately from
/// [`InstanceMetadata`] to enforce this boundary.
// FIXME: Better secrets management everywhere for InstanceSecrets
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceSecrets {
    /// Device signing key - the instance's private cryptographic identity.
    ///
    /// This key is generated once during instance creation and persists for the lifetime
    /// of the instance. It is used to sign system database entries and for sync identity.
    pub(crate) signing_key: PrivateKey,
}

// Category modules
pub mod database;
pub mod errors;

// Re-export main types for easier access
pub use errors::BackendError;

/// Verdict of a bounded, tree-scoped reachability query
/// ([`check_targets_reachable_from`](BackendImpl::check_targets_reachable_from)).
///
/// The three states exist so a caller can tell a *proven* negative apart from
/// "not enough local history to decide" — a distinction that matters for a sync
/// system, where the latter is transient and self-heals once more of the tree
/// arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reachability {
    /// Every target is an ancestor-or-equal of some `from` entry, proven
    /// against fully-present history within the height bound.
    Reachable,
    /// Proven negative: the region at or above the target floor was fully
    /// present locally and at least one target is not an ancestor of `from`
    /// (e.g. a snapshot regression, or a foreign/fabricated tip).
    Unreachable,
    /// Undecidable from local history: a target, a `from` tip, or an ancestor
    /// needed to reach a target is missing locally. `missing` is the set of
    /// entries whose absence blocked the decision — the caller should sync
    /// these and re-check. It is the current blocking *frontier*, not
    /// necessarily the whole gap: fetching it may reveal a further layer. The
    /// height bound keeps this set small.
    Indeterminate {
        /// Entries that must be synced before the query can be decided.
        missing: Vec<ID>,
    },
}

/// Verification status for entries in the backend.
///
/// This enum tracks whether an entry has been cryptographically verified
/// by the higher-level authentication system. The backend stores this status
/// but does not perform verification itself - that's handled by the Database/Transaction layers.
///
/// Only the local validation pass (`Transaction`) may assign `Verified`: it
/// is the sole code path that has actually checked the entry's signature and
/// permissions. Anything arriving from outside this node — over the sync
/// protocol or the service wire — enters as `Unverified` and can only be
/// promoted later by a local re-verification pass. A peer cannot assert
/// `Verified` for us; the wire carries no verification status.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Default,
)]
pub enum VerificationStatus {
    /// Entry has been cryptographically verified as authentic by *this*
    /// node's local validation pass. The default for locally created and
    /// signed entries; never assignable from off-node input.
    #[default]
    Verified,
    /// Entry has not yet been verified by this node — received before
    /// verification could complete (e.g. a delegated/`_settings` tree it
    /// depends on has not arrived yet). Transient and promotable: a future
    /// re-verification pass moves it to `Verified` once its pinned
    /// settings-ancestor set is present. Admitted into state, flagged.
    Unverified,
    /// Entry was checked and *definitively* failed verification — invalid
    /// signature, revoked key, etc. Terminal; never promoted.
    Failed,
}

impl VerificationStatus {
    /// Canonical persistence encoding. The single source of truth for the
    /// integer stored in the `verification_status` column; all backends use
    /// this rather than open-coding the mapping.
    pub fn as_db_int(self) -> i64 {
        match self {
            VerificationStatus::Verified => 0,
            VerificationStatus::Failed => 1,
            VerificationStatus::Unverified => 2,
        }
    }

    /// Inverse of [`as_db_int`](Self::as_db_int). Errors on an unknown code
    /// instead of silently collapsing it to `Failed` — a stray value means
    /// storage corruption, not a failed verification.
    ///
    /// This codec is intentionally *additively extensible*: a future state
    /// (e.g. a peer-attested `Trusted`) takes a fresh, never-reused integer.
    /// Old data keeps decoding; an old reader rejects the new code rather
    /// than misinterpreting it; and the wire carries no status at all, so
    /// adding a state is not a protocol change. Source-level it is
    /// deliberately *not* non-breaking — the `match` arms here and on
    /// `VerificationStatus` elsewhere are exhaustive so the compiler
    /// enumerates every site that must consciously handle the new state.
    pub fn from_db_int(code: i64) -> Result<Self> {
        match code {
            0 => Ok(VerificationStatus::Verified),
            1 => Ok(VerificationStatus::Failed),
            2 => Ok(VerificationStatus::Unverified),
            other => Err(BackendError::TreeIntegrityViolation {
                reason: format!("unknown verification_status code {other} in storage"),
            }
            .into()),
        }
    }
}

/// BackendImpl trait abstracting the underlying storage mechanism for Eidetica entries.
///
/// This trait defines the essential operations required for storing, retrieving,
/// and querying entries and their relationships within databases and stores.
/// Implementations of this trait handle the specifics of how data is persisted
/// (e.g., in memory, on disk, in a remote database).
///
/// Much of the performance-critical logic, particularly concerning tree traversal
/// and tip calculation, resides within `BackendImpl` implementations, as the optimal
/// approach often depends heavily on the underlying storage characteristics.
///
/// All backend implementations must be `Send` and `Sync` to allow sharing across threads,
/// and implement `Any` to allow for downcasting if needed.
///
/// Instance wraps BackendImpl in a `Backend` struct that provides additional coordination
/// and will enable future development.
///
/// ## Verification Status
///
/// The backend stores a verification status for each entry, indicating whether
/// the entry has been authenticated by the higher-level authentication system.
/// The backend itself does not perform verification - it only stores the status
/// set by the calling code (typically Database/Transaction implementations).
#[async_trait]
pub trait BackendImpl: Send + Sync + Any {
    /// Look up the published cached state for an exact request. Private
    /// builds are never returned.
    async fn resolve_store_state(
        &self,
        _request: &StoreStateRequest,
    ) -> Result<Option<RecordView>> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }

    /// Start a private build for a later atomic publish. The build is invisible
    /// to readers until published.
    async fn begin_store_state_staging(&self, _request: StoreStateRequest) -> Result<StagingToken> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }

    /// Query the backend-owned lifecycle, including terminal outcomes after
    /// private records have been removed. Unknown tokens are not safe to retry.
    async fn store_state_staging_status(
        &self,
        _token: &StagingToken,
    ) -> Result<Option<StagingStatus>> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }

    /// Look up an opaque token for a service request. The service MUST check
    /// database and user scope before exposing its status or performing writes.
    async fn store_state_staging_token(
        &self,
        _id: &str,
    ) -> Result<Option<(StagingToken, StagingStatus)>> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }

    /// Atomically validate a sequenced wire chunk and apply it once. Digest
    /// covers the encoded ordered wire mutations, before key collapse.
    async fn stage_store_state_chunk(
        &self,
        _token: &StagingToken,
        _sequence: u64,
        _digest: &[u8],
        _records: RecordMutations,
    ) -> Result<()> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }

    /// Apply an explicitly ordered physical chunk exactly once. Its digest is
    /// bound to the encoded wire chunk by the service, before any key collapse.
    async fn stage_store_state_ordered_chunk(
        &self,
        _token: &StagingToken,
        _sequence: u64,
        _digest: &[u8],
        _mutations: Vec<RecordMutation>,
    ) -> Result<()> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }

    /// Renew a private build's lease after an acknowledged operation.
    async fn renew_store_state_staging(&self, _token: &StagingToken) -> Result<()> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }

    /// Reclaim expired private builds. An active lease is never reclaimed.
    async fn reclaim_expired_store_state(&self) -> Result<u64> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }

    /// Test-only lease clock advance; never exposed through the service.
    #[cfg(feature = "testing")]
    async fn testing_age_store_state_staging(
        &self,
        _token: &StagingToken,
        _seconds: i64,
    ) -> Result<()> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }

    /// Add a chunk of records to a private build.
    async fn stage_store_state_records(
        &self,
        _token: &StagingToken,
        _records: RecordMutations,
    ) -> Result<()> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }

    /// Atomically publish a finished build as the cached state for its target.
    ///
    /// Returns the published record set. When a concurrent materializer published
    /// that same target first, its record set is adopted and this build is
    /// discarded — publishing the same state twice is a race, not an error.
    /// Repeating a publish with the same token is idempotent and must never
    /// remove the published state. Rejects a build holding a `None`-valued
    /// record, since staged deletes cannot be published yet.
    async fn publish_store_state(&self, _token: StagingToken) -> Result<RecordView> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }

    /// Discard a private build. Aborting an already-published token is harmless.
    async fn abort_store_state(&self, _token: StagingToken) -> Result<()> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }

    /// Read one exact record from published records. A missing key reads as
    /// `None`, never an error.
    async fn store_state_record_get(
        &self,
        _view: &RecordView,
        _key: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }

    /// Read one ordered page of records. A `limit` of zero yields an empty page
    /// with no continuation. A view that no longer identifies published
    /// records errors with `InvalidStoreStateView`, never an empty page.
    async fn store_state_record_scan(
        &self,
        _view: &RecordView,
        _range: &RecordRange,
        _after: Option<&[u8]>,
        _limit: usize,
    ) -> Result<RecordPage> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }

    /// Clear cached derived state. Authoritative state and private
    /// builds are untouched.
    async fn clear_derived_store_state(&self) -> Result<()> {
        Err(BackendError::StoreStateStorageUnsupported.into())
    }
    /// Retrieves an entry by its unique content-addressable ID.
    ///
    /// # Arguments
    /// * `id` - The ID of the entry to retrieve.
    ///
    /// # Returns
    /// A `Result` containing the `Entry` if found, or an `Error::NotFound` otherwise.
    /// Returns an owned copy to support concurrent access with internal synchronization.
    async fn get(&self, id: &ID) -> Result<Entry>;

    /// Gets the verification status of an entry.
    ///
    /// # Arguments
    /// * `id` - The ID of the entry to check.
    ///
    /// # Returns
    /// A `Result` containing the `VerificationStatus` if the entry exists, or an `Error::NotFound` otherwise.
    async fn get_verification_status(&self, id: &ID) -> Result<VerificationStatus>;

    /// Stores an entry.
    ///
    /// A **new** entry is stored as [`VerificationStatus::Unverified`]. The
    /// storage API deliberately does **not** accept a verification status: no
    /// caller may assert that an entry is verified. `Verified` is reached
    /// only by this node's local validation pass, which stores via `put` and
    /// then promotes the entry with
    /// [`update_verification_status`](Self::update_verification_status).
    ///
    /// If an entry with the same ID already exists, `put` is a **no-op**:
    /// entries are content-addressed and immutable, so the content is
    /// identical, and the existing verification status is left **untouched**.
    /// A re-`put` therefore never demotes a prior local promotion — routine
    /// on overlapping/bootstrap sync, where an already-`Verified` entry is
    /// commonly re-received. Status transitions go only through
    /// [`update_verification_status`](Self::update_verification_status).
    ///
    /// # Arguments
    /// * `entry` - The `Entry` to store.
    ///
    /// # Returns
    /// A `Result` indicating success or an error during storage.
    async fn put(&self, entry: Entry) -> Result<()>;

    /// Updates the verification status of an existing entry.
    ///
    /// This is the **only** way an entry becomes `Verified`, and it is
    /// reserved for this node's local validation pass (and a future
    /// re-verification pass). It is local-only — never reachable over the
    /// service wire — so a peer can never assert verification for us.
    ///
    /// # Arguments
    /// * `id` - The ID of the entry to update
    /// * `verification_status` - The new verification status
    ///
    /// # Returns
    /// A `Result` indicating success or `Error::NotFound` if the entry doesn't exist.
    async fn update_verification_status(
        &self,
        id: &ID,
        verification_status: VerificationStatus,
    ) -> Result<()>;

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
    ) -> Result<Vec<ID>>;

    /// Returns the current [`Snapshot`] of `tree` — its sorted, deduplicated
    /// set of DAG tips.
    ///
    /// Tips are entries within `tree` that have no children *within that same
    /// tree*: an entry is a child of another iff it lists the other entry in
    /// its `parents` list.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the tree to snapshot.
    async fn snapshot(&self, tree: &ID) -> Result<Snapshot>;

    /// Returns the snapshot of a specific store within a given tree.
    ///
    /// Store tips are entries within the store that have no children *within
    /// that same store*. An entry is a child of another within a store if it
    /// lists the other entry in its `store_parents` list for that store name.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the parent tree.
    /// * `store` - The name of the store for which to find tips.
    async fn store_snapshot(&self, tree: &ID, store: &str) -> Result<Snapshot>;

    /// Returns the store snapshot as of a specific main-tree snapshot.
    ///
    /// Finds all store entries reachable from the boundary's tips, then filters
    /// to the ones that are tips within the store.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the parent tree.
    /// * `store` - The name of the store for which to find tips.
    /// * `main_snapshot` - Snapshot of the parent tree defining the boundary.
    async fn store_snapshot_at(
        &self,
        tree: &ID,
        store: &str,
        main_snapshot: &Snapshot,
    ) -> Result<Snapshot>;

    /// Retrieves the IDs of all top-level root entries stored in the backend.
    ///
    /// Top-level roots are entries that are themselves roots of a tree
    /// (i.e., `entry.is_root()` is true) and are not part of a larger tree structure
    /// tracked by the backend (conceptually, their `tree.root` field is empty or refers to themselves,
    /// though the implementation detail might vary). These represent the starting points
    /// of distinct trees managed by the database.
    ///
    /// # Returns
    /// A `Result` containing a vector of top-level root entry IDs or an error.
    async fn all_roots(&self) -> Result<Vec<ID>>;

    /// Finds the merge base (common dominator) of the given entry IDs within a store.
    ///
    /// The merge base is the lowest ancestor that ALL paths from ALL entries must pass through.
    /// This is different from the traditional LCA - if there are parallel paths that bypass
    /// a common ancestor, that ancestor is not the merge base. This is used to determine
    /// optimal computation boundaries for CRDT state calculation.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the tree
    /// * `store` - The name of the store context
    /// * `entry_ids` - The entry IDs to find the merge base for
    ///
    /// # Returns
    /// A `Result` containing `Some(id)` for the merge base, or `None` when the
    /// entries share no common ancestor.
    ///
    /// `None` is a valid result, not an error. A store created independently on
    /// two peers — it did not exist at the point they forked, so neither side's
    /// first write has a store parent in common — has two roots and no shared
    /// ancestor. Those histories merge from the empty base, the same way the
    /// main tree already merges two disjoint roots into a diamond. Callers
    /// materialize from a default state and fold the full ancestry.
    async fn find_merge_base(&self, tree: &ID, store: &str, entry_ids: &[ID])
    -> Result<Option<ID>>;

    /// Returns a reference to the backend instance as a dynamic `Any` type.
    ///
    /// This allows for downcasting to a concrete backend implementation if necessary,
    /// enabling access to implementation-specific methods. Use with caution.
    fn as_any(&self) -> &dyn Any;

    /// Retrieves all entries belonging to a specific tree, sorted topologically.
    ///
    /// The entries are sorted primarily by their height (distance from the root)
    /// and secondarily by their ID to ensure a consistent, deterministic order suitable
    /// for reconstructing the tree's history.
    ///
    /// **Note:** This potentially loads the entire history of the tree. Use cautiously,
    /// especially with large trees, as it can be memory-intensive.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the tree to retrieve.
    ///
    /// # Returns
    /// A `Result` containing a vector of all `Entry` objects in the tree,
    /// sorted topologically, or an error.
    async fn get_tree(&self, tree: &ID) -> Result<Vec<Entry>>;

    /// Retrieves all entries belonging to a specific store within a tree, sorted topologically.
    ///
    /// Similar to `get_tree`, but limited to entries that are part of the specified store.
    /// The entries are sorted primarily by their height within the store (distance
    /// from the store's initial entry/entries) and secondarily by their ID.
    ///
    /// **Note:** This potentially loads the entire history of the store. Use with caution.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the parent tree.
    /// * `store` - The name of the store to retrieve.
    ///
    /// # Returns
    /// A `Result` containing a vector of all `Entry` objects in the store,
    /// sorted topologically according to their position within the store, or an error.
    async fn get_store(&self, tree: &ID, store: &str) -> Result<Vec<Entry>>;

    /// Retrieves all entries belonging to a specific tree up to the given tips, sorted topologically.
    ///
    /// Similar to `get_tree`, but only includes entries that are ancestors of the provided tips.
    /// This allows reading from a specific state of the tree defined by those tips.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the tree to retrieve.
    /// * `tips` - The tip IDs defining the state to read from.
    ///
    /// # Returns
    /// A `Result` containing a vector of `Entry` objects in the tree up to the given tips,
    /// sorted topologically, or an error.
    ///
    /// # Errors
    /// - `EntryNotFound` if any tip doesn't exist locally
    /// - `EntryNotInTree` if any tip belongs to a different tree
    async fn get_tree_from_tips(&self, tree: &ID, tips: &[ID]) -> Result<Vec<Entry>>;

    /// Within `tree`, decide whether every entry in `targets` is an
    /// ancestor-or-equal of some entry in `from` — i.e. whether the `from`
    /// snapshot is at-or-ahead-of the `targets` snapshot.
    ///
    /// Returns a three-state [`Reachability`] rather than a bare bool so a
    /// *proven* negative is distinguishable from "not enough local history to
    /// decide" (see [`Reachability::Indeterminate`]). This is the cheap
    /// counterpart to [`get_tree_from_tips`](Self::get_tree_from_tips) when only
    /// an at-or-ahead-of answer is needed, not the materialised ancestor set.
    ///
    /// **Bounded by the target floor.** The walk never descends below the
    /// minimum target height: an entry below every remaining target cannot be
    /// one, nor reach one through still-lower parents (parent heights strictly
    /// decrease). Cost therefore tracks the height gap between `from` and
    /// `targets` on *both* the reachable and unreachable paths, not the size of
    /// the tree — which matters because this runs on every delegated-entry
    /// validation (and re-validation).
    ///
    /// **Validation is symmetric.** Both `from` and `targets` are checked to be
    /// real entries of `tree`. An entry that exists but belongs to another tree
    /// is a `from`/target integrity violation and errors. An entry that is
    /// *missing locally* is not a negative: it makes the verdict
    /// [`Indeterminate`](Reachability::Indeterminate) and is reported in
    /// `missing`, so a partially-synced history is never mistaken for a
    /// regression. Membership is *presence in the tree*, not
    /// `VerificationStatus::Verified`.
    ///
    /// A target equal to a `from` entry is reached; an empty `targets` is
    /// vacuously [`Reachable`](Reachability::Reachable).
    ///
    /// # Errors
    /// - `EntryNotInTree` if any `from` or `targets` entry exists but belongs to
    ///   a different tree
    ///
    /// The default implementation performs the walk via [`get`](Self::get); a
    /// backend may override it with a single-query traversal.
    async fn check_targets_reachable_from(
        &self,
        tree: &ID,
        from: &[ID],
        targets: &[ID],
    ) -> Result<Reachability> {
        use std::collections::HashSet;

        // An empty `from` snapshot dominates nothing: any required target is
        // definitively unreachable. This is a *proven* negative, not
        // Indeterminate — no amount of syncing lets an empty claim catch up to a
        // non-empty floor, so we never load the targets to decide it.
        if from.is_empty() && !targets.is_empty() {
            return Ok(Reachability::Unreachable);
        }

        // Validate targets and take the height floor. A target that EXISTS but
        // is foreign is an integrity violation; a MISSING target means we can't
        // establish the floor, so it becomes a want-list item (Indeterminate),
        // never a silent negative.
        let mut unmet: HashSet<ID> = HashSet::with_capacity(targets.len());
        let mut missing: Vec<ID> = Vec::new();
        let mut floor_height = u64::MAX;
        for target in targets {
            match self.get(target).await {
                Ok(entry) => {
                    if !entry.in_tree(tree) {
                        return Err(BackendError::EntryNotInTree {
                            entry_id: target.clone(),
                            tree_id: tree.clone(),
                        }
                        .into());
                    }
                    floor_height = floor_height.min(entry.height());
                    unmet.insert(target.clone());
                }
                Err(_) => {
                    unmet.insert(target.clone());
                    missing.push(target.clone());
                }
            }
        }

        let mut visited: HashSet<ID> = HashSet::with_capacity(from.len());
        let mut stack: Vec<ID> = Vec::new();

        // Seed with `from`: a foreign tip is a forgery (error); a missing tip is
        // a want-list item. Only expand parents of entries above the floor.
        for tip in from {
            match self.get(tip).await {
                Ok(entry) => {
                    if !entry.in_tree(tree) {
                        return Err(BackendError::EntryNotInTree {
                            entry_id: tip.clone(),
                            tree_id: tree.clone(),
                        }
                        .into());
                    }
                    if visited.insert(tip.clone()) {
                        unmet.remove(tip);
                        if entry.height() > floor_height {
                            stack.extend(entry.parents()?);
                        }
                    }
                }
                Err(_) => missing.push(tip.clone()),
            }
        }

        // Bounded ancestor walk. A foreign ancestor is simply not part of this
        // tree's history (skip); a missing one blocks the decision (want-list).
        while !unmet.is_empty() {
            let Some(current) = stack.pop() else { break };
            if !visited.insert(current.clone()) {
                continue;
            }
            match self.get(&current).await {
                Ok(entry) => {
                    if !entry.in_tree(tree) {
                        continue;
                    }
                    unmet.remove(&current);
                    if entry.height() > floor_height {
                        stack.extend(entry.parents()?);
                    }
                }
                Err(_) => missing.push(current.clone()),
            }
        }

        Ok(if unmet.is_empty() {
            Reachability::Reachable
        } else if !missing.is_empty() {
            missing.sort();
            missing.dedup();
            Reachability::Indeterminate { missing }
        } else {
            Reachability::Unreachable
        })
    }

    /// Retrieves all entries belonging to a specific store at the given snapshot, sorted topologically.
    ///
    /// Returns entries that are ancestors of the provided store snapshot's tips.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the parent tree.
    /// * `store` - The name of the store to retrieve.
    /// * `snapshot` - The store snapshot defining the state to read from.
    async fn store_at(&self, tree: &ID, store: &str, snapshot: &Snapshot) -> Result<Vec<Entry>>;

    /// Get the store parent IDs for a specific entry and store, sorted by height then ID.
    ///
    /// This method retrieves the parent entry IDs for a given entry in a specific store
    /// context, sorted using the same deterministic ordering used throughout the system
    /// (height ascending, then ID ascending for ties).
    ///
    /// # Arguments
    /// * `tree_id` - The ID of the tree containing the entry
    /// * `entry_id` - The ID of the entry to get parents for
    /// * `store` - The name of the store context
    ///
    /// # Returns
    /// A `Result` containing a `Vec<ID>` of parent entry IDs sorted by (height, ID).
    /// Returns empty vec if the entry has no parents in the store.
    async fn get_sorted_store_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        store: &str,
    ) -> Result<Vec<ID>>;

    /// Gets all entries between one entry and multiple target entries (exclusive of start, inclusive of targets).
    ///
    /// This function correctly handles diamond patterns by finding ALL entries that are
    /// reachable from any of the to_ids by following parents back to from_id, not just single paths.
    /// The results are deduplicated and sorted by height then ID for deterministic CRDT merge ordering.
    ///
    /// # Arguments
    /// * `tree_id` - The ID of the tree containing the entries
    /// * `store` - The name of the store context
    /// * `from_id` - The starting entry ID (not included in result), or `None`
    ///   to walk the full ancestry of `to_ids` — the empty-base case, where the
    ///   targets share no common ancestor
    /// * `to_ids` - The target entry IDs (all included in result)
    ///
    /// # Returns
    /// A `Result<Vec<ID>>` containing all entry IDs between from and any of the targets, deduplicated and sorted by height then ID
    async fn get_path_from_to(
        &self,
        tree_id: &ID,
        store: &str,
        from_id: Option<&ID>,
        to_ids: &[ID],
    ) -> Result<Vec<ID>>;

    // === Instance Metadata Methods ===
    //
    // These methods manage persistent instance-level state including the device key
    // and system database IDs. The presence of metadata indicates an initialized instance.

    /// Get the instance metadata.
    ///
    /// Returns `None` for a fresh/uninitialized backend, `Some(metadata)` for an
    /// initialized instance. This is used during `Instance::open_backend()` to determine
    /// whether to create a new instance or load an existing one.
    ///
    /// # Returns
    /// A `Result` containing `Option<InstanceMetadata>`:
    /// - `Some(metadata)` if the instance has been initialized
    /// - `None` if the backend is fresh/uninitialized
    async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>>;

    /// Set the instance metadata.
    ///
    /// This is called during instance creation to persist the device public key and
    /// system database IDs. It may also be called when enabling sync to update
    /// the `sync_db` field.
    ///
    /// # Arguments
    /// * `metadata` - The instance metadata to persist
    ///
    /// # Returns
    /// A `Result` indicating success or an error during storage.
    async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()>;

    /// Get the instance secrets (private key material).
    ///
    /// Returns `None` if no secrets have been saved.
    async fn get_instance_secrets(&self) -> Result<Option<InstanceSecrets>>;

    /// Set the instance secrets (private key material).
    ///
    /// This is called during instance creation to persist the device signing key
    /// separately from the public metadata.
    ///
    /// # Arguments
    /// * `secrets` - The instance secrets to persist
    ///
    /// # Returns
    /// A `Result` indicating success or an error during storage.
    async fn set_instance_secrets(&self, secrets: &InstanceSecrets) -> Result<()>;
}

#[cfg(test)]
mod verification_status_codec_tests {
    use super::VerificationStatus;

    /// Every variant round-trips through the persistence codec, the codes
    /// are the expected stable values, and they are mutually distinct. This
    /// pins the on-disk contract so a future state must take a *new* code
    /// rather than renumber an existing one (which would silently
    /// reinterpret already-stored data).
    #[test]
    fn db_int_roundtrip_and_stable_codes() {
        for v in [
            VerificationStatus::Verified,
            VerificationStatus::Unverified,
            VerificationStatus::Failed,
        ] {
            assert_eq!(VerificationStatus::from_db_int(v.as_db_int()).unwrap(), v);
        }
        // Stable wire/disk values — changing any of these is a data-format
        // break, not a refactor.
        assert_eq!(VerificationStatus::Verified.as_db_int(), 0);
        assert_eq!(VerificationStatus::Failed.as_db_int(), 1);
        assert_eq!(VerificationStatus::Unverified.as_db_int(), 2);
    }

    /// An unknown code (e.g. one a *future* `Trusted` would use, or storage
    /// corruption) must be rejected, never silently mapped onto an existing
    /// state. This is what makes adding a state additively safe: an old
    /// binary fails closed on data it does not understand.
    #[test]
    fn unknown_db_int_is_rejected_not_coerced() {
        for code in [3_i64, 4, 99, -1] {
            assert!(
                VerificationStatus::from_db_int(code).is_err(),
                "code {code} must error, not coerce to an existing state"
            );
        }
    }
}
