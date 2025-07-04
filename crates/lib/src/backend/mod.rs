//! Backend implementations for Eidetica storage
//!
//! This module provides the core `Database` trait and various backend implementations
//! organized by category (database, file, network, cloud).
//!
//! The `Database` trait defines the interface for storing and retrieving `Entry` objects.
//! This allows the core database logic (`BaseDB`, `Tree`) to be independent of the specific storage mechanism.

use crate::Result;
use crate::entry::{Entry, ID};
use ed25519_dalek::SigningKey;
use std::any::Any;

// Category modules
pub mod database;

/// Verification status for entries in the backend.
///
/// This enum tracks whether an entry has been cryptographically verified
/// by the higher-level authentication system. The backend stores this status
/// but does not perform verification itself - that's handled by the Tree/Operation layers.
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

/// Database trait abstracting the underlying storage mechanism for Eidetica entries.
///
/// This trait defines the essential operations required for storing, retrieving,
/// and querying entries and their relationships within trees and subtrees.
/// Implementations of this trait handle the specifics of how data is persisted
/// (e.g., in memory, on disk, in a remote database).
///
/// Much of the performance-critical logic, particularly concerning tree traversal
/// and tip calculation, resides within `Database` implementations, as the optimal
/// approach often depends heavily on the underlying storage characteristics.
///
/// All database implementations must be `Send` and `Sync` to allow sharing across threads,
/// and implement `Any` to allow for downcasting if needed.
///
/// ## Verification Status
///
/// The database stores a verification status for each entry, indicating whether
/// the entry has been authenticated by the higher-level authentication system.
/// The database itself does not perform verification - it only stores the status
/// set by the calling code (typically Tree/Operation implementations).
pub trait Database: Send + Sync + Any {
    /// Retrieves an entry by its unique content-addressable ID.
    ///
    /// # Arguments
    /// * `id` - The ID of the entry to retrieve.
    ///
    /// # Returns
    /// A `Result` containing the `Entry` if found, or an `Error::NotFound` otherwise.
    /// Returns an owned copy to support concurrent access with internal synchronization.
    fn get(&self, id: &ID) -> Result<Entry>;

