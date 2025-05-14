//! Tree module provides functionality for managing collections of related entries.
//!
//! A `Tree` represents a hierarchical structure of entries, similar to a table in a database
//! or a branch in a version control system. Each tree has a root entry and maintains
//! the history and relationships between entries, interfacing with a backend storage system.

use crate::atomicop::AtomicOp;
use crate::backend::Backend;
use crate::constants::{ROOT, SETTINGS};
use crate::data::KVNested;
use crate::entry::{Entry, ID};
use crate::subtree::{KVStore, SubTree};
use crate::{Error, Result};

use serde_json;
use std::sync::{Arc, Mutex, MutexGuard};

/// Represents a collection of related entries, analogous to a table or a branch in a version control system.
///
/// Each `Tree` is identified by the ID of its root `Entry` and manages the history of data
/// associated with that root. It interacts with the underlying `Backend` for storage.
#[derive(Clone)]
pub struct Tree {
    root: ID,
    backend: Arc<Mutex<Box<dyn Backend>>>,
}

impl Tree {
    /// Creates a new `Tree` instance.
    ///
    /// Initializes the tree by creating a root `Entry` containing the provided settings
    /// and storing it in the backend.
    ///
    /// # Arguments
    /// * `settings` - A `KVNested` CRDT containing the initial settings for the tree.
    /// * `backend` - An `Arc<Mutex<>>` protected reference to the backend where the tree's entries will be stored.
    ///
    /// # Returns
    /// A `Result` containing the new `Tree` instance or an error.
    pub fn new(settings: KVNested, backend: Arc<Mutex<Box<dyn Backend>>>) -> Result<Self> {
        // Create a dummy tree pointing to the root of all roots
        // FIXME: This should use a None for the root ID
        let dummy_tree = Self {
            root: "".to_string(),
            backend: backend.clone(),
        };

        // Use an operation on the dummy tree to add the settings
        let op = dummy_tree.new_operation()?;
        op.update_subtree(SETTINGS, &serde_json::to_string(&settings)?)?;
        op.update_subtree(ROOT, &serde_json::to_string(&"".to_string())?)?;
        let root_id = op.commit()?;

        // Now create the real tree with the root ID
        Ok(Self {
            root: root_id,
            backend,
        })
    }

    /// Creates a new `Tree` instance from an existing ID.
    ///
    /// This constructor takes an existing `ID` and an `Arc<Mutex<Box<dyn Backend>>>`
    /// and constructs a `Tree` instance with the specified root ID.
    ///
    /// # Arguments
    /// * `id` - The `ID` of the root entry.
    /// * `backend` - An `Arc<Mutex<Box<dyn Backend>>>` protected reference to the backend where the tree's entries will be stored.
    ///
    /// # Returns
    /// A `Result` containing the new `Tree` instance or an error.
    pub(crate) fn new_from_id(id: ID, backend: Arc<Mutex<Box<dyn Backend>>>) -> Result<Self> {
        Ok(Self { root: id, backend })
    }

    /// Helper function to lock the backend mutex.
    pub(crate) fn lock_backend(&self) -> Result<MutexGuard<'_, Box<dyn Backend>>> {
        self.backend.lock().map_err(|_| {
            Error::Io(std::io::Error::other(
                "Failed to lock backend in Tree::lock_backend",
            ))
        })
    }

    /// Get the ID of the root entry
    pub fn root_id(&self) -> &ID {
        &self.root
    }

    /// Get a reference to the backend
    pub fn backend(&self) -> &Arc<Mutex<Box<dyn Backend>>> {
        &self.backend
    }

    /// Retrieve the root entry from the backend
    pub fn get_root(&self) -> Result<Entry> {
        let backend_guard = self.lock_backend()?;
        backend_guard.get(&self.root).cloned()
    }

    /// Get a settings store for the tree.
    ///
    /// Returns a KVStore subtree for managing the tree's settings.
    ///
    /// # Returns
    /// A `Result` containing the `KVStore` for settings or an error.
    pub fn get_settings(&self) -> Result<KVStore> {
        self.get_subtree_viewer::<KVStore>(SETTINGS)
    }

    /// Get the name of the tree from its settings subtree
    pub fn get_name(&self) -> Result<String> {
        // Get the settings subtree
        let settings = self.get_settings()?;

        // Get the name from the settings
        settings.get_string("name")
    }

    /// Create a new atomic operation on this tree
    ///
    /// This creates a new atomic operation containing a new Entry.
    /// The atomic operation will be initialized with the current state of the tree.
    ///
    /// # Returns
    /// A `Result<AtomicOp>` containing the new atomic operation
    pub fn new_operation(&self) -> Result<AtomicOp> {
        AtomicOp::new(self)
    }

    /// Insert an entry into the tree without modifying it.
    /// This is primarily for testing purposes or when you need full control over the entry.
    pub fn insert_raw(&self, entry: Entry) -> Result<ID> {
        let id = entry.id();

        let mut backend_guard = self.lock_backend()?;
        backend_guard.put(entry)?;

        Ok(id)
    }

    /// Get a SubTree type that will handle accesses to the SubTree
    /// This will return a SubTree initialized to point at the current state of the tree.
    ///
    /// The returned subtree should NOT be used to modify the tree, as it intentionally does not
    /// expose the AtomicOp.
    pub fn get_subtree_viewer<T>(&self, name: &str) -> Result<T>
    where
        T: SubTree,
    {
        let op = self.new_operation()?;
        T::new(&op, name)
    }

    /// Get the current tips (leaf entries) of the main tree branch.
    ///
    /// Tips represent the latest entries in the tree's main history, forming the heads of the DAG.
    ///
    /// # Returns
    /// A `Result` containing a vector of `ID`s for the tip entries or an error.
    pub fn get_tips(&self) -> Result<Vec<ID>> {
        let backend_guard = self.lock_backend()?;
        backend_guard.get_tips(&self.root)
    }

    /// Get the full `Entry` objects for the current tips of the main tree branch.
    ///
    /// # Returns
    /// A `Result` containing a vector of the tip `Entry` objects or an error.
    pub fn get_tip_entries(&self) -> Result<Vec<Entry>> {
        let backend_guard = self.lock_backend()?;
        let tips = backend_guard.get_tips(&self.root)?;
        let entries: Result<Vec<_>> = tips
            .iter()
            .map(|id| backend_guard.get(id).cloned())
            .collect();
        entries
    }
}
