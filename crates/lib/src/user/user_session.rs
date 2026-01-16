//! User session management
//!
//! Represents an authenticated user session with decrypted keys.
//!
//! # API Overview
//!
//! The User API is organized into three areas for managing Databases:
//!
//! ## Database Lifecycle
//!
//! - **`create_database()`** - Create a new database
//! - **`open_database()`** - Open an existing database
//! - **`find_database()`** - Search for databases by name
//!
//! ## Tracked Databases
//!
//! Manage your personal list of tracked databases:
//!
//! - **`databases()`** - List all tracked databases
//! - **`database()`** - Get a specific tracked database
//! - **`track_database()`** - Add or update a tracked database (upsert)
//! - **`untrack_database()`** - Remove a database from your tracked list
//!
//! ## Key-Database Mappings
//!
//! Control which keys access which databases:
//!
//! - **`map_key()`** - Map a key to a SigKey identifier for a database
//! - **`key_mapping()`** - Get the SigKey mapping for a key-database pair
//! - **`find_key()`** - Find which key can access a database
//!
//! This explicit approach ensures predictable behavior and avoids ambiguity about which
//! keys have access to which databases.

use handle_trait::Handle;

use super::{UserKeyManager, types::UserInfo};
use crate::{
    Database, Instance, Result,
    auth::{self, SigKey},
    store::Table,
    user::{TrackedDatabase, UserError},
};

/// User session object, returned after successful login
///
/// Represents an authenticated user with decrypted private keys loaded in memory.
/// The User struct provides access to key management, tracked databases, and
/// bootstrap approval operations.
pub struct User {
    /// Stable internal user UUID (Table primary key)
    user_uuid: String,

    /// Username (login identifier)
    username: String,

    /// User's private database (contains encrypted keys and tracked databases)
    user_database: Database,

    /// Instance reference for database operations
    instance: Instance,

    /// Decrypted user keys (in memory only during session)
    key_manager: UserKeyManager,

    /// User info (cached from _users database)
    user_info: UserInfo,
}

impl std::fmt::Debug for User {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("User")
            .field("user_uuid", &self.user_uuid)
            .field("username", &self.username)
            .field("user_database", &self.user_database)
            .field("instance", &self.instance)
            .field("key_manager", &"<KeyManager [sensitive]>")
            .field("user_info", &self.user_info)
            .finish()
    }
}

impl User {
    /// Create a new User session
    ///
    /// This is an internal constructor used after successful login.
    /// Use `Instance::login_user()` to create a User session.
    ///
    /// # Arguments
    /// * `user_uuid` - Internal UUID (Table primary key)
    /// * `user_info` - User information from _users database
    /// * `user_database` - The user's private database
    /// * `instance` - Instance reference
    /// * `key_manager` - Initialized key manager with decrypted keys
    #[allow(dead_code)]
    pub(crate) fn new(
        user_uuid: String,
        user_info: UserInfo,
        user_database: Database,
        instance: Instance,
        key_manager: UserKeyManager,
    ) -> Self {
        Self {
            user_uuid,
            username: user_info.username.clone(),
            user_database,
            instance,
            key_manager,
            user_info,
        }
    }

    // === Basic Session Methods ===

    /// Get the internal user UUID (stable identifier)
    pub fn user_uuid(&self) -> &str {
        &self.user_uuid
    }

    /// Get the username (login identifier)
    pub fn username(&self) -> &str {
        &self.username
    }

    /// Get a reference to the user's database
    pub fn user_database(&self) -> &Database {
        &self.user_database
    }

    /// Get a reference to the backend
    pub fn backend(&self) -> &crate::instance::backend::Backend {
        self.instance.backend()
    }

    /// Get a reference to the user info
    pub fn user_info(&self) -> &UserInfo {
        &self.user_info
    }

    /// Logout (consumes self and clears decrypted keys from memory)
    ///
    /// After logout, all decrypted keys are zeroized and the session is ended.
    /// Keys are automatically cleared when the User is dropped.
    pub fn logout(self) -> Result<()> {
        // Consume self, all keys are stored in other Types that zeroize themselves on drop
        Ok(())
    }

    // === Key Manager Access (Internal) ===

    /// Get a reference to the key manager (for internal use)
    #[allow(dead_code)]
    pub(crate) fn key_manager(&self) -> &UserKeyManager {
        &self.key_manager
    }

