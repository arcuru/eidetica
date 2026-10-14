//! AccountSession management
//!
//! Represents an authenticated account session with decrypted keys.
//!
//! # API Overview
//!
//! The AccountSession API is organized into three areas for managing Databases:
//!
//! ## Database Lifecycle
//!
//! - **`create_database()`** - Create a new database
//! - **`open_database()`** - Open an existing database
//! - **`open_database_with_key()`** - Open with an explicitly chosen account key
//! - **`join()`** - Join a database from a ticket and open it
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
//! - **`is_sync_enabled()`** - Check this account's sync preference for a database
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

use std::collections::HashMap;

use std::sync::Arc;

use super::{
    AccountKeyManager,
    admin::InstanceAdmin,
    types::{AccountInfo, AccountKey},
};
use crate::{
    Database, Error, Instance, Result, Snapshot, Transaction,
    account::{AccountError, SyncSettings, TrackedDatabase},
    auth::{Permission, SigKey, crypto::PublicKey},
    crdt::Doc,
    database::DatabaseKey,
    entry::ID,
    instance::{InstanceError, backend::Backend},
    store::{StoreError, Table},
    sync::{BootstrapRequest, DatabaseTicket, Sync, SyncError},
};

mod builder;
#[cfg(test)]
mod tests;

pub use builder::DatabaseBuilder;

/// AccountSession object, returned after successful login
///
/// Represents an authenticated account with decrypted private keys loaded in memory.
/// The AccountSession struct provides access to key management, tracked databases, and
/// bootstrap approval operations.
pub struct AccountSession {
    /// Stable internal account UUID (Table primary key)
    login_id: String,

    /// Username (login identifier)
    username: String,

    /// Account's private database (contains encrypted keys and tracked databases)
    account_database: Database,

    /// Instance reference for database operations
    instance: Instance,

    /// Decrypted account keys (in memory only during session)
    key_manager: AccountKeyManager,

    /// Account info (cached from _users database)
    account_info: AccountInfo,
}

impl std::fmt::Debug for AccountSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountSession")
            .field("login_id", &self.login_id)
            .field("username", &self.username)
            .field("account_database", &self.account_database)
            .field("instance", &self.instance)
            .field("key_manager", &"<KeyManager [sensitive]>")
            .field("account_info", &self.account_info)
            .finish()
    }
}

impl AccountSession {
    /// Create a new AccountSession
    ///
    /// This is an internal constructor used after successful login.
    /// Use `Instance::login_account()` to create an AccountSession.
    ///
    /// # Arguments
    /// * `login_id` - Internal UUID (Table primary key)
    /// * `account_info` - Account information from _users database
    /// * `account_database` - The account's private database
    /// * `instance` - Instance reference
    /// * `key_manager` - Initialized key manager with decrypted keys
    #[allow(dead_code)]
    pub(crate) fn new(
        login_id: String,
        account_info: AccountInfo,
        account_database: Database,
        instance: Instance,
        key_manager: AccountKeyManager,
    ) -> Self {
        Self {
            login_id,
            username: account_info.username.clone(),
            account_database,
            instance,
            key_manager,
            account_info,
        }
    }

    // === Basic Session Methods ===

    /// Instance-local login record UUID, not the Account's portable identity.
    /// Use [`Self::account_id`] for references to the Account database.
    pub fn login_id(&self) -> &str {
        &self.login_id
    }

    /// Get the username (login identifier)
    pub fn username(&self) -> &str {
        &self.username
    }

    /// The Account's ordinary private root database, bound to this session's key.
    ///
    /// Applications may store direct database IDs in their own stores here. A
    /// reference grants no authority and does not activate or download its target.
    /// The current root also retains legacy key and sync bookkeeping; this API
    /// does not yet separate private-key custody from portable application data.
    pub fn account(&self) -> &Database {
        &self.account_database
    }

    /// Stable Account identity: the root ID, independent of login and display names.
    pub fn account_id(&self) -> &ID {
        self.account_database.root_id()
    }

    /// Get a reference to the backend seam.
    pub fn backend(&self) -> &Arc<dyn Backend> {
        self.instance.backend()
    }

    /// Get a reference to the account info
    pub fn account_info(&self) -> &AccountInfo {
        &self.account_info
    }

    /// Whether this account is an instance admin.
    ///
    /// "Instance admin" means the account's default pubkey holds `Admin` in the
    /// `_users` system database's `auth_settings`. The `admin`/`admin` account
    /// created during instance bootstrap is auto-promoted; subsequent accounts
    /// land as non-admins until promoted via
    /// [`InstanceAdmin::grant_instance_admin`](crate::account::InstanceAdmin::grant_instance_admin).
    ///
    /// Returns `false` if the key resolution fails (key not in auth_settings,
    /// non-Admin permission, etc.). For the capability handle rather than a
    /// bool, use [`Self::admin`].
    pub async fn is_admin(&self) -> Result<bool> {
        self.admin_check().await
    }

