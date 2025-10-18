//! Database module provides functionality for managing collections of related entries.
//!
//! A `Database` represents a hierarchical structure of entries, like a traditional database
//! or a branch in a version control system. Each database has a root entry and maintains
//! the history and relationships between entries, interfacing with a backend storage system.

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use rand::{Rng, RngCore, distributions::Alphanumeric};
use serde_json;

use crate::{
    Result, Transaction,
    auth::{
        crypto::format_public_key,
        settings::AuthSettings,
        types::{AuthKey, Permission},
    },
    backend::BackendDB,
    constants::{ROOT, SETTINGS},
    crdt::{Doc, doc::Value},
    entry::{Entry, ID},
    instance::errors::InstanceError,
    store::{SettingsStore, Store},
    sync::hooks::SyncHookCollection,
};

/// Specifies where a Database gets its signing keys
#[derive(Clone)]
pub enum KeySource {
    /// Look up private key from backend storage using this key name
    /// The key name is also used as the SigKey identifier in auth settings
    BackendLookup(String),

    /// Use the provided signing key with specified SigKey identifier
    /// The signing key is already decrypted and ready to use (from UserKeyManager)
    /// The sigkey is the identifier used in the database's auth settings
    Provided {
        signing_key: Box<SigningKey>,
        sigkey: String,
    },
}

/// Represents a collection of related entries, like a traditional database or a branch in a version control system.
///
/// Each `Database` is identified by the ID of its root `Entry` and manages the history of data
/// associated with that root. It interacts with the underlying `Backend` for storage.
#[derive(Clone)]
pub struct Database {
    root: ID,
    backend: Arc<dyn BackendDB>,
    /// Key source for operations on this database
    key_source: Option<KeySource>,
    /// Optional sync hooks to execute after successful commits
    sync_hooks: Option<Arc<SyncHookCollection>>,
}