    /// Get a mutable reference to the key manager (for internal use)
    #[allow(dead_code)]
    pub(crate) fn key_manager_mut(&mut self) -> &mut UserKeyManager {
        &mut self.key_manager
    }

    // === Database Operations (User Context) ===

    /// Create a new database with explicit key selection.
    ///
    /// This method requires you to specify which key should be used to create and manage
    /// the database, providing explicit control over key-database relationships.
    ///
    /// # Arguments
    /// * `settings` - Initial database settings (metadata, name, etc.)
    /// * `key_id` - The ID of the key to use for this database (public key string)
    ///
    /// # Returns
    /// The created Database
    ///
    /// # Errors
    /// - Returns an error if the specified key_id doesn't exist
    /// - Returns an error if the key cannot be retrieved
    ///
    /// # Example
    /// ```rust,ignore
    /// // Get available keys
    /// let keys = user.list_keys()?;
    /// let key_id = &keys[1]; // Use the second key
    ///
    /// // Create database with explicit key selection
    /// let mut settings = Doc::new();
    /// settings.set("name", "My Database");
    /// let database = user.new_database(settings, key_id)?;
    /// ```
    pub async fn create_database(
        &mut self,
        settings: crate::crdt::Doc,
        key_id: &str,
    ) -> Result<crate::Database> {
        use crate::store::Table;
        use crate::user::types::{SyncSettings, UserKey};

        // Get the signing key from UserKeyManager
        let signing_key = self
            .key_manager
            .get_signing_key(key_id)
            .ok_or_else(|| crate::user::errors::UserError::KeyNotFound {
                key_id: key_id.to_string(),
            })?
            .clone();

        // Create the database with the provided key directly
        let database =
            Database::create(settings, &self.instance, signing_key, key_id.to_string()).await?;

        // Store the mapping in UserKey and track the database
        let tx = self.user_database.new_transaction().await?;
        let keys_table = tx.get_store::<Table<UserKey>>("keys").await?;

        // Find the key metadata in the database
        let (uuid_primary_key, mut metadata) = keys_table
            .search(|uk| uk.key_id == key_id)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| crate::user::errors::UserError::KeyNotFound {
                key_id: key_id.to_string(),
            })?;

        // Add the database sigkey mapping
        metadata
            .database_sigkeys
            .insert(database.root_id().clone(), key_id.to_string());

        // Update the key in user database using the UUID primary key
        keys_table.set(&uuid_primary_key, metadata.clone()).await?;

        // Also track the database in the databases table
        let databases_table = tx.get_store::<Table<TrackedDatabase>>("databases").await?;
        let tracked = TrackedDatabase {
            database_id: database.root_id().clone(),
            key_id: key_id.to_string(),
            sync_settings: SyncSettings::default(),
        };
        databases_table.set(database.root_id(), tracked).await?;

        tx.commit().await?;

        // Update the in-memory key manager with the updated metadata
        self.key_manager.add_key(metadata)?;

