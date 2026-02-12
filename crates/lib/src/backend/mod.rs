//! Backend implementations for Eidetica storage
//!
//! This module provides the core `BackendImpl` trait and various backend implementations
//! organized by category (database, file, network, cloud).
//!
//! The `BackendImpl` trait defines the interface for storing and retrieving `Entry` objects.
//! This allows the core database logic (`Instance`, `Database`) to be independent of the specific storage mechanism.
//!
//! Instance wraps BackendImpl in a `Backend` struct that provides a layer for future development.

use std::any::Any;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    Result,
    auth::crypto::PrivateKey,
    entry::{Entry, ID},
};

/// Persistent metadata for an Eidetica instance.
///
/// This struct consolidates all instance-level state that needs to persist across restarts:
/// - The device signing key (cryptographic identity)
/// - System database root IDs
/// - Optional sync database root ID
///
/// The presence of `InstanceMetadata` in a backend indicates an initialized instance.
/// A backend without metadata is treated as uninitialized and may trigger instance creation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceMetadata {
    /// Device signing key - the instance's cryptographic identity.
    ///
    /// This key is generated once during instance creation and persists for the lifetime
    /// of the instance. It is used to sign system database entries and for sync identity.
    pub device_key: PrivateKey,

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

// Category modules
pub mod database;
pub mod errors;

// Re-export main types for easier access
pub use errors::BackendError;

/// Verification status for entries in the backend.
///
/// This enum tracks whether an entry has been cryptographically verified
/// by the higher-level authentication system. The backend stores this status
/// but does not perform verification itself - that's handled by the Database/Transaction layers.
///
/// Currently all local entries must be authenticated (Verified), but this enum
/// is designed to support future sync scenarios where entries may be received
/// before verification is complete.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Default,
)]
pub enum VerificationStatus {
    /// Entry has been cryptographically verified as authentic.
    /// This is the default for all locally created entries.
    #[default]
    Verified,
    /// Entry failed verification (invalid signature, revoked key, etc.).
    /// Also used temporarily for entries awaiting verification during sync.
    Failed,
    // Future: Add `Unverified` when implementing remote sync:
    // /// Entry received from remote source, awaiting verification
    // Unverified,
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

    /// Stores an entry in the database with the specified verification status.
    ///
    /// If an entry with the same ID already exists, it may be overwritten,
    /// although the content-addressable nature means the content will be identical.
    /// The verification status will be updated to the provided value.
    ///
    /// # Arguments
    /// * `verification_status` - The verification status to assign to this entry
    /// * `entry` - The `Entry` to store.
    ///
    /// # Returns
    /// A `Result` indicating success or an error during storage.
    async fn put(&self, verification_status: VerificationStatus, entry: Entry) -> Result<()>;

    /// Stores an entry with verified status (convenience method for local entries).
    ///
    /// This is a convenience method that calls `put(VerificationStatus::Verified, entry)`.
    /// Use this for locally created and signed entries.
    ///
    /// # Arguments
    /// * `entry` - The `Entry` to store as verified.
    ///
    /// # Returns
    /// A `Result` indicating success or an error during storage.
    async fn put_verified(&self, entry: Entry) -> Result<()> {
        self.put(VerificationStatus::Verified, entry).await
    }

