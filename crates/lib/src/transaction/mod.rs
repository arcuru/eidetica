//! Transaction system for atomic database modifications
//!
//! This module provides the transaction API for making atomic changes to an Eidetica database.
//! Transactions ensure that all changes within a transaction are applied atomically and maintain
//! proper parent-child relationships in the Merkle-CRDT DAG structure.
//!
//! # Subtree Parent Management
//!
//! One of the critical responsibilities of the transaction system is establishing proper
//! subtree parent relationships. When a store (subtree) is accessed for the first time
//! in a transaction, the system must determine the correct parent entries for that subtree.
//! This involves:
//!
//! 1. Checking for existing subtree tips (leaf nodes)
//! 2. If no tips exist, traversing the DAG to find reachable subtree entries
//! 3. Setting appropriate parent relationships (empty for first entry, or proper parents)

pub mod errors;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use base64ct::{Base64, Encoding};
pub use errors::TransactionError;
use serde::{Deserialize, Serialize};

use crate::{
    Database, Result, Store,
    auth::{
        AuthSettings,
        crypto::{format_public_key, sign_entry},
        types::{Operation, SigInfo, SigKey},
        validation::AuthValidator,
    },
    constants::{INDEX, ROOT, SETTINGS},
    crdt::{CRDT, Doc, doc::Value},
    entry::{Entry, EntryBuilder, ID},
    height::HeightStrategy,
    store::{Registry, SettingsStore, StoreError},
};

/// Trait for encrypting/decrypting subtree data transparently
///
/// Encryptors are registered with a Transaction for specific subtrees, allowing
/// transparent encryption/decryption at the transaction boundary. When an encryptor
/// is registered:
///
/// - `get_full_state()` decrypts each historical entry before CRDT merging
/// - `get_local_data()` returns plaintext (cached in EntryBuilder)
/// - `update_subtree()` stores plaintext in cache, encrypted on commit
///
/// This ensures proper CRDT merge semantics while keeping data encrypted at rest.
///
/// # Wire Format
///
/// The trait operates on raw bytes, allowing implementations to define their own
/// wire format. For example, AES-GCM implementations typically use `nonce || ciphertext`.
/// The Transaction handles base64 encoding/decoding for storage.
///
/// # Example
///
/// ```rust,ignore
/// struct PasswordEncryptor { /* ... */ }
///
/// impl Encryptor for PasswordEncryptor {
///     fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>> {
///         let (nonce, ct) = ciphertext.split_at(12);
///         // decrypt with nonce and ciphertext...
///     }
///
///     fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
///         let nonce = generate_nonce();
///         let ct = encrypt(plaintext, &nonce);
///         // return nonce || ciphertext
///     }
/// }
/// ```
pub(crate) trait Encryptor: Send + Sync {
    /// Decrypt ciphertext bytes to plaintext bytes
    ///
    /// # Arguments
    /// * `ciphertext` - Encrypted data in implementation-defined format
    ///
    /// # Returns
    /// Plaintext bytes (typically UTF-8 encoded JSON for CRDT data)
    fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>>;

    /// Encrypt plaintext bytes to ciphertext bytes
    ///
    /// # Arguments
    /// * `plaintext` - Data to encrypt (typically UTF-8 encoded JSON for CRDT data)
    ///
    /// # Returns
    /// Encrypted data in implementation-defined format
    fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>>;
}

/// Metadata structure for entries
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EntryMetadata {
    /// Tips of the _settings subtree at the time this entry was created
    /// This is used for improving sync performance and for validation in sparse checkouts.
    settings_tips: Vec<ID>,
    /// Random entropy for ensuring unique IDs for root entries
    entropy: Option<u64>,
}

/// Represents a single, atomic transaction for modifying a `Database`.
///
/// An `Transaction` encapsulates a mutable `EntryBuilder` being constructed. Users interact with
/// specific `Store` instances obtained via `Transaction::get_store` to stage changes.
/// All staged changes across different subtrees within the transaction are recorded
/// in the internal `EntryBuilder`.
///
/// When `commit()` is called, the transaction:
/// 1. Finalizes the `EntryBuilder` by building an immutable `Entry`
/// 2. Calculates the entry's content-addressable ID
/// 3. Ensures the correct parent links are set based on the tree's state
/// 4. Removes any empty subtrees that didn't have data staged
/// 5. Signs the entry if authentication is configured
/// 6. Persists the resulting immutable `Entry` to the backend
///
/// `Transaction` instances are typically created via `Database::new_transaction()`.
#[derive(Clone)]
pub struct Transaction {
    /// The entry builder being modified, wrapped in Option to support consuming on commit
    entry_builder: Arc<Mutex<Option<EntryBuilder>>>,
    /// The database this transaction belongs to
    db: Database,
    /// Optional authentication key name for backend lookup
    auth_key_name: Option<String>,
    /// Optional provided signing key when key is already decrypted
    /// Tuple contains (SigningKey, SigKey identifier)
    provided_signing_key: Option<(ed25519_dalek::SigningKey, String)>,
    /// Registered encryptors for transparent encryption/decryption of specific subtrees
    /// Maps subtree name -> encryptor implementation
    /// When an encryptor is registered, the transaction automatically encrypts writes
    /// and decrypts reads for that subtree
    encryptors: Arc<Mutex<HashMap<String, Box<dyn Encryptor>>>>,
}

