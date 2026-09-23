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

#[cfg(test)]
mod tests;

use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub use errors::TransactionError;
use serde::{Deserialize, Serialize};

use crate::{
    Database, Result, Snapshot, Store,
    auth::{
        AuthSettings,
        crypto::{PrivateKey, sign_entry},
        types::{AuthInfo, SigKey},
        validation::AuthValidator,
    },
    backend::{RecordMutation, RecordMutations, RecordRange, RecordView, VerificationStatus},
    constants::{INDEX, ROOT, SETTINGS},
    crdt::{CRDT, Data, Doc, doc::Value},
    entry::{Entry, EntryBuilder, ID},
    height::HeightStrategy,
    instance::WriteSource,
    store::table::CursorKind,
    store::{
        ProjectionDescriptor, RecordProjection, Registry, SettingsStore, StoreError, TableCursor,
        state,
    },
};

fn page_from_history(
    history: &BTreeMap<Vec<u8>, Vec<u8>>,
    after: Option<&[u8]>,
    count: usize,
) -> Result<crate::backend::RecordPage> {
    let mut rows = history
        .iter()
        .filter(|(key, _)| after.is_none_or(|after| key.as_slice() > after))
        .take(count.saturating_add(1))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();
    let next = (rows.len() > count).then(|| rows[count - 1].0.clone());
    rows.truncate(count);
    Ok(crate::backend::RecordPage {
        records: rows,
        next,
    })
}

/// Creates a synthetic entry ID for multi-tip merged CRDT state caching.
///
/// Tips are sorted to ensure deterministic keys regardless of input order.
/// The resulting ID has format `merge:{tip1}:{tip2}:...` which is distinct
/// from real content-addressed entry IDs.
fn create_merge_cache_id(tip_ids: &[ID]) -> ID {
    let mut sorted_tips = tip_ids.to_vec();
    sorted_tips.sort();

    // Create a deterministic cache key by hashing the sorted tip IDs
    let mut key = String::from("merge");
    for tip in &sorted_tips {
        key.push(':');
        key.push_str(&tip.to_string());
    }
    ID::from_bytes(key.as_bytes())
}

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
/// Entry subtree payloads are opaque bytes, so ciphertext is stored verbatim with no
/// additional encoding.
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
    /// Plaintext bytes in whatever format the wrapped store produces (e.g.
    /// JSON for `DocStore`/`Table`, binary Yrs updates for `YDoc`).
    fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>>;

    /// Encrypt plaintext bytes to ciphertext bytes
    ///
    /// # Arguments
    /// * `plaintext` - Bytes to encrypt; format is whatever the wrapped store
    ///   produces (JSON, binary CRDT update, etc.). The Encryptor itself does
    ///   not interpret the bytes.
    ///
    /// # Returns
    /// Encrypted data in implementation-defined format
    fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>>;

    fn physical_record_key(&self, logical_key: &[u8]) -> Result<Vec<u8>> {
        Ok(logical_key.to_vec())
    }

    fn encrypt_record(&self, _logical_key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
        self.encrypt(plaintext)
    }

    fn decrypt_record(&self, physical_key: &[u8], ciphertext: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        Ok((physical_key.to_vec(), self.decrypt(ciphertext)?))
    }

    fn projection_descriptor(&self, descriptor: ProjectionDescriptor) -> ProjectionDescriptor {
        descriptor
    }
}

/// Metadata structure for entries
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct EntryMetadata {
    /// Snapshot of the `_settings` subtree at the time this entry was created.
    /// This is the entry's **pin**: the exact `_settings` state its
    /// signature must be validated against. Used for sync performance,
    /// sparse-checkout validation, and deferred re-verification.
    ///
    /// Wire name remains `settings_tips` for on-disk stability — `Snapshot`
    /// serializes as a bare ID array, identical to the legacy `Vec<ID>` shape.
    #[serde(rename = "settings_tips")]
    pub(crate) settings_snapshot: Snapshot,
    /// Random entropy for ensuring unique IDs for root entries
    pub(crate) entropy: Option<u64>,
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
    /// Shared by clones, never by separately opened transaction views.
    view_id: uuid::Uuid,
    /// The entry builder being modified, wrapped in Option to support consuming on commit
    entry_builder: Arc<Mutex<Option<EntryBuilder>>>,
    /// The database this transaction belongs to
    db: Database,
    /// Provided signing key paired with its auth identity
    provided_signing_key: Option<(PrivateKey, SigKey)>,
    /// Registered encryptors for transparent encryption/decryption of specific subtrees
    /// Maps subtree name -> encryptor implementation
    /// When an encryptor is registered, the transaction automatically encrypts writes
    /// and decrypts reads for that subtree
    encryptors: Arc<Mutex<HashMap<String, Box<dyn Encryptor>>>>,
    /// When true, `get_store` rejects any `_`-prefixed subtree name. Used by
    /// `Database::create_with_init` to keep its init callback from opening the
    /// system subtrees (`_settings`, `_root`, `_index`) that `create_with_init`
    /// itself manages. Legitimate internal paths — `get_index()` (via
    /// `Registry::new` → `DocStore::load`) and `Store::register`'s own
    /// `_index` updates — bypass `get_store` and remain unaffected.
    system_subtrees_locked: Arc<AtomicBool>,
    record_views: Arc<Mutex<HashMap<(String, ProjectionDescriptor), RecordView>>>,
    projected: Arc<Mutex<HashMap<String, ProjectedStage>>>,
    projected_sealed: Arc<AtomicBool>,
}

/// Canonical typed delta and both read-your-writes overlays share one revision.
#[derive(Clone)]
struct ProjectedStage {
    revision: u64,
    descriptor: ProjectionDescriptor,
    canonical: Arc<dyn StagedDelta>,
    logical: RecordMutations,
    physical: RecordMutations,
}

trait StagedDelta: Send + Sync {
    fn as_any(&self) -> &dyn std::any::Any;
    fn bytes(&self) -> Result<Vec<u8>>;
}

impl<D: CRDT + Send + Sync + 'static> StagedDelta for D {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn bytes(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }
}

/// RAII guard returned by [`Transaction::lock_system_subtrees`]. Releases the
/// lock on drop, covering early-return-via-`?` and panic-unwind alike.
pub(crate) struct SystemSubtreeLockGuard {
    flag: Arc<AtomicBool>,
}

impl Drop for SystemSubtreeLockGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

