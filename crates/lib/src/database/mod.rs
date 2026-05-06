//! Database module provides functionality for managing collections of related entries.
//!
//! A `Database` represents a hierarchical structure of entries, like a traditional database
//! or a branch in a version control system. Each database has a root entry and maintains
//! the history and relationships between entries. Database holds a weak reference to its
//! parent Instance, accessing storage and coordination services through that handle.

use std::future::Future;

use rand::{Rng, RngCore, distributions::Alphanumeric};
use serde_json;

use crate::{
    Error, Instance, Result, Transaction, WeakInstance,
    auth::{
        crypto::{PrivateKey, PublicKey},
        errors::AuthError,
        settings::AuthSettings,
        types::{AuthKey, Permission, SigKey},
        validation::AuthValidator,
    },
    backend::VerificationStatus,
    constants::{ROOT, SETTINGS},
    crdt::Doc,
    entry::{Entry, ID},
    instance::{WriteCallback, WriteEvent, backend::Backend, errors::InstanceError},
    store::{SettingsStore, Store},
};

#[cfg(test)]
mod tests;

/// A signing key bound to its identity in a database's auth settings.
///
/// Pairs the cryptographic signing key with information about how to look up
/// permissions in the database's auth configuration. The identity determines
/// which entry in `_settings.auth` this key maps to.
#[derive(Clone, Debug)]
pub struct DatabaseKey {
    signing_key: Box<PrivateKey>,
    identity: SigKey,
}

impl DatabaseKey {
    /// Identity = pubkey derived from signing key. Most common case.
    pub fn new(signing_key: PrivateKey) -> Self {
        let pubkey = signing_key.public_key();
        Self {
            signing_key: Box::new(signing_key),
            identity: SigKey::from_pubkey(&pubkey),
        }
    }

    /// Identity = explicit SigKey (name, global, delegation, etc.)
    pub fn with_identity(signing_key: PrivateKey, identity: SigKey) -> Self {
        Self {
            signing_key: Box::new(signing_key),
            identity,
        }
    }

    /// Identity = global permission with actual pubkey embedded for verification.
    pub fn global(signing_key: PrivateKey) -> Self {
        let pubkey = signing_key.public_key();
        Self {
            signing_key: Box::new(signing_key),
            identity: SigKey::global(&pubkey),
        }
    }

    /// Identity = key name lookup.
    pub fn with_name(signing_key: PrivateKey, name: impl Into<String>) -> Self {
        Self {
            signing_key: Box::new(signing_key),
            identity: SigKey::from_name(name),
        }
    }

    /// Get the signing key.
    pub fn signing_key(&self) -> &PrivateKey {
        &self.signing_key
    }

    /// Get the public key.
    pub fn public_key(&self) -> PublicKey {
        self.signing_key.public_key()
    }

    /// Get the identity used for auth settings lookup.
    pub fn identity(&self) -> &SigKey {
        &self.identity
    }

    /// Consume self and return the parts.
    pub fn into_parts(self) -> (PrivateKey, SigKey) {
        (*self.signing_key, self.identity)
    }
}

impl From<PrivateKey> for DatabaseKey {
    /// Convert a `PrivateKey` into a `DatabaseKey` with pubkey-derived identity.
    ///
    /// This is equivalent to [`DatabaseKey::new`] and covers the most common case
    /// where the key's identity in auth settings is its own public key.
    fn from(signing_key: PrivateKey) -> Self {
        Self::new(signing_key)
    }
}

/// Represents a collection of related entries, like a traditional database or a branch in a version control system.
///
/// Each `Database` is identified by the ID of its root `Entry` and manages the history of data
/// associated with that root. It interacts with the underlying storage through the Instance handle.
#[derive(Clone, Debug)]
pub struct Database {
    root: ID,
    instance: WeakInstance,
    /// Signing key bound to its auth identity for this database
    key: Option<DatabaseKey>,
}

