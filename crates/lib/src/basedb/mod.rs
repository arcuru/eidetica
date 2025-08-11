//!
//! Provides the main database structures (`BaseDB` and `Tree`).
//!
//! `BaseDB` manages multiple `Tree` instances and interacts with the storage `Database`.
//! `Tree` represents a single, independent history of data entries, analogous to a table or branch.

use crate::Result;
use crate::auth::crypto::{format_public_key, generate_keypair};
use crate::backend::Database;
use crate::crdt::Map;
use crate::entry::ID;
use crate::sync::Sync;
use crate::tree::Tree;
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::Rng;
use std::sync::Arc;

pub mod errors;

// Re-export main types for easier access
pub use errors::BaseError;

/// Database implementation on top of the storage backend.
///
/// This database is the base DB, other 'overlays' or 'plugins' should be implemented on top of this.
/// It manages collections of related entries, called `Tree`s, and interacts with a
/// pluggable `Database` for storage and retrieval.
/// Each `Tree` represents an independent history of data, identified by a root `Entry`.
pub struct BaseDB {
    /// The database storage used by the database.
    backend: Arc<dyn Database>,
    /// Synchronization module for this database instance.
    sync: Option<Sync>,
    // Blob storage will be separate, maybe even just an extension
    // storage: IPFS;
}

impl BaseDB {
    pub fn new(backend: Box<dyn Database>) -> Self {
        Self {
            backend: Arc::from(backend),
            sync: None,
        }
    }

    /// Get a reference to the backend
    pub fn backend(&self) -> &Arc<dyn Database> {
        &self.backend
    }

    /// Create a new tree in the database.
    ///
    /// A `Tree` represents a collection of related entries, analogous to a table.
    /// It is initialized with settings defined by a `Map` CRDT.
    /// All trees must now be created with authentication.
    ///
    /// # Arguments
    /// * `settings` - The initial settings for the tree, typically including metadata like a name.
    /// * `signing_key_name` - Authentication key name to use for the initial commit. Required for all trees.
    ///
    /// # Returns
    /// A `Result` containing the newly created `Tree` or an error.
    pub fn new_tree(&self, settings: Map, signing_key_name: impl AsRef<str>) -> Result<Tree> {
        Tree::new(settings, Arc::clone(&self.backend), signing_key_name)
    }

