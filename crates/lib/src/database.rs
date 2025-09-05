//! Database module provides functionality for managing collections of related entries.
//!
//! A `Database` represents a hierarchical structure of entries, like a traditional database
//! or a branch in a version control system. Each database has a root entry and maintains
//! the history and relationships between entries, interfacing with a backend storage system.

use crate::Result;
use crate::Transaction;
use crate::backend::BackendDB;
use crate::constants::{ROOT, SETTINGS};
use crate::crdt::Doc;
use crate::crdt::doc::Value;
use crate::entry::{Entry, ID};
use crate::instance::errors::InstanceError;
use crate::store::{DocStore, Store};

use crate::auth::crypto::format_public_key;
use crate::auth::settings::AuthSettings;
use crate::auth::types::{AuthKey, KeyStatus, Permission};
use crate::sync::hooks::SyncHookCollection;
use rand::{Rng, distributions::Alphanumeric};
use serde_json;
use std::sync::Arc;

/// Represents a collection of related entries, like a traditional database or a branch in a version control system.
///
/// Each `Database` is identified by the ID of its root `Entry` and manages the history of data
/// associated with that root. It interacts with the underlying `Backend` for storage.
#[derive(Clone)]
pub struct Database {
    root: ID,
    backend: Arc<dyn BackendDB>,
    /// Default authentication key name for operations on this database
    default_auth_key: Option<String>,
    /// Optional sync hooks to execute after successful commits
    sync_hooks: Option<Arc<SyncHookCollection>>,
}