        Ok(database)
    }

    /// Open an existing database by its root ID using this user's keys.
    ///
    /// This method automatically:
    /// 1. Finds an appropriate key that has access to the database
    /// 2. Retrieves the decrypted SigningKey from the UserKeyManager
    /// 3. Gets the SigKey mapping for this database
    /// 4. Creates a Database instance configured with the user's key
    ///
    /// The returned Database will use the user's provided key for all operations,
    /// without requiring backend key lookups.
    ///
    /// # Arguments
    /// * `root_id` - The root entry ID of the database
    ///
    /// # Returns
    /// The opened Database configured to use this user's keys
    ///
    /// # Errors
    /// - Returns an error if no key is found for the database
    /// - Returns an error if no SigKey mapping exists
    /// - Returns an error if the key is not in the UserKeyManager
    pub async fn open_database(&self, root_id: &crate::entry::ID) -> Result<crate::Database> {
        // Validate the root exists
        self.instance.backend().get(root_id).await?;

        // Find an appropriate key for this database
        let key_id =
            self.find_key(root_id)?
                .ok_or_else(|| super::errors::UserError::NoKeyForDatabase {
                    database_id: root_id.clone(),
                })?;

        // Get the SigningKey from UserKeyManager
        let signing_key = self.key_manager.get_signing_key(&key_id).ok_or_else(|| {
            super::errors::UserError::KeyNotFound {
                key_id: key_id.clone(),
            }
        })?;

        // Get the SigKey mapping for this database
        let sigkey = self.key_mapping(&key_id, root_id)?.ok_or_else(|| {
            super::errors::UserError::NoSigKeyMapping {
                key_id: key_id.clone(),
                database_id: root_id.clone(),
            }
        })?;

        // Create Database with user-provided key
        Database::open(self.instance.handle(), root_id, signing_key.clone(), sigkey).await
    }

    /// Find databases by name among the user's tracked databases.
    ///
    /// Searches only the databases this user has tracked for those matching the given name.
    ///
    /// # Arguments
    /// * `name` - Database name to search for
    ///
    /// # Returns
    /// Vector of matching databases from the user's tracked list
    pub async fn find_database(&self, name: impl AsRef<str>) -> Result<Vec<crate::Database>> {
        let name = name.as_ref();
        let tracked = self.databases().await?;
        let mut matching = Vec::new();

        for tracked_db in tracked {
            if let Ok(database) = self.instance.load_database(&tracked_db.database_id).await
                && let Ok(db_name) = database.get_name().await
                && db_name == name
            {
                matching.push(database);
            }
        }

        if matching.is_empty() {
            Err(crate::user::UserError::DatabaseNotTracked {
                database_id: format!("name:{name}"),
            }
            .into())
        } else {
            Ok(matching)
        }
    }

    /// Find which key can access a database.
    ///
    /// Searches this user's keys to find one that can access the specified database.
    /// Considers the SigKey mappings stored in user key metadata.
    ///
    /// Returns the key_id of a suitable key, preferring keys with mappings for this database.
    ///
    /// # Arguments
    /// * `database_id` - The ID of the database
    ///
    /// # Returns
    /// Some(key_id) if a suitable key is found, None if no keys can access this database
    pub fn find_key(&self, database_id: &crate::entry::ID) -> Result<Option<String>> {
        // Iterate through all keys and find ones with SigKey mappings for this database
        for key_id in self.key_manager.list_key_ids() {
            if let Some(metadata) = self.key_manager.get_key_metadata(&key_id)
                && metadata.database_sigkeys.contains_key(database_id)
            {
                return Ok(Some(key_id));
            }
        }

        // No key found with mapping for this database
        Ok(None)
    }

    /// Get the SigKey mapping for a key in a specific database.
    ///
    /// Users map their private keys to SigKey identifiers on a per-database basis.
    /// This retrieves the SigKey identifier that a specific key uses in
    /// a specific database's authentication settings.
    ///
    /// # Arguments
    /// * `key_id` - The user's key identifier
    /// * `database_id` - The database ID
    ///
    /// # Returns
    /// Some(sigkey) if a mapping exists, None if no mapping is configured
    ///
    /// # Errors
    /// Returns an error if the key_id doesn't exist in the UserKeyManager
    pub fn key_mapping(
        &self,
        key_id: &str,
        database_id: &crate::entry::ID,
    ) -> Result<Option<String>> {
        let metadata = self.key_manager.get_key_metadata(key_id).ok_or_else(|| {
            super::errors::UserError::KeyNotFound {
                key_id: key_id.to_string(),
            }
        })?;

        Ok(metadata.database_sigkeys.get(database_id).cloned())
    }

    /// Map a key to a SigKey identifier for a specific database.
    ///
    /// Registers that this user's key should be used with a specific SigKey identifier
    /// when interacting with a database. This is typically used when a user has been
    /// granted access to a database and needs to configure their local key to work with it.
    ///
    /// # Multi-Key Support
    ///
    /// **Note**: A database may have mappings to multiple keys. This is useful for
    /// multi-device scenarios where the same user wants to access a database from
    /// different devices, each with their own key.
    ///
    /// # Arguments
    /// * `key_id` - The user's key identifier (public key string)
    /// * `database_id` - The database ID
    /// * `sigkey` - The SigKey identifier to use for this database
    ///
    /// # Errors
    /// Returns an error if the key_id doesn't exist in the user database
    pub async fn map_key(
        &mut self,
        key_id: &str,
        database_id: &crate::entry::ID,
        sigkey: &str,
    ) -> Result<()> {
        let tx = self.user_database.new_transaction().await?;
        self.map_key_in_txn(&tx, key_id, database_id, sigkey)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Internal helper: Add a SigKey mapping within an existing transaction
    ///
    /// This is used internally by methods that manage their own transactions.
    /// For external use, call `map_key()` instead.
    async fn map_key_in_txn(
        &mut self,
        tx: &crate::Transaction,
        key_id: &str,
        database_id: &crate::entry::ID,
        sigkey: &str,
    ) -> Result<()> {
        use crate::store::Table;
        use crate::user::types::UserKey;

        let keys_table = tx.get_store::<Table<UserKey>>("keys").await?;

        // Find the key metadata in the database
        let (uuid_primary_key, mut metadata) = keys_table
            .search(|uk| uk.key_id == key_id)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| super::errors::UserError::KeyNotFound {
                key_id: key_id.to_string(),
            })?;

        // Add the database sigkey mapping
        metadata
            .database_sigkeys
            .insert(database_id.clone(), sigkey.to_string());

        // Update the key in user database using the UUID primary key
        keys_table.set(&uuid_primary_key, metadata.clone()).await?;

        // Update the in-memory key manager with the updated metadata
        self.key_manager.add_key(metadata)?;

        Ok(())
    }

    /// Internal helper: Validate key and set up SigKey mapping within an existing transaction
    ///
    /// This validates that a key exists and has access to a database, discovers the appropriate
    /// SigKey, and creates the mapping. Used by track_database (which has upsert behavior).
    async fn validate_and_map_key_in_txn(
        &mut self,
        tx: &crate::Transaction,
        database_id: &crate::entry::ID,
        key_id: &str,
    ) -> Result<()> {
        // Verify the key exists
        let signing_key =
            self.key_manager
                .get_signing_key(key_id)
                .ok_or_else(|| UserError::KeyNotFound {
                    key_id: key_id.to_string(),
                })?;

        // Get public key for SigKey discovery
        let verifying_key = signing_key.verifying_key();
        let public_key = auth::format_public_key(&verifying_key);

        // Discover available SigKeys for this public key
        let available_sigkeys =
            Database::find_sigkeys(&self.instance, database_id, &public_key).await?;

        if available_sigkeys.is_empty() {
            return Err(UserError::NoSigKeyFound {
                key_id: key_id.to_string(),
                database_id: database_id.clone(),
            }
            .into());
        }

        // Select the first SigKey (highest permission, since find_sigkeys returns sorted list)
        let (sigkey, _permission) = &available_sigkeys[0];
        let sigkey_str = match sigkey {
            SigKey::Direct(key_name) => key_name.clone(),
            SigKey::DelegationPath(_) => {
                // FIXME: Implement delegation path handling
                return Err(UserError::NoSigKeyFound {
                    key_id: key_id.to_string(),
                    database_id: database_id.clone(),
                }
                .into());
            }
        };

        // Create the key mapping within the provided transaction
        self.map_key_in_txn(tx, key_id, database_id, &sigkey_str)
            .await?;

        Ok(())
    }

    // === Key Management (User Context) ===

    /// Add a new private key to this user's keyring.
    ///
    /// Generates a new Ed25519 keypair, encrypts it (for password-protected users)
    /// or stores it unencrypted (for passwordless users), and adds it to the user's
    /// key database.
    ///
    /// # Arguments
    /// * `display_name` - Optional display name for the key
    ///
    /// # Returns
    /// The key ID (public key string)
    pub async fn add_private_key(&mut self, display_name: Option<&str>) -> Result<String> {
        use crate::auth::crypto::{format_public_key, generate_keypair};
        use crate::store::Table;
        use crate::user::types::{KeyEncryption, UserKey};

        // Generate new keypair
        let (private_key, public_key) = generate_keypair();
        let key_id = format_public_key(&public_key);

        // Get current timestamp using the instance's clock
        let timestamp = self.instance.clock().now_secs();

        // Prepare UserKey based on encryption type
        let user_key = if let Some(encryption_key) = self.key_manager.encryption_key() {
            // Password-protected user: encrypt the key
            use crate::user::crypto::encrypt_private_key;
            let (encrypted_bytes, nonce) = encrypt_private_key(&private_key, encryption_key)?;

            UserKey {
                key_id: key_id.clone(),
                private_key_bytes: encrypted_bytes,
                encryption: KeyEncryption::Encrypted { nonce },
                display_name: display_name.map(|s| s.to_string()),
                created_at: timestamp,
                last_used: None,
                is_default: false, // New keys are not default
                database_sigkeys: std::collections::HashMap::new(),
            }
        } else {
            // Passwordless user: store unencrypted
            UserKey {
                key_id: key_id.clone(),
                private_key_bytes: private_key.to_bytes().to_vec(),
                encryption: KeyEncryption::Unencrypted,
                display_name: display_name.map(|s| s.to_string()),
                created_at: timestamp,
                last_used: None,
                is_default: false, // New keys are not default
                database_sigkeys: std::collections::HashMap::new(),
            }
        };

        // Store in user database
        let tx = self.user_database.new_transaction().await?;
        let keys_table = tx.get_store::<Table<UserKey>>("keys").await?;
        keys_table.insert(user_key.clone()).await?;
        tx.commit().await?;

        // Add to in-memory key manager
        self.key_manager.add_key(user_key)?;

        Ok(key_id)
    }

    /// List all key IDs owned by this user.
    ///
    /// Keys are returned sorted by creation timestamp (oldest first), making the
    /// first key in the list the "default" key created when the user was set up.
    ///
    /// # Returns
    /// Vector of key IDs (public key strings) sorted by creation time
    pub fn list_keys(&self) -> Result<Vec<String>> {
        Ok(self.key_manager.list_key_ids())
    }

    /// Get the default key.
    ///
    /// Returns the key marked as is_default=true, or falls back to the oldest key
    /// by creation timestamp if no default is explicitly set.
    ///
    /// # Returns
    /// The key ID of the default key
    ///
    /// # Errors
    /// Returns an error if no keys exist
    pub fn get_default_key(&self) -> Result<String> {
        self.key_manager.get_default_key_id().ok_or_else(|| {
            crate::Error::from(crate::instance::InstanceError::AuthenticationRequired)
        })
    }

    /// Get a signing key by its ID.
    ///
    /// # Arguments
    /// * `key_id` - The key ID (public key string)
    ///
    /// # Returns
    /// The SigningKey if found
    pub fn get_signing_key(&self, key_id: &str) -> Result<ed25519_dalek::SigningKey> {
        // FIXME: get_signing_key should be private
        self.key_manager
            .get_signing_key(key_id)
            .cloned()
            .ok_or_else(|| {
                crate::user::errors::UserError::KeyNotFound {
                    key_id: key_id.to_string(),
                }
                .into()
            })
    }

    /// Get the formatted public key string for a given key ID.
    ///
    /// Returns the public key in the same format used throughout Eidetica's auth system.
    ///
    /// # Arguments
    /// * `key_id` - The key ID (public key string)
    ///
    /// # Returns
    /// The formatted public key string if the key is found
    pub fn get_public_key(&self, key_id: &str) -> Result<String> {
        let verifying_key = self.key_manager.get_public_key(key_id).ok_or_else(|| {
            crate::Error::from(crate::user::errors::UserError::KeyNotFound {
                key_id: key_id.to_string(),
            })
        })?;
        Ok(crate::auth::crypto::format_public_key(&verifying_key))
    }

    // === Bootstrap Request Management (User Context) ===

    /// Get all pending bootstrap requests from the sync system.
    ///
    /// This is a convenience method that requires the Instance's Sync to be initialized.
    ///
    /// # Arguments
    /// * `sync` - Reference to the Instance's Sync object
    ///
    /// # Returns
    /// A vector of (request_id, bootstrap_request) pairs for pending requests
    pub async fn pending_bootstrap_requests(
        &self,
        sync: &crate::sync::Sync,
    ) -> Result<Vec<(String, crate::sync::BootstrapRequest)>> {
        sync.pending_bootstrap_requests().await
    }

    /// Approve a bootstrap request and add the requesting key to the target database.
    ///
    /// The approving key must have Admin permission on the target database.
    ///
    /// # Arguments
    /// * `sync` - Mutable reference to the Instance's Sync object
    /// * `request_id` - The unique identifier of the request to approve
    /// * `approving_key_id` - The ID of this user's key to use for approval (must have Admin permission)
    ///
    /// # Returns
    /// Result indicating success or failure of the approval operation
    ///
    /// # Errors
    /// - Returns an error if the user doesn't own the specified approving key
    /// - Returns an error if the approving key doesn't have Admin permission on the target database
    /// - Returns an error if the request doesn't exist or isn't pending
    /// - Returns an error if the key addition to the database fails
    pub async fn approve_bootstrap_request(
        &self,
        sync: &crate::sync::Sync,
        request_id: &str,
        approving_key_id: &str,
    ) -> Result<()> {
        // Get the signing key from the key manager
        let signing_key = self
            .key_manager
            .get_signing_key(approving_key_id)
            .ok_or_else(|| super::errors::UserError::KeyNotFound {
                key_id: approving_key_id.to_string(),
            })?;

        // Delegate to Sync layer with the user-provided key
        // The Sync layer will validate permissions when committing the transaction
        sync.approve_bootstrap_request_with_key(request_id, signing_key, approving_key_id)
            .await?;

        Ok(())
    }

    /// Reject a bootstrap request.
    ///
    /// This method marks the request as rejected. The requesting device will not
    /// be granted access to the target database. Requires Admin permission on the
    /// target database to prevent unauthorized users from disrupting the bootstrap protocol.
    ///
    /// # Arguments
    /// * `sync` - Mutable reference to the Instance's Sync object
    /// * `request_id` - The unique identifier of the request to reject
    /// * `rejecting_key_id` - The ID of this user's key (for permission validation and audit trail)
    ///
    /// # Returns
    /// Result indicating success or failure of the rejection operation
    ///
    /// # Errors
    /// - Returns an error if the user doesn't own the specified rejecting key
    /// - Returns an error if the request doesn't exist or isn't pending
    /// - Returns an error if the rejecting key lacks Admin permission on the target database
    pub async fn reject_bootstrap_request(
        &self,
        sync: &crate::sync::Sync,
        request_id: &str,
        rejecting_key_id: &str,
    ) -> Result<()> {
        // Get the signing key from the key manager
        let signing_key = self
            .key_manager
            .get_signing_key(rejecting_key_id)
            .ok_or_else(|| super::errors::UserError::KeyNotFound {
                key_id: rejecting_key_id.to_string(),
            })?;

        // Delegate to Sync layer with the user-provided key
        // The Sync layer will validate Admin permission on the target database
        sync.reject_bootstrap_request_with_key(request_id, signing_key, rejecting_key_id)
            .await?;

        Ok(())
    }

    /// Request access to a database from a peer (bootstrap sync).
    ///
    /// This convenience method initiates a bootstrap sync request to access a database
    /// that this user doesn't have locally yet. The user's key will be sent to the peer
    /// to request the specified permission level.
    ///
    /// This is useful for multi-device scenarios where a user wants to access their
    /// existing database from a new device, or when requesting access to a database
    /// shared by another user.
    ///
    /// # Arguments
    /// * `sync` - Mutable reference to the Instance's Sync object
    /// * `peer_address` - The address of the peer to sync with (format: "host:port")
    /// * `database_id` - The ID of the database to request access to
    /// * `key_id` - The ID of this user's key to use for the request
    /// * `requested_permission` - The permission level being requested
    ///
    /// # Returns
    /// Result indicating success or failure of the bootstrap request
    ///
    /// # Errors
    /// - Returns an error if the user doesn't own the specified key
    /// - Returns an error if the peer is unreachable
    /// - Returns an error if the bootstrap sync fails
    ///
    /// # Example
    /// ```rust,ignore
    /// // Request write access to a shared database
    /// let user_key_id = user.get_default_key()?;
    /// user.request_database_access(
    ///     &mut sync,
    ///     "192.168.1.100:8080",
    ///     &shared_database_id,
    ///     &user_key_id,
    ///     Permission::Write(5),
    /// ).await?;
    ///
    /// // After approval, the database can be opened
    /// let database = user.open_database(&shared_database_id)?;
    /// ```
    pub async fn request_database_access(
        &self,
        sync: &crate::sync::Sync,
        peer_address: &str,
        database_id: &crate::entry::ID,
        key_id: &str,
        requested_permission: crate::auth::Permission,
    ) -> Result<()> {
        // Get the signing key from the key manager
        let signing_key = self.key_manager.get_signing_key(key_id).ok_or_else(|| {
            super::errors::UserError::KeyNotFound {
                key_id: key_id.to_string(),
            }
        })?;

        // Derive the public key from the signing key
        let verifying_key = signing_key.verifying_key();
        let public_key = crate::auth::crypto::format_public_key(&verifying_key);

        // Delegate to Sync layer with the public key
        sync.sync_with_peer_for_bootstrap_with_key(
            peer_address,
            database_id,
            &public_key,
            key_id,
            requested_permission,
        )
        .await?;

        Ok(())
    }

    // === Tracked Databases ===

    /// Track a database, adding it to this user's list with auto-discovery of SigKeys.
    ///
    /// This method adds an existing database to your tracked list, or updates it if
    /// already tracked (upsert behavior).
    ///
    /// When tracking:
    /// 1. Uses Database::find_sigkeys() to discover which SigKey the user can use
    /// 2. Automatically selects the SigKey with highest permission
    /// 3. Stores the key mapping and sync settings
    ///
    /// The sync_settings indicate your sync preferences, but do not automatically
    /// configure sync. Use the Sync module's peer and tree methods to set up actual
    /// sync relationships.
    ///
    /// # Arguments
    /// * `tracked` - The database to track, including database_id, key_id, and sync_settings
    ///
    /// # Returns
    /// Result indicating success or failure
    ///
    /// # Errors
    /// - Returns `NoSigKeyFound` if no SigKey can be found for the specified key
    /// - Returns `KeyNotFound` if the specified key_id doesn't exist
    pub async fn track_database(&mut self, tracked: TrackedDatabase) -> Result<()> {
        // Single transaction for all operations
        let tx = self.user_database.new_transaction().await?;
        let databases_table = tx.get_store::<Table<TrackedDatabase>>("databases").await?;

        // Use database ID as the key - check if it already exists (O(1))
        let db_id_key = tracked.database_id.to_string();
        let existing = databases_table.get(&db_id_key).await.ok();

        // Determine if we need to validate and setup key mapping
        let needs_key_validation = match &existing {
            Some(existing) => existing.key_id != tracked.key_id, // Key changed
            None => true,                                        // New database
        };

        // Validate key and set up mapping if needed
        if needs_key_validation {
            self.validate_and_map_key_in_txn(&tx, &tracked.database_id, &tracked.key_id)
                .await?;
        }

        // Store using database ID as explicit key (not using insert's auto-generated UUID)
        databases_table.set(&db_id_key, tracked).await?;

        // Single commit for all changes
        tx.commit().await?;

        // Update sync system to immediately recompute combined settings
        // This ensures automatic sync works right away, without waiting for background worker
        if let Some(sync) = self.instance.sync() {
            // Auto-sync user tracking if not already synced
            // This is idempotent - safe to call multiple times
            sync.sync_user(&self.user_uuid, self.user_database.root_id())
                .await?;
        }

        Ok(())
    }

    /// List all tracked databases.
    ///
    /// Returns all databases this user has added to their tracked list.
    ///
    /// # Returns
    /// Vector of TrackedDatabase entries
    pub async fn databases(&self) -> Result<Vec<TrackedDatabase>> {
        let databases_table = self
            .user_database
            .get_store_viewer::<Table<TrackedDatabase>>("databases")
            .await?;

        // Get all entries from the table (returns Vec<(key, value)>)
        let all_entries = databases_table.search(|_| true).await?;

        // Extract just the values
        let tracked: Vec<TrackedDatabase> = all_entries.into_iter().map(|(_key, db)| db).collect();

        Ok(tracked)
    }

    /// Get a specific tracked database by ID.
    ///
    /// # Arguments
    /// * `database_id` - The ID of the database
    ///
    /// # Returns
    /// The TrackedDatabase if it's in the user's tracked list
    ///
    /// # Errors
    /// Returns `DatabaseNotTracked` if the database is not in the user's list
    pub async fn database(&self, database_id: &crate::entry::ID) -> Result<TrackedDatabase> {
        let databases_table = self
            .user_database()
            .get_store_viewer::<Table<TrackedDatabase>>("databases")
            .await?;

        // Direct O(1) lookup using database ID as key
        let db_id_key = database_id.to_string();
        databases_table.get(&db_id_key).await.map_err(|_| {
            UserError::DatabaseNotTracked {
                database_id: database_id.to_string(),
            }
            .into()
        })
    }

    /// Stop tracking a database.
    ///
    /// This removes the database from the user's tracked list.
    /// It does not delete the database itself, remove key mappings, or delete any data.
    ///
    /// # Arguments
    /// * `database_id` - The ID of the database to stop tracking
    ///
    /// # Errors
    /// Returns `DatabaseNotTracked` if the database is not in the user's list
    pub async fn untrack_database(&mut self, database_id: &crate::entry::ID) -> Result<()> {
        let tx = self.user_database.new_transaction().await?;
        let databases_table = tx.get_store::<Table<TrackedDatabase>>("databases").await?;

        // Direct O(1) delete using database ID as key
        let db_id_key = database_id.to_string();

        // Verify it exists before deleting
        if databases_table.get(&db_id_key).await.is_err() {
            return Err(UserError::DatabaseNotTracked {
                database_id: database_id.to_string(),
            }
            .into());
        }

        // Delete using database ID as key
        databases_table.delete(&db_id_key).await?;
        tx.commit().await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Clock, SystemClock,
        auth::crypto::{format_public_key, generate_keypair},
        backend::{BackendImpl, database::InMemory},
        user::{
            crypto::{derive_encryption_key, encrypt_private_key, hash_password},
            types::{UserKey, UserStatus},
        },
    };
    use std::{collections::HashMap, sync::Arc};

    async fn create_test_user_session() -> User {
        let backend = Arc::new(InMemory::new());

        // Create user database
        let (device_key, device_pubkey) = generate_keypair();
        let device_pubkey_str = format_public_key(&device_pubkey);

        backend
            .store_private_key("admin", device_key.clone())
            .await
            .unwrap();

        let mut db_settings = crate::crdt::Doc::new();
        db_settings.set("name", "test_user_db");

        let mut auth_settings = crate::auth::settings::AuthSettings::new();
        auth_settings
            .add_key(
                "admin",
                crate::auth::types::AuthKey::active(
                    &device_pubkey_str,
                    crate::auth::types::Permission::Admin(0),
                )
                .unwrap(),
            )
            .unwrap();
        db_settings.set("auth", auth_settings.as_doc().clone());

        // Create Instance for test
        let instance = Instance::create_internal(backend.handle(), Arc::new(SystemClock))
            .await
            .unwrap();

        let user_database = Database::create(
            db_settings,
            &instance,
            device_key.clone(),
            "admin".to_string(),
        )
        .await
        .unwrap();

        // Create user info
        let password = "test_password";
        let (password_hash, password_salt) = hash_password(password).unwrap();

        let user_info = UserInfo {
            username: "test_user".to_string(),
            user_database_id: user_database.root_id().clone(),
            password_hash: Some(password_hash),
            password_salt: Some(password_salt.clone()),
            created_at: SystemClock.now_secs(),
            status: UserStatus::Active,
        };

        // Create encrypted key for key manager
        let encryption_key = derive_encryption_key(password, &password_salt).unwrap();
        let (encrypted_key, nonce) = encrypt_private_key(&device_key, &encryption_key).unwrap();

        let user_key = UserKey {
            key_id: "admin".to_string(),
            private_key_bytes: encrypted_key,
            encryption: crate::user::types::KeyEncryption::Encrypted { nonce },
            display_name: Some("Device Key".to_string()),
            created_at: SystemClock.now_secs(),
            last_used: None,
            is_default: true,
            database_sigkeys: HashMap::new(),
        };

        // Create key manager
        let key_manager = UserKeyManager::new(password, &password_salt, vec![user_key]).unwrap();

        // Create user with UUID (using a test UUID)
        User::new(
            "test-uuid-1234".to_string(),
            user_info,
            user_database,
            instance,
            key_manager,
        )
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
    async fn test_user_creation() {
        let user = create_test_user_session().await;
        assert_eq!(user.username(), "test_user");
        assert_eq!(user.user_uuid(), "test-uuid-1234");
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
    async fn test_user_getters() {
        let user = create_test_user_session().await;

        assert_eq!(user.username(), "test_user");
        assert_eq!(user.user_uuid(), "test-uuid-1234");
        assert_eq!(user.user_info().username, "test_user");
        assert!(!user.user_database().root_id().to_string().is_empty());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
    async fn test_user_logout() {
        let user = create_test_user_session().await;
        let username = user.username().to_string();

        // Logout consumes the user
        user.logout().unwrap();

        // User is dropped, keys should be cleared
        assert_eq!(username, "test_user");
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses Argon2 password hashing and SystemTime
    async fn test_user_drop() {
        {
            let _user = create_test_user_session().await;
            // User will be dropped when it goes out of scope
        }
        // Keys should be cleared automatically
    }
}
