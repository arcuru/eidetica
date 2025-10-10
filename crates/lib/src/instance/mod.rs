//!
//! Provides the main database structures (`Instance` and `Database`).
//!
//! `Instance` manages multiple `Database` instances and interacts with the storage `Database`.
//! `Database` represents a single, independent history of data entries, analogous to a table or branch.

use std::sync::Arc;

use ed25519_dalek::VerifyingKey;
use rand::Rng;

use crate::{
    Database, Result, auth::crypto::format_public_key, backend::BackendDB, crdt::Doc, entry::ID,
    sync::Sync, user::User,
};

pub mod errors;

// Re-export main types for easier access
pub use errors::InstanceError;

/// Private constants for device identity management
const DEVICE_KEY_NAME: &str = "_device_key";

/// Database implementation on top of the storage backend.
///
/// Instance manages infrastructure only:
/// - Backend storage and device identity (_device_key)
/// - System databases (_users, _databases, _sync)
/// - User account management (create, login, list)
///
/// All database creation and key operations happen through User after login.
///
/// ## Example
///
/// ```
/// # use eidetica::{backend::database::InMemory, Instance, crdt::Doc};
/// let instance = Instance::new_unified(Box::new(InMemory::new()))?;
///
/// // Create passwordless user
/// instance.create_user("alice", None)?;
/// let mut user = instance.login_user("alice", None)?;
///
/// // Use User API for operations
/// let mut settings = Doc::new();
/// settings.set_string("name", "my_database");
/// let db = user.new_database(settings)?;
/// # Ok::<(), eidetica::Error>(())
/// ```
///
/// # Legacy API (deprecated)
///
/// Use `Instance::new_unified()` instead of `Instance::new()`.
pub struct Instance {
    /// The database storage used by the database.
    backend: Arc<dyn BackendDB>,
    /// Synchronization module for this database instance.
    sync: Option<Sync>,
    /// System database for user directory (_users)
    users_db: Database,
    /// System database for database tracking (_databases)
    #[allow(dead_code)] // Reserved for future database tracking features
    databases_db: Database,
    // Blob storage will be separate, maybe even just an extension
    // storage: IPFS;
}