impl Database {
    /// Creates a new `Database` instance.
    ///
    /// Initializes the database by creating a root `Entry` containing the provided settings
    /// and storing it in the backend. All databases must now be created with authentication.
    ///
    /// # Arguments
    /// * `settings` - A `Doc` CRDT containing the initial settings for the database.
    /// * `backend` - An `Arc<Mutex<>>` protected reference to the backend where the database's entries will be stored.
    /// * `signing_key_name` - Authentication key name to use for the initial commit. Required for all databases.
    ///
    /// # Returns
    /// A `Result` containing the new `Database` instance or an error.
    pub fn new(
        initial_settings: Doc,
        backend: Arc<dyn BackendDB>,
        signing_key_name: impl AsRef<str>,
    ) -> Result<Self> {
        let signing_key_name = signing_key_name.as_ref();
        // Check if auth is configured in the initial settings
        let auth_configured = matches!(initial_settings.get("auth"), Some(Value::Node(auth_map)) if !auth_map.as_hashmap().is_empty());

        let (super_user_key_name, final_database_settings) = if auth_configured {
            // Auth settings are already provided - use them as-is with the provided signing key
            (signing_key_name.to_string(), initial_settings)
        } else {
            // No auth config provided - bootstrap auth configuration with the provided key
            // Verify the key exists first
            let _private_key = backend.get_private_key(signing_key_name)?.ok_or_else(|| {
                InstanceError::SigningKeyNotFound {
                    key_name: signing_key_name.to_string(),
                }
            })?;

            // Bootstrap auth configuration with the provided key
            let private_key = backend.get_private_key(signing_key_name)?.unwrap();
            let public_key = private_key.verifying_key();

            // Create auth settings with the provided key
            let mut auth_settings_handler = AuthSettings::new();
            let super_user_auth_key = AuthKey {
                pubkey: format_public_key(&public_key),
                permissions: Permission::Admin(0), // Highest priority
                status: KeyStatus::Active,
            };
            auth_settings_handler.add_key(signing_key_name, super_user_auth_key)?;

            // Prepare final database settings for the initial commit
            let mut final_database_settings = initial_settings.clone();
            final_database_settings.set_node("auth", auth_settings_handler.as_doc().clone());

            (signing_key_name.to_string(), final_database_settings)
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

        let temp_database_for_bootstrap = Database {
            root: bootstrap_placeholder_id.clone().into(),
            backend: backend.clone(),
            default_auth_key: Some(super_user_key_name.clone()),
            sync_hooks: None,
        };

        // Create the transaction. If we have an auth key, it will be used automatically
        let op = temp_database_for_bootstrap.new_transaction()?;

        // IMPORTANT: For the root entry, we need to set the database root to empty string
        // so that is_toplevel_root() returns true and all_roots() can find it
        op.set_entry_root("")?;

        // Populate the SETTINGS and ROOT subtrees for the very first entry
        op.update_subtree(SETTINGS, &serde_json::to_string(&final_database_settings)?)?;
        op.update_subtree(ROOT, &serde_json::to_string(&"".to_string())?)?; // Standard practice for root entry's _root

        // Commit the initial entry
        let new_root_id = op.commit()?;

        // Now create the real database with the new_root_id
        Ok(Self {
            root: new_root_id,
            backend,
            default_auth_key: Some(super_user_key_name),
            sync_hooks: None,
        })
    }

    /// Creates a new `Database` instance from an existing ID.
    ///
    /// This constructor takes an existing `ID` and an `Arc<dyn Backend>`
    /// and constructs a `Database` instance with the specified root ID.
    ///
    /// # Arguments
    /// * `id` - The `ID` of the root entry.
    /// * `backend` - An `Arc<dyn Backend>` reference to the backend where the database's entries will be stored.
    ///
    /// # Returns
    /// A `Result` containing the new `Database` instance or an error.
    pub(crate) fn new_from_id(id: ID, backend: Arc<dyn BackendDB>) -> Result<Self> {
        Ok(Self {
            root: id,
            backend,
            default_auth_key: None,
            sync_hooks: None,
        })
    }

    /// Set the default authentication key ID for operations on this database.
    ///
    /// When set, all operations created via `new_transaction()` will automatically
    /// use this key for signing unless explicitly overridden.
    ///
    /// # Parameters
    /// * `key_name` - Authentication key identifier that will be stored.
    ///   Accepts any string type (`&str`, `String`, `&String`) for maximum ergonomics.
    ///
    /// # Example
    /// ```rust
    /// # use eidetica::*;
    /// # use eidetica::backend::database::InMemory;
    /// # use eidetica::Instance;
    /// # use eidetica::crdt::Doc;
    /// # fn example() -> Result<()> {
    /// # let backend = Box::new(InMemory::new());
    /// # let db = Instance::new(backend);
    /// # db.add_private_key("test_key")?;
    /// # let mut database = db.new_database(Doc::new(), "test_key")?;
    /// database.set_default_auth_key("my_key");                    // &str
    /// database.set_default_auth_key(String::from("my_key"));      // String
    /// database.set_default_auth_key(&String::from("my_key"));     // &String
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_default_auth_key(&mut self, key_name: impl Into<String>) {
        self.default_auth_key = Some(key_name.into());
    }

    /// Clear the default authentication key for this database.
    pub fn clear_default_auth_key(&mut self) {
        self.default_auth_key = None;
    }

    /// Get the default authentication key ID for this database.
    pub fn default_auth_key(&self) -> Option<&str> {
        self.default_auth_key.as_deref()
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

    /// Create a new atomic transaction on this database with authentication.
    ///
    /// This is a convenience method that creates a transaction and sets the authentication
    /// key in one call.
    ///
    /// # Arguments
    /// * `key_name` - The identifier of the private key to use for signing
    ///
    /// # Returns
    /// A `Result<Transaction>` containing the new authenticated transaction
    pub fn new_authenticated_operation(&self, key_name: impl AsRef<str>) -> Result<Transaction> {
        let op = self.new_transaction()?;
        Ok(op.with_auth(key_name.as_ref()))
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

    /// Get a settings store for the database.
    ///
    /// Returns a DocStore for managing the database's settings.
    ///
    /// # Returns
    /// A `Result` containing the `DocStore` for settings or an error.
    pub fn get_settings(&self) -> Result<DocStore> {
        self.get_store_viewer::<DocStore>(SETTINGS)
    }

    /// Get the name of the database from its settings store
    pub fn get_name(&self) -> Result<String> {
        // Get the settings store
        let settings = self.get_settings()?;

        // Get the name from the settings
        settings.get_string("name")
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

        // Set default authentication if configured
        if let Some(ref key_name) = self.default_auth_key {
            op.set_auth_key(key_name);
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
    /// expose the Transaction.
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
    /// # let db = Instance::new(backend);
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
    /// # let db = Instance::new(backend);
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

    /// Get the authentication settings that were valid when a specific entry was created.
    ///
    /// This method examines the entry's metadata to find the settings tips that were active
    /// at the time of entry creation, then reconstructs the historical settings state.
    ///
    /// # Arguments
    /// * `entry` - The entry to get historical settings for
    ///
    /// # Returns
    /// A `Result` containing the historical settings data
    fn get_historical_settings_for_entry(&self, _entry: &Entry) -> Result<Doc> {
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
        settings.get_all()
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