impl Transaction {
    /// Creates a new atomic transaction for a specific `Database` with custom parent tips.
    ///
    /// Initializes an internal `EntryBuilder` with its main parent pointers set to the
    /// specified tips instead of the current database tips. This allows creating
    /// transactions that branch from specific points in the database history.
    ///
    /// This enables creating diamond patterns and other complex DAG structures
    /// for testing and advanced use cases.
    ///
    /// # Arguments
    /// * `database` - The `Database` this transaction will modify.
    /// * `tips` - The specific parent tips to use for this transaction. Must contain at least one tip.
    ///
    /// # Returns
    /// A `Result<Self>` containing the new transaction or an error if tips are empty or invalid.
    pub(crate) async fn new_with_tips(database: &Database, tips: &[ID]) -> Result<Self> {
        // Validate that tips are not empty, unless we're creating the root entry
        if tips.is_empty() {
            // Check if this is a root entry creation by seeing if the database root exists in backend
            let root_exists = database.backend()?.get(database.root_id()).await.is_ok();

            if root_exists {
                return Err(TransactionError::EmptyTipsNotAllowed.into());
            }
            // If root doesn't exist, this is valid (creating the root entry)
        }

        // Validate that all tips belong to the same tree
        let backend = database.backend()?;
        for tip_id in tips {
            let entry = backend.get(tip_id).await?;
            if !entry.in_tree(database.root_id()) {
                return Err(TransactionError::InvalidTip {
                    tip_id: tip_id.to_string(),
                }
                .into());
            }
        }

        // Start with a basic entry linked to the database's root.
        // Data and parents will be filled based on the transaction type.
        let mut builder = Entry::builder(database.root_id().clone());

        // Use the provided tips as parents (only if not empty)
        if !tips.is_empty() {
            builder.set_parents_mut(tips.to_vec());
        }

        Ok(Self {
            entry_builder: Arc::new(Mutex::new(Some(builder))),
            db: database.clone(),
            auth_key_name: None,
            provided_signing_key: None,
            encryptors: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Set the authentication key ID for signing entries created by this transaction.
    ///
    /// If set, the transaction will attempt to sign the entry with the specified
    /// private key during commit. The private key must be available in the backend's
    /// local key storage.
    ///
    /// # Arguments
    /// * `key_name` - The identifier of the private key to use for signing
    ///
    /// # Returns
    /// Self for method chaining
    pub fn with_auth(mut self, key_name: impl Into<String>) -> Self {
        self.auth_key_name = Some(key_name.into());
        self
    }

    /// Set the authentication key ID for this transaction (mutable version).
    ///
    /// # Arguments
    /// * `key_name` - The identifier of the private key to use for signing
    pub fn set_auth_key(&mut self, key_name: impl Into<String>) {
        self.auth_key_name = Some(key_name.into());
    }

    /// Set signing key directly for user context (internal API).
    ///
    /// This method is used when a Database is created with a user-provided key
    /// (via `Database::open()`). The provided SigningKey is already
    /// decrypted and ready to use, eliminating the need for backend key lookup.
    ///
    /// # Arguments
    /// * `signing_key` - The decrypted signing key from UserKeyManager
    /// * `sigkey` - The SigKey identifier used in database auth settings
    pub(crate) fn set_provided_key(
        &mut self,
        signing_key: ed25519_dalek::SigningKey,
        sigkey: String,
    ) {
        self.auth_key_name = Some(sigkey.clone());
        self.provided_signing_key = Some((signing_key, sigkey));
    }

    /// Get the current authentication key ID for this transaction.
    pub fn auth_key_name(&self) -> Option<&str> {
        self.auth_key_name.as_deref()
    }

    /// Get current time as RFC3339 string.
    ///
    /// Delegates to the underlying instance's clock.
    pub(crate) fn now_rfc3339(&self) -> Result<String> {
        Ok(self.db.instance()?.clock().now_rfc3339())
    }

    /// Register an encryptor for transparent encryption/decryption of a specific subtree.
    ///
    /// Once registered, the transaction will automatically:
    /// - Decrypt each historical entry before CRDT merging in `get_full_state()`
    /// - Return plaintext data from `get_local_data()` (cached in EntryBuilder)
    /// - Encrypt plaintext data before persisting in `commit()`
    ///
    /// This ensures proper CRDT merge semantics while keeping data encrypted at rest.
    ///
    /// # Arguments
    /// * `subtree` - The name of the subtree to encrypt/decrypt
    /// * `encryptor` - The encryptor implementation to use
    ///
    /// # Example
    ///
    /// For password-based encryption, use [`PasswordStore`] which handles
    /// encryptor registration automatically:
    ///
    /// ```rust,ignore
    /// let mut encrypted = tx.get_store::<PasswordStore>("secrets")?;
    /// encrypted.initialize("my_password", "docstore:v0", "{}")?;
    ///
    /// // PasswordStore registers the encryptor internally
    /// let docstore = encrypted.unwrap::<DocStore>()?;
    /// ```
    ///
    /// For custom encryption, implement the [`Encryptor`] trait:
    ///
    /// ```rust,ignore
    /// struct MyEncryptor { /* ... */ }
    /// impl Encryptor for MyEncryptor {
    ///     fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> { /* ... */ }
    ///     fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>> { /* ... */ }
    /// }
    ///
    /// transaction.register_encryptor("secrets", Box::new(MyEncryptor::new()))?;
    /// ```
    ///
    /// [`PasswordStore`]: crate::store::PasswordStore
    /// [`Encryptor`]: crate::Encryptor
    pub(crate) fn register_encryptor(
        &self,
        subtree: impl Into<String>,
        encryptor: Box<dyn Encryptor>,
    ) -> Result<()> {
        self.encryptors
            .lock()
            .unwrap()
            .insert(subtree.into(), encryptor);
        Ok(())
    }

    /// Decrypt data if an encryptor is registered, otherwise return as-is.
    ///
    /// This is used throughout Transaction to transparently decrypt encrypted data
    /// before deserializing into CRDT types. Encrypted data is stored as base64-encoded
    /// bytes in the entry.
    fn decrypt_if_needed(&self, subtree: &str, data: &str) -> Result<String> {
        if let Some(encryptor) = self.encryptors.lock().unwrap().get(subtree) {
            // Decode base64 to get ciphertext bytes
            let ciphertext =
                Base64::decode_vec(data).map_err(|e| StoreError::DeserializationFailed {
                    store: subtree.to_string(),
                    reason: format!("Failed to decode base64: {e}"),
                })?;
            // Decrypt the data
            let plaintext_bytes = encryptor.decrypt(&ciphertext)?;
            // Convert to UTF-8 string
            String::from_utf8(plaintext_bytes).map_err(|e| {
                StoreError::DeserializationFailed {
                    store: subtree.to_string(),
                    reason: format!("Invalid UTF-8 in decrypted data: {e}"),
                }
                .into()
            })
        } else {
            // No encryptor, return as-is
            Ok(data.to_string())
        }
    }

    /// Encrypts data if an encryptor is registered for the subtree.
    /// Returns the original data unchanged if no encryptor is registered.
    fn encrypt_if_needed(&self, subtree: &str, plaintext: &str) -> Result<String> {
        if let Some(encryptor) = self.encryptors.lock().unwrap().get(subtree) {
            // Encrypt the data
            let ciphertext = encryptor.encrypt(plaintext.as_bytes())?;
            // Encode as base64 for storage
            Ok(Base64::encode_string(&ciphertext))
        } else {
            // No encryptor, return as-is
            Ok(plaintext.to_string())
        }
    }

    /// Get a SettingsStore handle for the settings subtree within this transaction.
    ///
    /// This method returns a `SettingsStore` that provides specialized access to the `_settings` subtree,
    /// allowing you to read and modify settings data within this atomic transaction.
    /// The DocStore automatically merges historical settings from the database with any
    /// staged changes in this transaction.
    ///
    /// # Returns
    ///
    /// Returns a `Result<SettingsStore>` that can be used to:
    /// - Read current settings values (including both historical and staged data)
    /// - Stage new settings changes within this transaction
    /// - Access nested settings structures
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # async fn example(database: Database) -> eidetica::Result<()> {
    /// let op = database.new_transaction().await?;
    /// let settings = op.get_settings()?;
    ///
    /// // Read a setting
    /// if let Ok(name) = settings.get_name().await {
    ///     println!("Database name: {}", name);
    /// }
    ///
    /// // Modify a setting
    /// settings.set_name("Updated Database Name").await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Unable to create the SettingsStore for the settings subtree
    /// - Operation has already been committed
    pub fn get_settings(&self) -> Result<SettingsStore> {
        // Create a SettingsStore for the settings subtree
        SettingsStore::new(self)
    }

    /// Gets a handle to the Index for managing subtree registry and metadata.
    ///
    /// The Index provides access to the `_index` subtree, which stores metadata
    /// about all subtrees in the database including their type identifiers and configurations.
    ///
    /// # Returns
    ///
    /// A `Result<Registry>` containing the handle for managing the index.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Unable to create the Registry for the _index subtree
    /// - Operation has already been committed
    pub async fn get_index(&self) -> Result<Registry> {
        Registry::new(self, INDEX).await
    }

    /// Set the tree root field for the entry being built.
    ///
    /// This is primarily used during tree creation to ensure the root entry
    /// has an empty tree.root field, making it a proper top-level root.
    ///
    /// # Arguments
    /// * `root` - The tree root ID to set (use empty string for top-level roots)
    pub(crate) fn set_entry_root(&self, root: impl Into<String>) -> Result<()> {
        let mut builder_ref = self.entry_builder.lock().unwrap();
        let builder = builder_ref
            .as_mut()
            .ok_or(TransactionError::TransactionAlreadyCommitted)?;
        builder.set_root_mut(root.into());
        Ok(())
    }

    /// Set entropy in the entry metadata.
    ///
    /// This is used during database creation to ensure unique IDs for databases
    /// even when they have identical settings.
    ///
    /// # Arguments
    /// * `entropy` - Random entropy value
    pub(crate) fn set_metadata_entropy(&self, entropy: u64) -> Result<()> {
        let mut builder_ref = self.entry_builder.lock().unwrap();
        let builder = builder_ref
            .as_mut()
            .ok_or(TransactionError::TransactionAlreadyCommitted)?;

        // Parse existing metadata if present, or create new
        let mut metadata = builder
            .metadata()
            .and_then(|m| serde_json::from_str::<EntryMetadata>(m).ok())
            .unwrap_or_else(|| EntryMetadata {
                settings_tips: Vec::new(),
                entropy: None,
            });

        // Set entropy
        metadata.entropy = Some(entropy);

        // Serialize and set metadata
        let metadata_json = serde_json::to_string(&metadata)?;
        builder.set_metadata_mut(metadata_json);

        Ok(())
    }

    /// Stages an update for a specific subtree within this atomic transaction.
    ///
    /// This method is primarily intended for internal use by `Store` implementations
    /// (like `DocStore::set`). It records the serialized `data` for the given `subtree`
    /// name within the transaction's internal `EntryBuilder`.
    ///
    /// If this is the first modification to the named subtree within this transaction,
    /// it also fetches and records the current tips of that subtree from the backend
    /// to set the correct `subtree_parents` for the new entry.
    ///
    /// # Arguments
    /// * `subtree` - The name of the subtree to update.
    /// * `data` - The serialized CRDT data to stage for the subtree.
    ///
    /// # Returns
    /// A `Result<()>` indicating success or an error.
    pub(crate) async fn update_subtree(
        &self,
        subtree: impl AsRef<str>,
        data: impl AsRef<str>,
    ) -> Result<()> {
        let subtree = subtree.as_ref();
        let data = data.as_ref();

        // Check if we need to fetch tips (check without holding borrow across await)
        let needs_tips = {
            let builder_ref = self.entry_builder.lock().unwrap();
            let builder = builder_ref
                .as_ref()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?;
            !builder.subtrees().contains(&subtree.to_string())
        };

        // Fetch tips if needed (no borrow held across this await)
        let tips = if needs_tips {
            let backend = self.db.backend()?;
            // FIXME: we should get the subtree tips while still using the parent pointers
            Some(backend.get_store_tips(self.db.root_id(), subtree).await?)
        } else {
            None
        };

        // Now update the builder
        let mut builder_ref = self.entry_builder.lock().unwrap();
        let builder = builder_ref
            .as_mut()
            .ok_or(TransactionError::TransactionAlreadyCommitted)?;

        builder.set_subtree_data_mut(subtree.to_string(), data.to_string());
        if let Some(tips) = tips {
            builder.set_subtree_parents_mut(subtree, tips);
        }

        Ok(())
    }

    /// Gets a handle to a specific `Store` for modification within this transaction.
    ///
    /// This method creates and returns an instance of the specified `Store` type `T`,
    /// associated with this `Transaction`. The returned `Store` handle can be used to
    /// stage changes (e.g., using `DocStore::set`).
    /// These changes are recorded within this `Transaction`.
    ///
    /// If this is the first time this subtree is accessed within the transaction,
    /// its parent tips will be fetched and stored.
    ///
    /// # Type Parameters
    /// * `T` - The concrete `Store` implementation type to create.
    ///
    /// # Arguments
    /// * `subtree_name` - The name of the subtree to get a modification handle for.
    ///
    /// # Returns
    /// A `Result<T>` containing the `Store` handle.
    pub async fn get_store<T>(&self, subtree_name: impl Into<String> + Send) -> Result<T>
    where
        T: Store + Send,
    {
        let subtree_name = subtree_name.into();

        // Initialize subtree parents before checking _index
        self.init_subtree_parents(&subtree_name).await?;

        // Skip special system subtrees to avoid circular dependencies
        let is_system_subtree =
            subtree_name == INDEX || subtree_name == SETTINGS || subtree_name == ROOT;

        if is_system_subtree {
            // System subtrees don't use _index registration
            return T::new(self, subtree_name).await;
        }

        // Check _index to determine if this is a new or existing subtree
        let index_store = self.get_index().await?;
        if index_store.contains(&subtree_name).await {
            // Type validation for existing subtree
            let subtree_info = index_store.get_entry(&subtree_name).await?;

            if !T::supports_type_id(&subtree_info.type_id) {
                return Err(StoreError::TypeMismatch {
                    store: subtree_name,
                    expected: T::type_id().to_string(),
                    actual: subtree_info.type_id,
                }
                .into());
            }

            // Type supported - create the Store
            T::new(self, subtree_name).await
        } else {
            // New subtree - init registers it in _index
            T::init(self, subtree_name).await
        }
    }

    /// Get the subtree tips reachable from the given main tree entries.
    async fn get_subtree_tips(&self, subtree_name: &str, main_parents: &[ID]) -> Result<Vec<ID>> {
        self.db
            .backend()?
            .get_store_tips_up_to_entries(self.db.root_id(), subtree_name, main_parents)
            .await
    }

    /// Initialize subtree parents if this is the first time accessing this subtree
    /// in this transaction.
    pub(crate) async fn init_subtree_parents(&self, subtree_name: &str) -> Result<()> {
        let main_parents = {
            let builder_ref = self.entry_builder.lock().unwrap();
            let builder = builder_ref
                .as_ref()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?;

            let subtrees = builder.subtrees();
            if subtrees.contains(&subtree_name.to_string()) {
                return Ok(()); // Already initialized
            }
            builder.parents().unwrap_or_default()
        };

        let tips = self.get_subtree_tips(subtree_name, &main_parents).await?;

        let mut builder_ref = self.entry_builder.lock().unwrap();
        let builder = builder_ref
            .as_mut()
            .ok_or(TransactionError::TransactionAlreadyCommitted)?;

        // Initialize the subtree with proper parent relationships
        // set_subtree_parents_mut creates the subtree with data=None if it doesn't exist
        builder.set_subtree_parents_mut(subtree_name, tips);

        Ok(())
    }

    /// Gets the currently staged data for a specific subtree within this transaction.
    ///
    /// This is intended for use by `Store` implementations to retrieve the data
    /// they have staged locally within the `Transaction` before potentially merging
    /// it with historical data.
    ///
    /// # Type Parameters
    /// * `T` - The data type (expected to be a CRDT) to deserialize the staged data into.
    ///
    /// # Arguments
    /// * `subtree_name` - The name of the subtree whose staged data is needed.
    ///
    /// # Returns
    /// A `Result<T>` containing the deserialized staged data. Returns `Ok(T::default())`
    /// if no data has been staged for this subtree in this transaction yet.
    ///
    /// # Behavior
    /// - If the subtree doesn't exist, returns `T::default()`
    /// - If the subtree exists but has empty data (empty string or whitespace), returns `T::default()`
    /// - Otherwise deserializes the JSON data to type `T`
    ///
    /// # Errors
    /// Returns an error if the subtree data exists but cannot be deserialized to type `T`.
    pub fn get_local_data<T>(&self, subtree_name: impl AsRef<str>) -> Result<T>
    where
        T: crate::crdt::Data + Default,
    {
        let subtree_name = subtree_name.as_ref();
        let builder_ref = self.entry_builder.lock().unwrap();
        let builder = builder_ref
            .as_ref()
            .ok_or(TransactionError::TransactionAlreadyCommitted)?;

        if let Ok(data) = builder.data(subtree_name) {
            if data.trim().is_empty() {
                // If data is empty, return default
                Ok(T::default())
            } else {
                serde_json::from_str(data).map_err(|e| {
                    TransactionError::StoreDeserializationFailed {
                        store: subtree_name.to_string(),
                        reason: e.to_string(),
                    }
                    .into()
                })
            }
        } else {
            // If subtree doesn't exist or has no data, return default
            Ok(T::default())
        }
    }

    /// Gets the fully merged historical state of a subtree up to the point this transaction began.
    ///
    /// This retrieves all relevant historical entries for the `subtree_name` from the backend,
    /// considering the parent tips recorded when this `Transaction` was created (or when the
    /// subtree was first accessed within the transaction). It deserializes the data from each
    /// relevant entry into the CRDT type `T` and merges them according to `T`'s `CRDT::merge`
    /// implementation.
    ///
    /// This is intended for use by `Store` implementations (e.g., in their `get` or `get_all` methods)
    /// to provide the historical context against which staged changes might be applied or compared.
    ///
    /// # Type Parameters
    /// * `T` - The CRDT type to deserialize and merge the historical subtree data into.
    ///
    /// # Arguments
    /// * `subtree_name` - The name of the subtree.
    ///
    /// # Returns
    /// A `Result<T>` containing the merged historical data of type `T`. Returns `Ok(T::default())`
    /// if the subtree has no history prior to this transaction.
    pub(crate) async fn get_full_state<T>(&self, subtree_name: impl AsRef<str> + Send) -> Result<T>
    where
        T: CRDT + Default + Send,
    {
        let subtree_name = subtree_name.as_ref();

        // Check if we need to initialize subtree tips (get data from RefCell before await)
        let (needs_init, main_parents) = {
            let builder_ref = self.entry_builder.lock().unwrap();
            let builder = builder_ref
                .as_ref()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?;

            let subtrees = builder.subtrees();
            if subtrees.contains(&subtree_name.to_string()) {
                (false, Vec::new())
            } else {
                (true, builder.parents().unwrap_or_default())
            }
        };

        // Initialize subtree tips if needed (async operations)
        if needs_init {
            let current_database_tips = self.db.backend()?.get_tips(self.db.root_id()).await?;

            let tips = if main_parents == current_database_tips {
                let backend = self.db.backend()?;
                backend
                    .get_store_tips(self.db.root_id(), subtree_name)
                    .await?
            } else {
                // This transaction uses custom tips - use special handler
                self.db
                    .backend()?
                    .get_store_tips_up_to_entries(self.db.root_id(), subtree_name, &main_parents)
                    .await?
            };

            // Update RefCell after async operations
            let mut builder_ref = self.entry_builder.lock().unwrap();
            let builder = builder_ref
                .as_mut()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?;
            builder.set_subtree_parents_mut(subtree_name, tips);
        }

        // Get the parent pointers for this subtree
        let parents = {
            let builder_ref = self.entry_builder.lock().unwrap();
            let builder = builder_ref
                .as_ref()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?;
            builder.subtree_parents(subtree_name).unwrap_or_default()
        };

        // If there are no parents, return a default
        if parents.is_empty() {
            return Ok(T::default());
        }

        // Compute the CRDT state using merge-base ROOT-to-target computation
        self.compute_subtree_state_merge_based(subtree_name, &parents)
            .await
    }

    /// Computes the CRDT state for a subtree using correct recursive merge-base algorithm.
    ///
    /// Algorithm:
    /// 1. If no entries, return default state
    /// 2. If single entry, compute its state recursively
    /// 3. If multiple entries, find their merge base and compute state from there
    ///
    /// # Type Parameters
    /// * `T` - The CRDT type to compute the state for
    ///
    /// # Arguments
    /// * `subtree_name` - The name of the subtree
    /// * `entry_ids` - The entry IDs to compute the merged state for (tips)
    ///
    /// # Returns
    /// A `Result<T>` containing the computed CRDT state
    async fn compute_subtree_state_merge_based<T>(
        &self,
        subtree_name: impl AsRef<str> + Send,
        entry_ids: &[ID],
    ) -> Result<T>
    where
        T: CRDT + Default + Send,
    {
        // FIXME: Cache the merged state for multi-tip queries. Currently every read
        // with 2+ tips re-runs find_merge_base and re-merges the path. Should cache
        // keyed by (sorted_tip_ids, subtree_name) and invalidate when tips change.

        // Base case: no entries
        if entry_ids.is_empty() {
            return Ok(T::default());
        }

        let subtree_name = subtree_name.as_ref();

        // If we have a single entry, compute its state recursively
        if entry_ids.len() == 1 {
            return self
                .compute_single_entry_state_recursive(subtree_name, &entry_ids[0])
                .await;
        }

        // Multiple entries: find merge base and compute state from there
        let merge_base_id = self
            .db
            .backend()?
            .find_merge_base(self.db.root_id(), subtree_name, entry_ids)
            .await?;

        // Get the merge base state recursively
        let mut result = self
            .compute_single_entry_state_recursive(subtree_name, &merge_base_id)
            .await?;

        // Get all entries from merge base to all tip entries (deduplicated and sorted)
        let path_entries = {
            self.db
                .backend()?
                .get_path_from_to(self.db.root_id(), subtree_name, &merge_base_id, entry_ids)
                .await?
        };

        // Merge all path entries in order
        result = self
            .merge_path_entries(subtree_name, result, &path_entries)
            .await?;

        Ok(result)
    }

    /// Computes the CRDT state for a single entry using correct recursive merge-base algorithm.
    ///
    /// Algorithm:
    /// 1. Check if entry state is cached → return it
    /// 2. Find merge base of parents and get its state (recursively)
    /// 3. Merge all entries from merge base to current entry into that state
    ///
    /// # Type Parameters
    /// * `T` - The CRDT type to compute the state for
    ///
    /// # Arguments
    /// * `subtree_name` - The name of the subtree
    /// * `entry_id` - The entry ID to compute the state for
    ///
    /// # Returns
    /// A `Result<T>` containing the computed CRDT state for the entry
    fn compute_single_entry_state_recursive<'a, T>(
        &'a self,
        subtree_name: &'a str,
        entry_id: &'a ID,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T>> + Send + 'a>>
    where
        T: CRDT + Default + Send + 'a,
    {
        Box::pin(async move {
            // Step 1: Check if already cached
            if let Some(cached_state) = self
                .db
                .backend()?
                .get_cached_crdt_state(entry_id, subtree_name)
                .await?
            {
                // Decrypt cached state if encryptor is registered
                let decrypted = self.decrypt_if_needed(subtree_name, &cached_state)?;
                let result: T = serde_json::from_str(&decrypted)?;
                return Ok(result);
            }

            // Get the parents of this entry in the subtree
            let parents = self
                .db
                .backend()?
                .get_sorted_store_parents(self.db.root_id(), entry_id, subtree_name)
                .await?;

            // Step 2: Compute merge base state recursively
            let (merge_base_state, merge_base_id_opt) = if parents.is_empty() {
                // No parents - this is a root, start with default
                (T::default(), None)
            } else if parents.len() == 1 {
                // Single parent - recursively get its state
                (
                    self.compute_single_entry_state_recursive(subtree_name, &parents[0])
                        .await?,
                    None,
                )
            } else {
                // Multiple parents - find merge base and get its state
                let merge_base_id = self
                    .db
                    .backend()?
                    .find_merge_base(self.db.root_id(), subtree_name, &parents)
                    .await?;
                let merge_base_state = self
                    .compute_single_entry_state_recursive(subtree_name, &merge_base_id)
                    .await?;
                (merge_base_state, Some(merge_base_id))
            };

            // Step 3: Merge entries from merge base to current entry
            let mut result = merge_base_state;

            // If we have multiple parents, we need to merge paths from merge base to all parents
            if let Some(merge_base_id) = merge_base_id_opt {
                // Get all entries from merge base to all parents (deduplicated and sorted)
                let path_entries = self
                    .db
                    .backend()?
                    .get_path_from_to(self.db.root_id(), subtree_name, &merge_base_id, &parents)
                    .await?;

                // Merge all path entries in order
                result = self
                    .merge_path_entries(subtree_name, result, &path_entries)
                    .await?;
            }

            // Finally, merge the current entry's local data
            let local_data = {
                let entry = self.db.backend()?.get(entry_id).await?;
                if let Ok(data) = entry.data(subtree_name) {
                    // Decrypt before deserializing
                    let plaintext = self.decrypt_if_needed(subtree_name, data)?;
                    serde_json::from_str::<T>(&plaintext)?
                } else {
                    T::default()
                }
            };

            result = result.merge(&local_data)?;

            // Cache the result (encrypted if encryptor is registered)
            let serialized_state = serde_json::to_string(&result)?;
            let to_cache = self.encrypt_if_needed(subtree_name, &serialized_state)?;
            self.db
                .backend()?
                .cache_crdt_state(entry_id, subtree_name, to_cache)
                .await?;

            Ok(result)
        })
    }

    /// Merges a sequence of entries into a CRDT state.
    ///
    /// # Arguments
    /// * `subtree_name` - The name of the subtree
    /// * `initial_state` - The initial CRDT state to merge into
    /// * `entry_ids` - The entry IDs to merge in order
    ///
    /// # Returns
    /// A `Result<T>` containing the merged CRDT state
    async fn merge_path_entries<T>(
        &self,
        subtree_name: &str,
        mut state: T,
        entry_ids: &[ID],
    ) -> Result<T>
    where
        T: CRDT + Clone + Default + serde::de::DeserializeOwned,
    {
        for entry_id in entry_ids {
            let entry = self.db.backend()?.get(entry_id).await?;

            // Get local data for this entry in the subtree
            let local_data = if let Ok(data) = entry.data(subtree_name) {
                // Decrypt before deserializing
                let plaintext = self.decrypt_if_needed(subtree_name, data)?;
                serde_json::from_str::<T>(&plaintext)?
            } else {
                T::default()
            };

            state = state.merge(&local_data)?;
        }

        Ok(state)
    }

    /// Commits the transaction, finalizing and persisting the entry to the backend.
    ///
    /// This method:
    /// 1. Takes ownership of the `EntryBuilder` from the internal `Option`
    /// 2. Removes any empty subtrees
    /// 3. Adds metadata if appropriate
    /// 4. Sets authentication if configured
    /// 5. Builds the immutable `Entry` using `EntryBuilder::build()`
    /// 6. Signs the entry if authentication is configured
    /// 7. Validates authentication if present
    /// 8. Calculates the entry's content-addressable ID
    /// 9. Persists the entry to the backend
    /// 10. Returns the ID of the newly created entry
    ///
    /// After commit, the transaction cannot be used again, as the internal
    /// `EntryBuilder` has been consumed.
    ///
    /// # Returns
    /// A `Result<ID>` containing the ID of the committed entry.
    pub async fn commit(self) -> Result<ID> {
        // Check if this is a settings subtree update and get the effective settings before any borrowing
        let has_settings_update = {
            let builder_cell = self.entry_builder.lock().unwrap();
            let builder = builder_cell
                .as_ref()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?;
            builder.subtrees().contains(&SETTINGS.to_string())
        };

        // Get settings using full CRDT state computation
        let historical_settings = self.get_full_state::<Doc>(SETTINGS).await?;

        // However, if this is a settings update and there's no historical auth but staged auth exists,
        // use the staged settings for validation (this handles initial database creation with auth)
        let effective_settings_for_validation = if has_settings_update {
            let historical_has_auth = matches!(historical_settings.get("auth"), Some(Value::Doc(auth_map)) if !auth_map.is_empty());
            if !historical_has_auth {
                let staged_settings = self.get_local_data::<Doc>(SETTINGS)?;
                let staged_has_auth = matches!(staged_settings.get("auth"), Some(Value::Doc(auth_map)) if !auth_map.is_empty());
                if staged_has_auth {
                    staged_settings
                } else {
                    historical_settings
                }
            } else {
                historical_settings
            }
        } else {
            historical_settings
        };

        // VALIDATION: Ensure that the new settings state (after this transaction) doesn't corrupt auth
        // This prevents committing entries that would corrupt the database's auth configuration
        if has_settings_update {
            // Compute what the new settings state will be after merging local changes
            let local_settings = self.get_local_data::<Doc>(SETTINGS)?;
            let new_settings = effective_settings_for_validation.merge(&local_settings)?;

            // Check if the new settings would have corrupted auth
            if new_settings.is_tombstone("auth") {
                // Auth was explicitly deleted - this would corrupt the database
                return Err(TransactionError::CorruptedAuthConfiguration.into());
            } else if let Some(auth_value) = new_settings.get("auth") {
                // Auth exists in new settings - check if it's the right type
                if !matches!(auth_value, Value::Doc(_)) {
                    // Auth exists but has wrong type (not a Doc) - this would corrupt the database
                    return Err(TransactionError::CorruptedAuthConfiguration.into());
                }
            }
            // If auth is None (not configured), that's fine - we allow empty auth
        }

        // Ensure _index constraint: subtrees referenced in _index must appear in Entry.
        // This adds subtrees with None data if they're referenced in _index but not yet in builder.
        // First, get the data we need before any async operations
        let (_index_data_opt, main_parents, missing_subtrees) = {
            let builder_ref = self.entry_builder.lock().unwrap();
            let builder = builder_ref
                .as_ref()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?;

            let index_data_opt = builder.data(INDEX).ok().map(String::from);
            let main_parents = builder.parents().unwrap_or_default();
            let existing_subtrees = builder.subtrees();

            // Find missing subtrees
            let missing = if let Some(ref index_data) = index_data_opt
                && let Ok(index_doc) = serde_json::from_str::<Doc>(index_data)
            {
                index_doc
                    .keys()
                    .filter(|name| !existing_subtrees.contains(&name.to_string()))
                    .cloned()
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };

            (index_data_opt, main_parents, missing)
        };

        // Get tips for missing subtrees (async)
        let mut subtree_tips: Vec<(String, Vec<ID>)> = Vec::new();
        for subtree_name in missing_subtrees {
            let tips = self.get_subtree_tips(&subtree_name, &main_parents).await?;
            subtree_tips.push((subtree_name, tips));
        }

        // Now update the builder with the tips
        {
            let mut builder_ref = self.entry_builder.lock().unwrap();
            let builder = builder_ref
                .as_mut()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?;

            for (subtree_name, tips) in subtree_tips {
                builder.set_subtree_parents_mut(&subtree_name, tips);
            }

            builder.remove_empty_subtrees_mut()?;
        }

        // Add metadata with settings tips for all entries
        // Get the backend to access settings tips (do async ops before RefCell borrow)
        let db_tips = self.db.get_tips().await?;
        let settings_tips = self
            .db
            .backend()?
            .get_store_tips_up_to_entries(self.db.root_id(), SETTINGS, &db_tips)
            .await?;

        // Clone the builder from RefCell (limit borrow scope to avoid holding across await)
        let mut builder = {
            let builder_cell = self.entry_builder.lock().unwrap();
            let builder_from_cell = builder_cell
                .as_ref()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?;
            builder_from_cell.clone()
        };

        // Parse existing metadata if present, or create new
        let mut metadata = builder
            .metadata()
            .and_then(|m| serde_json::from_str::<EntryMetadata>(m).ok())
            .unwrap_or_else(|| EntryMetadata {
                settings_tips: Vec::new(),
                entropy: None,
            });

        // Update settings tips
        metadata.settings_tips = settings_tips;

        // Serialize the metadata
        let metadata_json = serde_json::to_string(&metadata)?;

        // Add metadata to the entry builder
        builder.set_metadata_mut(metadata_json);

        // Handle authentication configuration before building
        // All entries must now be authenticated - fail if no auth key is configured

        // Use provided signing key (all databases use KeySource::Provided now)
        let (signing_key, _sigkey_identifier) =
            if let Some((ref provided_key, ref sigkey)) = self.provided_signing_key {
                // Use provided signing key directly (already decrypted from UserKeyManager or device key)
                let key_clone = provided_key.clone();

                // Build SigInfo - sigkey already validated at Database::open time
                let mut sig_builder = SigInfo::builder().key(SigKey::Direct(sigkey.clone()));

                // Include pubkey only for global "*" permission
                if sigkey == "*" {
                    let public_key = provided_key.verifying_key();
                    let pubkey_string = format_public_key(&public_key);
                    sig_builder = sig_builder.pubkey(pubkey_string);
                }

                // Set auth ID on the entry builder (without signature initially)
                builder.set_sig_mut(sig_builder.build());

                (Some(key_clone), sigkey.clone())
            } else {
                // No authentication key configured - all databases should provide keys via KeySource::Provided
                return Err(TransactionError::AuthenticationRequired.into());
            };
        // Encrypt subtree data if encryptors are registered
        // This must happen before building the entry to ensure encrypted data is persisted
        {
            let encryptors = self.encryptors.lock().unwrap();
            for subtree_name in builder.subtrees() {
                if let Some(encryptor) = encryptors.get(&subtree_name) {
                    // Get the plaintext data from the builder
                    if let Ok(plaintext_data) = builder.data(&subtree_name)
                        && !plaintext_data.trim().is_empty()
                    {
                        // Encrypt the plaintext data (as bytes)
                        let ciphertext = encryptor.encrypt(plaintext_data.as_bytes())?;
                        // Encode as base64 for storage
                        let encoded = Base64::encode_string(&ciphertext);
                        // Update the builder with encrypted data
                        builder.set_subtree_data_mut(subtree_name.clone(), encoded);
                    }
                }
            }
        }

        // Extract height strategy from settings (defaults to Incremental)
        // If this transaction includes settings updates, merge them to get the effective strategy
        let settings_for_height = if has_settings_update {
            let local_settings = self.get_local_data::<Doc>(SETTINGS)?;
            effective_settings_for_validation.merge(&local_settings)?
        } else {
            effective_settings_for_validation.clone()
        };
        let height_strategy: HeightStrategy = settings_for_height
            .get_json("height_strategy")
            .unwrap_or_default();

        // Compute heights from parent entries using the configured strategy
        {
            let backend = self.db.backend()?;
            let instance = self.db.instance()?;
            let calculator = height_strategy.into_calculator(instance.clock_arc());

            // Compute main tree height using the height strategy
            let main_parents = builder.parents().unwrap_or_default();
            let max_parent_height = if main_parents.is_empty() {
                None
            } else {
                let mut max_height = 0u64;
                for parent_id in &main_parents {
                    if let Ok(parent) = backend.get(parent_id).await {
                        max_height = max_height.max(parent.height());
                    }
                }
                Some(max_height)
            };
            let tree_height = calculator.calculate_height(max_parent_height);
            builder.set_height_mut(tree_height);

            // Compute subtree heights based on per-subtree settings from _index
            // System subtrees (prefixed with _) always inherit from tree.
            // Regular subtrees check _index for a height_strategy override.
            //
            // If a subtree has no override, its height is left as None, which means
            // Entry.subtree_height() will return the tree height (inheritance).
            let index = self.get_index().await.ok();

            for subtree_name in builder.subtrees() {
                // Determine the effective strategy for this subtree:
                // - System subtrees (_settings, _index, etc.): inherit (None)
                // - User subtrees: look up in _index, default to inherit (None)
                let subtree_strategy: Option<HeightStrategy> = if subtree_name.starts_with('_') {
                    // System subtrees always inherit from tree
                    None
                } else if let Some(ref idx) = index {
                    idx.get_subtree_settings(&subtree_name)
                        .await
                        .ok()
                        .and_then(|s| s.height_strategy)
                } else {
                    None
                };

                match subtree_strategy {
                    None => {
                        // Inherit from tree - height stays None (default)
                        // Entry.subtree_height() will return tree height
                    }
                    Some(strategy) => {
                        // Calculate independent height from subtree parents
                        let subtree_calculator = strategy.into_calculator(instance.clock_arc());
                        let subtree_parents =
                            builder.subtree_parents(&subtree_name).unwrap_or_default();
                        let max_subtree_parent_height = if subtree_parents.is_empty() {
                            None
                        } else {
                            let mut max_height = 0u64;
                            for parent_id in &subtree_parents {
                                if let Ok(parent) = backend.get(parent_id).await
                                    && let Ok(height) = parent.subtree_height(&subtree_name)
                                {
                                    max_height = max_height.max(height);
                                }
                            }
                            Some(max_height)
                        };
                        let subtree_height =
                            subtree_calculator.calculate_height(max_subtree_parent_height);
                        builder.set_subtree_height_mut(&subtree_name, Some(subtree_height));
                    }
                }
            }
        }

        // Build the final immutable Entry
        let mut entry = builder.build()?;

        // CRITICAL VALIDATION: Ensure entry structural integrity before commit
        //
        // This validation is crucial because the transaction layer has already:
        // 1. Discovered proper parent relationships through DAG traversal
        // 2. Set up correct subtree parents via find_subtree_parents_from_main_parents()
        // 3. Ensured all references point to valid entries in the backend
        //
        // The validate() call here ensures that:
        // - Non-root entries have main tree parents (preventing orphaned nodes)
        // - Parent IDs are not empty strings (preventing reference errors)
        // - The entry structure is valid before signing and storage
        //
        // This catches any issues early in the transaction, providing clear error
        // messages before the entry is signed or reaches the backend storage layer.
        entry.validate()?;

        // Sign the entry if we have a signing key
        if let Some(signing_key) = signing_key {
            let signature = sign_entry(&entry, &signing_key)?;
            entry.sig.sig = Some(signature);
        }

        // Validate authentication (all entries must be authenticated)
        let mut validator = AuthValidator::new();

        // Get the final settings state for validation
        // IMPORTANT: For permission checking, we must use the historical auth configuration
        // (before this transaction), not the auth configuration from the current entry.
        // This prevents operations from modifying their own permission requirements.

        // Extract AuthSettings from effective settings for validation
        // IMPORTANT: Distinguish between empty auth vs corrupted/deleted auth:
        // - None: No auth ever configured → Allow unsigned operations (empty AuthSettings)
        // - Some(Doc): Normal auth configuration → Use it for validation
        // - Tombstone (deleted): Auth was configured then deleted → CORRUPTED (fail-safe)
        // - Some(other types): Wrong type in auth field → CORRUPTED (fail-safe)
        //
        // NOTE: Doc::get() hides tombstones (returns None for deleted values), so we need
        // to check for tombstones explicitly using is_tombstone() before using get().
        let auth_settings_for_validation = if effective_settings_for_validation.is_tombstone("auth")
        {
            // Auth was configured then explicitly deleted - this is corrupted
            return Err(TransactionError::CorruptedAuthConfiguration.into());
        } else {
            match effective_settings_for_validation.get("auth") {
                Some(Value::Doc(auth_doc)) => AuthSettings::from_doc(auth_doc.clone()),
                None => AuthSettings::new(), // Empty auth - never configured
                Some(_) => {
                    // Auth exists but has wrong type (not a Doc) - this is corrupted
                    return Err(TransactionError::CorruptedAuthConfiguration.into());
                }
            }
        };

        let instance = self.db.instance()?;

        let verification_status = match validator
            .validate_entry(&entry, &auth_settings_for_validation, Some(&instance))
            .await
        {
            Ok(true) => {
                // Authentication validation succeeded - check permissions
                // Check if we have auth configuration
                let has_auth_config = !auth_settings_for_validation.get_all_keys()?.is_empty();

                if has_auth_config {
                    // We have auth configuration, so check permissions
                    let operation_type = if has_settings_update
                        || entry.subtrees().contains(&SETTINGS.to_string())
                    {
                        Operation::WriteSettings // Modifying settings is a settings operation
                    } else {
                        Operation::WriteData // Default to write for other data modifications
                    };

                    let resolved_auth = validator
                        .resolve_sig_key_with_pubkey(
                            &entry.sig.key,
                            &auth_settings_for_validation,
                            Some(&instance),
                            entry.sig.pubkey.as_deref(),
                        )
                        .await?;

                    let has_permission =
                        validator.check_permissions(&resolved_auth, &operation_type)?;

                    if has_permission {
                        crate::backend::VerificationStatus::Verified
                    } else {
                        return Err(TransactionError::InsufficientPermissions.into());
                    }
                } else {
                    // No auth configuration found in historical settings
                    // Check if this is a bootstrap operation (adding auth config for the first time)
                    if has_settings_update || entry.subtrees().contains(&SETTINGS.to_string()) {
                        // This operation is updating settings - check if it's adding auth configuration
                        if let Ok(settings_data) = entry.data(SETTINGS) {
                            if let Ok(new_settings) = serde_json::from_str::<Doc>(settings_data) {
                                if matches!(new_settings.get("auth"), Some(Value::Doc(auth_map)) if !auth_map.is_empty())
                                {
                                    // This is a bootstrap operation - adding auth config for the first time
                                    // Allow it since it's setting up authentication
                                    crate::backend::VerificationStatus::Verified
                                } else {
                                    return Err(TransactionError::NoAuthConfiguration.into());
                                }
                            } else {
                                return Err(TransactionError::NoAuthConfiguration.into());
                            }
                        } else {
                            return Err(TransactionError::NoAuthConfiguration.into());
                        }
                    } else {
                        return Err(TransactionError::NoAuthConfiguration.into());
                    }
                }
            }
            Ok(false) => {
                // Signature verification failed
                return Err(TransactionError::SignatureVerificationFailed.into());
            }
            Err(e) => {
                // Authentication validation error
                return Err(e);
            }
        };

        // Get the entry's ID
        let id = entry.id();

        // Write entry through Instance which handles backend storage and callback dispatch
        let instance = self.db.instance()?;
        instance
            .put_entry(
                self.db.root_id(),
                verification_status,
                entry.clone(),
                crate::instance::WriteSource::Local,
            )
            .await?;

        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Instance, auth::crypto::generate_keypair, backend::database::InMemory, crdt::Doc,
        store::DocStore,
    };

    /// Test that corrupted auth configuration prevents commit
    ///
    /// Validates that transactions reject changes that would corrupt the auth configuration,
    /// preventing corrupted entries from entering the Merkle DAG.
    #[tokio::test]
    async fn test_prevent_auth_corruption() {
        let backend = InMemory::new();
        let instance = Instance::open(Box::new(backend)).await.unwrap();
        let (private_key, _) = generate_keypair();

        // Create database with the test key
        let database = Database::create(Doc::new(), &instance, private_key, "test_key".to_string())
            .await
            .unwrap();

        // Initial operation should work
        let tx = database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("data").await.unwrap();
        store.set("initial", "value").await.unwrap();
        tx.commit().await.expect("Initial operation should succeed");

        // Test corruption path 1: Set auth to wrong type (String instead of Doc)
        let tx = database.new_transaction().await.unwrap();
        let settings = tx.get_store::<DocStore>("_settings").await.unwrap();
        settings.set("auth", "corrupted_string").await.unwrap();

        let result = tx.commit().await;
        assert!(
            result.is_err(),
            "Corruption commit (wrong type) should fail immediately"
        );
        assert!(
            result.unwrap_err().is_authentication_error(),
            "Should be authentication error"
        );

        // Test corruption path 2: Delete auth (creates CRDT tombstone)
        let tx = database.new_transaction().await.unwrap();
        let settings = tx.get_store::<DocStore>("_settings").await.unwrap();
        settings.delete("auth").await.unwrap();

        let result = tx.commit().await;
        assert!(
            result.is_err(),
            "Deletion commit (tombstone) should fail immediately"
        );
        assert!(
            result.unwrap_err().is_authentication_error(),
            "Should be authentication error"
        );

        // Verify database is still functional after preventing corruption
        let tx = database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("data").await.unwrap();
        store
            .set("after_prevented_corruption", "value")
            .await
            .unwrap();
        tx.commit()
            .await
            .expect("Normal operations should still work");
    }
}
