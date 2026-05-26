//! Database module provides functionality for managing collections of related entries.
//!
//! A `Database` represents a hierarchical structure of entries, like a traditional database
//! or a branch in a version control system. Each database has a root entry and maintains
//! the history and relationships between entries. Database holds a weak reference to its
//! parent Instance, accessing storage and coordination services through that handle.

use std::{future::Future, sync::Arc};

use rand::{Rng, RngCore, distributions::Alphanumeric};
use serde_json;

#[cfg(all(unix, feature = "service"))]
use crate::instance::backend::RemoteBackend;
#[cfg(all(unix, feature = "service"))]
use crate::service::client::RemoteConnection;
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
    crdt::{CRDT, Doc},
    entry::{Entry, ID},
    instance::{WriteCallback, WriteEvent, backend::Backend, errors::InstanceError},
    store::{SettingsStore, Store},
};

#[cfg(test)]
mod tests;

tokio::task_local! {
    /// Set while a `verify()`/validation pass is on the call stack.
    ///
    /// Verification reads the database (delegation resolution opens trees,
    /// reads settings → tips), and the access-time auto-verify hook in
    /// [`Database::get_tips`] would otherwise re-enter verification
    /// unboundedly. While this is set, the hook is suppressed and reads
    /// return raw (still `Failed`-filtered) tips.
    static IN_VERIFY: bool;
}

fn auto_verify_suppressed() -> bool {
    IN_VERIFY.try_with(|v| *v).unwrap_or(false)
}

/// Outcome of reconstructing the `_settings` state an entry pins.
///
/// An entry records, in its signed metadata, the `_settings` tips its
/// signature must be validated against. We can only verify it if this node
/// holds that full pinned `_settings` ancestor set.
enum PinnedSettings {
    /// The pinned `_settings` set is fully present; here is its auth config.
    Complete(AuthSettings),
    /// This node does not yet hold the full pinned `_settings` set, so the
    /// entry cannot be verified yet (it stays `Unverified`).
    Incomplete,
}

