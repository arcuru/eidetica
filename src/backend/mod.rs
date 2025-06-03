//!
//! Defines the storage backend trait and implementations.
//!
//! The `Backend` trait defines the interface for storing and retrieving `Entry` objects.
//! This allows the core database logic (`BaseDB`, `Tree`) to be independent of the specific storage mechanism.

use crate::Result;
use crate::entry::{Entry, ID};
use ed25519_dalek::SigningKey;
use std::any::Any;

mod in_memory;

pub use in_memory::InMemoryBackend;

/// Backend trait abstracting the underlying storage mechanism for Eidetica entries.
///
/// This trait defines the essential operations required for storing, retrieving,
/// and querying entries and their relationships within trees and subtrees.
/// Implementations of this trait handle the specifics of how data is persisted
/// (e.g., in memory, on disk, in a remote database).
///
/// Much of the performance-critical logic, particularly concerning tree traversal
/// and tip calculation, resides within `Backend` implementations, as the optimal
/// approach often depends heavily on the underlying storage characteristics.
///
/// All backend implementations must be `Send` and `Sync` to allow sharing across threads,
/// and implement `Any` to allow for downcasting if needed.
pub trait Backend: Send + Sync + Any {
    /// Retrieves an entry by its unique content-addressable ID.
    ///
    /// # Arguments
    /// * `id` - The ID of the entry to retrieve.
    ///
    /// # Returns
    /// A `Result` containing a reference to the `Entry` if found, or an `Error::NotFound` otherwise.
    fn get(&self, id: &ID) -> Result<&Entry>;

    /// Stores an entry in the backend.
    ///
    /// If an entry with the same ID already exists, it may be overwritten,
    /// although the content-addressable nature means the content will be identical.
    ///
    /// # Arguments
    /// * `entry` - The `Entry` to store.
    ///
    /// # Returns
    /// A `Result` indicating success or an error during storage.
    fn put(&mut self, entry: Entry) -> Result<()>;

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
    fn store_private_key(&mut self, key_id: &str, private_key: SigningKey) -> Result<()>;

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
    fn remove_private_key(&mut self, key_id: &str) -> Result<()>;
}
