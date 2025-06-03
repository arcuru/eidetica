//!
//! Provides the main database structures (`BaseDB` and `Tree`).
//!
//! `BaseDB` manages multiple `Tree` instances and interacts with the storage `Backend`.
//! `Tree` represents a single, independent history of data entries, analogous to a table or branch.

use crate::auth::crypto::{format_public_key, generate_keypair};
use crate::backend::Backend;
use crate::data::KVNested;
use crate::entry::ID;
use crate::tree::Tree;
use crate::{Error, Result};
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::Rng;
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
        self.backend
            .lock()
            .map_err(|_| Error::Io(std::io::Error::other("Failed to lock backend")))
    }

    /// Create a new tree in the database.
    ///
    /// A `Tree` represents a collection of related entries, analogous to a table.
    /// It is initialized with settings defined by a `KVNested` CRDT.
    ///
    /// # Arguments
    /// * `settings` - The initial settings for the tree, typically including metadata like a name.
    ///
    /// # Returns
    /// A `Result` containing the newly created `Tree` or an error.
    pub fn new_tree(&self, settings: KVNested) -> Result<Tree> {
        Tree::new(settings, Arc::clone(&self.backend), None)
    }

    /// Create a new tree with default empty settings
    pub fn new_tree_default(&self) -> Result<Tree> {
        let mut settings = KVNested::new();

        // Add a unique tree identifier to ensure each tree gets a unique root ID
        // This prevents content-addressable collision when creating multiple trees
        // with identical settings
        let unique_id = format!(
            "tree_{}",
            rand::thread_rng()
                .sample_iter(&rand::distributions::Alphanumeric)
                .take(16)
                .map(char::from)
                .collect::<String>()
        );
        settings.set_string("tree_id", unique_id);

        self.new_tree(settings)
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

    // === Authentication Key Management ===
    //
    // These methods provide a high-level API for managing private keys used for
    // authentication and signing entries. Private keys are stored locally in the
    // backend and are never synchronized or shared.

    /// Generate a new Ed25519 keypair and store the private key locally.
    ///
    /// This is the primary method for adding new authentication keys to the database.
    /// The generated private key is stored in the backend's local key storage,
    /// and the public key is returned for use in authentication configuration.
    ///
    /// # Arguments
    /// * `key_id` - A unique identifier for the key (e.g., "KEY_LAPTOP", "ADMIN_KEY")
    ///
    /// # Returns
    /// A `Result` containing the generated public key or an error.
    ///
    /// # Example
    /// ```
    /// # use eidetica::{backend::InMemoryBackend, basedb::BaseDB};
    /// let backend = InMemoryBackend::new();
    /// let db = BaseDB::new(Box::new(backend));
    ///
    /// // Generate a new key for laptop
    /// let public_key = db.add_private_key("KEY_LAPTOP")?;
    /// println!("Generated public key: {}", eidetica::auth::crypto::format_public_key(&public_key));
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn add_private_key(&self, key_id: &str) -> Result<VerifyingKey> {
        let (signing_key, verifying_key) = generate_keypair();

        let mut backend_guard = self.lock_backend()?;
        backend_guard.store_private_key(key_id, signing_key)?;

        Ok(verifying_key)
    }

    /// Import an existing Ed25519 private key into local storage.
    ///
    /// This allows importing keys generated elsewhere or backing up/restoring keys.
    ///
    /// # Arguments
    /// * `key_id` - A unique identifier for the key
    /// * `private_key` - The Ed25519 private key to import
    ///
    /// # Returns
    /// A `Result` indicating success or an error.
    pub fn import_private_key(&self, key_id: &str, private_key: SigningKey) -> Result<()> {
        let mut backend_guard = self.lock_backend()?;
        backend_guard.store_private_key(key_id, private_key)
    }

    /// Get the public key corresponding to a stored private key.
    ///
    /// This is useful for displaying or verifying which public key corresponds
    /// to a locally stored private key identifier.
    ///
    /// # Arguments
    /// * `key_id` - The identifier of the private key
    ///
    /// # Returns
    /// A `Result` containing `Some(VerifyingKey)` if the key exists, `None` if not found.
    pub fn get_public_key(&self, key_id: &str) -> Result<Option<VerifyingKey>> {
        let backend_guard = self.lock_backend()?;
        if let Some(signing_key) = backend_guard.get_private_key(key_id)? {
            Ok(Some(signing_key.verifying_key()))
        } else {
            Ok(None)
        }
    }

    /// List all locally stored private key identifiers.
    ///
    /// This returns the identifiers of all private keys stored in the backend,
    /// but not the keys themselves for security reasons.
    ///
    /// # Returns
    /// A `Result` containing a vector of key identifiers.
    pub fn list_private_keys(&self) -> Result<Vec<String>> {
        let backend_guard = self.lock_backend()?;
        backend_guard.list_private_keys()
    }

    /// Remove a private key from local storage.
    ///
    /// **Warning**: This permanently removes the private key. Ensure you have
    /// backups or alternative authentication methods before removing keys.
    ///
    /// # Arguments
    /// * `key_id` - The identifier of the private key to remove
    ///
    /// # Returns
    /// A `Result` indicating success. Succeeds even if the key doesn't exist.
    pub fn remove_private_key(&self, key_id: &str) -> Result<()> {
        let mut backend_guard = self.lock_backend()?;
        backend_guard.remove_private_key(key_id)
    }

    /// Get a formatted public key string for a stored private key.
    ///
    /// This is a convenience method that combines `get_public_key` and `format_public_key`.
    ///
    /// # Arguments
    /// * `key_id` - The identifier of the private key
    ///
    /// # Returns
    /// A `Result` containing the formatted public key string if found.
    pub fn get_formatted_public_key(&self, key_id: &str) -> Result<Option<String>> {
        if let Some(public_key) = self.get_public_key(key_id)? {
            Ok(Some(format_public_key(&public_key)))
        } else {
            Ok(None)
        }
    }
}