impl Instance {
    /// Create a new Instance (legacy constructor).
    ///
    /// **DEPRECATED**: Use `Instance::new_unified()` for explicit user management.
    ///
    /// # Panics
    /// Panics if instance initialization fails. Use [`Instance::new_unified`] for error handling.
    ///
    /// # Arguments
    /// * `backend` - The storage backend to use
    #[deprecated(
        since = "0.1.0",
        note = "Use Instance::new_unified() for explicit user management. Migration: instance.create_user(\"user\", None)?; let user = instance.login_user(\"user\", None)?;"
    )]
    pub fn new(backend: Box<dyn BackendDB>) -> Self {
        Self::new_unified(backend).expect("Failed to initialize Instance")
    }

    /// Load an existing Instance or create a new one (recommended).
    ///
    /// This is the recommended method for initializing an Instance. It automatically detects
    /// whether the backend contains existing system state (device key and system databases)
    /// and loads them, or creates new ones if starting fresh.
    ///
    /// Instance manages infrastructure only:
    /// - Backend storage and device identity (_device_key)
    /// - System databases (_users, _databases, _sync)
    /// - User account management (create, login, list)
    ///
    /// All database creation and key operations require explicit User login.
    ///
    /// # Arguments
    /// * `backend` - The storage backend to use
    ///
    /// # Returns
    /// A Result containing the configured Instance
    ///
    /// # Example
    /// ```
    /// # use eidetica::{backend::database::InMemory, Instance, crdt::Doc};
    /// let backend = InMemory::new();
    /// let instance = Instance::load(Box::new(backend))?;
    ///
    /// // Create and login user explicitly
    /// instance.create_user("alice", None)?;
    /// let mut user = instance.login_user("alice", None)?;
    ///
    /// // Use User API for operations
    /// let mut settings = Doc::new();
    /// settings.set_string("name", "my_database");
    /// let db = user.new_database(settings)?;
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn load(backend: Box<dyn BackendDB>) -> Result<Self> {
        use crate::constants::{DATABASES, USERS};

        let backend: Arc<dyn BackendDB> = Arc::from(backend);

        // Check if this is an existing backend (has _device_key)
        let has_device_key = backend.get_private_key(DEVICE_KEY_NAME)?.is_some();

        if has_device_key {
            // Existing backend: load system databases
            let all_roots = backend.all_roots()?;

            // Find system databases by name
            let mut users_db = None;
            let mut databases_db = None;

            for root_id in all_roots {
                let db = Database::new_from_id(root_id, Arc::clone(&backend))?;
                if let Ok(name) = db.get_name() {
                    match name.as_str() {
                        USERS => users_db = Some(db),
                        DATABASES => databases_db = Some(db),
                        _ => {} // Ignore other databases
                    }
                }

                // Stop searching if we found both
                if users_db.is_some() && databases_db.is_some() {
                    break;
                }
            }

            // Verify we found both system databases
            let users_db = users_db.ok_or(
                InstanceError::DeviceKeyNotFound, // TODO: Better error for missing system DB
            )?;
            let databases_db = databases_db.ok_or(
                InstanceError::DeviceKeyNotFound, // TODO: Better error for missing system DB
            )?;

            Ok(Self {
                backend,
                sync: None,
                users_db,
                databases_db,
            })
        } else {
            // New backend: initialize like new_unified()
            Self::new_unified_internal(backend)
        }
    }

    /// Create a new Instance with explicit user management (recommended).
    ///
    /// This is the recommended constructor for all new code. Instance manages infrastructure only:
    /// - Backend storage and device identity (_device_key)
    /// - System databases (_users, _databases, _sync)
    /// - User account management (create, login, list)
    ///
    /// All database creation and key operations require explicit User login.
    ///
    /// # Arguments
    /// * `backend` - The storage backend to use
    ///
    /// # Returns
    /// A Result containing the configured Instance
    ///
    /// # Example
    /// ```
    /// # use eidetica::{backend::database::InMemory, Instance, crdt::Doc};
    /// let backend = InMemory::new();
    /// let instance = Instance::new_unified(Box::new(backend))?;
    ///
    /// // Create and login user explicitly
    /// instance.create_user("alice", None)?;
    /// let mut user = instance.login_user("alice", None)?;
    ///
    /// // Use User API for operations
    /// let mut settings = Doc::new();
    /// settings.set_string("name", "my_database");
    /// let db = user.new_database(settings)?;
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn new_unified(backend: Box<dyn BackendDB>) -> Result<Self> {
        Self::new_unified_internal(Arc::from(backend))
    }

    /// Internal implementation of new_unified that works with Arc<dyn BackendDB>
    fn new_unified_internal(backend: Arc<dyn BackendDB>) -> Result<Self> {
        use crate::{
            auth::crypto::{format_public_key, generate_keypair},
            user::system_databases::{create_databases_tracking, create_users_database},
        };

        // 1. Generate and store instance device key (_device_key)
        let (device_key, device_pubkey) = generate_keypair();
        let device_pubkey_str = format_public_key(&device_pubkey);
        backend.store_private_key(DEVICE_KEY_NAME, device_key)?;

        // 2. Create system databases with _device_key
        let users_db = create_users_database(Arc::clone(&backend), &device_pubkey_str)?;
        let databases_db = create_databases_tracking(Arc::clone(&backend), &device_pubkey_str)?;

        // 3. Return instance
        Ok(Self {
            backend,
            sync: None,
            users_db,
            databases_db,
        })
    }

    /// Get a reference to the backend
    pub fn backend(&self) -> &Arc<dyn BackendDB> {
        &self.backend
    }

    // === User Management ===

    /// Create a new user account with flexible password handling.
    ///
    /// Creates a user with or without password protection. Passwordless users are appropriate
    /// for embedded applications where filesystem access = database access.
    ///
    /// # Arguments
    /// * `user_id` - Unique user identifier (username)
    /// * `password` - Optional password. If None, user is passwordless (instant login, no encryption)
    ///
    /// # Returns
    /// A Result containing the user's UUID (stable internal identifier)
    pub fn create_user(&self, user_id: &str, password: Option<&str>) -> Result<String> {
        use crate::user::system_databases::create_user;

        let (user_uuid, _user_info) =
            create_user(&self.users_db, Arc::clone(&self.backend), user_id, password)?;
        Ok(user_uuid)
    }

    /// Login a user with flexible password handling.
    ///
    /// Returns a User session object that provides access to user operations.
    /// For password-protected users, provide the password. For passwordless users, pass None.
    ///
    /// # Arguments
    /// * `user_id` - User identifier (username)
    /// * `password` - Optional password. None for passwordless users.
    ///
    /// # Returns
    /// A Result containing the User session
    pub fn login_user(&self, user_id: &str, password: Option<&str>) -> Result<User> {
        use crate::user::system_databases::login_user;

        login_user(&self.users_db, Arc::clone(&self.backend), user_id, password)
    }

    /// List all user IDs.
    ///
    /// # Returns
    /// A Result containing a vector of user IDs
    pub fn list_users(&self) -> Result<Vec<String>> {
        use crate::user::system_databases::list_users;

        list_users(&self.users_db)
    }

    // === Device Identity Management ===
    //
    // The Instance's device identity (_device_key) is stored in the backend.

    /// Get the device ID (public key).
    ///
    /// The device key (_device_key) is stored in the backend.
    ///
    /// # Returns
    /// A `Result` containing the device's public key (device ID).
    pub fn device_id(&self) -> Result<VerifyingKey> {
        let device_key = self
            .backend
            .get_private_key(DEVICE_KEY_NAME)?
            .ok_or_else(|| crate::Error::from(InstanceError::DeviceKeyNotFound))?;
        Ok(device_key.verifying_key())
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

    /// Create a new database in the instance.
    ///
    /// **DEPRECATED**: Use `User::new_database()` instead. This method will be removed in a future version.
    ///
    /// # Arguments
    /// * `settings` - The initial settings for the database
    /// * `signing_key_name` - The name of the signing key to use
    ///
    /// # Returns
    /// A `Result` containing the newly created `Database` or an error.
    #[deprecated(
        since = "0.1.0",
        note = "Use User::new_database() instead for proper user context"
    )]
    pub fn new_database(
        &self,
        settings: Doc,
        signing_key_name: impl AsRef<str>,
    ) -> Result<Database> {
        let database = Database::new(
            settings,
            Arc::clone(&self.backend),
            signing_key_name.as_ref(),
        )?;
        Ok(self.configure_database_sync_hooks(database))
    }

    /// Create a new database with default empty settings.
    ///
    /// **DEPRECATED**: Use `User::new_database()` instead. This method will be removed in a future version.
    ///
    /// # Arguments
    /// * `signing_key_name` - The name of the signing key to use
    ///
    /// # Returns
    /// A `Result` containing the newly created `Database` or an error.
    #[deprecated(
        since = "0.1.0",
        note = "Use User::new_database() instead for proper user context"
    )]
    #[allow(deprecated)]
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

    // === Authentication Key Management (Deprecated Convenience) ===
    //
    // These methods provide deprecated convenience wrappers for key management.
    // Use the explicit User API for key management instead.

    /// Generate a new Ed25519 keypair (deprecated).
    ///
    /// **DEPRECATED**: Use `User::add_private_key()` instead. This method will be removed in a future version.
    ///
    /// # Arguments
    /// * `display_name` - Optional display name for the key
    ///
    /// # Returns
    /// A `Result` containing the key ID (public key string) or an error.
    #[deprecated(
        since = "0.1.0",
        note = "Use User::add_private_key() instead for proper user context"
    )]
    pub fn add_private_key(&self, display_name: &str) -> Result<VerifyingKey> {
        // Generate keypair using backend-stored keys (legacy path)
        use crate::auth::crypto::generate_keypair;
        let (signing_key, verifying_key) = generate_keypair();

        // Store in backend with display_name as the key name (legacy storage)
        self.backend.store_private_key(display_name, signing_key)?;

        Ok(verifying_key)
    }

    /// List all private key IDs.
    ///
    /// # Returns
    /// A `Result` containing a vector of key IDs or an error.
    pub fn list_private_keys(&self) -> Result<Vec<String>> {
        // List keys from backend storage
        self.backend.list_private_keys()
    }

    /// Import an existing Ed25519 keypair (deprecated).
    ///
    /// **DEPRECATED**: Use `User::add_private_key()` with generated keys instead.
    ///
    /// # Arguments
    /// * `key_id` - Key identifier (usually the public key string)
    /// * `signing_key` - The Ed25519 signing key to import
    ///
    /// # Returns
    /// A `Result` containing the key ID or an error.
    #[deprecated(
        since = "0.1.0",
        note = "Use User::add_private_key() instead for proper user context"
    )]
    pub fn import_private_key(
        &self,
        key_id: &str,
        signing_key: ed25519_dalek::SigningKey,
    ) -> Result<String> {
        // Import key into backend storage (legacy path)
        self.backend.store_private_key(key_id, signing_key)?;

        Ok(key_id.to_string())
    }

    /// Get the public key for a stored private key (deprecated).
    ///
    /// **DEPRECATED**: Use `User::get_signing_key()` instead.
    ///
    /// # Arguments
    /// * `key_id` - The key identifier
    ///
    /// # Returns
    /// A `Result` containing the public key or an error.
    #[deprecated(
        since = "0.1.0",
        note = "Use User::get_signing_key() instead for proper user context"
    )]
    pub fn get_public_key(&self, key_id: &str) -> Result<VerifyingKey> {
        // Get signing key from backend storage (legacy path)
        let signing_key = self.backend.get_private_key(key_id)?.ok_or_else(|| {
            InstanceError::SigningKeyNotFound {
                key_name: key_id.to_string(),
            }
        })?;

        Ok(signing_key.verifying_key())
    }

    /// Get the formatted public key string for a stored private key (deprecated).
    ///
    /// **DEPRECATED**: Use `User::get_signing_key()` instead and format manually if needed.
    ///
    /// # Arguments
    /// * `key_name` - The key identifier
    ///
    /// # Returns
    /// A `Result` containing the formatted public key string.
    #[deprecated(
        since = "0.1.0",
        note = "Use User::get_signing_key() and format_public_key() instead"
    )]
    #[allow(deprecated)]
    pub fn get_formatted_public_key(&self, key_name: impl AsRef<str>) -> Result<String> {
        let public_key = self.get_public_key(key_name.as_ref())?;
        Ok(format_public_key(&public_key))
    }

    /// Remove a private key (deprecated).
    ///
    /// **DEPRECATED**: Key removal not yet implemented through User API.
    ///
    /// # Arguments
    /// * `key_id` - The key identifier to remove
    ///
    /// # Returns
    /// A `Result` indicating success or failure.
    #[deprecated(since = "0.1.0", note = "Deprecated API, will be removed")]
    pub fn remove_private_key(&self, key_id: &str) -> Result<()> {
        Err(InstanceError::OperationNotSupported {
            operation: format!("remove_private_key('{}') not yet implemented", key_id),
        }
        .into())
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

    /// Get a mutable reference to the Sync module for this database.
    ///
    /// This allows calling mutable methods on the Sync module such as:
    /// - `enable_http_transport()`
    /// - `enable_iroh_transport()`
    /// - `start_server_async()`
    /// - `connect_to_peer()`
    /// - `register_peer()`
    /// - etc.
    ///
    /// # Returns
    /// An `Option` containing a mutable reference to the `Sync` module if initialized.
    pub fn sync_mut(&mut self) -> Option<&mut Sync> {
        self.sync.as_mut()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Error, backend::database::InMemory};

    #[test]
    #[allow(deprecated)]
    fn test_create_user() -> Result<(), Error> {
        let backend = InMemory::new();
        let instance = Instance::new_unified(Box::new(backend))?;

        // Create user with password
        let user_uuid = instance.create_user("alice", Some("password123")).unwrap();

        assert!(!user_uuid.is_empty());

        // Verify user appears in list
        let users = instance.list_users().unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0], "alice");
        Ok(())
    }

    #[test]
    #[allow(deprecated)]
    fn test_login_user() -> Result<(), Error> {
        let backend = InMemory::new();
        let instance = Instance::new_unified(Box::new(backend))?;

        // Create user
        instance.create_user("alice", Some("password123")).unwrap();

        // Login user
        let user = instance.login_user("alice", Some("password123")).unwrap();
        assert_eq!(user.username(), "alice");

        // Invalid password should fail
        let result = instance.login_user("alice", Some("wrong_password"));
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    #[allow(deprecated)]
    fn test_new_database() {
        let backend = InMemory::new();
        let instance = Instance::new(Box::new(backend));

        // Create database with deprecated API
        let mut settings = Doc::new();
        settings.set_string("name", "test_db");

        let database = instance.new_database(settings, "_device_key").unwrap();
        assert_eq!(database.get_name().unwrap(), "test_db");
    }

    #[test]
    #[allow(deprecated)]
    fn test_new_database_default() {
        let backend = InMemory::new();
        let instance = Instance::new(Box::new(backend));

        // Create database with default settings
        let database = instance.new_database_default("_device_key").unwrap();
        let settings = database.get_settings().unwrap();

        // Should have auto-generated database_id
        assert!(settings.get_string("database_id").is_ok());
    }

    #[test]
    #[allow(deprecated)]
    fn test_new_database_without_key_fails() -> Result<(), Error> {
        let backend = InMemory::new();
        let instance = Instance::new_unified(Box::new(backend))?;

        // Create database requires a signing key
        let mut settings = Doc::new();
        settings.set_string("name", "test_db");

        // This will succeed if a valid key is provided, but we're testing without a valid key
        let result = instance.new_database(settings, "nonexistent_key");
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    #[allow(deprecated)]
    fn test_load_database() {
        let backend = InMemory::new();
        let instance = Instance::new(Box::new(backend));

        // Create a database
        let mut settings = Doc::new();
        settings.set_string("name", "test_db");
        let database = instance.new_database(settings, "_device_key").unwrap();
        let root_id = database.root_id().clone();

        // Load the database
        let loaded_database = instance.load_database(&root_id).unwrap();
        assert_eq!(loaded_database.get_name().unwrap(), "test_db");
    }

    #[test]
    #[allow(deprecated)]
    fn test_all_databases() {
        let backend = InMemory::new();
        let instance = Instance::new(Box::new(backend));

        // Create multiple databases
        let mut settings1 = Doc::new();
        settings1.set_string("name", "db1");
        instance.new_database(settings1, "_device_key").unwrap();

        let mut settings2 = Doc::new();
        settings2.set_string("name", "db2");
        instance.new_database(settings2, "_device_key").unwrap();

        // Get all databases (should include system databases + user databases)
        let databases = instance.all_databases().unwrap();
        assert!(databases.len() >= 2); // At least our 2 databases + system databases
    }

    #[test]
    #[allow(deprecated)]
    fn test_find_database() {
        let backend = InMemory::new();
        let instance = Instance::new(Box::new(backend));

        // Create database with name
        let mut settings = Doc::new();
        settings.set_string("name", "my_special_db");
        instance.new_database(settings, "_device_key").unwrap();

        // Find by name
        let found = instance.find_database("my_special_db").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].get_name().unwrap(), "my_special_db");

        // Not found
        let result = instance.find_database("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_instance_load_new_backend() -> Result<(), Error> {
        // Test that Instance::load() creates new system state for empty backend
        let backend = InMemory::new();
        let instance = Instance::load(Box::new(backend))?;

        // Verify device key was created
        assert!(instance.device_id().is_ok());

        // Verify we can create and login a user
        instance.create_user("alice", None)?;
        let user = instance.login_user("alice", None)?;
        assert_eq!(user.username(), "alice");

        Ok(())
    }

    #[test]
    fn test_instance_load_existing_backend() -> Result<(), Error> {
        // Use a temporary file path for testing
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("eidetica_test_instance_load.json");

        // Create an instance and user, then save the backend
        let backend1 = InMemory::new();
        let instance1 = Instance::load(Box::new(backend1))?;
        instance1.create_user("bob", None)?;
        let mut user1 = instance1.login_user("bob", None)?;

        // Create a user database to verify it persists
        let mut settings = Doc::new();
        settings.set_string("name", "bob_database");
        user1.new_database(settings)?;

        // Save the backend to file
        let backend_guard = instance1.backend();
        if let Some(in_memory) = backend_guard.as_any().downcast_ref::<InMemory>() {
            in_memory.save_to_file(&path)?;
        }

        // Drop the first instance
        drop(instance1);
        drop(user1);

        // Load a new backend from the saved file
        let backend2 = InMemory::load_from_file(&path)?;
        let instance2 = Instance::load(Box::new(backend2))?;

        // Verify the user still exists
        let users = instance2.list_users()?;
        assert_eq!(users.len(), 1);
        assert_eq!(users[0], "bob");

        // Verify we can login the existing user
        let user2 = instance2.login_user("bob", None)?;
        assert_eq!(user2.username(), "bob");

        // Clean up the temporary file
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }

        Ok(())
    }

    #[test]
    fn test_instance_load_device_id_persistence() -> Result<(), Error> {
        // Test that device_id remains the same across reloads
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("eidetica_test_device_id.json");

        // Create instance and get device_id
        let backend1 = InMemory::new();
        let instance1 = Instance::load(Box::new(backend1))?;
        let device_id1 = instance1.device_id_string()?;

        // Save backend
        let backend_guard = instance1.backend();
        if let Some(in_memory) = backend_guard.as_any().downcast_ref::<InMemory>() {
            in_memory.save_to_file(&path)?;
        }
        drop(instance1);

        // Load backend and verify device_id is the same
        let backend2 = InMemory::load_from_file(&path)?;
        let instance2 = Instance::load(Box::new(backend2))?;
        let device_id2 = instance2.device_id_string()?;

        assert_eq!(
            device_id1, device_id2,
            "Device ID should persist across reloads"
        );

        // Clean up
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }

        Ok(())
    }

    #[test]
    fn test_instance_load_with_password_protected_users() -> Result<(), Error> {
        // Test that password-protected users work correctly after reload
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("eidetica_test_password_users.json");

        // Create instance with password-protected user
        let backend1 = InMemory::new();
        let instance1 = Instance::load(Box::new(backend1))?;
        instance1.create_user("secure_alice", Some("secret123"))?;
        let user1 = instance1.login_user("secure_alice", Some("secret123"))?;
        assert_eq!(user1.username(), "secure_alice");
        drop(user1);

        // Save backend
        let backend_guard = instance1.backend();
        if let Some(in_memory) = backend_guard.as_any().downcast_ref::<InMemory>() {
            in_memory.save_to_file(&path)?;
        }
        drop(instance1);

        // Reload and verify password still works
        let backend2 = InMemory::load_from_file(&path)?;
        let instance2 = Instance::load(Box::new(backend2))?;

        // Correct password should work
        let user2 = instance2.login_user("secure_alice", Some("secret123"))?;
        assert_eq!(user2.username(), "secure_alice");

        // Wrong password should fail
        let result = instance2.login_user("secure_alice", Some("wrong_password"));
        assert!(result.is_err(), "Login with wrong password should fail");

        // No password should fail
        let result = instance2.login_user("secure_alice", None);
        assert!(
            result.is_err(),
            "Login without password should fail for password-protected user"
        );

        // Clean up
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }

        Ok(())
    }

    #[test]
    fn test_instance_load_multiple_users() -> Result<(), Error> {
        // Test that multiple users persist correctly
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("eidetica_test_multiple_users.json");

        // Create instance with multiple users (mix of passwordless and password-protected)
        let backend1 = InMemory::new();
        let instance1 = Instance::load(Box::new(backend1))?;

        instance1.create_user("alice", None)?;
        instance1.create_user("bob", Some("bobpass"))?;
        instance1.create_user("charlie", None)?;
        instance1.create_user("diana", Some("dianapass"))?;

        // Verify all users can login
        instance1.login_user("alice", None)?;
        instance1.login_user("bob", Some("bobpass"))?;
        instance1.login_user("charlie", None)?;
        instance1.login_user("diana", Some("dianapass"))?;

        // Save backend
        let backend_guard = instance1.backend();
        if let Some(in_memory) = backend_guard.as_any().downcast_ref::<InMemory>() {
            in_memory.save_to_file(&path)?;
        }
        drop(instance1);

        // Reload and verify all users still exist and can login
        let backend2 = InMemory::load_from_file(&path)?;
        let instance2 = Instance::load(Box::new(backend2))?;

        let users = instance2.list_users()?;
        assert_eq!(users.len(), 4, "All 4 users should be present after reload");
        assert!(users.contains(&"alice".to_string()));
        assert!(users.contains(&"bob".to_string()));
        assert!(users.contains(&"charlie".to_string()));
        assert!(users.contains(&"diana".to_string()));

        // Verify login still works for all users
        instance2.login_user("alice", None)?;
        instance2.login_user("bob", Some("bobpass"))?;
        instance2.login_user("charlie", None)?;
        instance2.login_user("diana", Some("dianapass"))?;

        // Clean up
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }

        Ok(())
    }

    #[test]
    fn test_instance_load_user_databases_persist() -> Result<(), Error> {
        // Test that user-created databases persist across reloads
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("eidetica_test_user_dbs.json");

        // Create instance, user, and multiple databases
        let backend1 = InMemory::new();
        let instance1 = Instance::load(Box::new(backend1))?;
        instance1.create_user("eve", None)?;
        let mut user1 = instance1.login_user("eve", None)?;

        // Create multiple databases
        let mut settings1 = Doc::new();
        settings1.set_string("name", "database_one");
        settings1.set_string("purpose", "testing");
        let db1 = user1.new_database(settings1)?;
        let db1_root = db1.root_id().clone();

        let mut settings2 = Doc::new();
        settings2.set_string("name", "database_two");
        settings2.set_string("purpose", "production");
        let db2 = user1.new_database(settings2)?;
        let db2_root = db2.root_id().clone();

        drop(db1);
        drop(db2);
        drop(user1);

        // Save backend
        let backend_guard = instance1.backend();
        if let Some(in_memory) = backend_guard.as_any().downcast_ref::<InMemory>() {
            in_memory.save_to_file(&path)?;
        }
        drop(instance1);

        // Reload and verify databases still exist
        let backend2 = InMemory::load_from_file(&path)?;
        let instance2 = Instance::load(Box::new(backend2))?;
        let _user2 = instance2.login_user("eve", None)?;

        // Load databases by root_id and verify their settings
        let loaded_db1 = instance2.load_database(&db1_root)?;
        assert_eq!(loaded_db1.get_name()?, "database_one");
        let settings1_doc = loaded_db1.get_settings()?;
        assert_eq!(settings1_doc.get_string("purpose")?, "testing");

        let loaded_db2 = instance2.load_database(&db2_root)?;
        assert_eq!(loaded_db2.get_name()?, "database_two");
        let settings2_doc = loaded_db2.get_settings()?;
        assert_eq!(settings2_doc.get_string("purpose")?, "production");

        // Clean up
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }

        Ok(())
    }

    #[test]
    fn test_instance_load_idempotency() -> Result<(), Error> {
        // Test that loading the same backend multiple times gives consistent results
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("eidetica_test_idempotency.json");

        // Create and save initial state
        let backend1 = InMemory::new();
        let instance1 = Instance::load(Box::new(backend1))?;
        instance1.create_user("frank", None)?;
        let device_id1 = instance1.device_id_string()?;

        let backend_guard = instance1.backend();
        if let Some(in_memory) = backend_guard.as_any().downcast_ref::<InMemory>() {
            in_memory.save_to_file(&path)?;
        }
        drop(instance1);

        // Load the same backend multiple times and verify consistency
        for i in 0..3 {
            let backend = InMemory::load_from_file(&path)?;
            let instance = Instance::load(Box::new(backend))?;

            // Device ID should be the same every time
            let device_id = instance.device_id_string()?;
            assert_eq!(
                device_id, device_id1,
                "Device ID should be consistent on reload {}",
                i
            );

            // User list should be the same
            let users = instance.list_users()?;
            assert_eq!(users.len(), 1);
            assert_eq!(users[0], "frank");

            // Should be able to login
            let user = instance.login_user("frank", None)?;
            assert_eq!(user.username(), "frank");

            drop(user);
            drop(instance);
        }

        // Clean up
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }

        Ok(())
    }

    #[test]
    fn test_instance_load_new_vs_existing() -> Result<(), Error> {
        // Test the difference between loading new and existing backends
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("eidetica_test_new_vs_existing.json");

        // Create first instance (new backend)
        let backend1 = InMemory::new();
        let instance1 = Instance::load(Box::new(backend1))?;
        let device_id1 = instance1.device_id_string()?;
        instance1.create_user("grace", None)?;

        let backend_guard = instance1.backend();
        if let Some(in_memory) = backend_guard.as_any().downcast_ref::<InMemory>() {
            in_memory.save_to_file(&path)?;
        }
        drop(instance1);

        // Load existing backend
        let backend2 = InMemory::load_from_file(&path)?;
        let instance2 = Instance::load(Box::new(backend2))?;
        let device_id2 = instance2.device_id_string()?;

        // Device ID should match (existing backend)
        assert_eq!(device_id1, device_id2);

        // User should exist (existing backend)
        let users = instance2.list_users()?;
        assert_eq!(users.len(), 1);
        assert_eq!(users[0], "grace");
        drop(instance2);

        // Create completely new instance (different backend)
        let backend3 = InMemory::new();
        let instance3 = Instance::load(Box::new(backend3))?;
        let device_id3 = instance3.device_id_string()?;

        // Device ID should be different (new backend)
        assert_ne!(device_id1, device_id3);

        // No users should exist (new backend)
        let users = instance3.list_users()?;
        assert_eq!(users.len(), 0);

        // Clean up
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }

        Ok(())
    }
}