    /// Obtain the instance-admin capability view.
    ///
    /// Returns an [`InstanceAdmin`] only if this account's default key holds
    /// `Admin` on the `_users` system database. All admin-gated operations
    /// (creating accounts, listing accounts, promoting admins) live on the
    /// returned view, so the privilege boundary is explicit at the call site
    /// and those operations need no further permission check of their own.
    ///
    /// # Errors
    /// - [`AccountError::InsufficientPermissions`] if this account is not an
    ///   instance admin.
    pub async fn admin(&self) -> Result<InstanceAdmin<'_>> {
        if self.admin_check().await? {
            Ok(InstanceAdmin::new(self))
        } else {
            Err(AccountError::InsufficientPermissions.into())
        }
    }

    /// Single source of truth for "is this account an instance admin".
    ///
    /// Reads the `_users` `auth_settings` snapshot via the account's **session
    /// key** (not the device key), so it behaves identically on local and
    /// remote instances. Returns `Ok(false)` for a non-admin or unresolved
    /// key; propagates real infrastructure errors. Shared by
    /// [`Self::is_admin`] and [`Self::admin`].
    async fn admin_check(&self) -> Result<bool> {
        let default_pubkey =
            self.key_manager
                .get_default_key_id()
                .ok_or_else(|| AccountError::KeyNotFound {
                    key_id: "<default>".to_string(),
                })?;
        let signing_key = self.default_signing_key()?;
        let users_db = self.instance.users_db_for_session(&signing_key).await?;
        let tx = users_db.new_transaction().await?;
        let settings = tx.get_settings()?;
        let auth = settings.auth_snapshot().await?;
        match auth.get_key_by_pubkey(&default_pubkey) {
            Ok(key) => Ok(matches!(key.permissions(), Permission::Admin(_))),
            Err(_) => Ok(false),
        }
    }

    /// The account's default signing key (decrypted, in-session).
    ///
    /// Shared by admin / system-DB paths that must sign as the account
    /// directly, rather than via a tracked-database SigKey mapping.
    pub(crate) fn default_signing_key(&self) -> Result<crate::auth::crypto::PrivateKey> {
        let key_id =
            self.key_manager
                .get_default_key_id()
                .ok_or_else(|| AccountError::KeyNotFound {
                    key_id: "<default>".to_string(),
                })?;
        self.key_manager
            .get_signing_key(&key_id)
            .cloned()
            .ok_or_else(|| {
                AccountError::KeyNotFound {
                    key_id: key_id.to_string(),
                }
                .into()
            })
    }

    /// Instance reference (internal — for admin / system-DB helpers).
    pub(crate) fn instance(&self) -> &Instance {
        &self.instance
    }

    /// Logout (consumes self and clears decrypted keys from memory)
    ///
    /// After logout, all decrypted keys are zeroized and the session is ended.
    /// Keys are automatically cleared when the AccountSession is dropped.
    pub fn logout(self) -> Result<()> {
        // Consume self, all keys are stored in other Types that zeroize themselves on drop
        Ok(())
    }

    // === Key Manager Access (Internal) ===

    /// Get a reference to the key manager (for internal use)
    #[allow(dead_code)]
    pub(crate) fn key_manager(&self) -> &AccountKeyManager {
        &self.key_manager
    }

    /// Get a mutable reference to the key manager (for internal use)
    #[allow(dead_code)]
    pub(crate) fn key_manager_mut(&mut self) -> &mut AccountKeyManager {
        &mut self.key_manager
    }

    // === Database Operations (AccountSession Context) ===

    /// Start building a new database via the chainable [`DatabaseBuilder`] API.
    ///
    /// The builder collects settings, key policy, and store initializers, then
    /// produces a fully-initialized database in a single genesis entry when
    /// [`DatabaseBuilder::build`] is called.
    ///
    /// For the lower-level direct constructor see [`Self::create_database`].
    pub fn new_database(&mut self) -> DatabaseBuilder<'_> {
        DatabaseBuilder::new(self)
    }

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
    /// let keys = account.list_keys()?;
    /// let key_id = &keys[1]; // Use the second key
    ///
    /// // Create database with explicit key selection
    /// let mut settings = Doc::new();
    /// settings.set("name", "My Database");
    /// let database = account.new_database(settings, key_id)?;
    /// ```
    pub async fn create_database(&mut self, settings: Doc, key_id: &PublicKey) -> Result<Database> {
        self.create_database_with_init(settings, key_id, async |_| Ok(()))
            .await
    }

    /// Creates a new database with an initialization callback that runs inside
    /// the genesis transaction.
    ///
    /// This is the underlying constructor used by [`Self::create_database`] and by
    /// [`Self::new_database`] (the builder API). The callback receives the
    /// genesis transaction after `_settings` and `_root` have been staged but
    /// before commit, allowing additional subtrees to be written into the same
    /// entry that establishes the database root. See
    /// [`Database::create_with_init`] for details on the atomicity guarantee.
    ///
    /// After the genesis entry commits, this method performs the standard
    /// account-side tracking write (key-database mapping, `TrackedDatabase` entry)
    /// in a separate transaction on the account's own system database.
    pub async fn create_database_with_init<F>(
        &mut self,
        settings: Doc,
        key_id: &PublicKey,
        init: F,
    ) -> Result<Database>
    where
        F: AsyncFnOnce(&Transaction) -> Result<()>,
    {
        use crate::account::types::{AccountKey, SyncSettings};

        // Get the signing key from AccountKeyManager
        let signing_key = self
            .key_manager
            .get_signing_key(key_id)
            .ok_or_else(|| AccountError::KeyNotFound {
                key_id: key_id.to_string(),
            })?
            .clone();

        // Create the database with the provided key directly
        let database =
            Database::create_with_init(&self.instance, signing_key, settings, init).await?;

        // Store the mapping in AccountKey and track the database
        let tx = self.account_database.new_transaction().await?;
        let keys_table = tx.get_store::<Table<AccountKey>>("keys").await?;

        // Find the key metadata in the database
        let (uuid_primary_key, mut metadata) = keys_table
            .search(|uk| &uk.key_id == key_id)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| AccountError::KeyNotFound {
                key_id: key_id.to_string(),
            })?;

        // Add the database sigkey mapping (None = default pubkey identity)
        metadata
            .database_sigkeys
            .insert(database.root_id().clone(), None);

        // Update the key in account database using the UUID primary key
        keys_table.set(&uuid_primary_key, metadata.clone()).await?;

        // Also track the database in the databases table
        let databases_table = tx.get_store::<Table<TrackedDatabase>>("databases").await?;
        let tracked = TrackedDatabase {
            database_id: database.root_id().clone(),
            key_id: key_id.clone(),
            sync_settings: SyncSettings::disabled(),
        };
        databases_table
            .set(&database.root_id().to_string(), tracked)
            .await?;

        tx.commit().await?;

        // Update the in-memory key manager with the updated metadata
        self.key_manager.add_key(metadata)?;

        Ok(database.with_account_database(self.account_database.clone()))
    }

    /// Open an existing database by its root ID using this account's keys.
    ///
    /// This method automatically:
    /// 1. Finds an appropriate key that has access to the database
    /// 2. Retrieves the decrypted SigningKey from the AccountKeyManager
    /// 3. Gets the SigKey mapping for this database
    /// 4. Creates a Database instance configured with the account's key
    ///
    /// The returned Database will use the account's provided key for all operations,
    /// without requiring backend key lookups.
    ///
    /// # Arguments
    /// * `root_id` - The root entry ID of the database
    ///
    /// # Returns
    /// The opened Database configured to use this account's keys
    ///
    /// # Errors
    /// - Returns an error if no key is found for the database
    /// - Returns an error if no SigKey mapping exists
    /// - Returns an error if the key is not in the AccountKeyManager
    pub async fn open_database(&self, root_id: &ID) -> Result<Database> {
        // Find an appropriate key for this database
        let key_id = self.find_key(root_id)?.ok_or_else(|| {
            super::errors::AccountError::NoKeyForDatabase {
                database_id: root_id.clone(),
            }
        })?;

        self.open_database_with_key(root_id, &key_id).await
    }

    /// Open an existing database with an explicitly chosen key.
    ///
    /// Equivalent to `open_database`, but selects the account's signing key by
    /// public key instead of relying on `find_key`'s iteration order. Use this
    /// when the account holds multiple authorized keys for a database and you
    /// need writes signed by a specific one (e.g. agent-as-signer scenarios
    /// where cryptographic provenance matters, not just authorization).
    ///
    /// The `key_id` is purely a selector into this account's `AccountKeyManager`;
    /// no crypto material is passed in.
    ///
    /// # Arguments
    /// * `root_id` - The root entry ID of the database
    /// * `key_id` - Public key of the account-held key to sign with
    ///
    /// # Returns
    /// The opened Database configured to use the specified key
    ///
    /// # Errors
    /// - Returns an error if the root entry does not exist
    /// - Returns an error if the account does not hold a key with that pubkey
    /// - Returns an error if the key has no SigKey mapping for this database
    pub async fn open_database_with_key(
        &self,
        root_id: &ID,
        key_id: &PublicKey,
    ) -> Result<Database> {
        // Get the SigningKey from AccountKeyManager
        let signing_key = self.key_manager.get_signing_key(key_id).ok_or_else(|| {
            super::errors::AccountError::KeyNotFound {
                key_id: key_id.to_string(),
            }
        })?;

        // Get the SigKey mapping for this database
        let sigkey = self.key_mapping(key_id, root_id)?.ok_or_else(|| {
            super::errors::AccountError::NoSigKeyMapping {
                key_id: key_id.to_string(),
                database_id: root_id.clone(),
            }
        })?;

        // Create Database with account-provided key using resolved SigKey identity.
        //
        // On a connected (remote) instance, the read path must travel as the
        // account's per-DB identity so the daemon's per-tree gate sees a key the
        // tree actually authorises. `Database::open` would clone the
        // instance's session backend, which on a remote instance carries the
        // connection's login pubkey — and the account's login key is not a member
        // of every tree they hold a per-DB key for. Route through
        // `Database::open_remote` with the per-DB
        // identity instead, after proving possession of the per-DB key to
        // the daemon so the identity sits in the connection's session
        // keyset.
        let key = DatabaseKey::with_identity(signing_key.clone(), sigkey.clone());
        // A SigKey mapping exists (resolved above), so the database is one this
        // account has requested access to. If its root entry isn't present, the
        // access is still pending — the bootstrap request hasn't been approved or
        // the database hasn't synced yet. Surface that explicitly instead of a
        // bare backend "not found", on both the remote and local open paths.
        #[cfg(all(unix, feature = "service"))]
        if let Some(conn) = self.instance.remote_connection() {
            conn.register_session_key(signing_key).await?;
            return match Database::open_remote(&self.instance, conn, root_id, sigkey).await {
                Ok(database) => Ok(database
                    .with_key(key)
                    .with_account_database(self.account_database.clone())),
                Err(e) if e.is_not_found() => {
                    Err(super::errors::AccountError::DatabaseAccessPending {
                        database_id: root_id.clone(),
                    }
                    .into())
                }
                Err(e) => Err(e),
            };
        }
        match Database::open(&self.instance, root_id).await {
            Ok(database) => Ok(database
                .with_key(key)
                .with_account_database(self.account_database.clone())),
            Err(e) if e.is_not_found() => Err(super::errors::AccountError::DatabaseAccessPending {
                database_id: root_id.clone(),
            }
            .into()),
            Err(e) => Err(e),
        }
    }

    /// Find databases by name among the account's tracked databases.
    ///
    /// Searches only the databases this account has tracked for those matching the given name.
    ///
    /// # Arguments
    /// * `name` - Database name to search for
    ///
    /// # Returns
    /// Vector of matching databases from the account's tracked list
    pub async fn find_database(&self, name: impl AsRef<str>) -> Result<Vec<Database>> {
        let name = name.as_ref();
        let tracked = self.databases().await?;
        let mut matching = Vec::new();

        for tracked_db in tracked {
            if let Ok(database) = self.open_database(&tracked_db.database_id).await
                && let Ok(db_name) = database.get_name().await
                && db_name == name
            {
                matching.push(database);
            }
        }

        if matching.is_empty() {
            Err(AccountError::DatabaseNotFoundByName {
                name: name.to_string(),
            }
            .into())
        } else {
            Ok(matching)
        }
    }

    /// Find which key can access a database.
    ///
    /// Searches this account's keys to find one that can access the specified database.
    /// Considers the SigKey mappings stored in account key metadata.
    ///
    /// Returns the key_id of a suitable key, preferring keys with mappings for this database.
    ///
    /// # Arguments
    /// * `database_id` - The ID of the database
    ///
    /// # Returns
    /// Some(key_id) if a suitable key is found, None if no keys can access this database
    pub fn find_key(&self, database_id: &ID) -> Result<Option<PublicKey>> {
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

    /// Get the resolved SigKey mapping for a key in a specific database.
    ///
    /// Users map their private keys to SigKey identifiers on a per-database basis.
    /// This retrieves the resolved SigKey that a specific key uses in
    /// a specific database's authentication settings.
    ///
    /// Internally, `None` in the stored mapping means "default pubkey identity",
    /// which this method resolves to the concrete `SigKey::from_pubkey(...)` value.
    ///
    /// # Arguments
    /// * `key_id` - The account's key identifier
    /// * `database_id` - The database ID
    ///
    /// # Returns
    /// `Ok(Some(sigkey))` if a mapping exists (resolved to concrete SigKey),
    /// `Ok(None)` if no mapping is configured for this database
    ///
    /// # Errors
    /// Returns an error if the key_id doesn't exist in the AccountKeyManager
    pub fn key_mapping(&self, key_id: &PublicKey, database_id: &ID) -> Result<Option<SigKey>> {
        let metadata = self.key_manager.get_key_metadata(key_id).ok_or_else(|| {
            super::errors::AccountError::KeyNotFound {
                key_id: key_id.to_string(),
            }
        })?;

        match metadata.database_sigkeys.get(database_id) {
            None => Ok(None), // no mapping exists
            Some(None) => {
                // Default: pubkey identity derived directly from key_id
                Ok(Some(SigKey::from_pubkey(key_id)))
            }
            Some(Some(sigkey)) => Ok(Some(sigkey.clone())),
        }
    }

    /// Map a key to a SigKey identity for a specific database.
    ///
    /// Registers that this account's key should be used with a specific SigKey identity
    /// when interacting with a database. This is typically used when a account has been
    /// granted access to a database and needs to configure their local key to work with it.
    ///
    /// If the provided SigKey matches the default pubkey identity for this key,
    /// it is normalized to `None` internally (compact storage for the common case).
    ///
    /// # Multi-Key Support
    ///
    /// **Note**: A database may have mappings to multiple keys. This is useful for
    /// multi-device scenarios where the same account wants to access a database from
    /// different devices, each with their own key.
    ///
    /// # Arguments
    /// * `key_id` - The account's key identifier (public key)
    /// * `database_id` - The database ID
    /// * `sigkey` - The SigKey identity to use for this database
    ///
    /// # Errors
    /// Returns an error if the key_id doesn't exist in the account database
    pub async fn map_key(
        &mut self,
        key_id: &PublicKey,
        database_id: &ID,
        sigkey: SigKey,
    ) -> Result<()> {
        let (tx, guard) = self.key_mapping_transaction().await?;
        let (metadata, changed) = self
            .map_key_in_txn(&tx, key_id, database_id, sigkey)
            .await?;
        if changed {
            let result = tx.commit_under_tree_lock(guard).await;
            self.refresh_key_metadata(key_id).await?;
            result?;
        } else {
            drop(guard);
            self.key_manager.add_key(metadata)?;
        }
        Ok(())
    }

    /// Internal helper: Add a SigKey mapping within an existing transaction
    ///
    /// This is used internally by methods that manage their own transactions.
    /// For external use, call `map_key()` instead.
    ///
    /// Normalizes the stored value: if the sigkey matches the default pubkey
    /// identity for this key, stores `None` instead of `Some(sigkey)`.
    async fn map_key_in_txn(
        &self,
        tx: &Transaction,
        key_id: &PublicKey,
        database_id: &ID,
        sigkey: SigKey,
    ) -> Result<(AccountKey, bool)> {
        let keys_table = tx.get_store::<Table<AccountKey>>("keys").await?;

        // Find the key metadata in the database
        let (uuid_primary_key, mut metadata) = keys_table
            .search(|uk| &uk.key_id == key_id)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| super::errors::AccountError::KeyNotFound {
                key_id: key_id.to_string(),
            })?;

        // Normalize: if the sigkey matches the default pubkey identity, store None
        let default_sigkey = SigKey::from_pubkey(key_id);
        let stored = if sigkey == default_sigkey {
            None
        } else {
            Some(sigkey)
        };

        if metadata.database_sigkeys.get(database_id) == Some(&stored) {
            return Ok((metadata, false));
        }
        metadata
            .database_sigkeys
            .insert(database_id.clone(), stored);

        // Update the key in account database using the UUID primary key
        keys_table.set(&uuid_primary_key, metadata.clone()).await?;

        // Staging is not persistence; the caller publishes cache only after
        // commit acknowledgement or a read of the verified persisted state.
        Ok((metadata, true))
    }

    async fn refresh_key_metadata(&mut self, key_id: &PublicKey) -> Result<()> {
        let keys = self
            .account_database
            .get_store_viewer::<Table<AccountKey>>("keys")
            .await?;
        let (_, metadata) = keys
            .search(|key| &key.key_id == key_id)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| AccountError::KeyNotFound {
                key_id: key_id.to_string(),
            })?;
        self.key_manager.add_key(metadata)
    }

    async fn key_mapping_transaction(
        &self,
    ) -> Result<(Transaction, tokio::sync::OwnedMutexGuard<()>)> {
        // Resolve reusable committed-but-unacknowledged entries before locking.
        self.account_database.snapshot().await?;
        let guard = self
            .instance
            .tree_lock(self.account_database.root_id())
            .lock_owned()
            .await;
        let snapshot = if self.instance.backend().local_engine().is_some() {
            Snapshot::from(self.account_database.verified_frontier().await?)
        } else {
            self.account_database.snapshot().await?
        };
        Ok((
            self.account_database.new_transaction_at(&snapshot).await?,
            guard,
        ))
    }

    // === Key Management (AccountSession Context) ===

    /// Add a new private key to this account's keyring.
    ///
    /// Generates a new Ed25519 keypair, encrypts it (for password-protected accounts)
    /// or stores it unencrypted (for passwordless accounts), and adds it to the account's
    /// key database.
    ///
    /// # Arguments
    /// * `display_name` - Optional display name for the key
    ///
    /// # Returns
    /// The key ID (public key string)
    pub async fn add_private_key(&mut self, display_name: Option<&str>) -> Result<PublicKey> {
        use crate::account::types::{AccountKey, KeyStorage};
        use crate::auth::crypto::generate_keypair;

        // Generate new keypair
        let (private_key, public_key) = generate_keypair();

        // Get current timestamp using the instance's clock
        let timestamp = self.instance.clock().now_secs();

        // Prepare AccountKey based on encryption type
        let user_key = if let Some(encryption_key) = self.key_manager.encryption_key() {
            // Password-protected account: encrypt the key
            use crate::account::crypto::encrypt_private_key;
            let (ciphertext, nonce) = encrypt_private_key(&private_key, encryption_key)?;

            AccountKey {
                key_id: public_key.clone(),
                storage: KeyStorage::Encrypted {
                    algorithm: "aes-256-gcm".to_string(),
                    ciphertext,
                    nonce,
                },
                display_name: display_name.map(|s| s.to_string()),
                created_at: timestamp,
                last_used: None,
                is_default: false, // New keys are not default
                database_sigkeys: HashMap::new(),
            }
        } else {
            // Passwordless account: store unencrypted
            AccountKey {
                key_id: public_key.clone(),
                storage: KeyStorage::Unencrypted { key: private_key },
                display_name: display_name.map(|s| s.to_string()),
                created_at: timestamp,
                last_used: None,
                is_default: false, // New keys are not default
                database_sigkeys: HashMap::new(),
            }
        };

        // Store in account database
        let tx = self.account_database.new_transaction().await?;
        let keys_table = tx.get_store::<Table<AccountKey>>("keys").await?;
        keys_table.insert(user_key.clone()).await?;
        tx.commit().await?;

        // Add to in-memory key manager
        self.key_manager.add_key(user_key)?;

        Ok(public_key)
    }

    /// List all key IDs owned by this account.
    ///
    /// Keys are returned sorted by creation timestamp (oldest first), making the
    /// first key in the list the "default" key created when the account was set up.
    ///
    /// # Returns
    /// Vector of PublicKeys sorted by creation time
    pub fn list_keys(&self) -> Result<Vec<PublicKey>> {
        Ok(self.key_manager.list_key_ids())
    }

    /// Get the default key.
    ///
    /// Returns the key marked as is_default=true, or falls back to the oldest key
    /// by creation timestamp if no default is explicitly set.
    ///
    /// # Returns
    /// The PublicKey of the default key
    ///
    /// # Errors
    /// Returns an error if no keys exist
    pub fn get_default_key(&self) -> Result<PublicKey> {
        self.key_manager
            .get_default_key_id()
            .ok_or_else(|| Error::from(InstanceError::AuthenticationRequired))
    }

    /// Get the display name set for a key.
    ///
    /// Display names are caller-supplied human-readable labels passed to
    /// `add_private_key`. They have no cryptographic significance and are
    /// not unique across keys. Returns `None` if the account does not hold a
    /// key with this pubkey, or if it was added without a display name.
    ///
    /// # Arguments
    /// * `key_id` - Public key of a key held by this account
    pub fn key_display_name(&self, key_id: &PublicKey) -> Option<&str> {
        self.key_manager
            .get_key_metadata(key_id)
            .and_then(|metadata| metadata.display_name.as_deref())
    }

    /// Find account-held keys whose display name matches `name`.
    ///
    /// Display names are not unique, so this returns every key whose
    /// `display_name` exactly matches the provided string. Keys without a
    /// display name are never returned. Returns an empty `Vec` when there
    /// are no matches.
    ///
    /// # Arguments
    /// * `name` - Exact display name to match
    pub fn find_keys_by_display_name(&self, name: &str) -> Vec<PublicKey> {
        self.key_manager
            .list_key_ids()
            .into_iter()
            .filter(|key_id| {
                self.key_manager
                    .get_key_metadata(key_id)
                    .and_then(|metadata| metadata.display_name.as_deref())
                    == Some(name)
            })
            .collect()
    }

    /// Get a signing key by its ID.
    ///
    /// Hands out key material from this session's unlocked key manager. Callers
    /// that sign on the account's behalf need it — signing a sync request as the
    /// key whose access they are claiming, for example.
    ///
    /// # Arguments
    /// * `key_id` - The public key identifier
    ///
    /// # Returns
    /// The PrivateKey if found
    pub fn get_signing_key(&self, key_id: &PublicKey) -> Result<crate::auth::crypto::PrivateKey> {
        self.key_manager
            .get_signing_key(key_id)
            .cloned()
            .ok_or_else(|| {
                AccountError::KeyNotFound {
                    key_id: key_id.to_string(),
                }
                .into()
            })
    }

    // === Bootstrap Request Management (AccountSession Context) ===

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
        sync: &Sync,
    ) -> Result<Vec<(String, BootstrapRequest)>> {
        sync.pending_bootstrap_requests().await
    }

    /// Approve a bootstrap request and add the requesting key to the target database.
    ///
    /// The approving key must have Admin permission on the target database.
    ///
    /// # Arguments
    /// * `sync` - Mutable reference to the Instance's Sync object
    /// * `request_id` - The unique identifier of the request to approve
    /// * `approving_key_id` - The ID of this account's key to use for approval (must have Admin permission)
    ///
    /// # Returns
    /// Result indicating success or failure of the approval operation
    ///
    /// # Errors
    /// - Returns an error if the account doesn't own the specified approving key
    /// - Returns an error if the approving key doesn't have Admin permission on the target database
    /// - Returns an error if the request doesn't exist or was rejected
    /// - Reduced, removed or uncertain authority needs a fresh explicit grant
    /// - Returns an error if the key addition to the database fails
    pub async fn approve_bootstrap_request(
        &self,
        sync: &Sync,
        request_id: &str,
        approving_key_id: &PublicKey,
    ) -> Result<()> {
        // Get the signing key from the key manager
        let signing_key = self
            .key_manager
            .get_signing_key(approving_key_id)
            .ok_or_else(|| super::errors::AccountError::KeyNotFound {
                key_id: approving_key_id.to_string(),
            })?;

        // Delegate to Sync layer with the account-provided key
        // The Sync layer will validate permissions when committing the transaction
        let key = DatabaseKey::new(signing_key.clone());
        sync.approve_bootstrap_request_with_key(request_id, &key)
            .await?;

        Ok(())
    }

    /// Reject a bootstrap request.
    ///
    /// This method marks the request as rejected. The requesting device will not
    /// be granted access to the target database. Requires Admin permission on the
    /// target database to prevent unauthorized accounts from disrupting the bootstrap protocol.
    ///
    /// # Arguments
    /// * `sync` - Mutable reference to the Instance's Sync object
    /// * `request_id` - The unique identifier of the request to reject
    /// * `rejecting_key_id` - The ID of this account's key (for permission validation and audit trail)
    ///
    /// # Returns
    /// Result indicating success or failure of the rejection operation
    ///
    /// # Errors
    /// - Returns an error if the account doesn't own the specified rejecting key
    /// - Returns an error if the request doesn't exist or was approved
    /// - Returns an error if the rejecting key lacks Admin permission on the target database
    pub async fn reject_bootstrap_request(
        &self,
        sync: &Sync,
        request_id: &str,
        rejecting_key_id: &PublicKey,
    ) -> Result<()> {
        // Get the signing key from the key manager
        let signing_key = self
            .key_manager
            .get_signing_key(rejecting_key_id)
            .ok_or_else(|| super::errors::AccountError::KeyNotFound {
                key_id: rejecting_key_id.to_string(),
            })?;

        // Delegate to Sync layer with the account-provided key
        // The Sync layer will validate Admin permission on the target database
        let key = DatabaseKey::new(signing_key.clone());
        sync.reject_bootstrap_request_with_key(request_id, &key)
            .await?;

        Ok(())
    }

    /// Request access to a database from a peer (bootstrap sync).
    ///
    /// This convenience method initiates a bootstrap sync request to access a database
    /// that this account doesn't have locally yet. The account's key will be sent to the peer
    /// to request the specified permission level.
    ///
    /// This is useful for multi-device scenarios where a account wants to access their
    /// existing database from a new device, or when requesting access to a database
    /// shared by another account.
    ///
    /// On a successful (already-authorized) request the database is synced and its
    /// SigKey mapping is recorded, so [`open_database`](Self::open_database) works
    /// immediately — no separate [`track_database`](Self::track_database) call is
    /// needed just to open it. The supplied `sync_settings` are recorded with
    /// the database, including when a pending request is retried after approval.
    ///
    /// When approval is still pending the request returns
    /// [`SyncError::BootstrapPending`] without changing Account mappings or
    /// preferences. Supply the arguments again when retrying after approval.
    /// Existing legacy mappings are preserved rather than deleted.
    ///
    /// # Arguments
    /// * `sync` - Reference to the Instance's Sync object
    /// * `ticket` - A ticket containing the database ID and address hints
    /// * `key_id` - The ID of this account's key to use for the request
    /// * `requested_permission` - The permission level being requested
    /// * `sync_settings` - This account's durable synchronization preferences for
    ///   the database once its access mapping is recorded
    /// * `metadata` - Optional free-form context for the approver to inspect on
    ///   the stored bootstrap request when deciding whether to grant access
    ///
    /// # Returns
    /// Result indicating success or failure of the bootstrap request
    ///
    /// # Errors
    /// - Returns an error if the account doesn't own the specified key
    /// - Returns an error if all addresses in the ticket fail
    /// - Returns an error if the bootstrap sync fails
    ///
    /// # Example
    /// ```rust,ignore
    /// // Request write access to a shared database
    /// let user_key_id = account.get_default_key()?;
    /// let ticket: DatabaseTicket = "eidetica:?db=sha256:abc...&pr=http:192.168.1.1:8080".parse()?;
    /// account.request_database_access(
    ///     &sync,
    ///     &ticket,
    ///     &user_key_id,
    ///     Permission::Write(5),
    ///     SyncSettings::on_commit(),
    ///     None,
    /// ).await?;
    ///
    /// // After approval, the database can be opened
    /// let database = account.open_database(ticket.database_id())?;
    /// ```
    pub async fn request_database_access(
        &mut self,
        sync: &Sync,
        ticket: &DatabaseTicket,
        key_id: &PublicKey,
        requested_permission: Permission,
        sync_settings: SyncSettings,
        metadata: Option<Doc>,
    ) -> Result<()> {
        // The request is signed with this key, so the peer can tell an actual
        // key holder from someone naming a key they don't have.
        let signing_key = self
            .key_manager
            .get_signing_key(key_id)
            .ok_or_else(|| super::errors::AccountError::KeyNotFound {
                key_id: key_id.to_string(),
            })?
            .clone();

        let key_name = key_id.to_string();
        let database_id = ticket.database_id().clone();
        let original = self.tracked_database_before_join(&database_id).await?;

        let result = sync
            .bootstrap_with_ticket(
                ticket,
                &signing_key,
                &key_name,
                requested_permission,
                metadata,
            )
            .await;

        self.record_database_access_with_expected(
            &database_id,
            key_id,
            sync_settings,
            result,
            Some(original),
        )
        .await
    }

    /// Join a database from a ticket and open it.
    ///
    /// Works the same on an embedded and a connected instance. An embedded
    /// instance bootstraps through its own [`Sync`], which must be enabled.
    /// A connected instance asks the daemon to do it: the daemon chooses the
    /// route and owns the network exchange, while `key_id`'s private key
    /// stays in this process and signs only the peer-bound request proof. The
    /// call waits until the peer answers.
    ///
    /// A ticket locates a database; it does not grant access. The peer decides
    /// whether `key_id` may have `requested_permission`, exactly as for
    /// [`request_database_access`](Self::request_database_access), whose
    /// mapping and preference semantics this shares:
    ///
    /// - When access is granted, the database is synced, `key_id`'s SigKey
    ///   mapping and `sync_settings` are recorded, and the opened database is
    ///   returned.
    /// - When approval is pending, this returns [`SyncError::BootstrapPending`]
    ///   without changing Account mappings or preferences. Call `join` again with
    ///   the same arguments once the request is approved.
    /// - A rejected request returns [`SyncError::BootstrapRejected`] and
    ///   records nothing.
    ///
    /// Success is point-in-time usability of the verified supplied closure,
    /// not global catch-up. Identical completed settings are not written again;
    /// preferences changed during the network wait are preserved.
    /// Dropping the future, disconnecting, or an error may leave peer requests,
    /// replicated entries, routing or a committed Account Entry without its ack.
    /// Retry explicitly; nothing automatically resumes a join after restart.
    /// Held/unreadable acquisition remains refused. Missing delegated history
    /// stays Unverified until supplied and verified separately; this call does
    /// not acquire it automatically. Native conflicting authority is unchanged,
    /// so recovery from every interrupted transfer is not guaranteed.
    /// Joining grants user-key access, not device enrollment. Later daemon
    /// replication uses its device key and needs separate authorization (or a
    /// global grant); this call does not promise ongoing private replication.
    ///
    /// # Arguments
    /// * `ticket` - Database ID and address hints
    /// * `key_id` - This user's key to request access for
    /// * `requested_permission` - The permission level being requested
    /// * `sync_settings` - This user's replication preference for the database
    /// * `metadata` - Optional context for the approver
    ///
    /// # Errors
    /// - [`AccountError::KeyNotFound`] if this user does not hold `key_id`
    /// - [`SyncError::SyncNotEnabled`] on an embedded instance without sync
    /// - [`SyncError::BootstrapPending`] or [`SyncError::BootstrapRejected`]
    ///   as described above
    /// - Any network or sync error if no ticket address answered
    ///
    /// # Example
    /// ```rust,ignore
    /// let key_id = user.get_default_key()?;
    /// let database = user
    ///     .join(&ticket, &key_id, Permission::Write(10), SyncSettings::on_commit(), None)
    ///     .await?;
    /// ```
    pub async fn join(
        &mut self,
        ticket: &DatabaseTicket,
        key_id: &PublicKey,
        requested_permission: Permission,
        sync_settings: SyncSettings,
        metadata: Option<Doc>,
    ) -> Result<Database> {
        let database_id = ticket.database_id().clone();

        #[cfg(all(unix, feature = "service"))]
        if let Some(connection) = self.instance.remote_connection() {
            let original = self.tracked_database_before_join(&database_id).await?;
            let signing_key = self
                .key_manager
                .get_signing_key(key_id)
                .ok_or_else(|| super::errors::AccountError::KeyNotFound {
                    key_id: key_id.to_string(),
                })?
                .clone();
            let result = connection
                .bootstrap_ticket(
                    ticket,
                    &signing_key,
                    &key_id.to_string(),
                    requested_permission,
                    metadata,
                    self.instance.clock().now_millis(),
                )
                .await;
            self.record_database_access_with_expected(
                &database_id,
                key_id,
                sync_settings,
                result,
                Some(original),
            )
            .await?;
            return self
                .open_joined_database(&database_id, key_id, requested_permission)
                .await;
        }

        let sync = self.instance.sync().ok_or(SyncError::SyncNotEnabled)?;
        self.request_database_access(
            &sync,
            ticket,
            key_id,
            requested_permission,
            sync_settings,
            metadata,
        )
        .await?;
        self.open_joined_database(&database_id, key_id, requested_permission)
            .await
    }

    async fn open_joined_database(
        &self,
        database_id: &ID,
        key_id: &PublicKey,
        requested_permission: Permission,
    ) -> Result<Database> {
        let database = self.open_database_with_key(database_id, key_id).await?;
        if database.current_permission().await? < requested_permission {
            return Err(SyncError::PermissionDenied(
                "requested permission is no longer available at join completion".to_string(),
            )
            .into());
        }
        Ok(database)
    }

    /// Record the Account-layer SigKey mapping for a bootstrap whose network phase
    /// has already completed, given the [`Result`] returned by
    /// [`Sync::bootstrap_with_ticket`](crate::sync::Sync::bootstrap_with_ticket).
    ///
    /// [`request_database_access`](Self::request_database_access) is the usual
    /// entry point and performs the network bootstrap for you. This split-out
    /// method exists for callers that hold a coarse lock around the `AccountSession` (e.g.
    /// a daemon's per-session lock): they can run the network round-trip without
    /// the lock held and re-acquire it only for this cheap, local mapping write,
    /// so a slow or hung peer never blocks the rest of the session. The mapping
    /// semantics are identical to `request_database_access`. The split form
    /// treats supplied preferences as current explicit intent; use `join` or
    /// `request_database_access` to preserve preferences changed during their
    /// network wait.
    ///
    /// The `bootstrap_result` is consumed and its outcome re-raised unchanged so
    /// callers can react to [`SyncError::BootstrapPending`] et al.
    pub async fn record_database_access(
        &mut self,
        database_id: &ID,
        key_id: &PublicKey,
        sync_settings: SyncSettings,
        bootstrap_result: Result<()>,
    ) -> Result<()> {
        self.record_database_access_with_expected(
            database_id,
            key_id,
            sync_settings,
            bootstrap_result,
            None,
        )
        .await
    }

    async fn record_database_access_with_expected(
        &mut self,
        database_id: &ID,
        key_id: &PublicKey,
        sync_settings: SyncSettings,
        bootstrap_result: Result<()>,
        original: Option<Option<TrackedDatabase>>,
    ) -> Result<()> {
        // Pending, Rejected and failures do not create or delete AccountSession state.
        bootstrap_result?;
        self.track_database_with_expected(database_id.clone(), key_id, sync_settings, original)
            .await
    }

    async fn tracked_database_before_join(
        &self,
        database_id: &ID,
    ) -> Result<Option<TrackedDatabase>> {
        let table = self
            .account_database
            .get_store_viewer::<Table<TrackedDatabase>>("databases")
            .await?;
        match table.get(&database_id.to_string()).await {
            Ok(value) => Ok(Some(value)),
            Err(Error::Store(e)) if matches!(*e, StoreError::KeyNotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    // === Tracked Databases ===

    /// Track a database, adding it to this account's list with auto-discovery of SigKeys.
    ///
    /// This method adds an existing database to your tracked list, or updates it if
    /// already tracked (upsert behavior).
    ///
    /// When tracking:
    /// 1. Uses Database::find_sigkeys() to discover which SigKey the account can use
    /// 2. Automatically selects the SigKey with highest permission
    /// 3. Stores the key mapping and sync settings
    ///
    /// The sync_settings indicate your sync preferences, but do not automatically
    /// configure sync. Use the Sync module's peer and tree methods to set up actual
    /// sync relationships.
    ///
    /// # Arguments
    /// * `database_id` - ID of the database to track
    /// * `key_id` - Which account key to use for this database
    /// * `sync_settings` - Sync preferences for this database
    ///
    /// # Returns
    /// Result indicating success or failure
    ///
    /// # Errors
    /// - Returns `NoSigKeyFound` if no SigKey can be found for the specified key
    /// - Returns `KeyNotFound` if the specified key_id doesn't exist
    pub async fn track_database(
        &mut self,
        database_id: impl Into<ID>,
        key_id: &PublicKey,
        sync_settings: SyncSettings,
    ) -> Result<()> {
        self.track_database_with_expected(database_id.into(), key_id, sync_settings, None)
            .await
    }

    async fn track_database_with_expected(
        &mut self,
        database_id: ID,
        key_id: &PublicKey,
        sync_settings: SyncSettings,
        original: Option<Option<TrackedDatabase>>,
    ) -> Result<()> {
        let tracked = TrackedDatabase {
            database_id,
            key_id: key_id.clone(),
            sync_settings,
        };
        if self.key_manager.get_signing_key(key_id).is_none() {
            return Err(AccountError::KeyNotFound {
                key_id: key_id.to_string(),
            }
            .into());
        }
        // Discovery can read delegated databases, so do it before holding the
        // AccountSession write lock. Even an unchanged tracked key needs a valid identity.
        let available =
            Database::find_sigkeys(&self.instance, &tracked.database_id, key_id).await?;
        let (sigkey, _) = available
            .first()
            .ok_or_else(|| AccountError::NoSigKeyFound {
                key_id: key_id.to_string(),
                database_id: tracked.database_id.clone(),
            })?;
        let (tx, guard) = self.key_mapping_transaction().await?;
        let databases_table = tx.get_store::<Table<TrackedDatabase>>("databases").await?;

        // Use database ID as the key - check if it already exists (O(1))
        let db_id_key = tracked.database_id.to_string();
        let existing = match databases_table.get(&db_id_key).await {
            Ok(value) => Some(value),
            Err(Error::Store(e)) if matches!(*e, StoreError::KeyNotFound { .. }) => None,
            Err(e) => return Err(e),
        };
        // Even an unchanged tracked key may carry a legacy/provisional identity.
        let (metadata, mapping_changed) = self
            .map_key_in_txn(&tx, key_id, &tracked.database_id, sigkey.clone())
            .await?;
        // A network attempt must not overwrite intent changed while it waited.
        let settings_changed = original.as_ref().is_none_or(|before| *before == existing)
            && existing.as_ref() != Some(&tracked);
        if settings_changed {
            databases_table.set(&db_id_key, tracked).await?;
        }
        if mapping_changed || settings_changed {
            let result = tx.commit_under_tree_lock(guard).await;
            // A failed/lost acknowledgement is not rollback. Read the verified
            // state before publishing anything into the session cache.
            self.refresh_key_metadata(key_id).await?;
            result?;
        } else {
            drop(guard);
            self.key_manager.add_key(metadata)?;
        }

        // Update sync system to immediately recompute combined settings
        // This ensures automatic sync works right away, without waiting for background worker
        if let Some(sync) = self.instance.sync() {
            // Auto-sync account tracking if not already synced
            // This is idempotent - safe to call multiple times
            sync.sync_account(&self.login_id, self.account_database.root_id())
                .await?;
        }

        Ok(())
    }

    /// List all tracked databases.
    ///
    /// Returns all databases this account has added to their tracked list.
    ///
    /// # Returns
    /// Vector of TrackedDatabase entries
    pub async fn databases(&self) -> Result<Vec<TrackedDatabase>> {
        let databases_table = self
            .account_database
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
    /// The TrackedDatabase if it's in the account's tracked list
    ///
    /// # Errors
    /// Returns `DatabaseNotTracked` if the database is not in the account's list
    pub async fn database(&self, database_id: &ID) -> Result<TrackedDatabase> {
        let databases_table = self
            .account()
            .get_store_viewer::<Table<TrackedDatabase>>("databases")
            .await?;

        // Direct O(1) lookup using database ID as key
        let db_id_key = database_id.to_string();
        databases_table.get(&db_id_key).await.map_err(|_| {
            AccountError::DatabaseNotTracked {
                database_id: database_id.clone(),
            }
            .into()
        })
    }

    /// Check whether this account has sync enabled for a tracked database.
    ///
    /// Returns this account's own preference, not the host's combined state.
    /// A `false` result means this account is not personally sharing the database;
    /// another account on the same instance may still have sync enabled for it.
    ///
    /// Returns `Ok(false)` for databases not tracked by this account.
    pub async fn is_sync_enabled(&self, database_id: &ID) -> Result<bool> {
        let databases_table = self
            .account()
            .get_store_viewer::<Table<TrackedDatabase>>("databases")
            .await?;
        let db_id_key = database_id.to_string();
        match databases_table.get(&db_id_key).await {
            Ok(tracked) => Ok(tracked.sync_settings.sync_enabled),
            Err(_) => Ok(false),
        }
    }

    /// Stop tracking a database.
    ///
    /// This removes the database from the account's tracked list.
    /// It does not delete the database itself, remove key mappings, or delete any data.
    ///
    /// # Arguments
    /// * `database_id` - The ID of the database to stop tracking
    ///
    /// # Errors
    /// Returns `DatabaseNotTracked` if the database is not in the account's list
    pub async fn untrack_database(&mut self, database_id: &ID) -> Result<()> {
        let tx = self.account_database.new_transaction().await?;
        let databases_table = tx.get_store::<Table<TrackedDatabase>>("databases").await?;

        // Direct O(1) delete using database ID as key
        let db_id_key = database_id.to_string();

        // Verify it exists before deleting
        if databases_table.get(&db_id_key).await.is_err() {
            return Err(AccountError::DatabaseNotTracked {
                database_id: database_id.clone(),
            }
            .into());
        }

        // Delete using database ID as key
        databases_table.delete(&db_id_key).await?;
        tx.commit().await?;

        Ok(())
    }
}