impl Transaction {
    /// Creates a new atomic transaction for a specific `Database` anchored at a snapshot.
    ///
    /// Initializes an internal `EntryBuilder` with its main parent pointers set to the
    /// snapshot's tips instead of the database's current state. This allows creating
    /// transactions that branch from specific points in the database history (e.g.
    /// diamond patterns).
    ///
    /// # Arguments
    /// * `database` - The `Database` this transaction will modify.
    /// * `snapshot` - The snapshot to anchor the transaction at. Must contain at least one tip,
    ///   unless this transaction is creating the database's root entry.
    pub(crate) async fn new_at(database: &Database, snapshot: &Snapshot) -> Result<Self> {
        let tips = snapshot.tips();
        // Validate that tips are not empty, unless we're creating the root entry
        if tips.is_empty() {
            // Check if this is a root entry creation by seeing if the database root exists in backend
            let root_exists = database.ops().get(database.root_id()).await.is_ok();

            if root_exists {
                return Err(TransactionError::EmptyTipsNotAllowed.into());
            }
            // If root doesn't exist, this is valid (creating the root entry)
        }

        // Validate that all tips belong to the same tree
        let backend = database.ops();
        for tip_id in tips {
            let entry = backend.get(tip_id).await?;
            if !entry.in_tree(database.root_id()) {
                return Err(TransactionError::InvalidTip {
                    tip_id: tip_id.clone(),
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
            view_id: uuid::Uuid::new_v4(),
            entry_builder: Arc::new(Mutex::new(Some(builder))),
            db: database.clone(),
            provided_signing_key: None,
            encryptors: Arc::new(Mutex::new(HashMap::new())),
            system_subtrees_locked: Arc::new(AtomicBool::new(false)),
            record_views: Arc::new(Mutex::new(HashMap::new())),
            projected: Arc::new(Mutex::new(HashMap::new())),
            projected_sealed: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Lock the system subtrees (`_settings`, `_root`, `_index`) for the
    /// returned guard's lifetime.
    ///
    /// While the guard is live, [`Self::get_store`] rejects any subtree name
    /// beginning with `_` (the system-subtree prefix). Used by
    /// [`Database::create_with_init`] to guard its init callback against
    /// clobbering `_settings`/`_root`/`_index`, all of which `create_with_init`
    /// manages itself or via a dedicated accessor. The lock releases on the
    /// guard's `Drop`, so it covers both early-return-via-`?` and panic-unwind
    /// paths.
    ///
    /// The lock is process-local state on the (cloned-by-Arc) transaction;
    /// clones share the same flag. Legitimate internal paths — `get_index()`
    /// (via `Registry::new` → `DocStore::load`) and `Store::register`'s own
    /// `_index` writes — bypass `get_store` and are unaffected.
    pub(crate) fn lock_system_subtrees(&self) -> SystemSubtreeLockGuard {
        self.system_subtrees_locked.store(true, Ordering::Release);
        SystemSubtreeLockGuard {
            flag: self.system_subtrees_locked.clone(),
        }
    }

    /// Set signing key directly for user context (internal API).
    ///
    /// This method is used when a Database has a key attached
    /// (via `Database::open().with_key()`). The provided SigningKey is already
    /// decrypted and ready to use, eliminating the need for backend key lookup.
    ///
    /// # Arguments
    /// * `signing_key` - The decrypted signing key from UserKeyManager
    /// * `identity` - The SigKey identity used in database auth settings
    pub(crate) fn set_provided_key(&mut self, signing_key: PrivateKey, identity: SigKey) {
        self.provided_signing_key = Some((signing_key, identity));
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
    /// let mut encrypted = tx.get_store::<PasswordStore<DocStore>>("secrets")?;
    /// encrypted.initialize("my_password", Doc::new())?;
    ///
    /// // PasswordStore registers the encryptor internally
    /// let docstore = encrypted.inner()?;
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
        let subtree = subtree.into();
        let stages = self.projected.lock().unwrap();
        if self.projected_sealed.load(Ordering::Acquire) {
            return Err(TransactionError::TransactionAlreadyCommitted.into());
        }
        if stages.contains_key(&subtree) {
            return Err(StoreError::InvalidOperation {
                store: subtree,
                operation: "register_encryptor".to_string(),
                reason: "projection context already staged".to_string(),
            }
            .into());
        }
        self.encryptors.lock().unwrap().insert(subtree, encryptor);
        Ok(())
    }

    fn physical_record_key(&self, store: &str, logical_key: &[u8]) -> Result<Vec<u8>> {
        self.encryptors.lock().unwrap().get(store).map_or_else(
            || Ok(logical_key.to_vec()),
            |encryptor| encryptor.physical_record_key(logical_key),
        )
    }

    fn encrypt_record(&self, store: &str, logical_key: &[u8], value: &[u8]) -> Result<Vec<u8>> {
        self.encryptors.lock().unwrap().get(store).map_or_else(
            || Ok(value.to_vec()),
            |encryptor| encryptor.encrypt_record(logical_key, value),
        )
    }

    fn decrypt_record(&self, store: &str, key: &[u8], value: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        self.encryptors.lock().unwrap().get(store).map_or_else(
            || Ok((key.to_vec(), value.to_vec())),
            |encryptor| encryptor.decrypt_record(key, value),
        )
    }

    pub(crate) fn database_id(&self) -> &ID {
        self.db.root_id()
    }

    /// Decrypt bytes if an encryptor is registered, otherwise return them unchanged.
    ///
    /// This is used throughout Transaction to transparently decrypt encrypted payloads
    /// before deserializing into CRDT types.
    fn decrypt_if_needed(&self, subtree: &str, data: &[u8]) -> Result<Vec<u8>> {
        if let Some(encryptor) = self.encryptors.lock().unwrap().get(subtree) {
            encryptor.decrypt(data)
        } else {
            Ok(data.to_vec())
        }
    }

    /// Encrypt bytes if an encryptor is registered for the subtree, otherwise return them unchanged.
    fn encrypt_if_needed(&self, subtree: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
        if let Some(encryptor) = self.encryptors.lock().unwrap().get(subtree) {
            encryptor.encrypt(plaintext)
        } else {
            Ok(plaintext.to_vec())
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
    /// let txn = database.new_transaction().await?;
    /// let settings = txn.get_settings()?;
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
    /// Called by `Database::create()` to override the placeholder root with
    /// `ID::default()`, making the entry a proper top-level root.
    ///
    /// # Arguments
    /// * `root` - The tree root ID to set (use `ID::default()` for top-level roots)
    pub(crate) fn set_entry_root(&self, root: ID) -> Result<()> {
        let stages = self.projected.lock().unwrap();
        if !stages.is_empty() || self.projected_sealed.load(Ordering::Acquire) {
            return Err(TransactionError::TransactionAlreadyCommitted.into());
        }
        let mut builder_ref = self.entry_builder.lock().unwrap();
        let builder = builder_ref
            .as_mut()
            .ok_or(TransactionError::TransactionAlreadyCommitted)?;
        builder.set_root_mut(root);
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
        let stages = self.projected.lock().unwrap();
        if !stages.is_empty() || self.projected_sealed.load(Ordering::Acquire) {
            return Err(TransactionError::TransactionAlreadyCommitted.into());
        }
        let mut builder_ref = self.entry_builder.lock().unwrap();
        let builder = builder_ref
            .as_mut()
            .ok_or(TransactionError::TransactionAlreadyCommitted)?;

        // Parse existing metadata if present, or create new
        let mut metadata = builder
            .metadata()
            .and_then(|m| serde_json::from_slice::<EntryMetadata>(m).ok())
            .unwrap_or(EntryMetadata {
                settings_snapshot: Snapshot::EMPTY,
                entropy: None,
            });

        // Set entropy
        metadata.entropy = Some(entropy);

        // Serialize and set metadata
        let metadata_json = serde_json::to_vec(&metadata)?;
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
        data: impl Into<Vec<u8>>,
    ) -> Result<()> {
        let subtree = subtree.as_ref();
        let data = data.into();

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
            let backend = self.db.ops();
            // FIXME: we should get the subtree snapshot while still using the parent pointers
            Some(
                backend
                    .store_snapshot(self.db.root_id(), subtree)
                    .await?
                    .into_tips(),
            )
        } else {
            None
        };

        let stages = self.projected.lock().unwrap();
        if stages.contains_key(subtree) || self.projected_sealed.load(Ordering::Acquire) {
            return Err(TransactionError::TransactionAlreadyCommitted.into());
        }
        let mut builder_ref = self.entry_builder.lock().unwrap();
        let builder = builder_ref
            .as_mut()
            .ok_or(TransactionError::TransactionAlreadyCommitted)?;

        builder.set_subtree_data_mut(subtree.to_string(), data);
        if let Some(tips) = tips {
            builder.set_subtree_parents_mut(subtree, tips);
        }

        Ok(())
    }

    /// Stage one typed delta and its logical/physical read overlay at one revision.
    /// All fallible work happens outside the state lock; a competing writer forces
    /// recomputation rather than allowing an unlocked snapshot to overwrite it.
    pub(crate) async fn stage_projected_delta<D: CRDT + Send + Sync + 'static>(
        &self,
        store: &str,
        projection: &dyn RecordProjection<D>,
        delta: D,
    ) -> Result<()> {
        self.init_subtree_parents(store).await?;
        let stages = self.projected.lock().unwrap();
        let descriptor = self.encrypted_projection_descriptor(store, projection.descriptor());
        drop(stages);
        let incoming = projection.mutations(&delta)?.collect::<Result<Vec<_>>>()?;
        loop {
            let snapshot = self.projected.lock().unwrap().get(store).cloned();
            if let Some(state) = &snapshot
                && state.descriptor != descriptor
            {
                return Err(StoreError::TypeMismatch {
                    store: store.to_string(),
                    expected: format!("{:?}", state.descriptor),
                    actual: format!("{descriptor:?}"),
                }
                .into());
            }
            // Validate each incoming value before installing it; the accumulated
            // canonical Entry bytes are only produced once at commit.
            serde_json::to_vec(&delta)?;
            let canonical = if let Some(state) = &snapshot {
                let current = state
                    .canonical
                    .as_any()
                    .downcast_ref::<D>()
                    .ok_or_else(|| StoreError::TypeMismatch {
                        store: store.to_string(),
                        expected: format!("{:?}", state.descriptor),
                        actual: format!("{descriptor:?} (different delta type)"),
                    })?;
                current.merge(&delta)?
            } else {
                delta.clone()
            };
            let mut logical = snapshot
                .as_ref()
                .map_or_else(RecordMutations::new, |s| s.logical.clone());
            let mut physical = snapshot
                .as_ref()
                .map_or_else(RecordMutations::new, |s| s.physical.clone());
            for mutation in &incoming {
                let (raw_key, value) = match mutation {
                    RecordMutation::Put { key, value } => (key, Some(value)),
                    RecordMutation::Delete { key } => (key, None),
                };
                let Some(key) = projection.normalize_record_key(raw_key)? else {
                    continue;
                };
                let conflicts = logical
                    .keys()
                    .filter(|staged| projection.staged_keys_conflict(staged, &key))
                    .cloned()
                    .collect::<Vec<_>>();
                let physical_conflicts = conflicts
                    .iter()
                    .map(|key| self.physical_record_key(store, key))
                    .collect::<Result<Vec<_>>>()?;
                let physical_key = self.physical_record_key(store, &key)?;
                let encrypted = value
                    .map(|value| self.encrypt_record(store, &key, value))
                    .transpose()?;
                for conflict in &conflicts {
                    logical.remove(conflict);
                }
                for conflict in physical_conflicts {
                    physical.remove(&conflict);
                }
                logical.insert(key, value.cloned());
                physical.insert(physical_key, encrypted);
            }
            let mut stages = self.projected.lock().unwrap();
            if self.projected_sealed.load(Ordering::Acquire) {
                return Err(TransactionError::TransactionAlreadyCommitted.into());
            }
            if stages.get(store).map(|s| s.revision) != snapshot.as_ref().map(|s| s.revision) {
                continue;
            }
            let mut builder_ref = self.entry_builder.lock().unwrap();
            let builder = builder_ref
                .as_mut()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?;
            if builder.data(store).is_ok() {
                return Err(StoreError::InvalidOperation {
                    store: store.to_string(),
                    operation: "stage_projected_delta".to_string(),
                    reason: "subtree already has staged data outside the projected state"
                        .to_string(),
                }
                .into());
            }
            // No await or fallible transformation between canonical and overlay install.
            stages.insert(
                store.to_string(),
                ProjectedStage {
                    revision: snapshot.map_or(1, |s| s.revision + 1),
                    descriptor: descriptor.clone(),
                    canonical: Arc::new(canonical),
                    logical,
                    physical,
                },
            );
            return Ok(());
        }
    }

    /// Read an unlocked password projection without asking a read-only client to
    /// publish server records. The server authorizes the history before decryption.
    /// Remote reads currently fold the full state before selecting a physical key;
    /// an independently authenticated read-only record view can optimize this later.
    pub(crate) async fn unlocked_projected_get<S: Store>(
        &self,
        store: &str,
        projection: &dyn RecordProjection<S::Data>,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>>
    where
        S::Data: Send,
    {
        #[cfg(all(unix, feature = "service"))]
        if self.db.instance()?.remote_connection().is_some() {
            loop {
                let revision = self
                    .projected
                    .lock()
                    .unwrap()
                    .get(store)
                    .map_or(0, |s| s.revision);
                let state = self
                    .unlocked_store_state::<crate::store::PasswordStore<S>>(store)
                    .await?;
                let history = self.project_state(store, projection, &state)?;
                if self
                    .projected
                    .lock()
                    .unwrap()
                    .get(store)
                    .map_or(0, |s| s.revision)
                    == revision
                {
                    return self
                        .projected_get_with_history(store, projection, key, Some(&history))
                        .await;
                }
            }
        }
        self.projected_get(store, projection, key).await
    }

    pub(crate) async fn unlocked_projected_scan_page<S: Store>(
        &self,
        store: &str,
        projection: &dyn RecordProjection<S::Data>,
        cursor: Option<&TableCursor>,
        limit: usize,
    ) -> Result<(crate::backend::RecordPage, Option<TableCursor>)>
    where
        S::Data: Send,
    {
        #[cfg(all(unix, feature = "service"))]
        if self.db.instance()?.remote_connection().is_some() {
            let revision = self
                .projected
                .lock()
                .unwrap()
                .get(store)
                .map_or(0, |s| s.revision);
            let conn = self.db.instance()?.remote_connection().unwrap();
            let root = self.db.root_id().clone();
            let identity = self.db.auth_identity().cloned().unwrap_or_default();
            let (state, frontier) = conn
                .get_store_state_with_decrypt_and_frontier::<S::Data>(
                    root.clone(),
                    identity.clone(),
                    store.to_string(),
                    <crate::store::PasswordStore<S> as crate::store::Registered>::type_id(),
                    S::state_model().descriptor(),
                    |bytes| self.decrypt_if_needed(store, bytes),
                )
                .await?;
            let history = self.project_state(store, projection, &state)?;
            let Some(frontier) = frontier else {
                return Err(StoreError::RecordMaintenanceUnavailable {
                    store: store.into(),
                }
                .into());
            };
            let (page, next) = self
                .projected_scan_with_history(
                    store,
                    projection,
                    cursor,
                    limit,
                    history,
                    Some(frontier.clone()),
                )
                .await?;
            // The fold is pinned to its source tips, but a new Verified tip can
            // arrive during any await. Never return a page for a mixed frontier.
            self.check_remote_scan_frontier(
                store,
                revision,
                &frontier,
                conn.get_verified_tips(root, identity),
            )
            .await?;
            return Ok((page, next));
        }
        self.projected_record_scan_page(store, projection, cursor, limit)
            .await
    }

    #[cfg(all(unix, feature = "service"))]
    async fn check_remote_scan_frontier<F>(
        &self,
        store: &str,
        revision: u64,
        frontier: &Snapshot,
        check: F,
    ) -> Result<()>
    where
        F: Future<Output = Result<Snapshot>>,
    {
        let current = check.await;
        // Check the overlay after the network await, including when it fails.
        if self
            .projected
            .lock()
            .unwrap()
            .get(store)
            .map_or(0, |s| s.revision)
            != revision
        {
            return Err(StoreError::StaleCursor {
                store: store.into(),
            }
            .into());
        }
        if current? != *frontier {
            return Err(StoreError::StaleCursor {
                store: store.into(),
            }
            .into());
        }
        Ok(())
    }

    /// Read a typed row from the fixed historical view, overlaid with staged changes.
    pub(crate) async fn projected_get<D: CRDT + Send>(
        &self,
        store: &str,
        projection: &dyn RecordProjection<D>,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        self.projected_get_with_history(store, projection, key, None)
            .await
    }

    async fn projected_get_with_history<D: CRDT + Send>(
        &self,
        store: &str,
        projection: &dyn RecordProjection<D>,
        key: &[u8],
        history: Option<&BTreeMap<Vec<u8>, Vec<u8>>>,
    ) -> Result<Option<Vec<u8>>> {
        let Some(key) = projection.normalize_record_key(key)? else {
            return Ok(None);
        };
        loop {
            let snapshot = self.projected.lock().unwrap().get(store).cloned();
            let revision = snapshot.as_ref().map_or(0, |s| s.revision);
            let descriptor = self.encrypted_projection_descriptor(store, projection.descriptor());
            if let Some(state) = &snapshot
                && state.descriptor != descriptor
            {
                return Err(StoreError::TypeMismatch {
                    store: store.into(),
                    expected: format!("{:?}", state.descriptor),
                    actual: format!("{descriptor:?}"),
                }
                .into());
            }
            let staged = snapshot.as_ref().and_then(|s| {
                s.logical.get(&key).cloned().or_else(|| {
                    s.logical
                        .keys()
                        .any(|staged| projection.staged_key_shadows_cached(staged, &key))
                        .then_some(None)
                })
            });
            let value = if let Some(value) = staged {
                value
            } else {
                let physical = self.physical_record_key(store, &key)?;
                let value = if let Some(history) = history {
                    history.get(&physical).cloned()
                } else {
                    match self.record_view(store, projection).await {
                        Err(err) if err.is_unsupported_store_state() => self
                            .projected_history::<D>(store, projection)
                            .await?
                            .get(&physical)
                            .cloned(),
                        Err(err) => return Err(err),
                        Ok(view) => {
                            match self.db.ops().store_state_record_get(&view, &physical).await {
                                Err(err) if err.is_unsupported_store_state() => self
                                    .projected_history::<D>(store, projection)
                                    .await?
                                    .get(&physical)
                                    .cloned(),
                                Err(err) if err.is_invalid_store_state_view() => {
                                    self.invalidate_record_view(store);
                                    match self.record_view(store, projection).await {
                                        Err(err) if err.is_unsupported_store_state() => self
                                            .projected_history::<D>(store, projection)
                                            .await?
                                            .get(&physical)
                                            .cloned(),
                                        Err(err) => return Err(err),
                                        Ok(view) => match self
                                            .db
                                            .ops()
                                            .store_state_record_get(&view, &physical)
                                            .await
                                        {
                                            Err(err) if err.is_unsupported_store_state() => self
                                                .projected_history::<D>(store, projection)
                                                .await?
                                                .get(&physical)
                                                .cloned(),
                                            result => result?,
                                        },
                                    }
                                }
                                Err(err) => return Err(err),
                                Ok(value) => value,
                            }
                        }
                    }
                };
                value
                    .map(|value| {
                        self.decode_projected_record(store, &physical, &value)
                            .map(|(_, value)| value)
                    })
                    .transpose()?
            };
            if self.encrypted_projection_descriptor(store, projection.descriptor()) != descriptor {
                continue;
            }
            if self
                .projected
                .lock()
                .unwrap()
                .get(store)
                .map_or(0, |s| s.revision)
                == revision
            {
                return Ok(value);
            }
        }
    }

    /// Read the unlocked Store's typed state. On a service connection the
    /// server authorizes the canonical read before returning a capability
    /// refusal; only then may this client decrypt and fold Entry history.
    pub(crate) async fn unlocked_store_state<S: Store>(&self, store: &str) -> Result<S::Data>
    where
        S::Data: Send,
    {
        #[cfg(all(unix, feature = "service"))]
        if let Some(conn) = self.db.instance()?.remote_connection() {
            return conn
                .get_store_state_with_decrypt::<S::Data>(
                    self.db.root_id().clone(),
                    self.db.auth_identity().cloned().unwrap_or_default(),
                    store.to_string(),
                    S::type_id(),
                    S::state_model().descriptor(),
                    |bytes| self.decrypt_if_needed(store, bytes),
                )
                .await;
        }
        self.get_full_state_with_descriptor(store, S::state_model().descriptor())
            .await
    }

    /// Recordless fallback reduces typed history before projecting into physical order.
    async fn projected_history<D: CRDT + Send>(
        &self,
        store: &str,
        projection: &dyn RecordProjection<D>,
    ) -> Result<BTreeMap<Vec<u8>, Vec<u8>>> {
        let state: D = self
            .get_full_state_with_descriptor(store, projection.descriptor())
            .await?;
        self.project_state(store, projection, &state)
    }

    fn project_state<D: CRDT>(
        &self,
        store: &str,
        projection: &dyn RecordProjection<D>,
        state: &D,
    ) -> Result<BTreeMap<Vec<u8>, Vec<u8>>> {
        let mut records = BTreeMap::new();
        for mutation in projection.mutations(state)? {
            match mutation? {
                RecordMutation::Put { key, value } => {
                    let Some(key) = projection.normalize_record_key(&key)? else {
                        continue;
                    };
                    records.insert(
                        self.physical_record_key(store, &key)?,
                        self.encrypt_record(store, &key, &value)?,
                    );
                }
                RecordMutation::Delete { key } => {
                    if let Some(key) = projection.normalize_record_key(&key)? {
                        records.remove(&self.physical_record_key(store, &key)?);
                    }
                }
            }
        }
        Ok(records)
    }

    fn decode_projected_record(
        &self,
        store: &str,
        physical: &[u8],
        value: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let (logical, value) = self.decrypt_record(store, physical, value)?;
        if self.physical_record_key(store, &logical)? != physical {
            return Err(StoreError::DataCorruption {
                store: store.into(),
                reason: "record key does not match authenticated logical key".into(),
            }
            .into());
        }
        Ok((logical, value))
    }

    /// Scan a typed projection using the backend's real immutable RecordView.
    pub(crate) async fn projected_record_scan_page<D: CRDT + Send>(
        &self,
        store: &str,
        projection: &dyn RecordProjection<D>,
        cursor: Option<&TableCursor>,
        limit: usize,
    ) -> Result<(crate::backend::RecordPage, Option<TableCursor>)> {
        // Resolve before taking the immutable overlay snapshot. The page scanner
        // rejects any mutation racing resolution or a later backend fetch.
        let view = match self.record_view(store, projection).await {
            Err(err) if err.is_unsupported_store_state() => None,
            Err(err) => return Err(err),
            Ok(view) => Some(view),
        };
        let history = if view.is_none() {
            Some(self.projected_history::<D>(store, projection).await?)
        } else {
            None
        };
        let backend = self.db.ops();
        self.projected_scan_page(
            store,
            projection.descriptor(),
            cursor,
            limit,
            None,
            |after, count| {
                let view = view.clone();
                let history = history.clone();
                async move {
                    if let Some(view) = view {
                        let result = backend
                            .store_state_record_scan(
                                &view,
                                &RecordRange::default(),
                                after.as_deref(),
                                count,
                            )
                            .await;
                        match result {
                            Err(err) if err.is_invalid_store_state_view() => {
                                self.invalidate_record_view(store);
                                match self.record_view(store, projection).await {
                                    Ok(view) => match backend
                                        .store_state_record_scan(
                                            &view,
                                            &RecordRange::default(),
                                            after.as_deref(),
                                            count,
                                        )
                                        .await
                                    {
                                        Err(err) if err.is_unsupported_store_state() => {
                                            let history = self
                                                .projected_history::<D>(store, projection)
                                                .await?;
                                            page_from_history(&history, after.as_deref(), count)
                                        }
                                        result => result,
                                    },
                                    Err(err) if err.is_unsupported_store_state() => {
                                        let history =
                                            self.projected_history::<D>(store, projection).await?;
                                        page_from_history(&history, after.as_deref(), count)
                                    }
                                    Err(err) => Err(err),
                                }
                            }
                            Err(err) if err.is_unsupported_store_state() => {
                                let history =
                                    self.projected_history::<D>(store, projection).await?;
                                page_from_history(&history, after.as_deref(), count)
                            }
                            result => result,
                        }
                    } else {
                        let mut rows = history
                            .unwrap()
                            .into_iter()
                            .filter(|(key, _)| after.as_ref().is_none_or(|after| key > after))
                            .take(count + 1)
                            .collect::<Vec<_>>();
                        let next = (rows.len() > count).then(|| rows[count - 1].0.clone());
                        rows.truncate(count);
                        Ok(crate::backend::RecordPage {
                            records: rows,
                            next,
                        })
                    }
                }
            },
        )
        .await
    }

    async fn projected_scan_with_history<D: CRDT + Send>(
        &self,
        store: &str,
        projection: &dyn RecordProjection<D>,
        cursor: Option<&TableCursor>,
        limit: usize,
        history: BTreeMap<Vec<u8>, Vec<u8>>,
        frontier: Option<crate::Snapshot>,
    ) -> Result<(crate::backend::RecordPage, Option<TableCursor>)> {
        self.projected_scan_page(
            store,
            projection.descriptor(),
            cursor,
            limit,
            frontier,
            |after, count| {
                let mut rows = history
                    .iter()
                    .filter(|(key, _)| after.as_ref().is_none_or(|after| *key > after))
                    .take(count.saturating_add(1))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<Vec<_>>();
                let next = (rows.len() > count).then(|| rows[count - 1].0.clone());
                rows.truncate(count);
                std::future::ready(Ok(crate::backend::RecordPage {
                    records: rows,
                    next,
                }))
            },
        )
        .await
    }

    /// Scan a typed projection over one immutable transaction overlay. The
    /// fetcher supplies persisted physical-key pages; it must honor exclusive
    /// `after` and return pages in physical order.
    pub(crate) async fn projected_scan_page<F, Fut>(
        &self,
        store: &str,
        descriptor: ProjectionDescriptor,
        cursor: Option<&TableCursor>,
        limit: usize,
        frontier: Option<crate::Snapshot>,
        mut fetch: F,
    ) -> Result<(crate::backend::RecordPage, Option<TableCursor>)>
    where
        F: FnMut(Option<Vec<u8>>, usize) -> Fut,
        Fut: Future<Output = Result<crate::backend::RecordPage>>,
    {
        let context = self.encrypted_projection_descriptor(store, descriptor.clone());
        let snapshot = self.projected.lock().unwrap().get(store).cloned();
        let revision = snapshot.as_ref().map_or(0, |state| state.revision);
        if snapshot
            .as_ref()
            .is_some_and(|state| state.descriptor != context)
        {
            return Err(StoreError::StaleCursor {
                store: store.into(),
            }
            .into());
        }
        let after = match cursor.map(|cursor| &cursor.0) {
            None => None,
            Some(CursorKind::Projected {
                view,
                revision: cursor_revision,
                store: cursor_store,
                projection,
                frontier: cursor_frontier,
                last_physical_key,
            }) if *view == self.view_id
                && *cursor_revision == revision
                && cursor_store == store
                && projection == &context
                && cursor_frontier == &frontier =>
            {
                Some(last_physical_key.clone())
            }
            _ => {
                return Err(StoreError::StaleCursor {
                    store: store.into(),
                }
                .into());
            }
        };
        let check_revision = || -> Result<()> {
            let stages = self.projected.lock().unwrap();
            if stages.get(store).map_or(0, |state| state.revision) != revision
                || self.encrypted_projection_descriptor(store, descriptor.clone()) != context
            {
                return Err(StoreError::StaleCursor {
                    store: store.into(),
                }
                .into());
            }
            Ok(())
        };
        if limit == 0 {
            check_revision()?;
            return Ok((crate::backend::RecordPage::default(), None));
        }
        // Without staged rows, the backend already supplies a bounded ordered
        // page. Avoid copying it into a merge map, but keep the race checks.
        if snapshot.is_none() {
            let result = fetch(after, limit).await;
            check_revision()?;
            let mut page = result?;
            let next = page.next.take().map(|last_physical_key| {
                TableCursor(CursorKind::Projected {
                    view: self.view_id,
                    revision,
                    store: store.into(),
                    projection: context.clone(),
                    frontier: frontier.clone(),
                    last_physical_key,
                })
            });
            for (key, value) in &mut page.records {
                (*key, *value) = self.decode_projected_record(store, key, value)?;
            }
            check_revision()?;
            return Ok((page, next));
        }
        let mut merged = snapshot
            .as_ref()
            .map(|state| &state.physical)
            .into_iter()
            .flat_map(|mutations| mutations.iter())
            .filter_map(|(key, value)| {
                (after.as_ref().is_none_or(|after| key > after))
                    .then(|| value.as_ref().map(|value| (key.clone(), value.clone())))
                    .flatten()
            })
            .collect::<BTreeMap<_, _>>();
        let mut backend_after = after;
        let mut backend_has_more = true;
        while backend_has_more {
            let page = fetch(backend_after.clone(), limit.max(1)).await;
            check_revision()?; // A racing mutation wins even if the fetch failed.
            let page = page?;
            for (key, value) in page.records {
                if let Some(staged) = snapshot.as_ref().and_then(|state| state.physical.get(&key)) {
                    if let Some(staged) = staged {
                        merged.insert(key, staged.clone());
                    }
                } else {
                    merged.insert(key, value);
                }
            }
            backend_after = page.next;
            backend_has_more = backend_after.is_some();
            if merged
                .keys()
                .nth(limit)
                .is_some_and(|end| backend_after.as_ref().is_some_and(|last| last >= end))
            {
                break;
            }
        }
        let mut records = merged
            .into_iter()
            .take(limit.saturating_add(1))
            .collect::<Vec<_>>();
        let has_more = records.len() > limit || backend_has_more;
        records.truncate(limit);
        let next = has_more.then(|| {
            TableCursor(CursorKind::Projected {
                view: self.view_id,
                revision,
                store: store.into(),
                projection: context.clone(),
                frontier: frontier.clone(),
                last_physical_key: records.last().unwrap().0.clone(),
            })
        });
        for (key, value) in &mut records {
            (*key, *value) = self.decode_projected_record(store, key, value)?;
        }
        check_revision()?;
        Ok((
            crate::backend::RecordPage {
                records,
                next: None,
            },
            next,
        ))
    }

    fn encrypted_projection_descriptor(
        &self,
        store: &str,
        descriptor: ProjectionDescriptor,
    ) -> ProjectionDescriptor {
        self.encryptors
            .lock()
            .unwrap()
            .get(store)
            .map_or(descriptor.clone(), |encryptor| {
                encryptor.projection_descriptor(descriptor)
            })
    }

    async fn publish_record_view<D: CRDT>(
        &self,
        store: &str,
        projection: &dyn RecordProjection<D>,
        request: crate::backend::StoreStateRequest,
        entries: &[Entry],
    ) -> Result<RecordView> {
        if !self.encryptors.lock().unwrap().contains_key(store) {
            return state::publish_records(
                self.db.ops(),
                request,
                entries
                    .iter()
                    .filter_map(|entry| entry.data(store).ok().map(|bytes| bytes.as_slice())),
                projection,
            )
            .await;
        }

        let token = self.db.ops().begin_store_state_staging(request).await?;
        let result = async {
            let mut chunk = Vec::new();
            let mut chunk_bytes = 0;
            let mut sequence = 0;
            for entry in entries {
                if let Ok(bytes) = entry.data(store) {
                    let plaintext = self.decrypt_if_needed(store, bytes)?;
                    let delta: D = serde_json::from_slice(&plaintext)?;
                    for mutation in projection.mutations(&delta)? {
                        let mutation = match mutation? {
                            RecordMutation::Put { key, value } => RecordMutation::Put {
                                key: self.physical_record_key(store, &key)?,
                                value: self.encrypt_record(store, &key, &value)?,
                            },
                            RecordMutation::Delete { key } => RecordMutation::Delete {
                                key: self.physical_record_key(store, &key)?,
                            },
                        };
                        let size = serde_json::to_vec(&mutation)?.len();
                        if size > state::CHUNK_BYTES {
                            return Err(crate::backend::BackendError::RecordTooLarge {
                                encoded_bytes: size,
                            }
                            .into());
                        }
                        if !chunk.is_empty()
                            && (chunk.len() == 128 || chunk_bytes + size > state::CHUNK_BYTES)
                        {
                            state::stage_chunk(self.db.ops(), &token, &mut sequence, &mut chunk)
                                .await?;
                            chunk_bytes = 0;
                        }
                        chunk_bytes += size;
                        chunk.push(mutation);
                    }
                }
            }
            if !chunk.is_empty() {
                state::stage_chunk(self.db.ops(), &token, &mut sequence, &mut chunk).await?;
            }
            self.db.ops().publish_store_state(token.clone()).await
        }
        .await;
        if result.is_err() {
            let _ = self.db.ops().abort_store_state(token).await;
        }
        result
    }

    pub(crate) async fn ensure_record_view<D: CRDT>(
        &self,
        store: &str,
        projection: &dyn RecordProjection<D>,
    ) -> Result<()> {
        self.record_view(store, projection).await.map(|_| ())
    }

    fn invalidate_record_view(&self, store: &str) {
        self.record_views
            .lock()
            .unwrap()
            .retain(|(name, _), _| name != store);
    }

    async fn record_view<D: CRDT>(
        &self,
        store: &str,
        projection: &dyn RecordProjection<D>,
    ) -> Result<RecordView> {
        self.init_subtree_parents(store).await?;
        let parents = self
            .entry_builder
            .lock()
            .unwrap()
            .as_ref()
            .ok_or(TransactionError::TransactionAlreadyCommitted)?
            .subtree_parents(store)
            .unwrap_or_default();
        let source_key = create_merge_cache_id(&parents).to_string().into_bytes();
        let descriptor = self.encrypted_projection_descriptor(store, projection.descriptor());
        let cache_key = (store.to_string(), descriptor.clone());
        if let Some(view) = self.record_views.lock().unwrap().get(&cache_key).cloned() {
            return Ok(view);
        }
        let request = state::records_request(
            self.db.root_id(),
            store,
            descriptor,
            source_key,
            crate::backend::CacheScope::Shared,
        );
        let view = if let Some(view) = self.db.ops().resolve_store_state(&request).await? {
            view
        } else {
            let boundary = Snapshot::from(parents);
            let entries = self
                .db
                .ops()
                .store_at(self.db.root_id(), store, &boundary)
                .await?;
            self.publish_record_view(store, projection, request, &entries)
                .await?
        };
        self.record_views
            .lock()
            .unwrap()
            .insert(cache_key, view.clone());
        Ok(view)
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

        // Skip special system subtrees to avoid circular dependencies
        let is_system_subtree =
            subtree_name == INDEX || subtree_name == SETTINGS || subtree_name == ROOT;

        if is_system_subtree && self.system_subtrees_locked.load(Ordering::Acquire) {
            return Err(TransactionError::SystemSubtreeLocked { name: subtree_name }.into());
        }

        // Initialize subtree parents before checking _index
        self.init_subtree_parents(&subtree_name).await?;

        if is_system_subtree {
            // System subtrees don't use _index registration
            return T::load(self, subtree_name).await;
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
            T::load(self, subtree_name).await
        } else {
            // New subtree - register adds it to _index
            T::register(self, subtree_name).await
        }
    }

    /// Get the subtree tips reachable from the given main tree entries.
    async fn get_subtree_tips(&self, subtree_name: &str, main_parents: &[ID]) -> Result<Vec<ID>> {
        let boundary = Snapshot::from(main_parents.to_vec());
        self.db
            .ops()
            .store_snapshot_at(self.db.root_id(), subtree_name, &boundary)
            .await
            .map(Snapshot::into_tips)
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
    /// A `Result<Option<T>>`:
    ///
    /// # Behavior
    /// - If the subtree doesn't exist or has no data, returns `Ok(None)`
    /// - If the subtree exists but has empty data (empty string or whitespace), returns `Ok(None)`
    /// - Otherwise deserializes the JSON data to type `T` and returns `Ok(Some(T))`
    ///
    /// # Errors
    /// Returns an error if the transaction has already been committed or if the
    /// subtree data exists but cannot be deserialized to type `T`.
    pub fn get_local_data<T>(&self, subtree_name: impl AsRef<str>) -> Result<Option<T>>
    where
        T: Data,
    {
        let subtree_name = subtree_name.as_ref();
        let stages = self.projected.lock().unwrap();
        let builder_ref = self.entry_builder.lock().unwrap();
        let builder = builder_ref
            .as_ref()
            .ok_or(TransactionError::TransactionAlreadyCommitted)?;

        if let Some(stage) = stages.get(subtree_name) {
            return stage.canonical.bytes().and_then(|data| {
                serde_json::from_slice(&data).map(Some).map_err(|e| {
                    TransactionError::StoreDeserializationFailed {
                        store: subtree_name.to_string(),
                        reason: e.to_string(),
                    }
                    .into()
                })
            });
        }
        if let Ok(data) = builder.data(subtree_name) {
            if data.is_empty() {
                Ok(None)
            } else {
                serde_json::from_slice(data).map(Some).map_err(|e| {
                    TransactionError::StoreDeserializationFailed {
                        store: subtree_name.to_string(),
                        reason: e.to_string(),
                    }
                    .into()
                })
            }
        } else {
            Ok(None)
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
        T: CRDT + Send,
    {
        self.get_full_state_with_descriptor::<T>(
            subtree_name,
            ProjectionDescriptor {
                name: "eidetica/opaque".to_string(),
                version: 0,
            },
        )
        .await
    }

    pub(crate) async fn get_full_state_with_descriptor<T>(
        &self,
        subtree_name: impl AsRef<str> + Send,
        descriptor: ProjectionDescriptor,
    ) -> Result<T>
    where
        T: CRDT + Send,
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
            let current_database_snapshot = self.db.ops().snapshot(self.db.root_id()).await?;

            // Set-equal comparison via Snapshot canonical form.
            let parents_snapshot = Snapshot::from(main_parents.clone());
            let tips = if parents_snapshot == current_database_snapshot {
                let backend = self.db.ops();
                backend
                    .store_snapshot(self.db.root_id(), subtree_name)
                    .await?
                    .into_tips()
            } else {
                // This transaction uses custom tips - use special handler
                self.db
                    .ops()
                    .store_snapshot_at(self.db.root_id(), subtree_name, &parents_snapshot)
                    .await?
                    .into_tips()
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
        self.compute_subtree_state_merge_based(subtree_name, &parents, &descriptor)
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
        descriptor: &ProjectionDescriptor,
    ) -> Result<T>
    where
        T: CRDT + Send,
    {
        // Base case: no entries
        if entry_ids.is_empty() {
            return Ok(T::default());
        }

        let subtree_name = subtree_name.as_ref();

        // If we have a single entry, compute its state recursively
        if entry_ids.len() == 1 {
            return self
                .compute_single_entry_state_recursive(subtree_name, &entry_ids[0], descriptor)
                .await;
        }

        // Multiple entries: check multi-tip cache first
        let cache_id = create_merge_cache_id(entry_ids);

        let cache_request = state::opaque_request(
            self.db.root_id(),
            subtree_name,
            descriptor.clone(),
            cache_id.to_string().into_bytes(),
            crate::backend::CacheScope::Shared,
        );
        if let Some(bytes) = state::load_cached(self.db.ops(), &cache_request).await? {
            let decrypted = self.decrypt_if_needed(subtree_name, &bytes)?;
            let result: T = serde_json::from_slice(&decrypted)?;
            return Ok(result);
        }

        // Cache miss: resolve the merge base and the path to fold in a
        // single call, so both come from one view of the store.
        let merge = self
            .db
            .ops()
            .compute_merge_state(self.db.root_id(), subtree_name, entry_ids)
            .await?;

        let result = match &merge.merge_base {
            Some(base) => {
                // Compute the base state recursively, then fold the path
                // entries (deduplicated, height/ID sorted) on top of it.
                let state: T = self
                    .compute_single_entry_state_recursive(subtree_name, base, descriptor)
                    .await?;
                self.merge_path_entries(subtree_name, state, &merge.path)
                    .await?
            }
            // With no merge base the histories are disjoint — a store
            // created independently on both sides of a fork — so fold the
            // full ancestry from a default state. Convergence must not
            // depend on which peer created the store first. `store_at`
            // batch-fetches the whole entries in fold order: one query
            // instead of a per-ID fetch of the entire history.
            None => {
                let boundary = Snapshot::from(entry_ids.to_vec());
                let entries = self
                    .db
                    .ops()
                    .store_at(self.db.root_id(), subtree_name, &boundary)
                    .await?;
                self.fold_store_entries(subtree_name, &entries)?
            }
        };

        // Cache the computed merge result
        let bytes = self.encrypt_if_needed(subtree_name, &serde_json::to_vec(&result)?)?;
        state::store_cached(self.db.ops(), cache_request, bytes).await?;

        Ok(result)
    }

    /// Computes the CRDT state for a single entry using batch fetching.
    ///
    /// Algorithm:
    /// 1. Check if entry state is cached → return it
    /// 2. Fetch all ancestors in one batch query (sorted by height)
    /// 3. Merge all entries in order from root to target
    /// 4. Cache only the final result
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
        descriptor: &'a ProjectionDescriptor,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T>> + Send + 'a>>
    where
        T: CRDT + Send + 'a,
    {
        Box::pin(async move {
            let request = state::opaque_request(
                self.db.root_id(),
                subtree_name,
                descriptor.clone(),
                entry_id.to_string().into_bytes(),
                crate::backend::CacheScope::Shared,
            );
            if let Some(bytes) = state::load_cached(self.db.ops(), &request).await? {
                let decrypted = self.decrypt_if_needed(subtree_name, &bytes)?;
                let result: T = serde_json::from_slice(&decrypted)?;
                return Ok(result);
            }

            // Step 2: Batch fetch all ancestors sorted by height (root first)
            // This single query replaces N recursive queries
            let boundary = Snapshot::from([entry_id.clone()]);
            let entries = self
                .db
                .ops()
                .store_at(self.db.root_id(), subtree_name, &boundary)
                .await?;

            // Step 3: Merge all entries in order (already sorted by height, root first)
            let result: T = self.fold_store_entries(subtree_name, &entries)?;

            // Step 4: Cache only the final result (encrypted if encryptor is registered)
            let bytes = self.encrypt_if_needed(subtree_name, &serde_json::to_vec(&result)?)?;
            state::store_cached(self.db.ops(), request, bytes).await?;

            Ok(result)
        })
    }

    /// Folds already-fetched entries into a CRDT state from the default.
    ///
    /// The entries must be in fold order (height then ID, root first), as
    /// `store_at` returns them.
    fn fold_store_entries<T>(&self, subtree_name: &str, entries: &[Entry]) -> Result<T>
    where
        T: CRDT,
    {
        let mut result = T::default();
        for entry in entries {
            let local_data = if let Ok(data) = entry.data(subtree_name) {
                // Decrypt before deserializing
                let plaintext = self.decrypt_if_needed(subtree_name, data)?;
                serde_json::from_slice::<T>(&plaintext)?
            } else {
                T::default()
            };
            result = result.merge(&local_data)?;
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
    async fn merge_path_entries<T>(
        &self,
        subtree_name: &str,
        mut state: T,
        entry_ids: &[ID],
    ) -> Result<T>
    where
        T: CRDT,
    {
        for entry_id in entry_ids {
            let entry = self.db.ops().get(entry_id).await?;

            // Get local data for this entry in the subtree
            let local_data = if let Ok(data) = entry.data(subtree_name) {
                // Decrypt before deserializing
                let plaintext = self.decrypt_if_needed(subtree_name, data)?;
                serde_json::from_slice::<T>(&plaintext)?
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
        self.commit_inner(None).await
    }

    /// Commit while the caller holds this database's per-tree write lock.
    ///
    /// This is used by operations whose decision depends on a read made before
    /// the write. Ordinary transactions acquire the same lock when persisting.
    pub(crate) async fn commit_under_tree_lock(
        self,
        guard: tokio::sync::OwnedMutexGuard<()>,
    ) -> Result<ID> {
        self.commit_inner(Some(guard)).await
    }

    async fn commit_inner(self, guard: Option<tokio::sync::OwnedMutexGuard<()>>) -> Result<ID> {
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
                let staged_settings = self.get_local_data::<Doc>(SETTINGS)?.unwrap_or_default();
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
            let local_settings = self.get_local_data::<Doc>(SETTINGS)?.unwrap_or_default();
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

            let index_data_opt = builder.data(INDEX).ok().cloned();
            let main_parents = builder.parents().unwrap_or_default();
            let existing_subtrees = builder.subtrees();

            // Find missing subtrees
            let missing = if let Some(ref index_data) = index_data_opt
                && let Ok(index_doc) = serde_json::from_slice::<Doc>(index_data)
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

        // Serialize outside the install lock, then seal exactly the revision
        // whose canonical bytes were serialized. No await under either lock.
        loop {
            let snapshot = self.projected.lock().unwrap().clone();
            let bytes = snapshot
                .iter()
                .map(|(store, stage)| Ok((store.clone(), stage.canonical.bytes()?)))
                .collect::<Result<Vec<_>>>()?;
            let stages = self.projected.lock().unwrap();
            if stages.len() != snapshot.len()
                || stages.iter().any(|(store, stage)| {
                    snapshot.get(store).map(|old| old.revision) != Some(stage.revision)
                })
            {
                continue;
            }
            let mut builder_ref = self.entry_builder.lock().unwrap();
            let mut builder = builder_ref
                .as_ref()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?
                .clone();
            for (subtree_name, tips) in &subtree_tips {
                builder.set_subtree_parents_mut(subtree_name, tips.clone());
            }
            for (store, data) in bytes {
                builder.set_subtree_data_mut(store, data);
            }
            builder.remove_empty_subtrees_mut()?;
            *builder_ref = Some(builder);
            self.projected_sealed.store(true, Ordering::Release);
            break;
        }

        // Add metadata with settings snapshot for all entries
        // Get the backend to access the settings snapshot (do async ops before RefCell borrow)
        let db_snapshot = self.db.snapshot().await?;
        let settings_snapshot = self
            .db
            .ops()
            .store_snapshot_at(self.db.root_id(), SETTINGS, &db_snapshot)
            .await?;

        // Clone the sealed builder (no borrow held across awaits below).
        let mut builder = {
            let builder_cell = self.entry_builder.lock().unwrap();
            builder_cell
                .as_ref()
                .ok_or(TransactionError::TransactionAlreadyCommitted)?
                .clone()
        };

        // Parse existing metadata if present, or create new
        let mut metadata = builder
            .metadata()
            .and_then(|m| serde_json::from_slice::<EntryMetadata>(m).ok())
            .unwrap_or(EntryMetadata {
                settings_snapshot: Snapshot::EMPTY,
                entropy: None,
            });

        // Update settings snapshot
        metadata.settings_snapshot = settings_snapshot;

        // Serialize the metadata
        let metadata_json = serde_json::to_vec(&metadata)?;

        // Add metadata to the entry builder
        builder.set_metadata_mut(metadata_json);

        // Handle authentication configuration before building
        // All entries must now be authenticated - fail if no auth key is configured

        // Use provided signing key
        let signing_key = if let Some((ref provided_key, ref identity)) = self.provided_signing_key
        {
            // Use provided signing key directly (already decrypted from UserKeyManager or device key)
            let key_clone = provided_key.clone();

            // Build AuthInfo from the already-typed SigKey identity
            let sig_builder = AuthInfo::builder().key(identity.clone());

            // Set auth ID on the entry builder (without signature initially)
            builder.set_auth_mut(sig_builder.build());

            Some(key_clone)
        } else {
            // No authentication key configured
            return Err(TransactionError::AuthenticationRequired.into());
        };
        // Encrypt subtree data if encryptors are registered
        // This must happen before building the entry to ensure encrypted data is persisted
        {
            let encryptors = self.encryptors.lock().unwrap();
            for subtree_name in builder.subtrees() {
                if let Some(encryptor) = encryptors.get(&subtree_name)
                    && let Ok(plaintext_data) = builder.data(&subtree_name)
                    && !plaintext_data.is_empty()
                {
                    let ciphertext = encryptor.encrypt(plaintext_data)?;
                    builder.set_subtree_data_mut(subtree_name.clone(), ciphertext);
                }
            }
        }

        // Extract height strategy from settings (defaults to Incremental)
        // If this transaction includes settings updates, merge them to get the effective strategy
        let settings_for_height = if has_settings_update {
            let local_settings = self.get_local_data::<Doc>(SETTINGS)?.unwrap_or_default();
            effective_settings_for_validation.merge(&local_settings)?
        } else {
            effective_settings_for_validation.clone()
        };
        let height_strategy: HeightStrategy = settings_for_height
            .get_json("height_strategy")
            .unwrap_or_default();

        // Compute heights from parent entries using the configured strategy
        {
            let backend = self.db.ops();
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
            entry = entry.with_auth(|auth| auth.signature = Some(signature));
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
                Some(Value::Doc(auth_doc)) => auth_doc.clone().into(),
                None => AuthSettings::new(), // Empty auth - never configured
                Some(_) => {
                    // Auth exists but has wrong type (not a Doc) - this is corrupted
                    return Err(TransactionError::CorruptedAuthConfiguration.into());
                }
            }
        };

        let instance = self.db.instance()?;

        // Validate entry (signature + permissions)
        let is_valid = validator
            .validate_entry(&entry, &auth_settings_for_validation, Some(&instance))
            .await?;

        if !is_valid {
            return Err(TransactionError::EntryValidationFailed.into());
        }

        let verification_status = VerificationStatus::Verified;

        // Get the entry's ID
        let id = entry.id();

        // Write entry through Instance which handles backend storage and callback dispatch
        let instance = self.db.instance()?;
        if let Some(guard) = guard {
            instance
                .put_entry_under_tree_lock(
                    guard,
                    self.db.root_id(),
                    verification_status,
                    entry.clone(),
                    WriteSource::Local,
                )
                .await?;
        } else {
            instance
                .put_entry(
                    self.db.root_id(),
                    verification_status,
                    entry.clone(),
                    WriteSource::Local,
                )
                .await?;
        }

        Ok(id)
    }
}
