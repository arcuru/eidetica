//! Database module provides functionality for managing collections of related entries.
//!
//! A `Database` represents a hierarchical structure of entries, like a traditional database
//! or a branch in a version control system. Each database has a root entry and maintains
//! the history and relationships between entries. Database holds a weak reference to its
//! parent Instance, accessing storage and coordination services through that handle.

use ed25519_dalek::SigningKey;
use rand::{Rng, RngCore, distributions::Alphanumeric};
use serde_json;

use crate::{
    Error, Instance, Result, Transaction, WeakInstance,
    auth::{
        crypto::format_public_key,
        errors::AuthError,
        settings::AuthSettings,
        types::{AuthKey, Permission, SigKey},
    },
    constants::{ROOT, SETTINGS},
    crdt::{Doc, doc::Value},
    entry::{Entry, ID},
    instance::backend::Backend,
    instance::errors::InstanceError,
    store::{SettingsStore, Store},
};

/// Specifies the signing key for database operations
#[derive(Clone, Debug)]
pub struct KeySource {
    /// The signing key, already decrypted and ready to use (from UserKeyManager)
    pub signing_key: Box<SigningKey>,
    /// The SigKey identifier used in the database's auth settings
    pub sigkey: String,
}

/// Represents a collection of related entries, like a traditional database or a branch in a version control system.
///
/// Each `Database` is identified by the ID of its root `Entry` and manages the history of data
/// associated with that root. It interacts with the underlying storage through the Instance handle.
#[derive(Clone, Debug)]
pub struct Database {
    root: ID,
    instance: WeakInstance,
    /// Key source for operations on this database
    key_source: Option<KeySource>,
}