    /// Stores an entry with failed verification status (convenience method for sync scenarios).
    ///
    /// This is a convenience method that calls `put(VerificationStatus::Failed, entry)`.
    /// Use this for entries that failed verification or are pending verification from sync.
    /// In the future, this may use a dedicated `Unverified` status.
    ///
    /// # Arguments
    /// * `entry` - The `Entry` to store as failed/unverified.
    ///
    /// # Returns
    /// A `Result` indicating success or an error during storage.
    async fn put_unverified(&self, entry: Entry) -> Result<()> {
        self.put(VerificationStatus::Failed, entry).await
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

    /// Retrieves the IDs of the tip entries for a given tree.
    ///
    /// Tips are defined as the set of entries within the specified tree
    /// that have no children *within that same tree*. An entry is considered
    /// a child of another if it lists the other entry in its `parents` list.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the tree for which to find tips.
    ///
    /// # Returns
    /// A `Result` containing a vector of tip entry IDs or an error.
    async fn get_tips(&self, tree: &ID) -> Result<Vec<ID>>;

    /// Retrieves the IDs of the tip entries for a specific store within a given tree.
    ///
    /// Store tips are defined as the set of entries within the specified store
    /// that have no children *within that same store*. An entry is considered
    /// a child of another within a store if it lists the other entry in its
    /// `store_parents` list for that specific store name.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the parent tree.
    /// * `store` - The name of the store for which to find tips.
    ///
    /// # Returns
    /// A `Result` containing a vector of tip entry IDs for the store or an error.
    async fn get_store_tips(&self, tree: &ID, store: &str) -> Result<Vec<ID>>;

    /// Gets the store tips that exist up to a specific set of main tree entries.
    ///
    /// This method finds all store entries that are reachable from the specified
    /// main tree entries, then filters to find which of those are tips within the store.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the parent tree.
    /// * `store` - The name of the store for which to find tips.
    /// * `main_entries` - The main tree entry IDs to use as the boundary.
    ///
    /// # Returns
    /// A `Result` containing a vector of store tip entry IDs up to the main entries.
    async fn get_store_tips_up_to_entries(
        &self,
        tree: &ID,
        store: &str,
        main_entries: &[ID],
    ) -> Result<Vec<ID>>;

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
    /// A `Result` containing the merge base entry ID, or an error if no common ancestor exists
    async fn find_merge_base(&self, tree: &ID, store: &str, entry_ids: &[ID]) -> Result<ID>;

    /// Collects all entries from the tree root down to the target entry within a store.
    ///
    /// This method performs a complete traversal from the tree root to the target entry,
    /// collecting all entries that are ancestors of the target within the specified store.
    /// The result includes the tree root and the target entry itself.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the tree
    /// * `store` - The name of the store context
    /// * `target_entry` - The target entry to collect ancestors for
    ///
    /// # Returns
    /// A `Result` containing a vector of entry IDs from root to target, sorted by height
    async fn collect_root_to_target(
        &self,
        tree: &ID,
        store: &str,
        target_entry: &ID,
    ) -> Result<Vec<ID>>;

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

    /// Retrieves all entries belonging to a specific store within a tree up to the given tips, sorted topologically.
    ///
    /// Similar to `get_subtree`, but only includes entries that are ancestors of the provided store tips.
    /// This allows reading from a specific state of the store defined by those tips.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the parent tree.
    /// * `store` - The name of the store to retrieve.
    /// * `tips` - The tip IDs defining the state to read from.
    ///
    /// # Returns
    /// A `Result` containing a vector of `Entry` objects in the store up to the given tips,
    /// sorted topologically, or an error.
    async fn get_store_from_tips(&self, tree: &ID, store: &str, tips: &[ID]) -> Result<Vec<Entry>>;

    // === CRDT State Cache Methods ===
    //
    // These methods provide caching for computed CRDT state at specific
    // entry+store combinations. This optimizes repeated computations
    // of the same store state from the same set of tip entries.

    /// Get cached CRDT state for a store at a specific entry.
    ///
    /// # Arguments
    /// * `entry_id` - The entry ID where the state is cached
    /// * `store` - The name of the store
    ///
    /// # Returns
    /// A `Result` containing an `Option<String>`. Returns `None` if not cached.
    async fn get_cached_crdt_state(&self, entry_id: &ID, store: &str) -> Result<Option<String>>;

    /// Cache CRDT state for a store at a specific entry.
    ///
    /// # Arguments
    /// * `entry_id` - The entry ID where the state should be cached
    /// * `store` - The name of the store
    /// * `state` - The serialized CRDT state to cache
    ///
    /// # Returns
    /// A `Result` indicating success or an error during storage.
    async fn cache_crdt_state(&self, entry_id: &ID, store: &str, state: String) -> Result<()>;

    /// Clear all cached CRDT states.
    ///
    /// This is used when the CRDT computation algorithm changes and existing
    /// cached states may have been computed incorrectly.
    ///
    /// # Returns
    /// A `Result` indicating success or an error during the clear operation.
    async fn clear_crdt_cache(&self) -> Result<()>;

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
    /// * `from_id` - The starting entry ID (not included in result)
    /// * `to_ids` - The target entry IDs (all included in result)
    ///
    /// # Returns
    /// A `Result<Vec<ID>>` containing all entry IDs between from and any of the targets, deduplicated and sorted by height then ID
    async fn get_path_from_to(
        &self,
        tree_id: &ID,
        store: &str,
        from_id: &ID,
        to_ids: &[ID],
    ) -> Result<Vec<ID>>;

    // === Instance Metadata Methods ===
    //
    // These methods manage persistent instance-level state including the device key
    // and system database IDs. The presence of metadata indicates an initialized instance.

    /// Get the instance metadata.
    ///
    /// Returns `None` for a fresh/uninitialized backend, `Some(metadata)` for an
    /// initialized instance. This is used during `Instance::open()` to determine
    /// whether to create a new instance or load an existing one.
    ///
    /// # Returns
    /// A `Result` containing `Option<InstanceMetadata>`:
    /// - `Some(metadata)` if the instance has been initialized
    /// - `None` if the backend is fresh/uninitialized
    async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>>;

    /// Set the instance metadata.
    ///
    /// This is called during instance creation to persist the device key and
    /// system database IDs. It may also be called when enabling sync to update
    /// the `sync_db` field.
    ///
    /// # Arguments
    /// * `metadata` - The instance metadata to persist
    ///
    /// # Returns
    /// A `Result` indicating success or an error during storage.
    async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()>;
}