    /// Gets the verification status of an entry.
    ///
    /// # Arguments
    /// * `id` - The ID of the entry to check.
    ///
    /// # Returns
    /// A `Result` containing the `VerificationStatus` if the entry exists, or an `Error::NotFound` otherwise.
    fn get_verification_status(&self, id: &ID) -> Result<VerificationStatus>;

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
    fn put(&self, verification_status: VerificationStatus, entry: Entry) -> Result<()>;

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
    fn put_verified(&self, entry: Entry) -> Result<()> {
        self.put(VerificationStatus::Verified, entry)
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
    fn put_unverified(&self, entry: Entry) -> Result<()> {
        self.put(VerificationStatus::Failed, entry)
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
    fn update_verification_status(
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
    fn get_entries_by_verification_status(&self, status: VerificationStatus) -> Result<Vec<ID>>;

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
    fn get_tips(&self, tree: &ID) -> Result<Vec<ID>>;

    /// Retrieves the IDs of the tip entries for a specific subtree within a given tree.
    ///
    /// Subtree tips are defined as the set of entries within the specified subtree
    /// that have no children *within that same subtree*. An entry is considered
    /// a child of another within a subtree if it lists the other entry in its
    /// `subtree_parents` list for that specific subtree name.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the parent tree.
    /// * `subtree` - The name of the subtree for which to find tips.
    ///
    /// # Returns
    /// A `Result` containing a vector of tip entry IDs for the subtree or an error.
    fn get_subtree_tips(&self, tree: &ID, subtree: &str) -> Result<Vec<ID>>;

    /// Gets the subtree tips that exist up to a specific set of main tree entries.
    ///
    /// This method finds all subtree entries that are reachable from the specified
    /// main tree entries, then filters to find which of those are tips within the subtree.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the parent tree.
    /// * `subtree` - The name of the subtree for which to find tips.
    /// * `main_entries` - The main tree entry IDs to use as the boundary.
    ///
    /// # Returns
    /// A `Result` containing a vector of subtree tip entry IDs up to the main entries.
    fn get_subtree_tips_up_to_entries(
        &self,
        tree: &ID,
        subtree: &str,
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
    fn all_roots(&self) -> Result<Vec<ID>>;

    /// Finds the Lowest Common Ancestor (LCA) of the given entry IDs within a subtree.
    ///
    /// The LCA is the deepest entry that is an ancestor of all the given entries
    /// within the specified subtree context. This is used to determine optimal
    /// computation boundaries for CRDT state calculation.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the tree
    /// * `subtree` - The name of the subtree context
    /// * `entry_ids` - The entry IDs to find the LCA for
    ///
    /// # Returns
    /// A `Result` containing the LCA entry ID, or an error if no common ancestor exists
    fn find_lca(&self, tree: &ID, subtree: &str, entry_ids: &[ID]) -> Result<ID>;

    /// Collects all entries from the tree root down to the target entry within a subtree.
    ///
    /// This method performs a complete traversal from the tree root to the target entry,
    /// collecting all entries that are ancestors of the target within the specified subtree.
    /// The result includes the tree root and the target entry itself.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the tree
    /// * `subtree` - The name of the subtree context
    /// * `target_entry` - The target entry to collect ancestors for
    ///
    /// # Returns
    /// A `Result` containing a vector of entry IDs from root to target, sorted by height
    fn collect_root_to_target(
        &self,
        tree: &ID,
        subtree: &str,
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
    fn get_tree(&self, tree: &ID) -> Result<Vec<Entry>>;

    /// Retrieves all entries belonging to a specific subtree within a tree, sorted topologically.
    ///
    /// Similar to `get_tree`, but limited to entries that are part of the specified subtree.
    /// The entries are sorted primarily by their height within the subtree (distance
    /// from the subtree's initial entry/entries) and secondarily by their ID.
    ///
    /// **Note:** This potentially loads the entire history of the subtree. Use with caution.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the parent tree.
    /// * `subtree` - The name of the subtree to retrieve.
    ///
    /// # Returns
    /// A `Result` containing a vector of all `Entry` objects in the subtree,
    /// sorted topologically according to their position within the subtree, or an error.
    fn get_subtree(&self, tree: &ID, subtree: &str) -> Result<Vec<Entry>>;

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
    fn get_tree_from_tips(&self, tree: &ID, tips: &[ID]) -> Result<Vec<Entry>>;

    /// Retrieves all entries belonging to a specific subtree within a tree up to the given tips, sorted topologically.
    ///
    /// Similar to `get_subtree`, but only includes entries that are ancestors of the provided subtree tips.
    /// This allows reading from a specific state of the subtree defined by those tips.
    ///
    /// # Arguments
    /// * `tree` - The root ID of the parent tree.
    /// * `subtree` - The name of the subtree to retrieve.
    /// * `tips` - The tip IDs defining the state to read from.
    ///
    /// # Returns
    /// A `Result` containing a vector of `Entry` objects in the subtree up to the given tips,
    /// sorted topologically, or an error.
    fn get_subtree_from_tips(&self, tree: &ID, subtree: &str, tips: &[ID]) -> Result<Vec<Entry>>;

    // === Private Key Storage Methods ===
    //
    // These methods provide secure local storage for private keys outside of the Tree structures.
    // Private keys are stored separately from the content-addressable entries to maintain security
    // and allow for different storage policies (e.g., encryption, hardware security modules).

    /// Store a private key in the backend's local key storage.
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
    fn store_private_key(&self, key_id: &str, private_key: SigningKey) -> Result<()>;

    /// Retrieve a private key from the backend's local key storage.
    ///
    /// # Arguments
    /// * `key_id` - The unique identifier of the private key to retrieve
    ///
    /// # Returns
    /// A `Result` containing an `Option<SigningKey>`. Returns `None` if the key is not found.
    fn get_private_key(&self, key_id: &str) -> Result<Option<SigningKey>>;

    /// List all private key identifiers stored in the backend.
    ///
    /// # Returns
    /// A `Result` containing a vector of key identifiers, or an error.
    fn list_private_keys(&self) -> Result<Vec<String>>;

    /// Remove a private key from the backend's local key storage.
    ///
    /// # Arguments
    /// * `key_id` - The unique identifier of the private key to remove
    ///
    /// # Returns
    /// A `Result` indicating success or an error. Succeeds even if the key doesn't exist.
    fn remove_private_key(&self, key_id: &str) -> Result<()>;

    // === CRDT State Cache Methods ===
    //
    // These methods provide caching for computed CRDT state at specific
    // entry+subtree combinations. This optimizes repeated computations
    // of the same subtree state from the same set of tip entries.

    /// Get cached CRDT state for a subtree at a specific entry.
    ///
    /// # Arguments
    /// * `entry_id` - The entry ID where the state is cached
    /// * `subtree` - The name of the subtree
    ///
    /// # Returns
    /// A `Result` containing an `Option<String>`. Returns `None` if not cached.
    fn get_cached_crdt_state(&self, entry_id: &ID, subtree: &str) -> Result<Option<String>>;

    /// Cache CRDT state for a subtree at a specific entry.
    ///
    /// # Arguments
    /// * `entry_id` - The entry ID where the state should be cached
    /// * `subtree` - The name of the subtree
    /// * `state` - The serialized CRDT state to cache
    ///
    /// # Returns
    /// A `Result` indicating success or an error during storage.
    fn cache_crdt_state(&self, entry_id: &ID, subtree: &str, state: String) -> Result<()>;

    /// Clear all cached CRDT states.
    ///
    /// This is used when the CRDT computation algorithm changes and existing
    /// cached states may have been computed incorrectly.
    ///
    /// # Returns
    /// A `Result` indicating success or an error during the clear operation.
    fn clear_crdt_cache(&self) -> Result<()>;

    /// Get the subtree parent IDs for a specific entry and subtree, sorted by height then ID.
    ///
    /// This method retrieves the parent entry IDs for a given entry in a specific subtree
    /// context, sorted using the same deterministic ordering used throughout the system
    /// (height ascending, then ID ascending for ties).
    ///
    /// # Arguments
    /// * `tree_id` - The ID of the tree containing the entry
    /// * `entry_id` - The ID of the entry to get parents for
    /// * `subtree` - The name of the subtree context
    ///
    /// # Returns
    /// A `Result` containing a `Vec<ID>` of parent entry IDs sorted by (height, ID).
    /// Returns empty vec if the entry has no parents in the subtree.
    fn get_sorted_subtree_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        subtree: &str,
    ) -> Result<Vec<ID>>;

    /// Gets all entries between one entry and multiple target entries (exclusive of start, inclusive of targets).
    ///
    /// This function correctly handles diamond patterns by finding ALL entries that are
    /// reachable from any of the to_ids by following parents back to from_id, not just single paths.
    /// The results are deduplicated and sorted by height then ID for deterministic CRDT merge ordering.
    ///
    /// # Arguments
    /// * `tree_id` - The ID of the tree containing the entries
    /// * `subtree` - The name of the subtree context
    /// * `from_id` - The starting entry ID (not included in result)
    /// * `to_ids` - The target entry IDs (all included in result)
    ///
    /// # Returns
    /// A `Result<Vec<ID>>` containing all entry IDs between from and any of the targets, deduplicated and sorted by height then ID
    fn get_path_from_to(
        &self,
        tree_id: &ID,
        subtree: &str,
        from_id: &ID,
        to_ids: &[ID],
    ) -> Result<Vec<ID>>;
}