impl Database {
    /// Creates a new `Database` instance with a user-provided signing key.
    ///
    /// This constructor creates a new database using a signing key that's already in memory
    /// (e.g., from UserKeyManager), without requiring the key to be stored in the backend.
    /// This is the preferred method for creating databases in a User context where keys
    /// are managed separately from the backend.
    ///
    /// The created database will use `KeySource` for all subsequent operations,
    /// meaning transactions will use the provided key directly rather than looking it up
    /// from backend storage.
    ///
    /// # Key Management Models
    ///
    /// - **Backend-managed keys** (legacy): Use `Database::new()` - keys stored in backend
    /// - **User-managed keys** (recommended): Use this method - keys managed by UserKeyManager
    ///
    /// # Arguments
    /// * `initial_settings` - A `Doc` CRDT containing the initial settings for the database.
    ///   If no auth configuration is provided, it will be bootstrapped with the provided key.
    /// * `instance` - Instance handle for storage and coordination
    /// * `signing_key` - The signing key to use for the initial commit and subsequent operations.
    ///   This key should already be decrypted and ready to use.
    /// * `sigkey` - The SigKey identifier to use in the database's auth settings.
    ///   This is typically the public key string but can be any identifier.
    ///
    /// # Returns
    /// A `Result` containing the new `Database` instance configured with `KeySource`.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::backend::database::InMemory;
    /// # use eidetica::auth::crypto::{generate_keypair, format_public_key};
    /// # use eidetica::crdt::Doc;
    /// # #[tokio::main]
    /// # async fn main() -> Result<()> {
    /// let instance = Instance::open(Box::new(InMemory::new())).await?;
    /// let (signing_key, public_key) = generate_keypair();
    /// let sigkey = format_public_key(&public_key);
    ///
    /// let mut settings = Doc::new();
    /// settings.set("name", "my_database");
    ///
    /// // Create database with user-managed key (no backend storage needed)
    /// let database = Database::create(
    ///     settings,
    ///     &instance,
    ///     signing_key,
    ///     sigkey,
    /// ).await?;
    ///
    /// // All transactions automatically use the provided key
    /// let tx = database.new_transaction().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn create(
        initial_settings: Doc,
        instance: &crate::Instance,
        signing_key: SigningKey,
        sigkey: String,
    ) -> Result<Self> {
        // Check if auth is configured in the initial settings
        let auth_configured = matches!(initial_settings.get("auth"), Some(Value::Doc(auth_map)) if !auth_map.is_empty());

        // FIXME: this should merge the provided settings and the added signing key should be added as root

        let final_database_settings = if auth_configured {
            // Auth settings are already provided - use them as-is with the provided signing key
            initial_settings
        } else {
            // No auth config provided - bootstrap auth configuration with the provided key
            let public_key = signing_key.verifying_key();

            // Create auth settings with the provided key
            let mut auth_settings_handler = AuthSettings::new();
            let super_user_auth_key = AuthKey::active(
                format_public_key(&public_key),
                Permission::Admin(0), // Highest priority
            )
            .unwrap();
            auth_settings_handler.add_key(&sigkey, super_user_auth_key)?;

            // Prepare final database settings for the initial commit
            let mut final_database_settings = initial_settings.clone();
            final_database_settings.set("auth", auth_settings_handler.as_doc().clone());

            final_database_settings
        };

        // Create the initial root entry using a temporary Database and Transaction
        // This placeholder ID should not exist in the backend, so get_tips will be empty.
        let bootstrap_placeholder_id = format!(
            "bootstrap_root_{}",
            rand::thread_rng()
                .sample_iter(&Alphanumeric)
                .take(10)
                .map(char::from)
                .collect::<String>()
        );

        // Create temporary database for bootstrap with KeySource
        // This allows the bootstrap transaction to use the provided key directly
        let temp_database_for_bootstrap = Database {
            root: bootstrap_placeholder_id.clone().into(),
            instance: instance.downgrade(),
            key_source: Some(KeySource {
                signing_key: Box::new(signing_key.clone()),
                sigkey: sigkey.clone(),
            }),
        };

        // Create the transaction - it will use the provided key automatically
        let op = temp_database_for_bootstrap.new_transaction().await?;

        // IMPORTANT: For the root entry, we need to set the database root to empty string
        // so that is_root() returns true and all_roots() can find it
        op.set_entry_root("")?;

        // Populate the SETTINGS and ROOT subtrees for the very first entry
        op.update_subtree(SETTINGS, &serde_json::to_string(&final_database_settings)?)
            .await?;
        op.update_subtree(ROOT, &serde_json::to_string(&"".to_string())?)
            .await?; // Standard practice for root entry's _root

        // Add entropy to the entry metadata to ensure unique database IDs even with identical settings
        op.set_metadata_entropy(rand::thread_rng().next_u64())?;

        // Commit the initial entry
        let new_root_id = op.commit().await?;

        // Now create the real database with the new_root_id and KeySource
        Ok(Self {
            root: new_root_id,
            instance: instance.downgrade(),
            key_source: Some(KeySource {
                signing_key: Box::new(signing_key),
                sigkey,
            }),
        })
    }

    /// Opens a database for read-only access, bypassing authentication validation.
    ///
    /// # Internal Use Only
    ///
    /// This method bypasses authentication validation and is intended for internal
    /// operations that require reading database state (loading settings, checking
    /// permissions, resolving delegations, etc.).
    ///
    /// These operations should only be performed by the server/instance administrator,
    /// but we don't verify that yet. Future versions may add admin permission checks.
    ///
    /// # Behavior
    ///
    /// - No authentication validation is performed
    /// - The resulting database has no key source, so commits will fail
    /// - Used internally for system operations that need read access
    ///
    /// # Arguments
    /// * `id` - The `ID` of the root entry.
    /// * `instance` - Instance handle for storage and coordination
    ///
    /// # Returns
    /// A `Result` containing the new `Database` instance or an error.
    pub(crate) fn open_unauthenticated(id: ID, instance: &crate::Instance) -> Result<Self> {
        Ok(Self {
            root: id,
            instance: instance.downgrade(),
            key_source: None,
        })
    }

    /// Opens an existing `Database` with a user-provided signing key.
    ///
    /// This constructor opens an existing database by its root ID and configures it to use
    /// a user-provided signing key for all subsequent operations. This is used in the User
    /// context where keys are managed by UserKeyManager and already decrypted in memory.
    ///
    /// # Key Management
    ///
    /// This constructor uses **user-managed keys**:
    /// - The key is provided directly (e.g., from UserKeyManager)
    /// - Uses `KeySource` for all subsequent operations
    /// - No backend key storage needed
    ///
    /// Note: To **create** a new database with user-managed keys, use `create()`.
    /// This method is for **opening existing** databases.
    ///
    /// To discover which SigKey to use for a given public key, use `Database::find_sigkeys()`.
    ///
    /// # Arguments
    /// * `instance` - Instance handle for storage and coordination
    /// * `root_id` - The root entry ID of the existing database to open
    /// * `signing_key` - Decrypted signing key from UserKeyManager
    /// * `sigkey` - SigKey identifier string (use `find_sigkeys()` to discover available options)
    ///
    /// # Returns
    /// A `Result` containing the `Database` instance configured with `KeySource`
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::backend::database::InMemory;
    /// # use eidetica::auth::crypto::{generate_keypair, format_public_key};
    /// # use eidetica::auth::types::SigKey;
    /// # #[tokio::main]
    /// # async fn main() -> Result<()> {
    /// # let instance = Instance::open(Box::new(InMemory::new())).await?;
    /// # let (signing_key, verifying_key) = generate_keypair();
    /// # let root_id = "existing_database_root_id".into();
    /// // Find all SigKeys this public key can use
    /// let pubkey = format_public_key(&verifying_key);
    /// let sigkeys = Database::find_sigkeys(&instance, &root_id, &pubkey).await?;
    ///
    /// // Use the first available SigKey
    /// if let Some((sigkey, _permission)) = sigkeys.first() {
    ///     let sigkey_str = match sigkey {
    ///         SigKey::Direct(name) => name.clone(),
    ///         _ => panic!("Delegation paths not yet supported"),
    ///     };
    ///
    ///     // Open database with the resolved SigKey
    ///     let database = Database::open(instance, &root_id, signing_key, sigkey_str).await?;
    ///
    ///     // All transactions automatically use the provided key
    ///     let tx = database.new_transaction().await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn open(
        instance: Instance,
        root_id: &ID,
        signing_key: SigningKey,
        sigkey: String,
    ) -> Result<Self> {
        // Load auth settings to validate/resolve sigkey
        let temp_db = Self::open_unauthenticated(root_id.clone(), &instance)?;
        let settings_store = temp_db.get_settings().await?;
        let auth_settings = settings_store.get_auth_settings().await?;

        // Determine effective sigkey
        let effective_sigkey = if sigkey == "*" {
            // Explicit global - verify it exists
            if auth_settings.get_global_permission().is_none() {
                return Err(Error::Auth(AuthError::InvalidAuthConfiguration {
                    reason: "Global '*' permission not configured".to_string(),
                }));
            }
            "*".to_string()
        } else if auth_settings.get_key(&sigkey).is_ok() {
            // Key found in settings - use it
            sigkey
        } else if auth_settings.get_global_permission().is_some() {
            // Key not found but global exists - switch to "*"
            "*".to_string()
        } else {
            // Key not found and no global - error
            return Err(Error::Auth(AuthError::InvalidAuthConfiguration {
                reason: format!(
                    "Key '{}' not found in auth settings and no global permission",
                    sigkey
                ),
            }));
        };

        Ok(Self {
            root: root_id.clone(),
            instance: instance.downgrade(),
            key_source: Some(KeySource {
                signing_key: Box::new(signing_key),
                sigkey: effective_sigkey,
            }),
        })
    }

    /// Find all SigKeys that a public key can use to access a database.
    ///
    /// This static helper method loads a database's authentication settings and returns
    /// all possible SigKeys that can be used with the given public key. This is useful for
    /// discovering authentication options before opening a database.
    ///
    /// Returns all matching SigKeys including:
    /// - Specific key names where the pubkey matches
    /// - Global "*" permission if available
    /// - (Future) Delegation paths
    ///
    /// The results are **sorted by permission level, highest first**, making it easy to
    /// select the most privileged access available.
    ///
    /// # Arguments
    /// * `instance` - Instance handle for storage and coordination
    /// * `root_id` - Root entry ID of the database to check
    /// * `pubkey` - Public key string (e.g., "Ed25519:abc123...") to look up
    ///
    /// # Returns
    /// A vector of (SigKey, Permission) tuples, sorted by permission (highest first).
    /// Returns empty vector if no valid access methods are found.
    ///
    /// # Errors
    /// Returns an error if:
    /// - Database cannot be loaded
    /// - Auth settings cannot be parsed
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::backend::database::InMemory;
    /// # use eidetica::auth::crypto::{generate_keypair, format_public_key};
    /// # use eidetica::auth::types::SigKey;
    /// # #[tokio::main]
    /// # async fn main() -> Result<()> {
    /// # let instance = Instance::open(Box::new(InMemory::new())).await?;
    /// # let (signing_key, verifying_key) = generate_keypair();
    /// # let root_id = "database_root_id".into();
    /// // Get the public key string
    /// let pubkey = format_public_key(&verifying_key);
    ///
    /// // Find all SigKeys this pubkey can use (sorted highest permission first)
    /// let sigkeys = Database::find_sigkeys(&instance, &root_id, &pubkey).await?;
    ///
    /// // Use the first available SigKey (highest permission)
    /// if let Some((sigkey, _permission)) = sigkeys.first() {
    ///     let sigkey_str = match sigkey {
    ///         SigKey::Direct(name) => name.clone(),
    ///         _ => panic!("Delegation paths not yet supported"),
    ///     };
    ///     let database = Database::open(instance, &root_id, signing_key, sigkey_str).await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn find_sigkeys(
        instance: &Instance,
        root_id: &ID,
        pubkey: &str,
    ) -> Result<Vec<(SigKey, Permission)>> {
        // Create temporary database to load settings (no key source needed for reading)
        let temp_db = Self::open_unauthenticated(root_id.clone(), instance)?;

        // Load auth settings
        let settings_store = temp_db.get_settings().await?;
        let auth_settings = settings_store.get_auth_settings().await?;

        // Find all SigKeys for this pubkey (returns sorted by highest permission first)
        Ok(auth_settings.find_all_sigkeys_for_pubkey(pubkey))
    }

    /// Get the default authentication key ID for this database.
    pub fn default_auth_key(&self) -> Option<&str> {
        self.key_source.as_ref().map(|ks| ks.sigkey.as_str())
    }

    /// Register an Instance-wide callback to be invoked when entries are written locally to this database.
    ///
    /// Local writes are those originating from transaction commits in the current Instance.
    /// The callback receives the entry, database, and instance as parameters, providing
    /// full context for any coordination or side effects needed.
    ///
    /// **Important:** This callback is registered at the Instance level and will fire for all local
    /// writes to the database tree (identified by root ID), regardless of which Database handle
    /// performed the write. Multiple Database handles pointing to the same root ID share the same
    /// set of callbacks.
    ///
    /// # Arguments
    /// * `callback` - Function to invoke on local writes to this database tree
    ///
    /// # Returns
    /// A Result indicating success or failure
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::backend::database::InMemory;
    /// # use ed25519_dalek::SigningKey;
    /// # #[tokio::main]
    /// # async fn main() -> Result<()> {
    /// let instance = Instance::open(Box::new(InMemory::new())).await?;
    /// # let settings = eidetica::crdt::Doc::new();
    /// # let signing_key = SigningKey::from_bytes(&[0u8; 32]);
    /// # let database = Database::create(settings, &instance, signing_key, "key".to_string()).await?;
    ///
    /// database.on_local_write(|entry, db, _instance| {
    ///     let entry_id = entry.id().clone();
    ///     let db_id = db.root_id().clone();
    ///     Box::pin(async move {
    ///         println!("Entry {} written to database {}", entry_id, db_id);
    ///         Ok(())
    ///     })
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn on_local_write<F, Fut>(&self, callback: F) -> Result<()>
    where
        F: for<'a> Fn(&'a Entry, &'a Database, &'a Instance) -> Fut
            + Send
            + std::marker::Sync
            + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        let instance = self.instance()?;
        instance.register_write_callback(
            crate::instance::WriteSource::Local,
            self.root_id().clone(),
            callback,
        )
    }

    /// Register an Instance-wide callback to be invoked when entries are written remotely to this database.
    ///
    /// Remote writes are those originating from sync or replication from other nodes.
    /// The callback receives the entry, database, and instance as parameters.
    ///
    /// **Important:** This callback is registered at the Instance level and will fire for all remote
    /// writes to the database tree (identified by root ID), regardless of which Database handle
    /// registered the callback. Multiple Database handles pointing to the same root ID share the same
    /// set of callbacks.
    ///
    /// # Arguments
    /// * `callback` - Function to invoke on remote writes to this database tree
    ///
    /// # Returns
    /// A Result indicating success or failure
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::backend::database::InMemory;
    /// # use ed25519_dalek::SigningKey;
    /// # #[tokio::main]
    /// # async fn main() -> Result<()> {
    /// let instance = Instance::open(Box::new(InMemory::new())).await?;
    /// # let settings = eidetica::crdt::Doc::new();
    /// # let signing_key = SigningKey::from_bytes(&[0u8; 32]);
    /// # let database = Database::create(settings, &instance, signing_key, "key".to_string()).await?;
    ///
    /// database.on_remote_write(|entry, db, _instance| {
    ///     let entry_id = entry.id().clone();
    ///     let db_id = db.root_id().clone();
    ///     Box::pin(async move {
    ///         println!("Remote entry {} synced to database {}", entry_id, db_id);
    ///         Ok(())
    ///     })
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn on_remote_write<F, Fut>(&self, callback: F) -> Result<()>
    where
        F: for<'a> Fn(&'a Entry, &'a Database, &'a Instance) -> Fut
            + Send
            + std::marker::Sync
            + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        let instance = self.instance()?;
        instance.register_write_callback(
            crate::instance::WriteSource::Remote,
            self.root_id().clone(),
            callback,
        )
    }

    /// Get the ID of the root entry
    pub fn root_id(&self) -> &ID {
        &self.root
    }

    /// Upgrade the weak instance reference to a strong reference.
    ///
    /// # Returns
    /// A `Result` containing the Instance or an error if the Instance has been dropped.
    pub(crate) fn instance(&self) -> Result<Instance> {
        self.instance
            .upgrade()
            .ok_or_else(|| Error::Instance(InstanceError::InstanceDropped))
    }

    /// Get a reference to the backend
    pub fn backend(&self) -> Result<Backend> {
        Ok(self.instance()?.backend().clone())
    }

    /// Retrieve the root entry from the backend
    pub async fn get_root(&self) -> Result<Entry> {
        let instance = self.instance()?;
        instance.get(&self.root).await
    }

    /// Get a read-only settings store for the database.
    ///
    /// Returns a SettingsStore that provides access to the database's settings.
    /// Since this creates an internal transaction that is never committed, any
    /// modifications made through the returned store will not persist.
    ///
    /// For making persistent changes to settings, create a transaction and use
    /// `Transaction::get_settings()` instead.
    ///
    /// # Returns
    /// A `Result` containing the `SettingsStore` for settings or an error.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # async fn example(database: Database) -> eidetica::Result<()> {
    /// // Read-only access
    /// let settings = database.get_settings().await?;
    /// let name = settings.get_name().await?;
    ///
    /// // For modifications, use a transaction:
    /// let txn = database.new_transaction().await?;
    /// let settings = txn.get_settings()?;
    /// settings.set_name("new_name").await?;
    /// txn.commit().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_settings(&self) -> Result<SettingsStore> {
        let txn = self.new_transaction().await?;
        txn.get_settings()
    }

    /// Get the name of the database from its settings store
    pub async fn get_name(&self) -> Result<String> {
        let settings = self.get_settings().await?;
        settings.get_name().await
    }

    /// Create a new atomic transaction on this database
    ///
    /// This creates a new atomic transaction containing a new Entry.
    /// The atomic transaction will be initialized with the current state of the database.
    /// If a default authentication key is set, the transaction will use it for signing.
    ///
    /// # Returns
    /// A `Result<Transaction>` containing the new atomic transaction
    pub async fn new_transaction(&self) -> Result<Transaction> {
        let tips = self.get_tips().await?;
        self.new_transaction_with_tips(&tips).await
    }

    /// Create a new atomic transaction on this database with specific parent tips
    ///
    /// This creates a new atomic transaction that will have the specified entries as parents
    /// instead of using the current database tips. This allows creating complex DAG structures
    /// like diamond patterns for testing and advanced use cases.
    ///
    /// # Arguments
    /// * `tips` - The specific parent tips to use for this transaction
    ///
    /// # Returns
    /// A `Result<Transaction>` containing the new atomic transaction
    pub async fn new_transaction_with_tips(&self, tips: impl AsRef<[ID]>) -> Result<Transaction> {
        let mut op = Transaction::new_with_tips(self, tips.as_ref()).await?;

        // Set provided signing key (all databases use KeySource now)
        if let Some(KeySource {
            signing_key,
            sigkey,
        }) = &self.key_source
        {
            op.set_provided_key(*signing_key.clone(), sigkey.clone());
        }

        Ok(op)
    }

    /// Insert an entry into the database without modifying it.
    /// This is primarily for testing purposes or when you need full control over the entry.
    /// Note: This method assumes the entry is already properly signed and verified.
    pub async fn insert_raw(&self, entry: Entry) -> Result<ID> {
        let instance = self.instance()?;
        let id = entry.id();

        instance
            .put(crate::backend::VerificationStatus::Verified, entry)
            .await?;

        Ok(id)
    }

    /// Get a Store type that will handle accesses to the Store
    /// This will return a Store initialized to point at the current state of the database.
    ///
    /// The returned store should NOT be used to modify the database, as it intentionally does not
    /// expose the Transaction. Since the Transaction is never committed, it does not have any
    /// effect on the database.
    pub async fn get_store_viewer<T>(&self, name: impl Into<String>) -> Result<T>
    where
        T: Store,
    {
        let op = self.new_transaction().await?;
        T::new(&op, name.into()).await
    }

    /// Get the current tips (leaf entries) of the main database branch.
    ///
    /// Tips represent the latest entries in the database's main history, forming the heads of the DAG.
    ///
    /// # Returns
    /// A `Result` containing a vector of `ID`s for the tip entries or an error.
    pub async fn get_tips(&self) -> Result<Vec<ID>> {
        let instance = self.instance()?;
        instance.get_tips(&self.root).await
    }

    /// Get the full `Entry` objects for the current tips of the main database branch.
    ///
    /// # Returns
    /// A `Result` containing a vector of the tip `Entry` objects or an error.
    pub async fn get_tip_entries(&self) -> Result<Vec<Entry>> {
        let instance = self.instance()?;
        let tips = instance.get_tips(&self.root).await?;
        let mut entries = Vec::new();
        for id in &tips {
            entries.push(instance.get(id).await?);
        }
        Ok(entries)
    }

    /// Get a single entry by ID from this database.
    ///
    /// This is the primary method for retrieving entries after commit operations.
    /// It provides safe, high-level access to entry data without exposing backend details.
    ///
    /// The method verifies that the entry belongs to this database by checking its root ID.
    /// If the entry exists but belongs to a different database, an error is returned.
    ///
    /// # Arguments
    /// * `entry_id` - The ID of the entry to retrieve (accepts anything that converts to ID/String)
    ///
    /// # Returns
    /// A `Result` containing the `Entry` or an error if not found or not part of this database
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::Instance;
    /// # use eidetica::backend::database::InMemory;
    /// # use eidetica::crdt::Doc;
    /// # #[tokio::main]
    /// # async fn main() -> Result<()> {
    /// # let backend = Box::new(InMemory::new());
    /// # let instance = Instance::open(backend).await?;
    /// # instance.create_user("test", None).await?;
    /// # let mut user = instance.login_user("test", None).await?;
    /// # let key_id = user.add_private_key(None).await?;
    /// # let tree = user.create_database(Doc::new(), &key_id).await?;
    /// # let op = tree.new_transaction().await?;
    /// let entry_id = op.commit().await?;
    /// let entry = tree.get_entry(&entry_id).await?;           // Using &String
    /// let entry = tree.get_entry("some_entry_id").await?;     // Using &str
    /// let entry = tree.get_entry(entry_id.clone()).await?;    // Using String
    /// println!("Entry signature: {:?}", entry.sig);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_entry<I: Into<ID>>(&self, entry_id: I) -> Result<Entry> {
        let instance = self.instance()?;
        let id = entry_id.into();
        let entry = instance.get(&id).await?;

        // Check if the entry belongs to this database
        if !entry.in_tree(&self.root) {
            return Err(InstanceError::EntryNotInDatabase {
                entry_id: id,
                database_id: self.root.clone(),
            }
            .into());
        }

        Ok(entry)
    }

    /// Get multiple entries by ID efficiently.
    ///
    /// This method retrieves multiple entries more efficiently than multiple `get_entry()` calls
    /// by minimizing conversion overhead and pre-allocating the result vector.
    ///
    /// The method verifies that all entries belong to this database by checking their root IDs.
    /// If any entry exists but belongs to a different database, an error is returned.
    ///
    /// # Parameters
    /// * `entry_ids` - An iterable of entry IDs to retrieve. Accepts any string or ID types
    ///   that can be converted to `ID` (`&str`, `String`, `&ID`, etc.)
    ///
    /// # Returns
    /// A `Result` containing a vector of `Entry` objects or an error if any entry is not found or not part of this database
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::Instance;
    /// # use eidetica::backend::database::InMemory;
    /// # use eidetica::crdt::Doc;
    /// # #[tokio::main]
    /// # async fn main() -> Result<()> {
    /// # let backend = Box::new(InMemory::new());
    /// # let instance = Instance::open(backend).await?;
    /// # instance.create_user("test", None).await?;
    /// # let mut user = instance.login_user("test", None).await?;
    /// # let key_id = user.add_private_key(None).await?;
    /// # let tree = user.create_database(Doc::new(), &key_id).await?;
    /// let entry_ids = vec!["id1", "id2", "id3"];
    /// let entries = tree.get_entries(entry_ids).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_entries<I, T>(&self, entry_ids: I) -> Result<Vec<Entry>>
    where
        I: IntoIterator<Item = T>,
        T: Into<ID>,
    {
        // Collect IDs first to minimize conversions and avoid repeat work in iterator chain
        let ids: Vec<ID> = entry_ids.into_iter().map(Into::into).collect();
        let instance = self.instance()?;
        let mut entries = Vec::with_capacity(ids.len());

        for id in ids {
            let entry = instance.get(&id).await?;

            // Check if the entry belongs to this database
            if !entry.in_tree(&self.root) {
                return Err(InstanceError::EntryNotInDatabase {
                    entry_id: id,
                    database_id: self.root.clone(),
                }
                .into());
            }

            entries.push(entry);
        }

        Ok(entries)
    }

    // === AUTHENTICATION HELPERS ===

    /// Verify an entry's signature and authentication against the database's configuration that was valid at the time of entry creation.
    ///
    /// This method validates that:
    /// 1. The entry belongs to this database
    /// 2. The entry is properly signed with a key that was authorized in the database's authentication settings at the time the entry was created
    /// 3. The signature is cryptographically valid
    ///
    /// The method uses the entry's metadata to determine which authentication settings were active when the entry was signed,
    /// ensuring that entries remain valid even if keys are later revoked or settings change.
    ///
    /// # Arguments
    /// * `entry_id` - The ID of the entry to verify (accepts anything that converts to ID/String)
    ///
    /// # Returns
    /// A `Result` containing `true` if the entry is valid and properly authenticated, `false` if authentication fails
    ///
    /// # Errors
    /// Returns an error if:
    /// - The entry is not found
    /// - The entry does not belong to this database
    /// - The entry's metadata cannot be parsed
    /// - The historical authentication settings cannot be retrieved
    pub async fn verify_entry_signature<I: Into<ID>>(&self, entry_id: I) -> Result<bool> {
        let entry = self.get_entry(entry_id).await?;

        // If the entry has no authentication, it's considered valid for backward compatibility
        if entry.sig.key == crate::auth::types::SigKey::default() {
            return Ok(true);
        }

        // Get the authentication settings that were valid at the time this entry was created
        let historical_settings = self.get_historical_settings_for_entry(&entry).await?;

        // Use the authentication validator with historical settings
        let instance = self.instance()?;
        let mut validator = crate::auth::validation::AuthValidator::new();
        validator
            .validate_entry(&entry, &historical_settings, Some(&instance))
            .await
    }

    /// Get the effective permission level for a given SigKey in this database.
    ///
    /// This method checks the database's authentication settings to determine what permission
    /// level (if any) the specified SigKey has. This is useful for validating that a user
    /// has the required permission before performing sensitive operations.
    ///
    /// # Arguments
    /// * `sigkey` - The SigKey identifier to check permissions for
    ///
    /// # Returns
    /// The effective Permission for the SigKey if found
    ///
    /// # Errors
    /// Returns an error if:
    /// - The database settings cannot be retrieved
    /// - The authentication settings cannot be parsed
    /// - The SigKey is not found in the authentication settings
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::backend::database::InMemory;
    /// # use eidetica::auth::crypto::{generate_keypair, format_public_key};
    /// # #[tokio::main]
    /// # async fn main() -> Result<()> {
    /// # let instance = Instance::open(Box::new(InMemory::new())).await?;
    /// # let (signing_key, _) = generate_keypair();
    /// # let database = Database::create(
    /// #     eidetica::crdt::Doc::new(),
    /// #     &instance,
    /// #     signing_key,
    /// #     "my_key".to_string(),
    /// # ).await?;
    /// // Check if a key has Admin permission
    /// let permission = database.get_sigkey_permission("my_key").await?;
    /// if permission.can_admin() {
    ///     println!("Key has Admin permission!");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_sigkey_permission(&self, sigkey: &str) -> Result<Permission> {
        // Get database settings
        let settings_store = self.get_settings().await?;

        // Get auth settings from the settings store
        let auth_settings = settings_store.get_auth_settings().await?;

        // Create SigKey and validate entry auth to get effective permission
        let instance = self.instance()?;
        let sig_key = crate::auth::types::SigKey::Direct(sigkey.to_string());
        let resolved_auth = auth_settings
            .validate_entry_auth(&sig_key, Some(&instance))
            .await?;

        Ok(resolved_auth.effective_permission)
    }

    /// Get the authentication settings that were valid when a specific entry was created.
    ///
    /// This method examines the entry's metadata to find the settings tips that were active
    /// at the time of entry creation, then reconstructs the historical settings state.
    ///
    /// # Arguments
    /// * `entry` - The entry to get historical settings for
    ///
    /// # Returns
    /// A `Result` containing the historical authentication settings
    async fn get_historical_settings_for_entry(&self, _entry: &Entry) -> Result<AuthSettings> {
        // TODO: Implement full historical settings reconstruction from entry metadata
        // For now, use current settings for simplicity and backward compatibility
        //
        // The complete implementation would:
        // 1. Parse entry metadata to get settings tips active at entry creation time
        // 2. Reconstruct the CRDT state from those historical tips
        // 3. Validate against that historical state
        //
        // This ensures entries remain valid even if keys are later revoked,
        // but requires more complex CRDT state reconstruction logic.

        let settings = self.get_settings().await?;
        settings.get_auth_settings().await
    }

    // === DATABASE QUERIES ===

    /// Get all entries in this database.
    ///
    /// ⚠️ **Warning**: This method loads all entries into memory. Use with caution on large databases.
    /// Consider using `get_tips()` or `get_tip_entries()` for more efficient access patterns.
    ///
    /// # Returns
    /// A `Result` containing a vector of all `Entry` objects in the database
    pub async fn get_all_entries(&self) -> Result<Vec<Entry>> {
        let instance = self.instance()?;
        instance.backend().get_tree(&self.root).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth::crypto::generate_keypair, backend::database::InMemory};

    #[tokio::test]
    async fn test_find_sigkeys_returns_sorted_by_permission() -> Result<()> {
        // Create instance
        let instance = Instance::open(Box::new(InMemory::new())).await?;

        // Generate a test key
        let (signing_key, public_key) = generate_keypair();
        let pubkey_str = format_public_key(&public_key);

        // Create initial settings with multiple keys having different permissions
        let mut settings = Doc::new();
        settings.set("name", "test_db");

        let mut auth_settings = AuthSettings::new();

        // Add keys with varying permissions (intentionally out of order)
        auth_settings.add_key(
            "key_write",
            AuthKey::active(&pubkey_str, Permission::Write(10))?,
        )?;
        auth_settings.add_key(
            "key_admin",
            AuthKey::active(&pubkey_str, Permission::Admin(5))?,
        )?;
        auth_settings.add_key("key_read", AuthKey::active(&pubkey_str, Permission::Read)?)?;
        auth_settings.add_key(
            "key_write_high",
            AuthKey::active(&pubkey_str, Permission::Write(2))?,
        )?;

        settings.set("auth", auth_settings.as_doc().clone());

        // Create database
        let db =
            Database::create(settings, &instance, signing_key, "key_admin".to_string()).await?;

        // Call find_sigkeys
        let results = Database::find_sigkeys(&instance, db.root_id(), &pubkey_str).await?;

        // Verify we got all 4 keys
        assert_eq!(results.len(), 4, "Should find all 4 keys");

        // Verify they're sorted by permission, highest first
        // Admin(5) > Write(2) > Write(10) > Read
        assert_eq!(
            results[0].1,
            Permission::Admin(5),
            "First should be Admin(5)"
        );
        assert_eq!(
            results[1].1,
            Permission::Write(2),
            "Second should be Write(2)"
        );
        assert_eq!(
            results[2].1,
            Permission::Write(10),
            "Third should be Write(10)"
        );
        assert_eq!(results[3].1, Permission::Read, "Fourth should be Read");

        // Verify the SigKey names match the permissions
        assert_eq!(results[0].0, SigKey::Direct("key_admin".to_string()));
        assert_eq!(results[1].0, SigKey::Direct("key_write_high".to_string()));
        assert_eq!(results[2].0, SigKey::Direct("key_write".to_string()));
        assert_eq!(results[3].0, SigKey::Direct("key_read".to_string()));

        Ok(())
    }
}
