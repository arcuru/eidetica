//!
//! Provides the main database structures (`Instance` and `Database`).
//!
//! `Instance` manages multiple `Database` instances and interacts with the storage `Database`.
//! `Database` represents a single, independent history of data entries, analogous to a table or branch.

use crate::Database;
use crate::Result;
use crate::auth::crypto::{format_public_key, generate_keypair};
use crate::backend::BackendDB;
use crate::crdt::Doc;
use crate::entry::ID;
use crate::sync::Sync;
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::Rng;
use std::sync::Arc;

pub mod errors;

// Re-export main types for easier access
pub use errors::InstanceError;

/// Private constants for device identity management
const DEVICE_KEY_NAME: &str = "_device_key";

/// Database implementation on top of the storage backend.
///
/// This database is the base DB, other 'overlays' or 'plugins' should be implemented on top of this.
/// It manages collections of related entries, called `Database`s, and interacts with a
/// pluggable `Database` for storage and retrieval.
/// Each `Database` represents an independent history of data, identified by a root `Entry`.
///
/// Each Instance instance has a unique device identity represented by an Ed25519 keypair.
/// The public key serves as the device ID for sync operations.
pub struct Instance {
    /// The database storage used by the database.
    backend: Arc<dyn BackendDB>,
    /// Synchronization module for this database instance.
    sync: Option<Sync>,
    // Blob storage will be separate, maybe even just an extension
    // storage: IPFS;
}

impl Instance {
    pub fn new(backend: Box<dyn BackendDB>) -> Self {
        let db = Self {
            backend: Arc::from(backend),
            sync: None,
        };

        // Ensure device ID is generated during construction
        db.device_id()
            .expect("Failed to generate device ID during Instance construction");

        db
    }

    /// Get a reference to the backend
    pub fn backend(&self) -> &Arc<dyn BackendDB> {
        &self.backend
    }

    // === Device Identity Management ===
    //
    // Each Instance instance has a unique device identity represented by an Ed25519 keypair.
    // The device key is automatically generated on first access and stored persistently.
    // The public key serves as the device ID for identification in sync operations.

    /// Ensure device key exists and return the device ID (public key).
    ///
    /// This method automatically generates and stores a device keypair if one doesn't exist.
    /// The device key is stored with the reserved name "_device_key" and should not be
    /// modified or removed manually.
    ///
    /// # Returns
    /// A `Result` containing the device's public key (device ID).
    pub fn device_id(&self) -> Result<VerifyingKey> {
        // Check if device key already exists
        if let Some(device_key) = self.backend.get_private_key(DEVICE_KEY_NAME)? {
            return Ok(device_key.verifying_key());
        }

        // Generate new device key
        let (signing_key, verifying_key) = generate_keypair();
        self.backend
            .store_private_key(DEVICE_KEY_NAME, signing_key)?;

        Ok(verifying_key)
    }

    /// Get the device ID as a formatted string.
    ///
    /// This is a convenience method that returns the device ID (public key)
    /// in a standard formatted string representation.
    ///
    /// # Returns
    /// A `Result` containing the formatted device ID string.
    pub fn device_id_string(&self) -> Result<String> {
        let device_key = self.device_id()?;
        Ok(format_public_key(&device_key))
    }

    /// Check if this database has a device key configured.
    ///
    /// # Returns
    /// A `Result` containing `true` if a device key exists, `false` otherwise.
    pub fn has_device_key(&self) -> Result<bool> {
        Ok(self.backend.get_private_key(DEVICE_KEY_NAME)?.is_some())
    }

    /// Create a new database in the instance.
    ///
    /// A `Database` represents a collection of related entries, analogous to a table.
    /// It is initialized with settings defined by a `Doc` CRDT.
    /// All databases must now be created with authentication.
    ///
    /// # Arguments
    /// * `settings` - The initial settings for the database, typically including metadata like a name.
    /// * `signing_key_name` - Authentication key name to use for the initial commit. Required for all databases.
    ///
    /// # Returns
    /// A `Result` containing the newly created `Database` or an error.
    pub fn new_database(
        &self,
        settings: Doc,
        signing_key_name: impl AsRef<str>,
    ) -> Result<Database> {
        let database = Database::new(settings, Arc::clone(&self.backend), signing_key_name)?;
        Ok(self.configure_database_sync_hooks(database))
    }

    /// Create a new database with default empty settings
    /// All databases must now be created with authentication.
    ///
    /// # Arguments
    /// * `signing_key_name` - Authentication key name to use for the initial commit. Required for all databases.
    ///
    /// # Returns
    /// A `Result` containing the newly created `Database` or an error.
    pub fn new_database_default(&self, signing_key_name: impl AsRef<str>) -> Result<Database> {
        let mut settings = Doc::new();

        // Add a unique database identifier to ensure each database gets a unique root ID
        // This prevents content-addressable collision when creating multiple databases
        // with identical settings
        let unique_id = format!(
            "database_{}",
            rand::thread_rng()
                .sample_iter(&rand::distributions::Alphanumeric)
                .take(16)
                .map(char::from)
                .collect::<String>()
        );
        settings.set_string("database_id", unique_id);

        self.new_database(settings, signing_key_name)
    }

