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

use std::{cell::RefCell, rc::Rc, sync::Arc};

pub use errors::TransactionError;
use serde::{Deserialize, Serialize};

use crate::{
    Database, Result, Store,
    auth::{
        crypto::{format_public_key, sign_entry},
        types::{AuthKey, Operation, Permission, SigInfo, SigKey},
        validation::AuthValidator,
    },
    constants::SETTINGS,
    crdt::{CRDT, Doc, doc::Value},
    entry::{Entry, EntryBuilder, ID},
    store::SettingsStore,
    sync::hooks::{SyncHookCollection, SyncHookContext},
};

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
    entry_builder: Rc<RefCell<Option<EntryBuilder>>>,
    /// The database this transaction belongs to
    db: Database,
    /// Optional authentication key ID for signing entries
    auth_key_name: Option<String>,
    /// Optional sync hooks to execute after successful commit
    sync_hooks: Option<Arc<SyncHookCollection>>,
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
    pub(crate) fn new_with_tips(database: &Database, tips: &[ID]) -> Result<Self> {
        // Validate that tips are not empty, unless we're creating the root entry
        if tips.is_empty() {
            // Check if this is a root entry creation by seeing if the database root exists in backend
            let root_exists = database.backend().get(database.root_id()).is_ok();

            if root_exists {
                return Err(TransactionError::EmptyTipsNotAllowed.into());
            }
            // If root doesn't exist, this is valid (creating the root entry)
        }

        // Validate that all tips belong to the same tree
        for tip_id in tips {
            let entry = database.backend().get(tip_id)?;
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
            entry_builder: Rc::new(RefCell::new(Some(builder))),
            db: database.clone(),
            auth_key_name: None,
            sync_hooks: None,
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

    /// Set sync hooks for this transaction.
    ///
    /// Sync hooks are called after successful commit to notify the sync system
    /// about new entries that may need to be synchronized.
    ///
    /// # Arguments
    /// * `hooks` - The sync hook collection to execute after commit
    ///
    /// # Returns
    /// Self for method chaining
    pub fn with_sync_hooks(mut self, hooks: Arc<SyncHookCollection>) -> Self {
        self.sync_hooks = Some(hooks);
        self
    }

    /// Set sync hooks for this transaction (mutable version).
    ///
    /// # Arguments
    /// * `hooks` - The sync hook collection to execute after commit
    pub fn set_sync_hooks(&mut self, hooks: Arc<SyncHookCollection>) {
        self.sync_hooks = Some(hooks);
    }

    /// Set the authentication key ID for this transaction (mutable version).
    ///
    /// # Arguments
    /// * `key_name` - The identifier of the private key to use for signing
    pub fn set_auth_key(&mut self, key_name: impl Into<String>) {
        self.auth_key_name = Some(key_name.into());
    }

    /// Get the current authentication key ID for this transaction.
    pub fn auth_key_name(&self) -> Option<&str> {
        self.auth_key_name.as_deref()
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
    /// # let database: Database = unimplemented!();
    /// let op = database.new_transaction()?;
    /// let settings = op.get_settings()?;
    ///
    /// // Read a setting
    /// if let Ok(name) = settings.get_name() {
    ///     println!("Database name: {}", name);
    /// }
    ///
    /// // Modify a setting
    /// settings.set_name("Updated Database Name")?;
    /// # Ok::<(), eidetica::Error>(())
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

    /// Set the tree root field for the entry being built.
    ///
    /// This is primarily used during tree creation to ensure the root entry
    /// has an empty tree.root field, making it a proper top-level root.
    ///
    /// # Arguments
    /// * `root` - The tree root ID to set (use empty string for top-level roots)
    pub(crate) fn set_entry_root(&self, root: impl Into<String>) -> Result<()> {
        let mut builder_ref = self.entry_builder.borrow_mut();
        let builder = builder_ref
            .as_mut()
            .ok_or(TransactionError::TransactionAlreadyCommitted)?;
        builder.set_root_mut(root.into());
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
    pub(crate) fn update_subtree(
        &self,
        subtree: impl AsRef<str>,
        data: impl AsRef<str>,
    ) -> Result<()> {
        let subtree = subtree.as_ref();
        let data = data.as_ref();
        let mut builder_ref = self.entry_builder.borrow_mut();
        let builder = builder_ref
            .as_mut()
            .ok_or(TransactionError::TransactionAlreadyCommitted)?;

        // If we haven't cached the tips for this subtree yet, get them now
        let subtrees = builder.subtrees();

        if !subtrees.contains(&subtree.to_string()) {
            // FIXME: we should get the subtree tips while still using the parent pointers
            let tips = self
                .db
                .backend()
                .get_store_tips(self.db.root_id(), subtree)?;
            builder.set_subtree_data_mut(subtree.to_string(), data.to_string());
            builder.set_subtree_parents_mut(subtree, tips);
        } else {
            builder.set_subtree_data_mut(subtree.to_string(), data.to_string());
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
    pub fn get_store<T>(&self, subtree_name: impl Into<String>) -> Result<T>
    where
        T: Store,
    {
        let subtree_name = subtree_name.into();
        {
            let mut builder_ref = self.entry_builder.borrow_mut();
            let builder = builder_ref
                .as_mut()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?;

            // Initialize subtree parents if this is the first time accessing this subtree
            // in this transaction. This ensures proper parent relationships are established
            // before any operations on the subtree.
            let subtrees = builder.subtrees();

            if !subtrees.contains(&subtree_name) {
                // Determine whether this transaction is using custom parent tips or current database tips
                // This affects how we calculate subtree parents
                let main_parents = builder.parents().unwrap_or_default();
                let current_database_tips = self.db.backend().get_tips(self.db.root_id())?;

                // Get subtree tips based on the transaction's parent context
                let tips = if main_parents == current_database_tips {
                    // Using current database tips - get all current subtree tips
                    self.db
                        .backend()
                        .get_store_tips(self.db.root_id(), &subtree_name)?
                } else {
                    // Using custom parent tips - get subtree tips reachable from those parents
                    self.db.backend().get_store_tips_up_to_entries(
                        self.db.root_id(),
                        &subtree_name,
                        &main_parents,
                    )?
                };

                // Initialize the subtree with proper parent relationships
                builder.set_subtree_data_mut(subtree_name.clone(), String::new());
                builder.set_subtree_parents_mut(&subtree_name, tips);
            }
        }

        // Now create the Store referencing this Transaction
        T::new(self, subtree_name)
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
        let builder_ref = self.entry_builder.borrow();
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
    pub(crate) fn get_full_state<T>(&self, subtree_name: impl AsRef<str>) -> Result<T>
    where
        T: CRDT + Default,
    {
        let subtree_name = subtree_name.as_ref();
        // Get the entry builder to get parent pointers
        let mut builder_ref = self.entry_builder.borrow_mut();
        let builder = builder_ref
            .as_mut()
            .ok_or(TransactionError::TransactionAlreadyCommitted)?;

        // If we haven't cached the tips for this subtree yet, get them now
        let subtrees = builder.subtrees();
        if !subtrees.contains(&subtree_name.to_string()) {
            // Check if this transaction was created with custom tips vs current tips
            let main_parents = builder.parents().unwrap_or_default();
            let current_database_tips = self.db.backend().get_tips(self.db.root_id())?;

            let tips = if main_parents == current_database_tips {
                self.db
                    .backend()
                    .get_store_tips(self.db.root_id(), subtree_name)?
            } else {
                // This transaction uses custom tips - use special handler
                self.db.backend().get_store_tips_up_to_entries(
                    self.db.root_id(),
                    subtree_name,
                    &main_parents,
                )?
            };
            builder.set_subtree_data_mut(subtree_name.to_string(), String::new());
            builder.set_subtree_parents_mut(subtree_name, tips);
        }

        // Get the parent pointers for this subtree
        let parents = builder.subtree_parents(subtree_name).unwrap_or_default();

        // If there are no parents, return a default
        if parents.is_empty() {
            return Ok(T::default());
        }

        // Compute the CRDT state using LCA-based ROOT-to-target computation
        self.compute_subtree_state_lca_based(subtree_name, &parents)
    }

    /// Computes the CRDT state for a subtree using correct recursive LCA-based algorithm.
    ///
    /// Algorithm:
    /// 1. If no entries, return default state
    /// 2. If single entry, compute its state recursively
    /// 3. If multiple entries, find their LCA and compute state from that LCA
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
    fn compute_subtree_state_lca_based<T>(
        &self,
        subtree_name: impl AsRef<str>,
        entry_ids: &[ID],
    ) -> Result<T>
    where
        T: CRDT + Default,
    {
        // Base case: no entries
        if entry_ids.is_empty() {
            return Ok(T::default());
        }

        let subtree_name = subtree_name.as_ref();

        // If we have a single entry, compute its state recursively
        if entry_ids.len() == 1 {
            return self.compute_single_entry_state_recursive(subtree_name, &entry_ids[0]);
        }

        // Multiple entries: find LCA and compute state from there
        let lca_id = self
            .db
            .backend()
            .find_lca(self.db.root_id(), subtree_name, entry_ids)?;

        // Get the LCA state recursively
        let mut result = self.compute_single_entry_state_recursive(subtree_name, &lca_id)?;

        // Get all entries from LCA to all tip entries (deduplicated and sorted)
        let path_entries = {
            self.db.backend().get_path_from_to(
                self.db.root_id(),
                subtree_name,
                &lca_id,
                entry_ids,
            )?
        };

        // Merge all path entries in order
        result = self.merge_path_entries(subtree_name, result, &path_entries)?;

        Ok(result)
    }

    /// Computes the CRDT state for a single entry using correct recursive LCA algorithm.
    ///
    /// Algorithm:
    /// 1. Check if entry state is cached → return it
    /// 2. Find LCA of parents and get its state (recursively)
    /// 3. Merge all entries from LCA to current entry into that state
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
    fn compute_single_entry_state_recursive<T>(
        &self,
        subtree_name: &str,
        entry_id: &ID,
    ) -> Result<T>
    where
        T: CRDT + Default,
    {
        // Step 1: Check if already cached
        {
            if let Some(cached_state) = self
                .db
                .backend()
                .get_cached_crdt_state(entry_id, subtree_name)?
            {
                let result: T = serde_json::from_str(&cached_state)?;
                return Ok(result);
            }
        }

        // Get the parents of this entry in the subtree
        let parents = {
            self.db
                .backend()
                .get_sorted_store_parents(self.db.root_id(), entry_id, subtree_name)?
        };

        // Step 2: Compute LCA state recursively
        let (lca_state, lca_id_opt) = if parents.is_empty() {
            // No parents - this is a root, start with default
            (T::default(), None)
        } else if parents.len() == 1 {
            // Single parent - recursively get its state
            (
                self.compute_single_entry_state_recursive(subtree_name, &parents[0])?,
                None,
            )
        } else {
            // Multiple parents - find LCA and get its state
            let lca_id = {
                self.db
                    .backend()
                    .find_lca(self.db.root_id(), subtree_name, &parents)?
            };
            let lca_state = self.compute_single_entry_state_recursive(subtree_name, &lca_id)?;
            (lca_state, Some(lca_id))
        };

        // Step 3: Merge entries from LCA to current entry
        let mut result = lca_state;

        // If we have multiple parents, we need to merge paths from LCA to all parents
        if let Some(lca_id) = lca_id_opt {
            // Get all entries from LCA to all parents (deduplicated and sorted)
            let path_entries = {
                self.db.backend().get_path_from_to(
                    self.db.root_id(),
                    subtree_name,
                    &lca_id,
                    &parents,
                )?
            };

            // Merge all path entries in order
            result = self.merge_path_entries(subtree_name, result, &path_entries)?;
        }

        // Finally, merge the current entry's local data
        let local_data = {
            let entry = self.db.backend().get(entry_id)?;
            if let Ok(data) = entry.data(subtree_name) {
                serde_json::from_str::<T>(data)?
            } else {
                T::default()
            }
        };

        result = result.merge(&local_data)?;

        // Cache the result
        {
            let serialized_state = serde_json::to_string(&result)?;
            self.db
                .backend()
                .cache_crdt_state(entry_id, subtree_name, serialized_state)?;
        }

        Ok(result)
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
    fn merge_path_entries<T>(&self, subtree_name: &str, mut state: T, entry_ids: &[ID]) -> Result<T>
    where
        T: CRDT + Clone + Default + serde::de::DeserializeOwned,
    {
        for entry_id in entry_ids {
            let entry = self.db.backend().get(entry_id)?;

            // Get local data for this entry in the subtree
            let local_data = if let Ok(data) = entry.data(subtree_name) {
                serde_json::from_str::<T>(data)?
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
    pub fn commit(self) -> Result<ID> {
        // Check if this is a settings subtree update and get the effective settings before any borrowing
        let has_settings_update = {
            let builder_cell = self.entry_builder.borrow();
            let builder = builder_cell
                .as_ref()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?;
            builder.subtrees().contains(&SETTINGS.to_string())
        };

        // Get settings using full CRDT state computation
        let historical_settings = self.get_full_state::<Doc>(SETTINGS)?;

        // However, if this is a settings update and there's no historical auth but staged auth exists,
        // use the staged settings for validation (this handles initial database creation with auth)
        let effective_settings_for_validation = if has_settings_update {
            let historical_has_auth = matches!(historical_settings.get("auth"), Some(Value::Node(auth_map)) if !auth_map.as_hashmap().is_empty());
            if !historical_has_auth {
                let staged_settings = self.get_local_data::<Doc>(SETTINGS)?;
                let staged_has_auth = matches!(staged_settings.get("auth"), Some(Value::Node(auth_map)) if !auth_map.as_hashmap().is_empty());
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

        // Get the entry out of the RefCell, consuming self in the process
        let builder_cell = self.entry_builder.borrow_mut();
        let builder_from_cell = builder_cell
            .as_ref()
            .ok_or(TransactionError::TransactionAlreadyCommitted)?;

        // Clone the builder since we can't easily take ownership from RefCell<Option<>>
        let mut builder = builder_from_cell.clone();

        // Add metadata with settings tips for all entries
        // Get the backend to access settings tips
        let settings_tips = self.db.backend().get_store_tips_up_to_entries(
            self.db.root_id(),
            SETTINGS,
            &self.db.get_tips()?,
        )?;

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
        let signing_key = if let Some(key_name) = &self.auth_key_name {
            // Get the private key from backend for signing
            let signing_key = self.db.backend().get_private_key(key_name)?;

            if signing_key.is_none() {
                return Err(TransactionError::SigningKeyNotFound {
                    key_name: key_name.clone(),
                }
                .into());
            }

            // Build SigInfo using the builder pattern
            let mut sig_builder = SigInfo::builder().key(SigKey::Direct(key_name.clone()));

            // For global permissions '*', include the public key directly
            if key_name == "*"
                && let Some(ref private_key) = signing_key
            {
                let public_key = private_key.verifying_key();
                let pubkey_string = format_public_key(&public_key);
                sig_builder = sig_builder.pubkey(pubkey_string);
            }

            // Set auth ID on the entry builder (without signature initially)
            builder.set_sig_mut(sig_builder.build());

            // Check if we need to bootstrap auth configuration
            // First check if auth is configured in the historical settings
            let auth_configured_historical = matches!(effective_settings_for_validation.get("auth"), Some(Value::Node(auth_map)) if !auth_map.as_hashmap().is_empty());

            // If not configured historically, check if this entry is setting up auth for the first time
            let auth_configured = if !auth_configured_historical && has_settings_update {
                // Check if the staged settings contain auth configuration
                let staged_settings = self.get_local_data::<Doc>(SETTINGS)?;
                matches!(staged_settings.get("auth"), Some(Value::Node(auth_map)) if !auth_map.as_hashmap().is_empty())
            } else {
                auth_configured_historical
            };

            if !auth_configured {
                // Bootstrap auth configuration by adding this key as admin:0
                let public_key = signing_key.as_ref().unwrap().verifying_key();

                let mut auth_settings = crate::auth::settings::AuthSettings::new();
                let super_user_auth_key = AuthKey::active(
                    format_public_key(&public_key),
                    Permission::Admin(0), // Highest priority
                )
                .unwrap();
                auth_settings.add_key(key_name, super_user_auth_key)?;

                // Update the settings subtree to include auth configuration
                // We need to merge with existing settings and add the auth section
                let mut updated_settings = effective_settings_for_validation.clone();
                updated_settings.set_node("auth", auth_settings.as_doc().clone());

                // Update the SETTINGS subtree data in the entry builder
                let settings_json = serde_json::to_string(&updated_settings)?;
                builder.set_subtree_data_mut(SETTINGS, settings_json);

                // Make sure we track that this is now a settings update
                // Note: we don't change has_settings_update here since it was calculated earlier
                // and is used for metadata logic
            }
            // If auth is already configured, the validation will check if the key exists
            // and fail appropriately if it doesn't

            signing_key
        } else {
            // No authentication key configured
            return Err(TransactionError::AuthenticationRequired.into());
        };

        // Remove empty subtrees and build the final immutable Entry
        let mut entry = builder.remove_empty_subtrees().build()?;

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
        let settings_for_validation = effective_settings_for_validation.clone();

        let verification_status = match validator.validate_entry(
            &entry,
            &settings_for_validation,
            Some(self.db.backend()),
        ) {
            Ok(true) => {
                // Authentication validation succeeded - check permissions
                match settings_for_validation.get("auth") {
                    Some(Value::Node(auth_map)) if !auth_map.as_hashmap().is_empty() => {
                        // We have auth configuration, so check permissions
                        let operation_type = if has_settings_update
                            || entry.subtrees().contains(&SETTINGS.to_string())
                        {
                            Operation::WriteSettings // Modifying settings is a settings operation
                        } else {
                            Operation::WriteData // Default to write for other data modifications
                        };

                        let resolved_auth = validator.resolve_sig_key_with_pubkey(
                            &entry.sig.key,
                            &settings_for_validation,
                            Some(self.db.backend()),
                            entry.sig.pubkey.as_deref(),
                        )?;

                        let has_permission =
                            validator.check_permissions(&resolved_auth, &operation_type)?;

                        if has_permission {
                            crate::backend::VerificationStatus::Verified
                        } else {
                            return Err(TransactionError::InsufficientPermissions.into());
                        }
                    }
                    _ => {
                        // No auth configuration found in historical settings
                        // Check if this is a bootstrap operation (adding auth config for the first time)
                        if has_settings_update || entry.subtrees().contains(&SETTINGS.to_string()) {
                            // This operation is updating settings - check if it's adding auth configuration
                            if let Ok(settings_data) = entry.data(SETTINGS) {
                                if let Ok(new_settings) = serde_json::from_str::<Doc>(settings_data)
                                {
                                    if matches!(new_settings.get("auth"), Some(Value::Node(auth_map)) if !auth_map.as_hashmap().is_empty())
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

        // Store in the backend with the determined verification status
        self.db.backend().put(verification_status, entry.clone())?;

        // Execute sync hooks if present
        if let Some(hooks) = &self.sync_hooks
            && hooks.has_hooks()
        {
            let context = SyncHookContext {
                tree_id: self.db.root_id().clone(),
                entry: entry.clone(),
                is_root_entry: entry.root().is_empty(),
            };

            // Execute hooks - failures are logged but don't fail the commit
            if let Err(e) = hooks.execute_hooks(&context) {
                tracing::error!("Sync hook execution failed (commit succeeded): {e}");
            }
        }

        Ok(id)
    }
}