    /// Create a new tree with default empty settings
    /// All trees must now be created with authentication.
    ///
    /// # Arguments
    /// * `signing_key_name` - Authentication key name to use for the initial commit. Required for all trees.
    ///
    /// # Returns
    /// A `Result` containing the newly created `Tree` or an error.
    pub fn new_tree_default(&self, signing_key_name: impl AsRef<str>) -> Result<Tree> {
        let mut settings = Map::new();

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

        self.new_tree(settings, signing_key_name)
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
        // Make sure the entry exists
        self.backend.get(root_id)?;

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
        let root_ids = self.backend.all_roots()?;
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
    /// Returns `BaseError::TreeNotFound` if no trees with the specified name are found.
    pub fn find_tree(&self, name: impl AsRef<str>) -> Result<Vec<Tree>> {
        let name = name.as_ref();
        let all_trees = self.all_trees()?;
        let mut matching_trees = Vec::new();

        for tree in all_trees {
            // Attempt to get the name from the tree's settings
            if let Ok(tree_name) = tree.get_name()
                && tree_name == name
            {
                matching_trees.push(tree);
            }
            // Ignore trees where getting the name fails or doesn't match
        }

        if matching_trees.is_empty() {
            Err(BaseError::TreeNotFound {
                name: name.to_string(),
            }
            .into())
        } else {
            Ok(matching_trees)
        }
    }

    // === Authentication Key Management ===
    //
    // These methods provide a high-level API for managing private keys used for
    // authentication and signing entries. Private keys are stored locally in the
    // database storage and are never synchronized or shared.

    /// Generate a new Ed25519 keypair and store the private key locally.
    ///
    /// This is the primary method for adding new authentication keys to the database.
    /// The generated private key is stored in the database's local key storage,
    /// and the public key is returned for use in authentication configuration.
    ///
    /// # Arguments
    /// * `key_name` - A unique identifier for the key (e.g., "KEY_LAPTOP", "ADMIN_KEY")
    ///
    /// # Returns
    /// A `Result` containing the generated public key or an error.
    ///
    /// # Example
    /// ```
    /// # use eidetica::{backend::database::InMemory, basedb::BaseDB};
    /// let backend = InMemory::new();
    /// let db = BaseDB::new(Box::new(backend));
    ///
    /// // Generate a new key for laptop
    /// let public_key = db.add_private_key("KEY_LAPTOP")?;
    /// println!("Generated public key: {}", eidetica::auth::crypto::format_public_key(&public_key));
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn add_private_key(&self, key_name: impl AsRef<str>) -> Result<VerifyingKey> {
        let key_name = key_name.as_ref();
        let (signing_key, verifying_key) = generate_keypair();

        self.backend.store_private_key(key_name, signing_key)?;

        Ok(verifying_key)
    }

    /// Import an existing Ed25519 private key into local storage.
    ///
    /// This allows importing keys generated elsewhere or backing up/restoring keys.
    ///
    /// # Arguments
    /// * `key_name` - A unique identifier for the key
    /// * `private_key` - The Ed25519 private key to import
    ///
    /// # Returns
    /// A `Result` indicating success or an error.
    pub fn import_private_key(
        &self,
        key_name: impl AsRef<str>,
        private_key: SigningKey,
    ) -> Result<()> {
        self.backend
            .store_private_key(key_name.as_ref(), private_key)
    }

    /// Get the public key corresponding to a stored private key.
    ///
    /// This is useful for displaying or verifying which public key corresponds
    /// to a locally stored private key identifier.
    ///
    /// # Arguments
    /// * `key_name` - The identifier of the private key
    ///
    /// # Returns
    /// A `Result` containing `Some(VerifyingKey)` if the key exists, `None` if not found.
    pub fn get_public_key(&self, key_name: impl AsRef<str>) -> Result<Option<VerifyingKey>> {
        if let Some(signing_key) = self.backend.get_private_key(key_name.as_ref())? {
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
        self.backend.list_private_keys()
    }

    /// Remove a private key from local storage.
    ///
    /// **Warning**: This permanently removes the private key. Ensure you have
    /// backups or alternative authentication methods before removing keys.
    ///
    /// # Arguments
    /// * `key_name` - The identifier of the private key to remove
    ///
    /// # Returns
    /// A `Result` indicating success. Succeeds even if the key doesn't exist.
    pub fn remove_private_key(&self, key_name: impl AsRef<str>) -> Result<()> {
        self.backend.remove_private_key(key_name.as_ref())
    }

    /// Get a formatted public key string for a stored private key.
    ///
    /// This is a convenience method that combines `get_public_key` and `format_public_key`.
    ///
    /// # Arguments
    /// * `key_name` - The identifier of the private key
    ///
    /// # Returns
    /// A `Result` containing the formatted public key string if found.
    pub fn get_formatted_public_key(&self, key_name: impl AsRef<str>) -> Result<Option<String>> {
        if let Some(public_key) = self.get_public_key(key_name)? {
            Ok(Some(format_public_key(&public_key)))
        } else {
            Ok(None)
        }
    }

    // === Synchronization Management ===
    //
    // These methods provide access to the Sync module for managing synchronization
    // settings and state for this database instance.

    /// Initialize the Sync module for this database.
    ///
    /// Creates a new sync settings tree and initializes the sync module.
    /// This method should be called once per database instance to enable sync functionality.
    ///
    /// # Arguments
    /// * `signing_key_name` - The key name to use for authenticating sync operations
    ///
    /// # Returns
    /// A `Result` containing a new BaseDB with the sync module initialized.
    pub fn with_sync(mut self, signing_key_name: impl AsRef<str>) -> Result<Self> {
        let sync = Sync::new(Arc::clone(&self.backend), signing_key_name)?;
        self.sync = Some(sync);
        Ok(self)
    }

    /// Get a reference to the Sync module for this database.
    ///
    /// # Returns
    /// An `Option` containing a reference to the `Sync` module if initialized.
    pub fn sync(&self) -> Option<&Sync> {
        self.sync.as_ref()
    }

    /// Load an existing Sync module from a sync tree root ID.
    ///
    /// # Arguments
    /// * `sync_tree_root_id` - The root ID of an existing sync tree
    ///
    /// # Returns
    /// A `Result` containing a new BaseDB with the sync module loaded.
    pub fn with_sync_from_tree(mut self, sync_tree_root_id: &ID) -> Result<Self> {
        let sync = Sync::load(Arc::clone(&self.backend), sync_tree_root_id)?;
        self.sync = Some(sync);
        Ok(self)
    }
}