    /// Load an existing database from the backend by its root ID.
    ///
    /// # Arguments
    /// * `root_id` - The content-addressable ID of the root `Entry` of the database to load.
    ///
    /// # Returns
    /// A `Result` containing the loaded `Database` or an error if the root ID is not found.
    pub fn load_database(&self, root_id: &ID) -> Result<Database> {
        // First validate the root_id exists in the backend
        // Make sure the entry exists
        self.backend.get(root_id)?;

        // Create a database object with the given root_id
        let database = Database::new_from_id(root_id.clone(), Arc::clone(&self.backend))?;
        Ok(self.configure_database_sync_hooks(database))
    }

    /// Load all databases stored in the backend.
    ///
    /// This retrieves all known root entry IDs from the backend and constructs
    /// `Database` instances for each.
    ///
    /// # Returns
    /// A `Result` containing a vector of all `Database` instances or an error.
    pub fn all_databases(&self) -> Result<Vec<Database>> {
        let root_ids = self.backend.all_roots()?;
        let mut databases = Vec::new();

        for root_id in root_ids {
            let database = Database::new_from_id(root_id.clone(), Arc::clone(&self.backend))?;
            databases.push(self.configure_database_sync_hooks(database));
        }

        Ok(databases)
    }

    /// Find databases by their assigned name.
    ///
    /// Searches through all databases in the backend and returns those whose "name"
    /// setting matches the provided name.
    ///
    /// # Arguments
    /// * `name` - The name to search for.
    ///
    /// # Returns
    /// A `Result` containing a vector of `Database` instances whose name matches,
    /// or an error.
    ///
    /// # Errors
    /// Returns `InstanceError::DatabaseNotFound` if no databases with the specified name are found.
    pub fn find_database(&self, name: impl AsRef<str>) -> Result<Vec<Database>> {
        let name = name.as_ref();
        let all_databases = self.all_databases()?;
        let mut matching_databases = Vec::new();

        for database in all_databases {
            // Attempt to get the name from the database's settings
            if let Ok(database_name) = database.get_name()
                && database_name == name
            {
                matching_databases.push(database);
            }
            // Ignore databases where getting the name fails or doesn't match
        }

        if matching_databases.is_empty() {
            Err(InstanceError::DatabaseNotFound {
                name: name.to_string(),
            }
            .into())
        } else {
            Ok(matching_databases)
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
    /// # use eidetica::{backend::database::InMemory, instance::Instance};
    /// let backend = InMemory::new();
    /// let db = Instance::new(Box::new(backend));
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
        let all_keys = self.backend.list_private_keys()?;
        // Filter out the device key as it's for internal use only
        Ok(all_keys
            .into_iter()
            .filter(|key| key != DEVICE_KEY_NAME)
            .collect())
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
    /// Creates a new sync settings database and initializes the sync module.
    /// This method should be called once per database instance to enable sync functionality.
    /// The sync module will have access to this database's device identity through the backend.
    ///
    /// # Returns
    /// A `Result` containing a new Instance with the sync module initialized.
    pub fn with_sync(mut self) -> Result<Self> {
        // Ensure device key exists before creating sync
        self.device_id()?;

        let sync = Sync::new(Arc::clone(&self.backend))?;
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

    /// Load an existing Sync module from a sync database root ID.
    ///
    /// # Arguments
    /// * `sync_database_root_id` - The root ID of an existing sync database
    ///
    /// # Returns
    /// A `Result` containing a new Instance with the sync module loaded.
    pub fn with_sync_from_database(mut self, sync_database_root_id: &ID) -> Result<Self> {
        // Ensure device key exists before loading sync
        self.device_id()?;

        let sync = Sync::load(Arc::clone(&self.backend), sync_database_root_id)?;
        self.sync = Some(sync);
        Ok(self)
    }

    /// Configure a database with sync hooks if sync is enabled.
    ///
    /// This is a helper method that sets up sync hooks for a database
    /// when sync is available in this Instance instance.
    ///
    /// # Arguments
    /// * `database` - The database to configure with sync hooks
    ///
    /// # Returns
    /// The database with sync hooks configured if sync is enabled
    fn configure_database_sync_hooks(&self, database: Database) -> Database {
        // TODO: Implement database sync hooks for the new BackgroundSync architecture
        // The new architecture requires per-peer hooks rather than a collection
        // For now, sync hooks need to be set up manually per peer
        database
    }
}