impl Database {
    /// Creates a new `Database` instance with a user-provided signing key.
    ///
    /// This constructor creates a new database using a signing key that's already in memory
    /// (e.g., from UserKeyManager), without requiring the key to be stored in the backend.
    /// This is the preferred method for creating databases in a User context where keys
    /// are managed separately from the backend.
    ///
    /// The created database will use `KeySource::Provided` for all subsequent operations,
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
    /// * `backend` - Backend storage reference where database entries will be stored
    /// * `signing_key` - The signing key to use for the initial commit and subsequent operations.
    ///   This key should already be decrypted and ready to use.
    /// * `sigkey` - The SigKey identifier to use in the database's auth settings.
    ///   This is typically the public key string but can be any identifier.
    ///
    /// # Returns
    /// A `Result` containing the new `Database` instance configured with `KeySource::Provided`.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::backend::database::InMemory;
    /// # use eidetica::auth::crypto::{generate_keypair, format_public_key};
    /// # use eidetica::crdt::Doc;
    /// # use std::sync::Arc;
    /// # fn example() -> Result<()> {
    /// let backend = Arc::new(InMemory::new());
    /// let (signing_key, public_key) = generate_keypair();
    /// let sigkey = format_public_key(&public_key);
    ///
    /// let mut settings = Doc::new();
    /// settings.set_string("name", "my_database");
    ///
    /// // Create database with user-managed key (no backend storage needed)
    /// let database = Database::create(
    ///     settings,
    ///     backend,
    ///     signing_key,
    ///     sigkey,
    /// )?;
    ///
    /// // All transactions automatically use the provided key
    /// let tx = database.new_transaction()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn create(
        initial_settings: Doc,
        backend: Arc<dyn BackendDB>,
        signing_key: SigningKey,
        sigkey: String,
    ) -> Result<Self> {
        // Check if auth is configured in the initial settings
        let auth_configured = matches!(initial_settings.get("auth"), Some(Value::Doc(auth_map)) if !auth_map.as_hashmap().is_empty());

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
            final_database_settings.set_doc("auth", auth_settings_handler.as_doc().clone());

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

        // Create temporary database for bootstrap with KeySource::Provided
        // This allows the bootstrap transaction to use the provided key directly
        let temp_database_for_bootstrap = Database {
            root: bootstrap_placeholder_id.clone().into(),
            backend: backend.clone(),
            key_source: Some(KeySource::Provided {
                signing_key: Box::new(signing_key.clone()),
                sigkey: sigkey.clone(),
            }),
            sync_hooks: None,
        };

        // Create the transaction - it will use the provided key automatically
        let op = temp_database_for_bootstrap.new_transaction()?;

        // IMPORTANT: For the root entry, we need to set the database root to empty string
        // so that is_root() returns true and all_roots() can find it
        op.set_entry_root("")?;

        // Populate the SETTINGS and ROOT subtrees for the very first entry
        op.update_subtree(SETTINGS, &serde_json::to_string(&final_database_settings)?)?;
        op.update_subtree(ROOT, &serde_json::to_string(&"".to_string())?)?; // Standard practice for root entry's _root

        // Add entropy to the entry metadata to ensure unique database IDs even with identical settings
        op.set_metadata_entropy(rand::thread_rng().next_u64())?;

        // Commit the initial entry
        let new_root_id = op.commit()?;

        // Now create the real database with the new_root_id and KeySource::Provided
        Ok(Self {
            root: new_root_id,
            backend,
            key_source: Some(KeySource::Provided {
                signing_key: Box::new(signing_key),
                sigkey,
            }),
            sync_hooks: None,
        })
    }

    /// Creates a new `Database` instance from an existing ID without authentication.
    ///
    /// This constructor takes an existing `ID` and an `Arc<dyn Backend>`
    /// and constructs a `Database` instance with the specified root ID.
    /// The resulting database has no key source set, so operations will fail
    /// at commit time without authentication.
    ///
    /// This is useful for read-only access or testing scenarios.
    /// For normal use with the User API, use `Database::open()` instead.
    ///
    /// # Arguments
    /// * `id` - The `ID` of the root entry.
    /// * `backend` - An `Arc<dyn Backend>` reference to the backend where the database's entries will be stored.
    ///
    /// # Returns
    /// A `Result` containing the new `Database` instance or an error.
    pub fn open_readonly(id: ID, backend: Arc<dyn BackendDB>) -> Result<Self> {
        Ok(Self {
            root: id,
            backend,
            key_source: None,
            sync_hooks: None,
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
    /// - Uses `KeySource::Provided` for all subsequent operations
    /// - No backend key storage needed
    ///
    /// Note: To **create** a new database with user-managed keys, use `create()`.
    /// This method is for **opening existing** databases.
    ///
    /// # Arguments
    /// * `backend` - Backend storage reference
    /// * `root_id` - The root entry ID of the existing database to open
    /// * `signing_key` - Decrypted signing key from UserKeyManager
    /// * `sigkey` - SigKey identifier used in database auth settings
    ///
    /// # Returns
    /// A `Result` containing the `Database` instance configured with `KeySource::Provided`
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::backend::database::InMemory;
    /// # use eidetica::auth::crypto::generate_keypair;
    /// # use std::sync::Arc;
    /// # fn example() -> Result<()> {
    /// # let backend = Arc::new(InMemory::new());
    /// # let (signing_key, _) = generate_keypair();
    /// # let root_id = "existing_database_root_id".into();
    /// // Open existing database with user-managed key
    /// let database = Database::open(
    ///     backend,
    ///     &root_id,
    ///     signing_key,
    ///     "my_sigkey".to_string(),
    /// )?;
    ///
    /// // All transactions automatically use the provided key
    /// let tx = database.new_transaction()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn open(
        backend: Arc<dyn BackendDB>,
        root_id: &ID,
        signing_key: SigningKey,
        sigkey: String,
    ) -> Result<Self> {
        Ok(Self {
            root: root_id.clone(),
            backend,
            key_source: Some(KeySource::Provided {
                signing_key: Box::new(signing_key),
                sigkey,
            }),
            sync_hooks: None,
        })
    }

    /// Get the default authentication key ID for this database.
    pub fn default_auth_key(&self) -> Option<&str> {
        match &self.key_source {
            Some(KeySource::BackendLookup(key_name)) => Some(key_name.as_str()),
            Some(KeySource::Provided { sigkey, .. }) => Some(sigkey.as_str()),
            None => None,
        }
    }

    /// Set sync hooks for this database.
    ///
    /// When sync hooks are set, all operations created via `new_transaction()` and
    /// `new_transaction_with_tips()` will automatically include these hooks and execute
    /// them after successful commits.
    ///
    /// # Arguments
    /// * `hooks` - The sync hook collection to use for operations on this database
    pub fn set_sync_hooks(&mut self, hooks: Arc<SyncHookCollection>) {
        self.sync_hooks = Some(hooks);
    }

    /// Clear sync hooks for this database.
    pub fn clear_sync_hooks(&mut self) {
        self.sync_hooks = None;
    }

    /// Get the sync hooks for this database.
    pub fn sync_hooks(&self) -> Option<&Arc<SyncHookCollection>> {
        self.sync_hooks.as_ref()
    }

    /// Get the ID of the root entry
    pub fn root_id(&self) -> &ID {
        &self.root
    }

    /// Get a reference to the backend
    pub fn backend(&self) -> &Arc<dyn BackendDB> {
        &self.backend
    }

    /// Retrieve the root entry from the backend
    pub fn get_root(&self) -> Result<Entry> {
        self.backend.get(&self.root)
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
    /// # let database: Database = unimplemented!();
    /// // Read-only access
    /// let settings = database.get_settings()?;
    /// let name = settings.get_name()?;
    ///
    /// // For modifications, use a transaction:
    /// let txn = database.new_transaction()?;
    /// let settings = txn.get_settings()?;
    /// settings.set_name("new_name")?;
    /// txn.commit()?;
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn get_settings(&self) -> Result<SettingsStore> {
        let txn = self.new_transaction()?;
        txn.get_settings()
    }

    /// Get the name of the database from its settings store
    pub fn get_name(&self) -> Result<String> {
        let settings = self.get_settings()?;
        settings.get_name()
    }

    /// Create a new atomic transaction on this database
    ///
    /// This creates a new atomic transaction containing a new Entry.
    /// The atomic transaction will be initialized with the current state of the database.
    /// If a default authentication key is set, the transaction will use it for signing.
    ///
    /// # Returns
    /// A `Result<Transaction>` containing the new atomic transaction
    pub fn new_transaction(&self) -> Result<Transaction> {
        let tips = self.get_tips()?;
        self.new_transaction_with_tips(&tips)
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
    pub fn new_transaction_with_tips(&self, tips: impl AsRef<[ID]>) -> Result<Transaction> {
        let mut op = Transaction::new_with_tips(self, tips.as_ref())?;

        // Set provided signing key (all databases use KeySource::Provided now)
        if let Some(KeySource::Provided {
            signing_key,
            sigkey,
        }) = &self.key_source
        {
            op.set_provided_key(*signing_key.clone(), sigkey.clone());
        }

        // Set sync hooks if configured
        if let Some(ref hooks) = self.sync_hooks {
            op.set_sync_hooks(hooks.clone());
        }

        Ok(op)
    }

    /// Insert an entry into the database without modifying it.
    /// This is primarily for testing purposes or when you need full control over the entry.
    /// Note: Since all entries must now be authenticated, this method assumes the entry
    /// is already properly signed and verified.
    pub fn insert_raw(&self, entry: Entry) -> Result<ID> {
        let id = entry.id();

        self.backend.put_verified(entry)?;

        Ok(id)
    }

    /// Get a Store type that will handle accesses to the Store
    /// This will return a Store initialized to point at the current state of the database.
    ///
    /// The returned store should NOT be used to modify the database, as it intentionally does not
    /// expose the Transaction. Since the Transaction is never committed, it does not have any
    /// effect on the database.
    pub fn get_store_viewer<T>(&self, name: impl Into<String>) -> Result<T>
    where
        T: Store,
    {
        let op = self.new_transaction()?;
        T::new(&op, name)
    }

    /// Get the current tips (leaf entries) of the main database branch.
    ///
    /// Tips represent the latest entries in the database's main history, forming the heads of the DAG.
    ///
    /// # Returns
    /// A `Result` containing a vector of `ID`s for the tip entries or an error.
    pub fn get_tips(&self) -> Result<Vec<ID>> {
        self.backend.get_tips(&self.root)
    }

    /// Get the full `Entry` objects for the current tips of the main database branch.
    ///
    /// # Returns
    /// A `Result` containing a vector of the tip `Entry` objects or an error.
    pub fn get_tip_entries(&self) -> Result<Vec<Entry>> {
        let tips = self.backend.get_tips(&self.root)?;
        let entries: Result<Vec<_>> = tips.iter().map(|id| self.backend.get(id)).collect();
        entries
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
    /// # fn main() -> Result<()> {
    /// # let backend = Box::new(InMemory::new());
    /// # let db = Instance::open(backend)?;
    /// # db.add_private_key("TEST_KEY")?;
    /// # let tree = db.new_database(Doc::new(), "TEST_KEY")?;
    /// # let op = tree.new_transaction()?;
    /// let entry_id = op.commit()?;
    /// let entry = tree.get_entry(&entry_id)?;           // Using &String
    /// let entry = tree.get_entry("some_entry_id")?;     // Using &str
    /// let entry = tree.get_entry(entry_id.clone())?;    // Using String
    /// println!("Entry signature: {:?}", entry.sig);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_entry<I: Into<ID>>(&self, entry_id: I) -> Result<Entry> {
        let id = entry_id.into();
        let entry = self.backend.get(&id)?;

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
    /// # fn main() -> Result<()> {
    /// # let backend = Box::new(InMemory::new());
    /// # let db = Instance::open(backend)?;
    /// # db.add_private_key("TEST_KEY")?;
    /// # let tree = db.new_database(Doc::new(), "TEST_KEY")?;
    /// let entry_ids = vec!["id1", "id2", "id3"];
    /// let entries = tree.get_entries(entry_ids)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_entries<I, T>(&self, entry_ids: I) -> Result<Vec<Entry>>
    where
        I: IntoIterator<Item = T>,
        T: Into<ID>,
    {
        // Collect IDs first to minimize conversions and avoid repeat work in iterator chain
        let ids: Vec<ID> = entry_ids.into_iter().map(Into::into).collect();
        let mut entries = Vec::with_capacity(ids.len());

        for id in ids {
            let entry = self.backend.get(&id)?;

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
    pub fn verify_entry_signature<I: Into<ID>>(&self, entry_id: I) -> Result<bool> {
        let entry = self.get_entry(entry_id)?;

        // If the entry has no authentication, it's considered valid for backward compatibility
        if entry.sig.key == crate::auth::types::SigKey::default() {
            return Ok(true);
        }

        // Get the authentication settings that were valid at the time this entry was created
        let historical_settings = self.get_historical_settings_for_entry(&entry)?;

        // Use the authentication validator with historical settings
        let mut validator = crate::auth::validation::AuthValidator::new();
        validator.validate_entry(&entry, &historical_settings, Some(&self.backend))
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
    /// # use std::sync::Arc;
    /// # fn example() -> Result<()> {
    /// # let backend = Arc::new(InMemory::new());
    /// # let (signing_key, _) = generate_keypair();
    /// # let database = Database::create(
    /// #     eidetica::crdt::Doc::new(),
    /// #     backend,
    /// #     signing_key,
    /// #     "my_key".to_string(),
    /// # )?;
    /// // Check if a key has Admin permission
    /// let permission = database.get_sigkey_permission("my_key")?;
    /// if permission.can_admin() {
    ///     println!("Key has Admin permission!");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_sigkey_permission(&self, sigkey: &str) -> Result<Permission> {
        // Get database settings
        let settings_store = self.get_settings()?;

        // Get auth settings from the settings store
        let auth_settings = settings_store.get_auth_settings()?;

        // Create SigKey and validate entry auth to get effective permission
        let sig_key = crate::auth::types::SigKey::Direct(sigkey.to_string());
        let resolved_auth = auth_settings.validate_entry_auth(&sig_key, Some(&self.backend))?;

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
    fn get_historical_settings_for_entry(&self, _entry: &Entry) -> Result<AuthSettings> {
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

        let settings = self.get_settings()?;
        settings.get_auth_settings()
    }

    // === DATABASE QUERIES ===

    /// Get all entries in this database.
    ///
    /// ⚠️ **Warning**: This method loads all entries into memory. Use with caution on large databases.
    /// Consider using `get_tips()` or `get_tip_entries()` for more efficient access patterns.
    ///
    /// # Returns
    /// A `Result` containing a vector of all `Entry` objects in the database
    pub fn get_all_entries(&self) -> Result<Vec<Entry>> {
        self.backend.get_tree(&self.root)
    }
}
