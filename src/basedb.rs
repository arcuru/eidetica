//!
//! Provides the main database structures (`BaseDB` and `Tree`).
//!
//! `BaseDB` manages multiple `Tree` instances and interacts with the storage `Backend`.
//! `Tree` represents a single, independent history of data entries, analogous to a table or branch.

use crate::backend::Backend;
use crate::data::KVOverWrite;
use crate::entry::ID;
use crate::tree::Tree;
use crate::{Error, Result};
use std::sync::{Arc, Mutex, MutexGuard};

/// Database implementation on top of the backend.
///
/// This database is the base DB, other 'overlays' or 'plugins' should be implemented on top of this.
/// It manages collections of related entries, called `Tree`s, and interacts with a
/// pluggable `Backend` for storage and retrieval.
/// Each `Tree` represents an independent history of data, identified by a root `Entry`.
pub struct BaseDB {
    /// The backend used by the database.
    backend: Arc<Mutex<Box<dyn Backend>>>,
    // Blob storage will be separate, maybe even just an extension
    // storage: IPFS;
}

impl BaseDB {
    pub fn new(backend: Box<dyn Backend>) -> Self {
        Self {
            backend: Arc::new(Mutex::new(backend)),
        }
    }

    /// Get a reference to the backend
    pub fn backend(&self) -> &Arc<Mutex<Box<dyn Backend>>> {
        &self.backend
    }

    /// Helper function to lock the backend mutex.
    fn lock_backend(&self) -> Result<MutexGuard<'_, Box<dyn Backend>>> {
        self.backend.lock().map_err(|_| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Failed to lock backend",
            ))
        })
    }

    /// Create a new tree in the database.
    ///
    /// A `Tree` represents a collection of related entries, analogous to a table.
    /// It is initialized with settings defined by a `KVOverWrite` CRDT.
    ///
    /// # Arguments
    /// * `settings` - The initial settings for the tree, typically including metadata like a name.
    ///
    /// # Returns
    /// A `Result` containing the newly created `Tree` or an error.
    pub fn new_tree(&self, settings: KVOverWrite) -> Result<Tree> {
        Tree::new(settings, Arc::clone(&self.backend))
    }

    /// Load an existing tree from the database by its root ID.
    ///
    /// # Arguments
    /// * `root_id` - The content-addressable ID of the root `Entry` of the tree to load.
    ///
    /// # Returns
    /// A `Result` containing the loaded `Tree` or an error if the root ID is not found.
    pub fn load_tree(&self, root_id: &ID) -> Result<Tree> {
        // First validate the root_id exists in the backend
        {
            let backend_guard = self.lock_backend()?;
            // Make sure the entry exists
            backend_guard.get(root_id)?;
        }

        // Create a tree object with the given root_id
        Tree::new_from_id(root_id.clone(), Arc::clone(&self.backend))
    }

    /// Load all trees stored in the backend.
    ///
    /// This retrieves all known root entry IDs from the backend and constructs
    /// `Tree` instances for each.
    ///
    /// # Returns
    /// A `Result` containing a vector of all `Tree` instances or an error.
    pub fn all_trees(&self) -> Result<Vec<Tree>> {
        let root_ids = {
            let backend_guard = self.lock_backend()?;
            backend_guard.all_roots()?
        };
        let mut trees = Vec::new();

        for root_id in root_ids {
            trees.push(Tree::new_from_id(
                root_id.clone(),
                Arc::clone(&self.backend),
            )?);
        }

        Ok(trees)
    }

    /// Find trees by their assigned name.
    ///
    /// Searches through all trees in the database and returns those whose "name"
    /// setting matches the provided name.
    ///
    /// # Arguments
    /// * `name` - The name to search for.
    ///
    /// # Returns
    /// A `Result` containing a vector of `Tree` instances whose name matches,
    /// or an error.
    ///
    /// # Errors
    /// Returns `Error::NotFound` if no trees with the specified name are found.
    pub fn find_tree(&self, name: &str) -> Result<Vec<Tree>> {
        let all_trees = self.all_trees()?;
        let mut matching_trees = Vec::new();

        for tree in all_trees {
            // Attempt to get the name from the tree's settings
            if let Ok(tree_name) = tree.get_name() {
                if tree_name == name {
                    matching_trees.push(tree);
                }
            }
            // Ignore trees where getting the name fails or doesn't match
        }

        if matching_trees.is_empty() {
            Err(Error::NotFound)
        } else {
            Ok(matching_trees)
        }
    }
}