impl Database {
    /// Creates a new `Database` instance with a user-provided signing key.
    ///
    /// This constructor creates a new database using a signing key that's already in memory
    /// (e.g., from UserKeyManager), without requiring the key to be stored in the backend.
    /// This is the preferred method for creating databases in a User context where keys
    /// are managed separately from the backend.
    ///
    /// The created database will use a `DatabaseKey` for all subsequent operations,
    /// meaning transactions will use the provided key directly rather than looking it up
    /// from backend storage.
    ///
    /// # Auth Bootstrapping
    ///
    /// Auth is always bootstrapped with the signing key as `Admin(0)`. Passing auth
    /// configuration in `initial_settings` is an error — additional keys must be added
    /// via follow-up transactions after creation.
    ///
    /// # Arguments
    /// * `instance` - Instance handle for storage and coordination
    /// * `signing_key` - The signing key to use for the initial commit and subsequent operations.
    ///   This key should already be decrypted and ready to use. The public key is derived
    ///   automatically and used as the key identifier in auth settings.
    /// * `initial_settings` - `Doc` CRDT containing the initial settings for the database.
    ///   Use `Doc::new()` for an empty settings document.
    ///
    /// # Returns
    /// A `Result` containing the new `Database` instance configured with a `DatabaseKey`.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::backend::database::InMemory;
    /// # use eidetica::auth::crypto::generate_keypair;
    /// # use eidetica::crdt::Doc;
    /// # #[tokio::main]
    /// # async fn main() -> Result<()> {
    /// let instance = Instance::open(Box::new(InMemory::new())).await?;
    /// let (signing_key, _public_key) = generate_keypair();
    ///
    /// let mut settings = Doc::new();
    /// settings.set("name", "my_database");
    ///
    /// // Create database with user-managed key (no backend storage needed)
    /// let database = Database::create(&instance, signing_key, settings).await?;
    ///
    /// // All transactions automatically use the provided key
    /// let tx = database.new_transaction().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn create(
        instance: &Instance,
        signing_key: PrivateKey,
        initial_settings: Doc,
    ) -> Result<Self> {
        let mut initial_settings = initial_settings;
        let pubkey = signing_key.public_key();

        // Reject preconfigured auth — Database::create owns auth bootstrapping entirely.
        if initial_settings.get("auth").is_some() {
            return Err(Error::Auth(Box::new(AuthError::InvalidAuthConfiguration {
                reason: "initial_settings must not contain auth configuration; \
                         Database::create bootstraps auth with the signing key as Admin(0)"
                    .to_string(),
            })));
        }

        // Bootstrap auth with the signing key as Admin(0)
        let mut auth_settings = AuthSettings::new();
        auth_settings.add_key(&pubkey, AuthKey::active(None, Permission::Admin(0)))?;
        initial_settings.set("auth", auth_settings.as_doc().clone());

        // Create the initial root entry using a temporary Database and Transaction.
        // This placeholder ID should not exist in the backend, so get_tips will be empty.
        let bootstrap_placeholder_id = format!(
            "bootstrap_root_{}",
            rand::thread_rng()
                .sample_iter(&Alphanumeric)
                .take(10)
                .map(char::from)
                .collect::<String>()
        );

        // Create temporary database for bootstrap with DatabaseKey.
        // This allows the bootstrap transaction to use the provided key directly.
        let temp_database_for_bootstrap = Database {
            root: ID::from_bytes(bootstrap_placeholder_id.as_bytes()),
            instance: instance.downgrade(),
            key: Some(DatabaseKey::new(signing_key.clone())),
        };

        // Create the transaction - it will use the provided key automatically
        let txn = temp_database_for_bootstrap.new_transaction().await?;

        // IMPORTANT: For the root entry, we need to set the database root to empty/default
        // so that is_root() returns true and all_roots() can find it
        txn.set_entry_root(ID::default())?;

        // Populate the SETTINGS and ROOT subtrees for the very first entry
        txn.update_subtree(SETTINGS, serde_json::to_vec(&initial_settings)?)
            .await?;
        txn.update_subtree(ROOT, serde_json::to_vec("")?).await?; // Standard practice for root entry's _root

        // Add entropy to the entry metadata to ensure unique database IDs even with identical settings
        txn.set_metadata_entropy(rand::thread_rng().next_u64())?;

        // Commit the initial entry
        let new_root_id = txn.commit().await?;

        // Now create the real database with the new_root_id and DatabaseKey
        Ok(Self {
            root: new_root_id,
            instance: instance.downgrade(),
            key: Some(DatabaseKey::new(signing_key)),
        })
    }

    /// Opens an existing database by its root ID.
    ///
    /// Verifies the root entry exists in the backend, then returns a handle
    /// for read-only access. To perform authenticated writes, chain
    /// `.with_key(key)` after opening.
    ///
    /// # Arguments
    /// * `instance` - Instance handle for storage and coordination
    /// * `root_id` - The root entry ID of the database to open
    ///
    /// # Errors
    /// Returns an error if the root entry does not exist in the backend.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::backend::database::InMemory;
    /// # use eidetica::auth::crypto::generate_keypair;
    /// # #[tokio::main]
    /// # async fn main() -> Result<()> {
    /// # let instance = Instance::open(Box::new(InMemory::new())).await?;
    /// # let (signing_key, _verifying_key) = generate_keypair();
    /// # let root_id = ID::from_bytes(b"existing_database_root_id");
    /// // Open database for reading
    /// let db = Database::open(&instance, &root_id).await?;
    ///
    /// // Open database with a signing key for writes
    /// let db = Database::open(&instance, &root_id).await?.with_key(signing_key);
    /// let tx = db.new_transaction().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn open(instance: &Instance, root_id: &ID) -> Result<Self> {
        // Verify the root entry exists. Surfaces "database doesn't exist"
        // at open time instead of at first read/write.
        instance.backend().get(root_id).await?;

        Ok(Self {
            root: root_id.clone(),
            instance: instance.downgrade(),
            key: None,
        })
    }

    /// Attach a signing key to this database handle.
    ///
    /// The key is stored for use by future transactions. No validation is
    /// performed; invalid keys will cause errors at commit time or when
    /// calling [`current_permission`](Self::current_permission).
    ///
    /// Calling `with_key` again replaces any previously-attached key — the
    /// most recent call wins.
    ///
    /// To discover which `SigKey` identity to use for a given public key,
    /// use [`Database::find_sigkeys`].
    pub fn with_key(self, key: impl Into<DatabaseKey>) -> Self {
        Self {
            key: Some(key.into()),
            ..self
        }
    }

    /// Validate a `DatabaseKey` against this database's auth settings.
    ///
    /// Checks that:
    /// 1. The signing key derives to the public key claimed by the identity
    /// 2. The identity exists in the database's auth settings
    ///
    /// Returns the effective permission for the validated key. Callers wanting
    /// to fail fast on an invalid key should call
    /// [`current_permission`](Self::current_permission), which wraps this.
    async fn validate_key(&self, key: &DatabaseKey) -> Result<Permission> {
        let settings_store = self.get_settings().await?;
        let auth_settings = settings_store.auth_snapshot().await?;

        // Derive actual pubkey from the signing key
        let actual_pubkey = key.public_key();

        match key.identity() {
            SigKey::Direct { hint } if hint.is_global() => {
                // Verify the embedded pubkey matches the actual signing key
                if let Some(embedded_pubkey) = &hint.pubkey
                    && *embedded_pubkey != actual_pubkey
                {
                    return Err(Error::Auth(Box::new(AuthError::SigningKeyMismatch {
                        reason: format!(
                            "signing key derives pubkey '{actual_pubkey}' \
                                 but global identity claims '{embedded_pubkey}'"
                        ),
                    })));
                }
                auth_settings.get_global_permission().ok_or_else(|| {
                    Error::Auth(Box::new(AuthError::InvalidAuthConfiguration {
                        reason: "Global '*' permission not configured".to_string(),
                    }))
                })
            }
            SigKey::Direct { hint } => match (&hint.pubkey, &hint.name) {
                (Some(pubkey), _) => {
                    // Verify the claimed pubkey matches the actual signing key
                    if *pubkey != actual_pubkey {
                        return Err(Error::Auth(Box::new(AuthError::SigningKeyMismatch {
                            reason: format!(
                                "signing key derives pubkey '{actual_pubkey}' \
                                 but identity claims '{pubkey}'"
                            ),
                        })));
                    }
                    let auth_key = auth_settings.get_key_by_pubkey(pubkey)?;
                    Ok(*auth_key.permissions())
                }
                (_, Some(name)) => {
                    let matches = auth_settings.find_keys_by_name(name);
                    if matches.is_empty() {
                        return Err(Error::Auth(Box::new(AuthError::KeyNotFound {
                            key_name: name.clone(),
                        })));
                    }
                    // Find the named key whose pubkey matches our actual signing key
                    let actual_pubkey_str = actual_pubkey.to_string();
                    let (_, auth_key) = matches
                        .iter()
                        .find(|(pk, _)| *pk == actual_pubkey_str)
                        .ok_or_else(|| {
                            Error::Auth(Box::new(AuthError::SigningKeyMismatch {
                                reason: format!(
                                    "signing key derives pubkey '{actual_pubkey}' \
                                     but no key named '{name}' has that pubkey"
                                ),
                            }))
                        })?;
                    Ok(*auth_key.permissions())
                }
                _ => Err(Error::Auth(Box::new(AuthError::InvalidAuthConfiguration {
                    reason: "DatabaseKey has empty identity hint".to_string(),
                }))),
            },
            SigKey::Delegation { .. } => {
                // Resolve delegation path through AuthValidator
                let instance = self.instance()?;
                let mut validator = AuthValidator::new();
                let resolved_auths = validator
                    .resolve_sig_key(key.identity(), &auth_settings, Some(&instance))
                    .await
                    .map_err(|e| {
                        Error::Auth(Box::new(AuthError::InvalidAuthConfiguration {
                            reason: format!("Delegation resolution failed: {e}"),
                        }))
                    })?;

                // Find a resolved auth whose pubkey matches our signing key
                resolved_auths
                    .into_iter()
                    .find(|ra| ra.public_key == actual_pubkey)
                    .map(|ra| ra.effective_permission)
                    .ok_or_else(|| {
                        Error::Auth(Box::new(AuthError::SigningKeyMismatch {
                            reason: format!(
                                "signing key derives pubkey '{actual_pubkey}' \
                                 but no resolved delegation key matches"
                            ),
                        }))
                    })
            }
        }
    }

    /// Find all SigKeys that a public key can use to access a database.
    ///
    /// This static helper method loads a database's authentication settings and returns
    /// all possible SigKeys that can be used with the given public key. This is useful for
    /// discovering authentication options before opening a database.
    ///
    /// Returns all matching SigKeys including:
    /// - Specific key names where the pubkey matches
    /// - Global permission if available
    /// - Single-hop delegation paths (pubkey found in a directly delegated tree)
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
    /// # use eidetica::database::DatabaseKey;
    /// # use eidetica::backend::database::InMemory;
    /// # use eidetica::auth::crypto::generate_keypair;
    /// # use eidetica::auth::types::SigKey;
    /// # #[tokio::main]
    /// # async fn main() -> Result<()> {
    /// # let instance = Instance::open(Box::new(InMemory::new())).await?;
    /// # let (signing_key, pubkey) = generate_keypair();
    /// # let root_id = ID::from_bytes(b"database_root_id");
    /// // Find all SigKeys this pubkey can use (sorted highest permission first)
    /// let sigkeys = Database::find_sigkeys(&instance, &root_id, &pubkey).await?;
    ///
    /// // Use the first available SigKey (highest permission)
    /// if let Some((sigkey, _permission)) = sigkeys.first() {
    ///     let key = DatabaseKey::with_identity(signing_key, sigkey.clone());
    ///     let database = Database::open(&instance, &root_id).await?.with_key(key);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn find_sigkeys(
        instance: &Instance,
        root_id: &ID,
        pubkey: &PublicKey,
    ) -> Result<Vec<(SigKey, Permission)>> {
        use crate::auth::{permission::clamp_permission, types::DelegationStep};

        // Create temporary database to load settings (no key source needed for reading)
        let temp_db = Self::open(instance, root_id).await?;

        // Load auth settings
        let settings_store = temp_db.get_settings().await?;
        let auth_settings = settings_store.auth_snapshot().await?;

        // Find direct SigKeys for this pubkey
        let mut results = auth_settings.find_all_sigkeys_for_pubkey(pubkey);

        // Scan single-hop delegation paths
        // FIXME: deep nested delegations can't use this
        if let Ok(delegated_trees) = auth_settings.get_all_delegated_trees() {
            for (delegated_root_id, delegated_tree_ref) in &delegated_trees {
                // Load the delegated tree's auth settings
                let delegated_db = match Self::open(instance, delegated_root_id).await {
                    Ok(db) => db,
                    Err(_) => continue,
                };
                let delegated_settings = match delegated_db.get_settings().await {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let delegated_auth = match delegated_settings.auth_snapshot().await {
                    Ok(a) => a,
                    Err(_) => continue,
                };

                // Check if pubkey exists in the delegated tree
                let delegated_sigkeys = delegated_auth.find_all_sigkeys_for_pubkey(pubkey);
                if delegated_sigkeys.is_empty() {
                    continue;
                }

                // Get current tips for the delegated tree
                let tips = match instance.backend().get_tips(delegated_root_id).await {
                    Ok(t) => t,
                    Err(_) => continue,
                };

                // For each matching key in the delegated tree, construct a delegation SigKey
                for (delegated_sk, delegated_perm) in delegated_sigkeys {
                    // Clamp the delegated permission through the bounds
                    let effective_perm =
                        clamp_permission(delegated_perm, &delegated_tree_ref.permission_bounds);

                    // Construct the delegation SigKey using the hint from the direct key
                    let delegation_sigkey = SigKey::Delegation {
                        path: vec![DelegationStep {
                            tree: delegated_root_id.clone(),
                            tips: tips.clone(),
                        }],
                        hint: delegated_sk.hint().clone(),
                    };

                    results.push((delegation_sigkey, effective_perm));
                }
            }
        }

        // Sort by permission, highest first
        results.sort_by_key(|b| std::cmp::Reverse(b.1));
        Ok(results)
    }

    /// Get the auth identity for this database's configured key.
    pub fn auth_identity(&self) -> Option<&SigKey> {
        self.key.as_ref().map(|k| &k.identity)
    }

    /// Register a callback to be invoked when entries are written to this database.
    ///
    /// The callback fires for **both** local writes (transaction commits) and remote
    /// writes (sync). Branch on [`WriteEvent::source`](crate::WriteEvent::source) inside
    /// the closure if you only care about one.
    ///
    /// Returns a [`WriteCallback`] handle. **Drop it to unregister.** Call
    /// [`WriteCallback::detach`] to leave the callback registered for the life
    /// of the [`Instance`] without holding the handle.
    ///
    /// **Important:** Callbacks are registered at the Instance level and fire for all
    /// writes to the database tree (identified by root ID), regardless of which
    /// `Database` handle performed the write or registered the callback.
    ///
    /// # Callback contract
    ///
    /// - **Local writes**: fires once per transaction commit; the [`WriteEvent`]
    ///   contains exactly one entry.
    /// - **Remote writes**: fires once per sync batch (not per entry); the
    ///   [`WriteEvent`] may contain multiple entries received together.
    /// - All entries in the event are fully persisted before the callback fires.
    /// - [`WriteEvent::previous_tips`] contains the DAG tips from before the
    ///   write, so consumers can determine exactly what changed.
    /// - Errors are logged but do not prevent other callbacks from running.
    /// - The `db` argument is a **read-only** [`Database`] handle (no
    ///   [`DatabaseKey`] configured): you can read settings, entries, and
    ///   metadata, but cannot commit transactions through it. To write from
    ///   inside a callback, resolve through `db.instance()?` and open the
    ///   database with the appropriate key.
    /// - **Reentrance**: writes are serialized per-tree via an async lock
    ///   that is held while callbacks run. A callback must not commit a
    ///   transaction on the same tree it was invoked for — that would
    ///   deadlock. Spawn a task or write to a different tree instead.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::crdt::Doc;
    /// # use eidetica::backend::database::InMemory;
    /// # use eidetica::auth::crypto::PrivateKey;
    /// # #[tokio::main]
    /// # async fn main() -> Result<()> {
    /// let instance = Instance::open(Box::new(InMemory::new())).await?;
    /// # let signing_key = PrivateKey::generate();
    /// # let database = Database::create(&instance, signing_key, Doc::new()).await?;
    ///
    /// let cb = database.on_write(|event, db| {
    ///     let count = event.entries().len();
    ///     let source = event.source();
    ///     let db_id = db.root_id().clone();
    ///     async move {
    ///         println!("{count} entries written to {db_id} ({source:?})");
    ///         Ok(())
    ///     }
    /// })?;
    ///
    /// // Drop `cb` to unregister, or:
    /// cb.detach(); // keep registered for the life of the Instance
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// If a callback needs the [`Instance`], call [`Database::instance`] on
    /// the `db` argument.
    pub fn on_write<F, Fut>(&self, callback: F) -> Result<WriteCallback>
    where
        F: for<'a> Fn(&'a WriteEvent, &'a Database) -> Fut + Send + std::marker::Sync + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        let instance = self.instance()?;
        let tree_id = self.root_id().clone();
        let id = instance.register_write_callback(tree_id.clone(), callback);
        Ok(WriteCallback::new_per_database(
            instance.downgrade(),
            tree_id,
            id,
        ))
    }

    /// Get the ID of the root entry
    pub fn root_id(&self) -> &ID {
        &self.root
    }

    /// Upgrade the weak instance reference to a strong reference.
    ///
    /// `Database` holds a [`WeakInstance`](crate::WeakInstance), so this can
    /// fail if the owning [`Instance`] has already been dropped.
    pub fn instance(&self) -> Result<Instance> {
        self.instance
            .upgrade()
            .ok_or_else(|| Error::Instance(Box::new(InstanceError::InstanceDropped)))
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
        let mut txn = Transaction::new_with_tips(self, tips.as_ref()).await?;

        // Set provided signing key from DatabaseKey
        if let Some(key) = &self.key {
            txn.set_provided_key(*key.signing_key.clone(), key.identity.clone());
        }

        Ok(txn)
    }

    /// Execute a closure within a transaction and commit the result.
    ///
    /// This is a convenience wrapper for the common pattern of creating a transaction,
    /// performing store operations, and committing. The transaction is committed after
    /// the closure returns `Ok`. If the closure returns `Err`, the transaction is
    /// dropped without committing.
    ///
    /// For read-only access, use [`get_store_viewer`](Self::get_store_viewer) instead.
    ///
    /// # Arguments
    /// * `f` - A closure that receives the [`Transaction`] and performs store operations.
    ///   The closure should return `Ok(R)` on success.
    ///
    /// # Returns
    /// On success, returns the value produced by the closure after committing.
    /// The commit ID is not returned; use [`new_transaction`](Self::new_transaction)
    /// directly if you need it.
    ///
    /// # Errors
    /// Returns an error if transaction creation, the closure, or commit fails.
    /// If the closure fails, the transaction is not committed.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::store::Table;
    /// # use serde::{Serialize, Deserialize};
    /// # #[derive(Clone, Serialize, Deserialize)]
    /// # struct Todo { title: String }
    /// # async fn example(db: Database) -> Result<()> {
    /// // Insert a record and get its generated key
    /// let key = db.with_transaction(|txn| async move {
    ///     let store = txn.get_store::<Table<Todo>>("todos").await?;
    ///     store.insert(Todo { title: "Buy milk".into() }).await
    /// }).await?;
    ///
    /// // Multiple operations in one atomic transaction
    /// db.with_transaction(|txn| async move {
    ///     let store = txn.get_store::<Table<Todo>>("todos").await?;
    ///     store.insert(Todo { title: "First".into() }).await?;
    ///     store.insert(Todo { title: "Second".into() }).await?;
    ///     Ok(())
    /// }).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn with_transaction<F, Fut, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(Transaction) -> Fut + Send,
        Fut: Future<Output = Result<R>> + Send,
    {
        let txn = self.new_transaction().await?;
        let commit_handle = txn.clone();
        let result = f(txn).await?;
        commit_handle.commit().await?;
        Ok(result)
    }

    /// Insert an entry into the database without modifying it.
    /// This is primarily for testing purposes or when you need full control over the entry.
    /// Note: This method assumes the entry is already properly signed and verified.
    pub async fn insert_raw(&self, entry: Entry) -> Result<ID> {
        let instance = self.instance()?;
        let id = entry.id();

        instance.put(VerificationStatus::Verified, entry).await?;

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
        let txn = self.new_transaction().await?;
        T::new(&txn, name.into()).await
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
    /// # let txn = tree.new_transaction().await?;
    /// let entry_id = txn.commit().await?;
    /// let entry = tree.get_entry(&entry_id).await?;           // Using &ID
    /// let entry = tree.get_entry(entry_id.clone()).await?;    // Using ID
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
    /// * `entry_ids` - An iterable of entry IDs to retrieve
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
    /// let entry_ids = vec![ID::from_bytes("id1"), ID::from_bytes("id2")];
    /// let entries = tree.get_entries(entry_ids).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_entries<I, T>(&self, entry_ids: I) -> Result<Vec<Entry>>
    where
        I: IntoIterator<Item = T>,
        T: std::borrow::Borrow<ID>,
    {
        let ids: Vec<ID> = entry_ids.into_iter().map(|t| t.borrow().clone()).collect();
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

        // Get the authentication settings that were valid at the time this entry was created
        let historical_settings = self.get_historical_settings_for_entry(&entry).await?;

        // Use the authentication validator with historical settings
        let instance = self.instance()?;
        let mut validator = AuthValidator::new();
        validator
            .validate_entry(&entry, &historical_settings, Some(&instance))
            .await
    }

    /// Get the permission level for this database's configured signing key.
    ///
    /// Returns the effective permission for the key that was configured when opening
    /// or creating this database. This uses the already-resolved identity stored in
    /// the database's `DatabaseKey`.
    ///
    /// # Returns
    /// The effective Permission for the configured signing key.
    ///
    /// # Errors
    /// Returns an error if:
    /// - No signing key is configured (database opened without authentication)
    /// - The database settings cannot be retrieved
    /// - The key is no longer valid in the current auth settings
    ///
    /// # Example
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::crdt::Doc;
    /// # use eidetica::backend::database::InMemory;
    /// # use eidetica::auth::crypto::generate_keypair;
    /// # #[tokio::main]
    /// # async fn main() -> Result<()> {
    /// # let instance = Instance::open(Box::new(InMemory::new())).await?;
    /// # let (signing_key, _public_key) = generate_keypair();
    /// # let database = Database::create(&instance, signing_key, Doc::new()).await?;
    /// // Check if the current key has Admin permission
    /// let permission = database.current_permission().await?;
    /// if permission.can_admin() {
    ///     println!("Current key has Admin permission!");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn current_permission(&self) -> Result<Permission> {
        let key = self
            .key
            .as_ref()
            .ok_or(AuthError::InvalidAuthConfiguration {
                reason: "No signing key configured for this database".to_string(),
            })?;
        self.validate_key(key).await
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
        settings.auth_snapshot().await
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