/// Summary of a [`Database::verify`] pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VerifyReport {
    /// Entries promoted `Unverified` → `Verified` this pass.
    pub verified: usize,
    /// Entries marked `Unverified` → `Failed` (definitively bad) this pass.
    pub failed: usize,
    /// Entries left `Unverified` (pinned `_settings` not yet held locally).
    pub still_unverified: usize,
}

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
    /// Storage seam `Transaction`/`Store` reads flow through. On a local
    /// instance this is a clone of the instance's own [`Backend`] (forwarding
    /// to the backing engine); on a connected instance a per-handle
    /// [`RemoteBackend`] bound to this database's acting identity. Derived from
    /// `instance`/construction only — carrying it across
    /// `with_key`/`allow_unverified` (`..self`) rebuilds is correct.
    ops: Arc<dyn Backend>,
    /// Signing key bound to its auth identity for this database
    key: Option<DatabaseKey>,
    /// When `false` (default), reads expose only the maximal all-`Verified`
    /// prefix of the DAG (the "Verified frontier"). When `true`, reads also
    /// include `Unverified` entries. `Failed` entries are dropped regardless.
    /// Set via [`Database::allow_unverified`].
    allow_unverified: bool,
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
            ops: instance.backend().clone(),
            key: Some(DatabaseKey::new(signing_key.clone())),
            allow_unverified: false,
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

        // Construct the returned Database, wiring its `ops` to match the
        // instance's flavour:
        //
        // - **Connected (remote) instance** — bind a `RemoteBackend` to the
        //   *new database's* identity (the signing-key's pubkey, self-signed
        //   as `Admin(0)` by the genesis). The connection's login pubkey is
        //   the *caller's* (e.g. the registering admin), which is **not** a
        //   member of the new tree's auth. If we cloned the instance's
        //   session backend here, every read `Transaction::commit` performs
        //   on this database would carry the connection's login identity, and
        //   the server's per-tree gate would deny it. A per-database identity
        //   makes all reads use the tree's own member key — but the server's
        //   gate also requires that key to be in the connection's *session
        //   keyset*, so we `register_session_key(signing_key)` first to do the
        //   proof-of-possession handshake that adds it.
        // - **Local instance** — clone the instance's backend, unchanged.
        #[cfg(all(unix, feature = "service"))]
        if let Some(conn) = instance.remote_connection() {
            let pubkey_for_identity = signing_key.public_key();
            conn.register_session_key(&signing_key).await?;
            return Ok(Self {
                root: new_root_id.clone(),
                instance: instance.downgrade(),
                ops: Arc::new(RemoteBackend::new(
                    conn,
                    Some(SigKey::from_pubkey(&pubkey_for_identity)),
                )),
                key: Some(DatabaseKey::new(signing_key)),
                allow_unverified: false,
            });
        }

        Ok(Self {
            root: new_root_id,
            instance: instance.downgrade(),
            ops: instance.backend().clone(),
            key: Some(DatabaseKey::new(signing_key)),
            allow_unverified: false,
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
            ops: instance.backend().clone(),
            key: None,
            allow_unverified: false,
        })
    }

    /// Open a database for remote access through a service connection.
    ///
    /// Constructs a [`Database`] handle whose backing
    /// [`Backend`](crate::instance::backend::Backend) is a
    /// [`RemoteBackend`](crate::instance::backend::RemoteBackend) bound to
    /// `identity`, so every [`Transaction`]/[`Store`] read and write travels
    /// over the connection as a `DatabaseOp` under that identity. The
    /// `identity` must match the database's auth settings for the caller's
    /// key. `Instance::connect` must be used to create the instance.
    #[cfg(all(unix, feature = "service"))]
    pub async fn open_remote(
        instance: &Instance,
        conn: RemoteConnection,
        root_id: &ID,
        identity: SigKey,
    ) -> Result<Self> {
        instance.backend().get(root_id).await?;
        Ok(Self {
            root: root_id.clone(),
            instance: instance.downgrade(),
            ops: Arc::new(RemoteBackend::new(conn, Some(identity))),
            key: None,
            allow_unverified: false,
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

    /// Include `Unverified` entries in this handle's reads.
    ///
    /// By default a `Database` exposes only the **Verified frontier**: the
    /// maximal prefix of the DAG (an ancestor-closed set, starting at the
    /// root) in which every entry is `Verified`. Tips that are still
    /// `Unverified` — and everything reachable only through them — are hidden,
    /// so a default read never reflects state this node could not authenticate.
    ///
    /// Calling `allow_unverified` opts this handle into the looser view that
    /// also includes `Unverified` entries (everything except `Failed`, which
    /// is always dropped). Use it when you explicitly want to observe
    /// not-yet-verified data — e.g. freshly synced entries whose pinned
    /// `_settings` this node does not hold yet.
    ///
    /// This is a per-handle setting and composes with [`with_key`](Self::with_key):
    ///
    /// ```rust,no_run
    /// # use eidetica::*;
    /// # use eidetica::auth::crypto::generate_keypair;
    /// # async fn example(instance: Instance, root_id: ID) -> Result<()> {
    /// # let (signing_key, _) = generate_keypair();
    /// let db = Database::open(&instance, &root_id)
    ///     .await?
    ///     .with_key(signing_key)
    ///     .allow_unverified();
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Note on CRDT coherence
    ///
    /// The Verified frontier is a *prefix* cut, not a per-value filter. An
    /// interior `Unverified` entry hides all of its descendants from the
    /// default view even if those descendants are themselves `Verified`,
    /// because exposing them without their unverifiable ancestor would yield
    /// an incoherent CRDT state. Run [`verify`](Self::verify) to promote the
    /// blocking entry, or `allow_unverified` to read past it.
    pub fn allow_unverified(self) -> Self {
        Self {
            allow_unverified: true,
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
        let actual_pubkey = key.public_key();
        let instance = match key.identity() {
            // Delegation resolution needs an Instance for cross-tree lookups;
            // direct identities resolve from `auth_settings` alone.
            SigKey::Delegation { .. } => Some(self.instance()?),
            _ => None,
        };
        crate::auth::validation::permissions::resolve_identity_permission(
            &actual_pubkey,
            key.identity(),
            &auth_settings,
            instance.as_ref(),
        )
        .await
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
    /// # ⚠️ Limited semantics on a connected (remote) [`Instance`]
    ///
    /// On a daemon-backed [`Instance`], `on_write` is **best-effort and partial**.
    /// It only observes writes whose commit ran through this client's
    /// [`Instance::put_entry`]. The following writes are **not** observed:
    ///
    /// - Commits made by other client processes connected to the same daemon.
    /// - Entries the daemon receives via sync from peers.
    /// - Anything the daemon writes outside this client's commit path.
    ///
    /// In addition, [`WriteEvent::previous_tips`] is **always empty** for writes
    /// that go through a remote backend — the canonical DAG lives on the daemon
    /// and the client has nothing local to read tips from. Callbacks that diff
    /// `previous_tips` against `event.entries()` will see "the world was empty"
    /// on every event.
    ///
    /// If you need full cross-client / cross-sync notification semantics, use a
    /// local [`Instance`] for now. A server-push subscription path is planned —
    /// when it lands, this method will gain the same contract on remote.
    ///
    /// # Callback contract (local [`Instance`])
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

    /// Get a clone of the backend seam.
    pub fn backend(&self) -> Result<Arc<dyn Backend>> {
        Ok(self.instance()?.backend().clone())
    }

    /// The storage seam this handle's `Transaction`/`Store` reads/writes flow
    /// through (a [`LocalBackend`](crate::instance::backend::LocalBackend) clone
    /// of the instance's backend, or a per-handle
    /// [`RemoteBackend`](crate::instance::backend::RemoteBackend)).
    pub(crate) fn ops(&self) -> &dyn Backend {
        self.ops.as_ref()
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

    /// Gather everything a client needs to build and sign a transaction
    /// locally for the given stores, with parents drawn from `scope`'s
    /// projection.
    ///
    /// This is **single-sourced**: both the server's `BeginTransaction`
    /// handler and the Phase-3 remote seam call it, so
    /// `Transaction::commit`'s build-sign path has one source of truth for
    /// context gathering.
    ///
    /// `scope=AllowUnverified` opens against the raw DAG (only `Failed`
    /// dropped); the default `Verified` scope uses the Verified frontier.
    /// The returned [`TransactionContext`] carries everything needed for
    /// one round-trip transaction build: main parents + heights, per-store
    /// subtree parents + heights, settings tips, and the merged `_settings`
    /// CRDT state this entry is authored against.
    #[cfg(all(unix, feature = "service"))]
    pub async fn transaction_context(
        &self,
        stores: &[String],
        scope: crate::service::protocol::ReadScope,
    ) -> Result<crate::service::protocol::TransactionContext> {
        use crate::service::protocol::{ReadScope, TransactionContext};

        // -- scope-sensitive main tips --------------------------------
        let db_for_tips = Database {
            allow_unverified: matches!(scope, ReadScope::AllowUnverified),
            ..self.clone()
        };
        let main_tips = db_for_tips.get_tips().await?;

        // -- main parents: (tip, height) ------------------------------
        let mut main_parents = Vec::with_capacity(main_tips.len());
        for tip in &main_tips {
            let entry = self.ops().get(tip).await?;
            main_parents.push((tip.clone(), entry.height()));
        }

        // -- per-store subtree parents: (tip, subtree_height) ---------
        let mut subtree_parents = std::collections::BTreeMap::new();
        for store in stores {
            let child_tips = self
                .ops()
                .get_store_tips_up_to_entries(self.root_id(), store, &main_tips)
                .await?;
            let mut pairs = Vec::with_capacity(child_tips.len());
            for tip in &child_tips {
                let entry = self.ops().get(tip).await?;
                let height = entry.subtree_height(store).unwrap_or(0);
                pairs.push((tip.clone(), height));
            }
            subtree_parents.insert(store.clone(), pairs);
        }

        // -- settings tips (pinned in entry metadata) -----------------
        let settings_tips = self
            .ops()
            .get_store_tips_up_to_entries(self.root_id(), SETTINGS, &main_tips)
            .await?;

        // -- merged _settings state as serde_json::Value --------------
        let txn = Transaction::new_with_tips(self, &main_tips).await?;
        let settings_doc: Doc = txn.get_full_state(SETTINGS).await?;
        let settings_value = serde_json::to_value(&settings_doc)?;

        Ok(TransactionContext {
            main_parents,
            subtree_parents,
            settings_tips,
            settings_value,
        })
    }

    /// Server-materialized merged state of an **unencrypted** store, as a
    /// `serde_json::Value` against the database's Verified frontier.
    ///
    /// Creates an ephemeral transaction, deserializes every entry's
    /// store data as [`Doc`], and merges them via Doc's LWW merge —
    /// the same merge `Store<T>` would perform client-side. All current
    /// store types (DocStore, Table, Settings) serialize their data as
    /// JSON, so `Doc`-typed deserialization works universally.
    ///
    /// # Encrypted stores
    ///
    /// Encrypted stores cannot be materialized this way (the ephemeral
    /// transaction has no encryptor, so `serde_json::from_slice::<Doc>`
    /// would fail on ciphertext). The caller must use
    /// [`get_store_entries`](Self::get_store_entries) for encrypted
    /// stores and decrypt+merge client-side.
    pub async fn get_store_state(&self, store: &str) -> Result<serde_json::Value> {
        let txn = self.new_transaction().await?;
        let state: Doc = txn.get_full_state(store).await?;
        Ok(serde_json::to_value(&state)?)
    }

    /// Ordered (by subtree height), verifiable, opaque store entries
    /// reachable from `tips` within `scope`.
    ///
    /// This is the **universal** primitive — works for encrypted and
    /// unencrypted stores alike because it returns raw [`Entry`] records
    /// with opaque [`RawData`](crate::entry::RawData); no deserialization
    /// or merge runs server-side. The per-subtree-height ordering
    /// (ascending, then by ID for tiebreaking) is exactly the canonical
    /// CRDT replay order produced by
    /// [`sort_entries_by_subtree_height`](crate::backend::database::in_memory::cache::sort_entries_by_subtree_height).
    ///
    /// When `scope` is [`ReadScope::Verified`] and `tips` are the
    /// Verified-frontier tips from [`get_tips`](Self::get_tips),
    /// every returned entry is guaranteed `Verified` (the frontier is
    /// ancestor-closed). For [`ReadScope::AllowUnverified`], entries
    /// reachable from unverified tips are included.
    #[cfg(all(unix, feature = "service"))]
    pub async fn get_store_entries(
        &self,
        store: &str,
        tips: &[ID],
        _scope: crate::service::protocol::ReadScope,
    ) -> Result<Vec<Entry>> {
        self.ops()
            .get_store_from_tips(self.root_id(), store, tips)
            .await
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

    /// Insert an entry into the database without modifying or validating it.
    /// Primarily for testing / full control over raw entry storage.
    ///
    /// The entry is stored `Unverified`: this path runs no validation, so it
    /// cannot honestly claim the entry is verified, and the storage API no
    /// longer accepts a caller-asserted status. Only the local validation
    /// pass promotes entries to `Verified`.
    pub async fn insert_raw(&self, entry: Entry) -> Result<ID> {
        let instance = self.instance()?;
        let id = entry.id();

        instance.put(entry).await?;

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
    /// If any raw tip is `Unverified`, an opportunistic [`Self::verify`] pass
    /// runs first (entries arrive `Unverified` from sync; this promotes the
    /// ones whose pinned `_settings` are now held).
    ///
    /// The returned tips then depend on the handle's view:
    ///
    /// - **default** — the **Verified frontier**: the tips of the maximal
    ///   ancestor-closed, all-`Verified` prefix of the DAG. A still-`Unverified`
    ///   tip is replaced by its nearest `Verified` ancestors; anything reachable
    ///   only through an `Unverified` entry is excluded.
    /// - **[`allow_unverified`](Self::allow_unverified)** — the raw tips with
    ///   only `Failed` entries dropped (`Unverified` tips are kept).
    ///
    /// `Failed` entries are dropped in both cases. While a [`verify`](Self::verify)
    /// pass is on the stack the frontier is bypassed (its own reads must see the
    /// raw DAG to reconstruct pinned `_settings`); a remote backend returns its
    /// raw tips unchanged (the server owns verification).
    ///
    /// # Returns
    /// A `Result` containing a vector of `ID`s for the tip entries or an error.
    pub async fn get_tips(&self) -> Result<Vec<ID>> {
        let instance = self.instance()?;

        // On a remote instance the server owns verification: `get_verified_tips`
        // already returns the server-side Verified frontier (or empty for a
        // not-yet-propagated tree, e.g. `Database::create`'s bootstrap
        // placeholder root — `EntryNotFound` is mapped to empty to match
        // `Backend::get_tips`'s contract). Return it directly: the local
        // verification machinery below (status probe, auto-verify,
        // `verified_frontier`) is local-only and would fail on a remote
        // backend anyway (e.g. `verified_frontier`'s `backend.get_tree(...)`).
        //
        // Delegate to `self.ops()` rather than calling the connection
        // directly: when this handle was built via `Database::create` or
        // `Database::open_remote` its `ops` is a `RemoteBackend` carrying the
        // *per-database* identity (the new tree's own member key, or the
        // caller's chosen identity), which the server's per-tree gate accepts.
        // Routing through `conn.session_identity()` here would instead use the
        // connection's (caller's) session pubkey, which is not a member of a
        // freshly-created tree and gets denied. A handle from `Database::open`
        // on a connected instance instead clones the instance's session
        // backend, keeping the session-identity semantics for that path.
        #[cfg(all(unix, feature = "service"))]
        if instance.remote_connection().is_some() {
            return match self.ops().get_tips(&self.root).await {
                Ok(tips) => Ok(tips),
                Err(e) if e.is_not_found() => Ok(Vec::new()),
                Err(e) => Err(e),
            };
        }

        // Local path: verification-status probing needs the concrete engine.
        let backend = instance.require_local_engine()?;
        let tips = self.ops().get_tips(&self.root).await?;

        // Verification status ops are local-only. On a remote backend the
        // server owns verification (and stores everything Unverified until
        // it verifies); the client returns verified tips unchanged.
        if let Some(first) = tips.first()
            && backend.get_verification_status(first).await.is_err()
        {
            return Ok(tips);
        }

        // Access-time opportunistic verification: if any tip is still
        // Unverified, attempt to resolve it now. Best-effort — a failure or a
        // still-incomplete pin must not block the read. Suppressed while a
        // verify pass is already on the stack (its own reads land here).
        let tips = if auto_verify_suppressed() {
            tips
        } else {
            let mut any_unverified = false;
            for t in &tips {
                if backend
                    .get_verification_status(t)
                    .await
                    .unwrap_or(VerificationStatus::Unverified)
                    == VerificationStatus::Unverified
                {
                    any_unverified = true;
                    break;
                }
            }
            if any_unverified {
                // Boxed: this call closes a get_tips → verify →
                // validate_entry → delegation → get_settings → get_tips
                // async cycle; the box gives it a finite future size.
                let _ = Box::pin(self.verify()).await;
                self.ops().get_tips(&self.root).await?
            } else {
                tips
            }
        };

        // Default view: cut to the Verified frontier. Suppressed while a
        // verify pass is on the stack — its reads must see the raw DAG to
        // reconstruct pinned `_settings` (the frontier filter itself depends
        // on verification status, which is exactly what verify is computing).
        if !self.allow_unverified && !auto_verify_suppressed() {
            return self.verified_frontier().await;
        }

        // `allow_unverified` view: keep Unverified tips, drop only Failed.
        let mut visible = Vec::with_capacity(tips.len());
        for t in tips {
            if backend.get_verification_status(&t).await? != VerificationStatus::Failed {
                visible.push(t);
            }
        }
        Ok(visible)
    }

    /// Compute the tips of the maximal all-`Verified` prefix of the DAG.
    ///
    /// An entry is in the prefix iff it is `Verified` **and** every one of its
    /// parents is in the prefix (the prefix is ancestor-closed). The frontier
    /// is the set of prefix entries that are not the parent of any other
    /// prefix entry — i.e. the tips of the verified subgraph.
    ///
    /// Returns an empty vector if the root itself is not `Verified` (nothing
    /// is observable in the default view until verification reaches the root).
    async fn verified_frontier(&self) -> Result<Vec<ID>> {
        let instance = self.instance()?;
        let backend = instance.require_local_engine()?;

        // Topologically sorted (height then ID): every parent precedes its
        // children, so a single forward pass can decide prefix membership.
        let entries = backend.get_tree(self.root_id()).await?;

        let mut in_prefix: std::collections::HashSet<ID> = std::collections::HashSet::new();
        let mut covered: std::collections::HashSet<ID> = std::collections::HashSet::new();

        for e in &entries {
            let id = e.id();
            if backend.get_verification_status(&id).await? != VerificationStatus::Verified {
                continue;
            }
            let parents = e.parents().unwrap_or_default();
            if parents.iter().all(|p| in_prefix.contains(p)) {
                in_prefix.insert(id);
                // Every parent now has a verified child, so it is interior to
                // the prefix and cannot itself be a frontier tip.
                for p in parents {
                    covered.insert(p);
                }
            }
        }

        let frontier: Vec<ID> = entries
            .into_iter()
            .map(|e| e.id())
            .filter(|id| in_prefix.contains(id) && !covered.contains(id))
            .collect();
        Ok(frontier)
    }

    /// Get the full `Entry` objects for the current tips of the main database branch.
    ///
    /// # Returns
    /// A `Result` containing a vector of the tip `Entry` objects or an error.
    pub async fn get_tip_entries(&self) -> Result<Vec<Entry>> {
        let instance = self.instance()?;
        let tips = self.get_tips().await?;
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
    /// # let (_instance, mut user) = Instance::create(
    /// #     Box::new(InMemory::new()),
    /// #     NewUser::passwordless("test"),
    /// # ).await?;
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
        let id = entry_id.into();
        // Route through `self.ops()` so handles built via `Database::create`
        // or `Database::open_remote` read with the per-DB identity from
        // their `RemoteBackend`. Going through `instance.get(id)` would
        // use the connection's login pubkey, which on a remote instance is
        // denied by the per-tree gate when the login key isn't a member of
        // this tree (e.g. user-tree key created via `User::add_private_key`
        // and used to author a database that doesn't grant the root key).
        let entry = self.ops().get(&id).await?;

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
    /// # let (_instance, mut user) = Instance::create(
    /// #     Box::new(InMemory::new()),
    /// #     NewUser::passwordless("test"),
    /// # ).await?;
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

        // Validate against the `_settings` the entry pins, not current
        // settings — so a later key revocation cannot retroactively
        // invalidate (or validate) historical entries.
        match self.get_historical_settings_for_entry(&entry).await? {
            // We do not hold the pinned `_settings` set, so we cannot make a
            // verification decision: report not-verified rather than guess.
            PinnedSettings::Incomplete => Ok(false),
            PinnedSettings::Complete(auth_settings) => {
                let instance = self.instance()?;
                let mut validator = AuthValidator::new();
                validator
                    .validate_entry(&entry, &auth_settings, Some(&instance))
                    .await
            }
        }
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

    /// Reconstruct the `_settings` auth config an entry's signature is pinned
    /// to, from the `settings_tips` recorded in its signed metadata.
    ///
    /// Validation must run against the settings the entry pinned — not the
    /// current settings — so granting authority later cannot retroactively
    /// invalidate an entry that pinned less, and (once revocation lands)
    /// removals are handled on a separate, current-settings path.
    ///
    /// Returns [`PinnedSettings::Incomplete`] when this node does not hold the
    /// full pinned `_settings` ancestor set; the caller must then leave the
    /// entry `Unverified` rather than guess against whatever it does hold.
    async fn get_historical_settings_for_entry(&self, entry: &Entry) -> Result<PinnedSettings> {
        let instance = self.instance()?;
        let backend = instance.backend();

        // The pin: `_settings` tips recorded in the entry's signed metadata.
        let settings_tips: Vec<ID> = match entry.metadata() {
            Some(raw) => match serde_json::from_slice::<crate::transaction::EntryMetadata>(raw) {
                Ok(md) => md.settings_tips,
                // Unparsable metadata ⇒ we cannot establish the pin.
                Err(_) => return Ok(PinnedSettings::Incomplete),
            },
            None => Vec::new(),
        };

        // Resolve the effective `_settings` tips to validate against.
        let effective_tips: Vec<ID> = if settings_tips.is_empty() {
            if entry.in_subtree(SETTINGS) {
                // Genesis / bootstrap: no prior `_settings` exists, so the
                // entry is self-authorising — validate against the auth it
                // itself establishes (TOFU), mirroring how the transaction
                // validates initial database creation. Seeding the
                // reconstruction with the entry itself folds in its own
                // `_settings` contribution.
                vec![entry.id()]
            } else {
                // No auth context at all (no settings ever configured) —
                // mirrors the transaction path's "auth never configured" case.
                return Ok(PinnedSettings::Complete(AuthSettings::new()));
            }
        } else {
            settings_tips
        };

        // Completeness: every pinned tip and its full `_settings` ancestor
        // closure must be present locally. `get_store_from_tips` silently
        // skips absent ancestors, so an explicit walk is required — a missing
        // ancestor would otherwise yield a wrong (partial) auth config.
        let mut stack: Vec<ID> = effective_tips.clone();
        let mut seen: std::collections::HashSet<ID> = std::collections::HashSet::new();
        while let Some(id) = stack.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let Ok(e) = backend.get(&id).await else {
                return Ok(PinnedSettings::Incomplete);
            };
            // Walk both the `_settings` subtree DAG and the main parents that
            // carry it, so the closure can't be short-circuited.
            for p in e.subtree_parents(SETTINGS).unwrap_or_default() {
                stack.push(p);
            }
            for p in e.parents().unwrap_or_default() {
                stack.push(p);
            }
        }

        // Reconstruct the merged `_settings` Doc as of the pinned tips.
        // Entries come back root-first; `_settings` is a system subtree and
        // is never encrypted, so deserialize directly.
        let entries = backend
            .get_store_from_tips(self.root_id(), SETTINGS, &effective_tips)
            .await?;
        let mut settings_doc = Doc::default();
        for e in &entries {
            if let Ok(data) = e.data(SETTINGS) {
                let part: Doc = serde_json::from_slice(data)?;
                settings_doc = settings_doc.merge(&part)?;
            }
        }

        let auth_settings = match settings_doc.get("auth") {
            Some(crate::crdt::doc::Value::Doc(auth_doc)) => auth_doc.clone().into(),
            _ => AuthSettings::new(),
        };
        Ok(PinnedSettings::Complete(auth_settings))
    }

    /// Attempt to verify every `Unverified` entry in this database.
    ///
    /// For each `Unverified` entry, reconstruct the `_settings` it pins
    /// (see [`Self::get_historical_settings_for_entry`]) and validate its
    /// signature + permissions against that:
    ///
    /// - an ancestor is `Failed` → this entry is `Failed` too (quarantine
    ///   propagates down the branch);
    /// - an ancestor is still `Unverified`, or not held locally yet (partial
    ///   sync) → left `Unverified` (retried once the ancestor verifies / the
    ///   missing entry arrives);
    /// - pinned `_settings` not fully held locally → left `Unverified`
    ///   (a later pass retries once the set syncs in);
    /// - signature + permissions valid → promoted to `Verified`;
    /// - definitively invalid → marked `Failed` (dropped from reads).
    ///
    /// Verification is **prefix-closed**: an entry is `Verified` only if its
    /// entire ancestor history is `Verified`. It is therefore impossible for a
    /// tip to be `Verified` while one of its ancestors is not, which is what
    /// makes the Verified set ancestor-closed (see [`Self::allow_unverified`]).
    ///
    /// Already-`Verified` entries are never demoted here; that is a separate,
    /// not-yet-built path. Local-only — verification is a per-node decision
    /// and is never delegated to a peer.
    pub async fn verify(&self) -> Result<VerifyReport> {
        // Suppress the access-time auto-verify hook for the whole pass:
        // validation reads the database (delegation → settings → tips) and
        // must not recurse back into verification.
        IN_VERIFY
            .scope(true, async move {
                let instance = self.instance()?;
                let backend = instance.require_local_engine()?;

                // Raw tree walk (not `get_tips`) so traversal itself never
                // re-enters the hook even before the guard is observed.
                let entries = backend.get_tree(self.root_id()).await?;
                let mut report = VerifyReport::default();

                for entry in &entries {
                    let id = entry.id();
                    if backend.get_verification_status(&id).await? != VerificationStatus::Unverified
                    {
                        continue;
                    }

                    // Verification is prefix-closed: an entry's trust rests on
                    // its entire history, so it can only be `Verified` if every
                    // ancestor is. `get_tree` is topo-sorted (parents before
                    // children), so each parent's status is already final for
                    // this pass.
                    //
                    // - any ancestor `Failed` → the branch is tainted; this
                    //   entry is `Failed` too (quarantine propagates forward);
                    // - any ancestor still `Unverified` → cannot trust this
                    //   entry yet; leave it for a later pass.
                    let parents = entry.parents().unwrap_or_default();
                    let mut compromised = false;
                    let mut blocked = false;
                    for p in &parents {
                        match backend.get_verification_status(p).await {
                            Ok(VerificationStatus::Verified) => {}
                            Ok(VerificationStatus::Failed) => compromised = true,
                            Ok(VerificationStatus::Unverified) => blocked = true,
                            // Parent not held locally yet (partial sync): we
                            // cannot establish the history, so this entry is
                            // blocked until the parent arrives — not an error.
                            Err(e) if e.is_not_found() => blocked = true,
                            Err(e) => return Err(e),
                        }
                    }
                    if compromised {
                        backend
                            .update_verification_status(&id, VerificationStatus::Failed)
                            .await?;
                        report.failed += 1;
                        continue;
                    }
                    if blocked {
                        report.still_unverified += 1;
                        continue;
                    }

                    match self.get_historical_settings_for_entry(entry).await? {
                        PinnedSettings::Incomplete => report.still_unverified += 1,
                        PinnedSettings::Complete(auth_settings) => {
                            let mut validator = AuthValidator::new();
                            let valid = validator
                                .validate_entry(entry, &auth_settings, Some(&instance))
                                .await
                                .unwrap_or(false);
                            let new_status = if valid {
                                report.verified += 1;
                                VerificationStatus::Verified
                            } else {
                                report.failed += 1;
                                VerificationStatus::Failed
                            };
                            backend.update_verification_status(&id, new_status).await?;
                        }
                    }
                }
                Ok(report)
            })
            .await
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
        instance.require_local_engine()?.get_tree(&self.root).await
    }
}
