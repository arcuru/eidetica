//!
//! Provides the main database structures (`Instance` and `Database`).
//!
//! `Instance` manages multiple `Database` instances and interacts with the storage `Database`.
//! `Database` represents a single, independent history of data entries, analogous to a table or branch.

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use handle_trait::Handle;

use crate::{
    Clock, Database, Entry, Result, SystemClock,
    auth::crypto::{PrivateKey, PublicKey},
    backend::{BackendImpl, InstanceMetadata, InstanceSecrets},
    entry::ID,
    sync::Sync,
    user::User,
};

pub mod backend;
pub mod errors;
pub mod settings_merge;

#[cfg(test)]
mod tests;

// Re-export main types for easier access
use backend::Backend;
pub use errors::InstanceError;

/// Indicates whether an entry write originated locally or from a remote source (e.g., sync).
///
/// This distinction allows different callbacks to be triggered based on the write source,
/// enabling behaviors like "only trigger sync for local writes" or "only update UI for remote writes".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum WriteSource {
    /// Write originated from a local transaction commit
    Local,
    /// Write originated from a remote source (e.g., sync, replication)
    Remote,
}

/// Context provided to write callbacks describing what changed in the database.
///
/// For local writes (transaction commits), this contains a single entry.
/// For remote writes (sync), this may contain a batch of entries that were
/// received and stored together.
///
/// # Catching up on missed writes
///
/// The `previous_tips` field contains the DAG tips of the database *before* the
/// write(s) that triggered this callback. Consumers can use this to determine
/// exactly what changed by walking the DAG from the current tips back to these
/// previous tips. This is analogous to `git log previous_tip..HEAD`.
///
/// This design means callbacks never need to "miss" writes — even if multiple
/// entries are batched (as in sync), the consumer can reconstruct the full set
/// of changes from the tip diff.
///
/// # Example
///
/// ```rust,no_run
/// # use eidetica::instance::WriteEvent;
/// # fn example(event: &WriteEvent) {
/// // Check what stores were touched
/// for entry in event.entries() {
///     if entry.in_subtree("messages") {
///         // A write touched the "messages" store
///     }
/// }
///
/// // Use previous_tips to find what's new
/// let prev = event.previous_tips();
/// // Walk DAG from current tips back to prev to find all new entries
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct WriteEvent {
    /// The entries written in this event. For local writes, this is always
    /// exactly one entry. For remote sync, this is the full batch of entries
    /// that were received and stored together.
    entries: Vec<Entry>,
    /// The DAG tips of the database immediately before this write.
    /// Consumers can diff current tips against these to determine what changed.
    previous_tips: Vec<ID>,
    /// Whether this write originated locally or from a remote sync.
    source: WriteSource,
}

impl WriteEvent {
    /// Get the entries written in this event.
    ///
    /// For local writes (transaction commits), this always contains exactly one entry.
    /// For remote writes (sync), this contains the full batch of entries received together.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Get the DAG tips of the database before this write.
    ///
    /// Use these to determine what changed: walk from the database's current tips
    /// back to these previous tips to find all new entries.
    pub fn previous_tips(&self) -> &[ID] {
        &self.previous_tips
    }

    /// The source of this write (local commit or remote sync).
    pub fn source(&self) -> WriteSource {
        self.source
    }
}

/// Boxed future returned by the internal async callback dispatcher.
pub(crate) type AsyncWriteCallbackFuture<'a> =
    Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// Internal async callback function type. The user-facing callback contract
/// is documented on [`Database::on_write`](crate::Database::on_write).
pub(crate) type AsyncWriteCallbackFn = Arc<
    dyn for<'a> Fn(&'a WriteEvent, &'a Database) -> AsyncWriteCallbackFuture<'a>
        + Send
        + std::marker::Sync,
>;

/// Opaque identifier for a registered callback. Stable for the life of the registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct CallbackId(u64);

/// Type alias for a collection of write callbacks paired with their ids.
type CallbackVec = Vec<(CallbackId, AsyncWriteCallbackFn)>;

/// Handle to a registered write callback. **Drop to unregister.**
///
/// Returned by [`Database::on_write`](crate::Database::on_write). While this
/// value is alive the callback fires on writes; dropping it removes the
/// registration. Use [`detach`](Self::detach) to keep the callback registered
/// for the life of the [`Instance`] when you don't want to manage the lifetime
/// yourself.
///
/// Holds a weak reference to the [`Instance`], so a `WriteCallback` will not
/// keep the Instance alive on its own.
#[must_use = "dropping a WriteCallback unregisters it; call .detach() to keep the callback registered"]
pub struct WriteCallback {
    instance: WeakInstance,
    tree_id: ID,
    id: CallbackId,
    detached: bool,
}

impl WriteCallback {
    pub(crate) fn new_per_database(instance: WeakInstance, tree_id: ID, id: CallbackId) -> Self {
        Self {
            instance,
            tree_id,
            id,
            detached: false,
        }
    }

    /// Consume the handle without unregistering. The callback remains active
    /// for the life of the [`Instance`].
    ///
    /// Implementation note: this sets a flag rather than calling `mem::forget`
    /// so that field destructors (the `WeakInstance`'s weak count, the
    /// `tree_id`'s heap allocation) still run — only our `Drop` impl is
    /// short-circuited.
    pub fn detach(mut self) {
        self.detached = true;
    }
}

impl std::fmt::Debug for WriteCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteCallback")
            .field("id", &self.id)
            .field("tree_id", &self.tree_id)
            .field("detached", &self.detached)
            .finish()
    }
}

impl Drop for WriteCallback {
    fn drop(&mut self) {
        if self.detached {
            return;
        }
        if let Some(instance) = self.instance.upgrade() {
            instance.remove_write_callback(&self.tree_id, self.id);
        }
    }
}

/// Internal state for Instance
///
/// This structure holds the actual implementation data for Instance.
/// Instance itself is just a cheap-to-clone handle wrapping Arc<InstanceInternal>.
pub(crate) struct InstanceInternal {
    /// The database storage backend
    backend: Backend,
    /// Time provider for timestamps
    clock: Arc<dyn Clock>,
    /// Synchronization module for this database instance
    /// TODO: Overengineered, Sync can be created by default but disabled
    sync: std::sync::OnceLock<Arc<Sync>>,
    /// Public instance metadata (device identity, system database IDs)
    metadata: InstanceMetadata,
    /// Private instance secrets (None for remote instances without key access)
    secrets: Option<InstanceSecrets>,
    /// Per-database callbacks keyed by tree_id. Each callback fires for both
    /// local and remote writes; consumers branch on [`WriteEvent::source`] if
    /// they only care about one.
    write_callbacks: Mutex<HashMap<ID, CallbackVec>>,
    /// Global callbacks fired for every write across every database.
    global_write_callbacks: Mutex<CallbackVec>,
    /// Monotonic id source for [`CallbackId`].
    next_callback_id: AtomicU64,
    /// Per-tree async locks serializing the
    /// `get_tips` → backend write → callback dispatch sequence so
    /// `WriteEvent::previous_tips` is consistent for concurrent writers
    /// to the same tree.
    tree_locks: Mutex<HashMap<ID, Arc<tokio::sync::Mutex<()>>>>,
}

impl std::fmt::Debug for InstanceInternal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstanceInternal")
            .field("backend", &"<BackendDB>")
            .field("clock", &self.clock)
            .field("sync", &self.sync)
            .field("metadata", &self.metadata)
            .field("secrets", &self.secrets.is_some())
            .field(
                "write_callbacks",
                &format!(
                    "<{} per-db callbacks>",
                    self.write_callbacks.lock().unwrap().len()
                ),
            )
            .field(
                "global_write_callbacks",
                &format!(
                    "<{} global callbacks>",
                    self.global_write_callbacks.lock().unwrap().len()
                ),
            )
            .field(
                "next_callback_id",
                &self.next_callback_id.load(Ordering::Relaxed),
            )
            .finish()
    }
}
/// Database implementation on top of the storage backend.
///
/// Instance manages infrastructure only:
/// - Backend storage and device identity
/// - System databases (_users, _databases, _sync)
/// - User account management (create, login, list)
///
/// All database creation and key operations happen through User after login.
///
/// Instance is a cheap-to-clone handle around `Arc<InstanceInternal>`.
///
/// ## Example
///
/// ```
/// # use eidetica::{backend::database::InMemory, Instance, crdt::Doc};
/// # #[tokio::main]
/// # async fn main() -> eidetica::Result<()> {
/// let instance = Instance::open(Box::new(InMemory::new())).await?;
///
/// // Create passwordless user
/// instance.create_user("alice", None).await?;
/// let mut user = instance.login_user("alice", None).await?;
///
/// // Use User API for operations
/// let mut settings = Doc::new();
/// settings.set("name", "my_database");
/// let default_key = user.get_default_key()?;
/// let db = user.create_database(settings, &default_key).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Handle)]
pub struct Instance {
    inner: Arc<InstanceInternal>,
}

/// Weak reference to an Instance.
///
/// This is a weak handle that does not prevent the Instance from being dropped.
/// Dependent objects (Database, Sync, BackgroundSync) hold weak references to avoid
/// circular reference cycles that would leak memory.
///
/// Use `upgrade()` to convert to a strong `Instance` reference.
#[derive(Clone, Debug, Handle)]
pub struct WeakInstance {
    inner: Weak<InstanceInternal>,
}

impl Instance {
    /// Connect to a running Eidetica service daemon over a Unix domain socket.
    ///
    /// This creates a `RemoteConnection` that forwards all storage operations
    /// to the daemon via RPC, wrapped in a `Backend::Remote` variant.
    ///
    /// Authentication uses client-side signing. `login_user()` runs the
    /// challenge-response handshake, decrypting the user's signing key
    /// in-process and signing the challenge locally; the daemon never
    /// receives the password or the plaintext signing key and never signs
    /// on the client's behalf. A connected Instance holds no local instance
    /// secrets. A key-holding daemon was considered and rejected — see the
    /// Service Architecture doc § Decision record for the rationale.
    ///
    /// # Arguments
    /// * `socket_path` - Path to the Unix domain socket
    ///
    /// # Returns
    /// A Result containing the connected Instance
    #[cfg(all(unix, feature = "service"))]
    pub async fn connect(socket_path: impl AsRef<std::path::Path>) -> Result<Self> {
        let conn = crate::service::client::RemoteConnection::connect(socket_path).await?;
        let backend = Backend::Remote(conn);

        // Load metadata from the remote backend
        let metadata = backend
            .get_instance_metadata()
            .await?
            .ok_or(InstanceError::DeviceKeyNotFound)?;

        // No local secrets — keys are held server-side after login
        let inner = Arc::new(InstanceInternal {
            backend,
            clock: Arc::new(SystemClock),
            sync: std::sync::OnceLock::new(),
            metadata,
            secrets: None,
            write_callbacks: Mutex::new(HashMap::new()),
            global_write_callbacks: Mutex::new(Vec::new()),
            next_callback_id: AtomicU64::new(0),
            tree_locks: Mutex::new(HashMap::new()),
        });
        Ok(Self { inner })
    }

    /// Load an existing Instance or create a new one (recommended).
    ///
    /// This is the recommended method for initializing an Instance. It automatically detects
    /// whether the backend contains existing system state (device key and system databases)
    /// and loads them, or creates new ones if starting fresh.
    ///
    /// Instance manages infrastructure only:
    /// - Backend storage and device identity
    /// - System databases (_users, _databases, _sync)
    /// - User account management (create, login, list)
    ///
    /// All database creation and key operations require explicit User login.
    ///
    /// # Arguments
    /// * `backend` - The storage backend to use
    ///
    /// # Returns
    /// A Result containing the configured Instance
    ///
    /// # Example
    /// ```
    /// # use eidetica::{backend::database::InMemory, Instance, crdt::Doc};
    /// # #[tokio::main]
    /// # async fn main() -> eidetica::Result<()> {
    /// let backend = InMemory::new();
    /// let instance = Instance::open(Box::new(backend)).await?;
    ///
    /// // Create and login user explicitly
    /// instance.create_user("alice", None).await?;
    /// let mut user = instance.login_user("alice", None).await?;
    ///
    /// // Use User API for operations
    /// let mut settings = Doc::new();
    /// settings.set("name", "my_database");
    /// let default_key = user.get_default_key()?;
    /// let db = user.create_database(settings, &default_key).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn open(backend: Box<dyn BackendImpl>) -> Result<Self> {
        Self::open_impl(backend, Arc::new(SystemClock)).await
    }

    /// Load an existing Instance or create a new one with a custom clock.
    ///
    /// This is the same as [`Instance::open`] but allows injecting a custom clock
    /// for controllable timestamps in tests. The clock is used for timestamps in
    /// height calculations and peer tracking.
    ///
    /// Only available with the `testing` feature or in test builds.
    ///
    /// # Arguments
    /// * `backend` - The storage backend to use
    /// * `clock` - The time provider to use (typically [`FixedClock`](crate::FixedClock))
    ///
    /// # Returns
    /// A Result containing the configured Instance
    #[cfg(any(test, feature = "testing"))]
    pub async fn open_with_clock(
        backend: Box<dyn BackendImpl>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        Self::open_impl(backend, clock).await
    }

    /// Internal implementation of open that works with any clock.
    async fn open_impl(backend: Box<dyn BackendImpl>, clock: Arc<dyn Clock>) -> Result<Self> {
        let backend: Arc<dyn BackendImpl> = Arc::from(backend);

        // Check for existing InstanceMetadata
        match backend.get_instance_metadata().await? {
            Some(metadata) => {
                // Load secrets (contains the private key)
                let secrets = backend.get_instance_secrets().await?;

                // If secrets are present, verify they match the metadata
                if let Some(ref secrets) = secrets {
                    let derived_id = secrets.signing_key.public_key();
                    if derived_id != metadata.id {
                        return Err(InstanceError::DeviceKeyMismatch.into());
                    }
                }

                // Existing backend: load from metadata + secrets
                let inner = Arc::new(InstanceInternal {
                    backend: Backend::new(backend),
                    clock,
                    sync: std::sync::OnceLock::new(),
                    metadata,
                    secrets,
                    write_callbacks: Mutex::new(HashMap::new()),
                    global_write_callbacks: Mutex::new(Vec::new()),
                    next_callback_id: AtomicU64::new(0),
                    tree_locks: Mutex::new(HashMap::new()),
                });
                Ok(Self { inner })
            }
            None => {
                // New backend: initialize
                Self::create_internal(backend, clock).await
            }
        }
    }

    /// Create a new Instance on a fresh backend (strict creation).
    ///
    /// This method creates a new Instance and fails if the backend is already initialized
    /// (contains a device key and system databases). Use this when you want to ensure
    /// you're creating a fresh instance.
    ///
    /// Instance manages infrastructure only:
    /// - Backend storage and device identity
    /// - System databases (_users, _databases, _sync)
    /// - User account management (create, login, list)
    ///
    /// All database creation and key operations require explicit User login.
    ///
    /// For most use cases, prefer `Instance::open()` which automatically handles both
    /// new and existing backends.
    ///
    /// # Arguments
    /// * `backend` - The storage backend to use (must be uninitialized)
    ///
    /// # Returns
    /// A Result containing the configured Instance, or InstanceAlreadyExists error
    /// if the backend is already initialized.
    ///
    /// # Example
    /// ```
    /// # use eidetica::{backend::database::InMemory, Instance, crdt::Doc};
    /// # #[tokio::main]
    /// # async fn main() -> eidetica::Result<()> {
    /// let backend = InMemory::new();
    /// let instance = Instance::create(Box::new(backend)).await?;
    ///
    /// // Create and login user explicitly
    /// instance.create_user("alice", None).await?;
    /// let mut user = instance.login_user("alice", None).await?;
    ///
    /// // Use User API for operations
    /// let mut settings = Doc::new();
    /// settings.set("name", "my_database");
    /// let default_key = user.get_default_key()?;
    /// let db = user.create_database(settings, &default_key).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn create(backend: Box<dyn BackendImpl>) -> Result<Self> {
        let backend: Arc<dyn BackendImpl> = Arc::from(backend);

        // Check if already initialized
        if backend.get_instance_metadata().await?.is_some() {
            return Err(InstanceError::InstanceAlreadyExists.into());
        }

        // Create new instance
        Self::create_internal(backend, Arc::new(SystemClock)).await
    }

    /// Internal implementation of new that works with Arc<dyn BackendImpl>
    pub(crate) async fn create_internal(
        backend: Arc<dyn BackendImpl>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        use crate::user::system_databases::{create_databases_tracking, create_users_database};

        // 1. Generate device key
        let device_key = PrivateKey::generate();
        let device_id = device_key.public_key();

        // 2. Create system databases with device_key passed directly
        // Create a temporary Instance for database creation (databases will store full IDs later)
        //
        // SAFETY: The temporary instance has empty users_db_id and databases_db_id placeholders.
        // This is safe because:
        // 1. We only use it to create new system databases via Database::create()
        // 2. Database::create() doesn't access the instance's system database IDs
        // 3. The system databases don't exist yet, so their IDs can't be referenced
        // 4. The temporary instance is only used during initial setup and discarded
        // 5. The real instance is constructed afterward with the correct database IDs
        let temp_instance = Self {
            inner: Arc::new(InstanceInternal {
                backend: Backend::new(Arc::clone(&backend)),
                clock: Arc::clone(&clock),
                sync: std::sync::OnceLock::new(),
                metadata: InstanceMetadata {
                    id: device_id.clone(),
                    users_db: ID::default(), // Placeholder - system DBs don't exist yet
                    databases_db: ID::default(), // Placeholder - system DBs don't exist yet
                    sync_db: None,
                },
                secrets: Some(InstanceSecrets {
                    signing_key: device_key.clone(),
                }),
                write_callbacks: Mutex::new(HashMap::new()),
                global_write_callbacks: Mutex::new(Vec::new()),
                next_callback_id: AtomicU64::new(0),
                tree_locks: Mutex::new(HashMap::new()),
            }),
        };
        let users_db = create_users_database(&temp_instance, &device_key).await?;
        let databases_db = create_databases_tracking(&temp_instance, &device_key).await?;

        // 3. Save metadata and secrets (marks instance as initialized)
        // NB: Ordering matters. Secrets are stored first, then Metadata.
        // The presence of the Metadata indicates the instance is fully initialized.
        let secrets = InstanceSecrets {
            signing_key: device_key,
        };
        backend.set_instance_secrets(&secrets).await?;

        let metadata = InstanceMetadata {
            id: device_id,
            users_db: users_db.root_id().clone(),
            databases_db: databases_db.root_id().clone(),
            sync_db: None,
        };
        backend.set_instance_metadata(&metadata).await?;

        // 4. Build real instance and return
        let inner = Arc::new(InstanceInternal {
            backend: Backend::new(backend),
            clock,
            sync: std::sync::OnceLock::new(),
            metadata,
            secrets: Some(secrets),
            write_callbacks: Mutex::new(HashMap::new()),
            global_write_callbacks: Mutex::new(Vec::new()),
            next_callback_id: AtomicU64::new(0),
            tree_locks: Mutex::new(HashMap::new()),
        });

        Ok(Self { inner })
    }

    /// Get a reference to the backend
    pub fn backend(&self) -> &Backend {
        &self.inner.backend
    }

    /// Check if an entry exists in storage.
    pub async fn has_entry(&self, id: &ID) -> bool {
        self.inner.backend.get(id).await.is_ok()
    }

    /// Check if a database is present locally.
    ///
    /// This differs from `has_entry` in that it checks for the active tracking
    /// of the database by the Instance. This method checks if we're tracking
    /// the database's tip state.
    pub async fn has_database(&self, root_id: &ID) -> bool {
        match self.inner.backend.get_tips(root_id).await {
            Ok(tips) => !tips.is_empty(),
            Err(_) => false,
        }
    }

    /// Get a reference to the clock.
    ///
    /// The clock is used for timestamps in height calculations and peer tracking.
    pub(crate) fn clock(&self) -> &dyn Clock {
        &*self.inner.clock
    }

    /// Get a cloned Arc of the clock.
    ///
    /// Used when passing the clock to components that need ownership (e.g., HeightCalculator).
    pub(crate) fn clock_arc(&self) -> Arc<dyn Clock> {
        self.inner.clock.clone()
    }

    // === Backend pass-through methods (pub(crate) for internal use) ===

    /// Get an entry from the backend
    pub(crate) async fn get(&self, id: &crate::entry::ID) -> Result<crate::entry::Entry> {
        self.inner.backend.get(id).await
    }

    /// Put an entry into the backend
    pub(crate) async fn put(
        &self,
        verification_status: crate::backend::VerificationStatus,
        entry: crate::entry::Entry,
    ) -> Result<()> {
        self.inner.backend.put(verification_status, entry).await
    }

    /// Get tips for a tree
    pub(crate) async fn get_tips(&self, tree: &crate::entry::ID) -> Result<Vec<crate::entry::ID>> {
        self.inner.backend.get_tips(tree).await
    }

    // === System database accessors ===

    /// Get the _users database
    ///
    /// This constructs a Database instance on-the-fly to avoid circular references.
    /// On a local instance the device signing key is attached so users-table
    /// writes (e.g., the local `create_user` path) can sign. On a remote
    /// instance the device key lives on the daemon side and isn't available
    /// locally; the returned Database is read-only from the client and any
    /// write paths go through dedicated RPCs (`CreateUser`, etc.) instead of
    /// being signed locally.
    pub(crate) async fn users_db(&self) -> Result<Database> {
        let db = Database::open(self, &self.inner.metadata.users_db).await?;
        #[cfg(all(unix, feature = "service"))]
        if let Backend::Remote(_) = self.backend() {
            return Ok(db);
        }
        Ok(db.with_key(self.signing_key()?.clone()))
    }

    /// Get the _databases tracking database
    ///
    /// Parallel to `users_db()` — opens the instance's database-registry
    /// system DB with the device signing key attached. Used by the
    /// instance-admin bootstrap path (`system_databases::create_user`) to add
    /// the first user's pubkey as `Admin(0)` on the registry, so subsequent
    /// admin-gated instance ops (e.g., `SetInstanceMetadata`) can authorize
    /// against the user's key instead of the device key.
    pub(crate) async fn databases_db(&self) -> Result<Database> {
        Ok(Database::open(self, &self.inner.metadata.databases_db)
            .await?
            .with_key(self.signing_key()?.clone()))
    }

    /// Root id of the `_databases` system DB.
    ///
    /// The service daemon uses this to gate admin-only ops
    /// (e.g., `SetInstanceMetadata`) against `_databases.auth_settings`:
    /// an instance admin is a user with `Admin` on `_databases`.
    pub(crate) fn databases_db_id(&self) -> &ID {
        &self.inner.metadata.databases_db
    }

    /// Root id of the `_users` system DB.
    ///
    /// Parallel to `databases_db_id()`. Lets an instance admin open `_users`
    /// keyed by their own signing key (rather than the device key that
    /// `users_db()` attaches), so admin-gated edits to `_users.auth_settings`
    /// resolve against the admin's identity.
    pub(crate) fn users_db_id(&self) -> &ID {
        &self.inner.metadata.users_db
    }

    // === User Management ===

    /// Create a new user account with flexible password handling.
    ///
    /// Creates a user with or without password protection. Passwordless users are appropriate
    /// for embedded applications where filesystem access = database access.
    ///
    /// # Arguments
    /// * `user_id` - Unique user identifier (username)
    /// * `password` - Optional password. If None, user is passwordless (instant login, no encryption)
    ///
    /// # Returns
    /// A Result containing the user's UUID (stable internal identifier)
    pub async fn create_user(&self, user_id: &str, password: Option<&str>) -> Result<String> {
        use crate::user::system_databases::create_user;
        let users_db = self.users_db().await?;
        let (user_uuid, _user_info) = create_user(&users_db, self, user_id, password).await?;
        Ok(user_uuid)
    }

    /// Login a user with flexible password handling.
    ///
    /// Returns a User session object that provides access to user operations.
    /// For password-protected users, provide the password. For passwordless users, pass None.
    ///
    /// # Arguments
    /// * `user_id` - User identifier (username)
    /// * `password` - Optional password. None for passwordless users.
    ///
    /// # Returns
    /// A Result containing the User session
    pub async fn login_user(&self, user_id: &str, password: Option<&str>) -> Result<User> {
        // On a remote instance, the `TrustedLogin*` handshake authenticates the
        // socket connection AND ships back the user's full `UserInfo` plus the
        // decrypted root signing key. Build the `User` session from those —
        // the per-tree gate means a freshly-logged-in user with no permissions
        // on `_users` couldn't re-read it over the wire anyway, so we don't try.
        #[cfg(all(unix, feature = "service"))]
        if let Backend::Remote(conn) = self.backend() {
            let (user_uuid, user_info, signing_key) = conn.trusted_login(user_id, password).await?;
            return crate::user::system_databases::build_user_session(
                self,
                &user_uuid,
                &user_info,
                signing_key,
                password,
            )
            .await;
        }

        use crate::user::system_databases::login_user;
        let users_db = self.users_db().await?;
        login_user(&users_db, self, user_id, password).await
    }

    /// List all user IDs.
    ///
    /// # Returns
    /// A Result containing a vector of user IDs
    pub async fn list_users(&self) -> Result<Vec<String>> {
        use crate::user::system_databases::list_users;
        let users_db = self.users_db().await?;
        list_users(&users_db).await
    }

    // === User-Sync Integration ===

    // === Device Identity Management ===
    //
    // The Instance's public identity is stored in InstanceMetadata, and the private
    // signing key is stored in InstanceSecrets. Both are cached in memory.

    /// Get the device signing key.
    ///
    /// # Internal Use Only
    ///
    /// This method provides direct access to the instance's cryptographic identity
    /// and is intended for internal operations that require the device key (sync,
    /// system database creation, authentication validation, etc.).
    ///
    /// These operations should only be performed by the server/instance administrator,
    /// but we don't verify that yet. Future versions may add admin permission checks.
    ///
    /// Similar to `Database::open` (without a key), this is a controlled escape hatch
    /// for internal library operations. Use with care - prefer User API for normal operations.
    ///
    /// Returns an error if this is a remote Instance that does not have access to the
    /// device key (e.g., connected via RPC where secrets are never transmitted).
    #[cfg(not(any(test, feature = "testing")))]
    pub(crate) fn signing_key(&self) -> Result<&PrivateKey> {
        self.inner
            .secrets
            .as_ref()
            .map(|s| &s.signing_key)
            .ok_or_else(|| InstanceError::DeviceKeyNotFound.into())
    }

    /// Test-only: Get the device signing key.
    ///
    /// This is exposed for testing purposes only. In production, use the User API.
    ///
    /// Returns an error if this is a remote Instance that does not have access to the
    /// device key.
    #[cfg(any(test, feature = "testing"))]
    pub fn signing_key(&self) -> Result<&PrivateKey> {
        self.inner
            .secrets
            .as_ref()
            .map(|s| &s.signing_key)
            .ok_or_else(|| InstanceError::DeviceKeyNotFound.into())
    }

    /// Get the instance identity (public key).
    ///
    /// # Returns
    /// The instance's public key identity.
    pub fn id(&self) -> PublicKey {
        self.inner.metadata.id.clone()
    }

    // === Synchronization Management ===
    //
    // These methods provide access to the Sync module for managing synchronization
    // settings and state for this database instance.

    /// Initializes the Sync module for this instance.
    ///
    /// Enables synchronization operations for this instance. This method is idempotent;
    /// calling it multiple times has no effect.
    ///
    /// # Errors
    /// Returns an error if the sync settings database cannot be created or if device key
    /// generation/storage fails.
    pub async fn enable_sync(&self) -> Result<()> {
        // Check if there is an existing Sync already loaded
        if self.inner.sync.get().is_some() {
            return Ok(());
        }

        // A remote Instance must not run sync client-side: building a Sync
        // here would spin up a background sync engine that drives RPCs against
        // the daemon's backend — duplicating (and racing) the daemon's own
        // sync. Sync is owned by the process that owns the Instance. Fail
        // loudly instead of silently constructing a useless module. A future
        // `EnableSync` RPC will delegate this to the server (see
        // `service` module § V1 Limitations).
        #[cfg(all(unix, feature = "service"))]
        if let Backend::Remote(_) = self.backend() {
            return Err(InstanceError::OperationNotSupported {
                operation: "enable_sync on a remote Instance (sync runs daemon-side)".to_string(),
            }
            .into());
        }

        // Check InstanceMetadata for existing sync_db
        let metadata = self
            .backend()
            .get_instance_metadata()
            .await?
            .ok_or(InstanceError::DeviceKeyNotFound)?; // Metadata must exist if instance is initialized

        let sync = if let Some(ref sync_db) = metadata.sync_db {
            // Load existing sync tree
            Sync::load(self.clone(), sync_db).await?
        } else {
            // Create new sync tree
            let sync = Sync::new(self.clone()).await?;

            // Save sync_db to metadata
            let mut new_metadata = metadata;
            new_metadata.sync_db = Some(sync.sync_tree_root_id().clone());
            self.backend().set_instance_metadata(&new_metadata).await?;

            sync
        };

        let sync_arc = Arc::new(sync);

        // Initialize the sync engine (no transports registered yet)
        // Users should call register_transport() to add transports
        sync_arc.start_background_sync()?;

        // Register global callback for automatic sync on local writes.
        // Detached: lives for the life of the Instance.
        let sync_for_callback = Arc::clone(&sync_arc);
        self.register_global_write_callback(move |event, database| {
            let sync = Arc::clone(&sync_for_callback);
            let event = event.clone();
            let database = database.clone();
            async move {
                if event.source() != WriteSource::Local {
                    return Ok(());
                }
                // Local writes always carry exactly one entry today, but
                // the loop in `Sync::on_local_write` is intentionally
                // ready for future multi-entry local events.
                sync.on_local_write(&event, &database).await
            }
        });

        let _ = self.inner.sync.set(sync_arc);
        Ok(())
    }

    /// Get a reference to the Sync module.
    ///
    /// Returns a cheap-to-clone Arc handle to the Sync module. The Sync module
    /// uses interior mutability (AtomicBool and OnceLock) so &self methods are sufficient.
    ///
    /// # Returns
    /// An `Option` containing an `Arc<Sync>` if the Sync module is initialized.
    pub fn sync(&self) -> Option<Arc<Sync>> {
        self.inner.sync.get().map(Arc::clone)
    }

    /// Flush all pending sync operations.
    ///
    /// This is a convenience method that processes all queued entries and
    /// retries any failed sends. If sync is not enabled, returns Ok(()).
    ///
    /// This is useful to force pending syncs to complete, e.g. on program shutdown.
    ///
    /// # Returns
    /// `Ok(())` if sync is not enabled or all operations completed successfully,
    /// or an error if sends failed.
    pub async fn flush_sync(&self) -> Result<()> {
        if let Some(sync) = self.sync() {
            sync.flush().await
        } else {
            Ok(())
        }
    }

    // === Entry Write Coordination ===
    //
    // All entry writes go through Instance::put_entry() which handles backend storage
    // and callback dispatch. This centralizes write coordination and ensures hooks fire.

    /// Register a per-database callback. Fires for both local and remote writes.
    ///
    /// Returns the [`CallbackId`] of the registration. Callers wrap this in a
    /// [`WriteCallback`] handle (see [`Database::on_write`]) to manage lifetime.
    pub(crate) fn register_write_callback<F, Fut>(&self, tree_id: ID, callback: F) -> CallbackId
    where
        F: for<'a> Fn(&'a WriteEvent, &'a Database) -> Fut + Send + std::marker::Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let id = CallbackId(self.inner.next_callback_id.fetch_add(1, Ordering::Relaxed));
        let cb: AsyncWriteCallbackFn = Arc::new(move |event: &WriteEvent, database: &Database| {
            let fut = callback(event, database);
            Box::pin(fut) as AsyncWriteCallbackFuture<'_>
        });
        self.inner
            .write_callbacks
            .lock()
            .unwrap()
            .entry(tree_id)
            .or_default()
            .push((id, cb));
        id
    }

    /// Register a global callback fired for every write across every database.
    ///
    /// Callers branch on [`WriteEvent::source`] inside the closure if they
    /// only care about one source.
    ///
    /// Note: there is intentionally no `WriteCallback` handle or remove path
    /// for global callbacks. The only current caller is sync's permanent hook
    /// (OnceLock-guarded) which is registered for the life of the Instance.
    /// If a future caller needs lifecycle management on a global callback,
    /// add `remove_global_write_callback` and a global variant of
    /// `WriteCallback`.
    pub(crate) fn register_global_write_callback<F, Fut>(&self, callback: F)
    where
        F: for<'a> Fn(&'a WriteEvent, &'a Database) -> Fut + Send + std::marker::Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let id = CallbackId(self.inner.next_callback_id.fetch_add(1, Ordering::Relaxed));
        let cb: AsyncWriteCallbackFn = Arc::new(move |event: &WriteEvent, database: &Database| {
            let fut = callback(event, database);
            Box::pin(fut) as AsyncWriteCallbackFuture<'_>
        });
        self.inner
            .global_write_callbacks
            .lock()
            .unwrap()
            .push((id, cb));
    }

    /// Remove a per-database callback by id. No-op if not found.
    pub(crate) fn remove_write_callback(&self, tree_id: &ID, id: CallbackId) {
        let mut callbacks = self.inner.write_callbacks.lock().unwrap();
        if let Some(vec) = callbacks.get_mut(tree_id) {
            vec.retain(|(cb_id, _)| *cb_id != id);
            if vec.is_empty() {
                callbacks.remove(tree_id);
            }
        }
    }

    /// Acquire (or create) the per-tree async lock that serializes the
    /// `get_tips` → backend write → callback dispatch sequence.
    ///
    /// Without this, two concurrent writers to the same tree both snapshot
    /// `previous_tips` before either writes, so the second callback's
    /// `previous_tips` would not reflect the first write — breaking the
    /// "diff against current tips" contract documented on [`WriteEvent`].
    fn tree_lock(&self, tree_id: &ID) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.inner.tree_locks.lock().unwrap();
        Arc::clone(
            locks
                .entry(tree_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    /// Write an entry to the backend and dispatch callbacks.
    ///
    /// This is the central coordination point for all entry writes in the system.
    /// All writes must go through this method to ensure:
    /// - Entries are persisted to the backend
    /// - Appropriate callbacks are triggered based on write source
    /// - Hooks have full context (entry, database, instance)
    ///
    /// Serialized per-tree against [`Self::put_remote_entries`] so
    /// [`WriteEvent::previous_tips`] is consistent.
    ///
    /// # Arguments
    /// * `tree_id` - The root ID of the database being written to
    /// * `verification` - Authentication verification status of the entry
    /// * `entry` - The entry to write
    /// * `source` - Whether this is a local or remote write
    ///
    /// # Returns
    /// A Result indicating success or failure
    pub async fn put_entry(
        &self,
        tree_id: &ID,
        verification: crate::backend::VerificationStatus,
        entry: Entry,
        source: WriteSource,
    ) -> Result<()> {
        let lock = self.tree_lock(tree_id);
        let _guard = lock.lock().await;

        // 1. Capture tips before the write so callbacks know what changed
        let previous_tips = self.get_tips(tree_id).await?;

        // 2. Persist to backend storage (and notify server for remote backends)
        self.backend()
            .write_entry(tree_id, verification, entry.clone(), source)
            .await?;

        // 3. Build event and fire callbacks
        let event = WriteEvent {
            entries: vec![entry],
            previous_tips,
            source,
        };
        self.fire_write_callbacks(tree_id, &event).await;

        Ok(())
    }

    /// Store a batch of remotely-received entries and fire callbacks once.
    ///
    /// This is the correct way to ingest entries from sync. All entries are
    /// persisted first, then callbacks fire exactly once with the full batch
    /// and the tips from before ingestion. This ensures:
    ///
    /// - The database is fully consistent when callbacks execute
    /// - Callbacks fire once per sync exchange, not once per entry
    /// - `previous_tips` lets consumers reconstruct exactly what changed
    ///
    /// Entries that fail to store are logged and skipped — remaining entries
    /// are still stored and callbacks still fire for whatever was persisted.
    /// Returns the number of entries that were successfully persisted.
    ///
    /// Serialized per-tree against [`Self::put_entry`] and other concurrent
    /// `put_remote_entries` calls so `previous_tips` is consistent across
    /// writers.
    ///
    /// # Arguments
    /// * `tree_id` - The root ID of the database receiving the batch
    /// * `verification` - Authentication verification status to apply to
    ///   each entry. Pass `VerificationStatus::Failed` for entries received
    ///   over the wire that have not been signature-checked (the project's
    ///   current stand-in for an `Unverified` state).
    /// * `entries` - The entries to ingest
    pub(crate) async fn put_remote_entries(
        &self,
        tree_id: &ID,
        verification: crate::backend::VerificationStatus,
        entries: Vec<Entry>,
    ) -> Result<usize> {
        if entries.is_empty() {
            return Ok(0);
        }

        let lock = self.tree_lock(tree_id);
        let _guard = lock.lock().await;

        // 1. Capture tips before any writes
        let previous_tips = self.get_tips(tree_id).await?;

        // 2. Store all entries
        let mut stored_entries = Vec::with_capacity(entries.len());
        for entry in entries {
            match self.backend().put(verification, entry.clone()).await {
                Ok(_) => stored_entries.push(entry),
                Err(e) => {
                    tracing::error!(
                        tree_id = %tree_id,
                        entry_id = %entry.id(),
                        "Failed to store remote entry: {}", e
                    );
                }
            }
        }

        let stored_count = stored_entries.len();

        // 3. Fire callbacks once for the whole batch
        if !stored_entries.is_empty() {
            let event = WriteEvent {
                entries: stored_entries,
                previous_tips,
                source: WriteSource::Remote,
            };
            self.fire_write_callbacks(tree_id, &event).await;
        }

        Ok(stored_count)
    }

    /// Dispatch callbacks for a write event.
    ///
    /// Fires per-database callbacks for `tree_id` then global callbacks. Each
    /// callback fires for both local and remote writes; consumers branch on
    /// [`WriteEvent::source`] internally.
    async fn fire_write_callbacks(&self, tree_id: &ID, event: &WriteEvent) {
        let per_db_callbacks = self
            .inner
            .write_callbacks
            .lock()
            .unwrap()
            .get(tree_id)
            .cloned();

        let global_callbacks = self.inner.global_write_callbacks.lock().unwrap().clone();

        let has_callbacks = per_db_callbacks.is_some() || !global_callbacks.is_empty();
        if !has_callbacks {
            return;
        }

        // Create a Database handle for the callbacks
        let database = match Database::open(self, tree_id).await {
            Ok(db) => db,
            Err(e) => {
                tracing::error!(tree_id = %tree_id, "Failed to open database for callbacks: {}", e);
                return;
            }
        };

        if let Some(callbacks) = per_db_callbacks {
            for (id, callback) in callbacks {
                if let Err(e) = callback(event, &database).await {
                    tracing::error!(
                        tree_id = %tree_id,
                        source = ?event.source(),
                        callback_id = ?id,
                        "Per-database callback failed: {}", e
                    );
                }
            }
        }

        for (id, callback) in global_callbacks {
            if let Err(e) = callback(event, &database).await {
                tracing::error!(
                    tree_id = %tree_id,
                    source = ?event.source(),
                    callback_id = ?id,
                    "Global callback failed: {}", e
                );
            }
        }
    }

    /// Dispatch write callbacks for an entry that has already been stored.
    ///
    /// This is used by the service server when handling `NotifyEntryWritten` RPCs.
    /// The entry is already in the backend; this method only fires callbacks via
    /// the standard `fire_write_callbacks` path.
    ///
    /// TODO(service): `previous_tips` is approximated here — for remotely-notified
    /// writes, the daemon doesn't currently send the pre-write tips, so we read
    /// current tips minus the new entry's id. A future revision of the
    /// NotifyEntryWritten RPC should carry `previous_tips` explicitly.
    pub(crate) async fn dispatch_write_callbacks(
        &self,
        tree_id: &ID,
        entry: &Entry,
        source: WriteSource,
    ) -> Result<()> {
        let entry_id = entry.id();
        let previous_tips: Vec<ID> = self
            .get_tips(tree_id)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|t| t != &entry_id)
            .collect();
        let event = WriteEvent {
            entries: vec![entry.clone()],
            previous_tips,
            source,
        };
        self.fire_write_callbacks(tree_id, &event).await;
        Ok(())
    }

    /// Downgrade to a weak reference.
    ///
    /// Creates a weak reference that does not prevent the Instance from being dropped.
    /// This is useful for preventing circular reference cycles in dependent objects.
    ///
    /// # Returns
    /// A `WeakInstance` that can be upgraded back to a strong reference.
    pub fn downgrade(&self) -> WeakInstance {
        WeakInstance {
            inner: Arc::downgrade(&self.inner),
        }
    }
}

impl WeakInstance {
    /// Upgrade to a strong reference.
    ///
    /// Attempts to upgrade this weak reference to a strong `Instance` reference.
    /// Returns `None` if the Instance has already been dropped.
    ///
    /// # Returns
    /// `Some(Instance)` if the Instance still exists, `None` otherwise.
    ///
    /// # Example
    /// ```
    /// # use eidetica::{backend::database::InMemory, Instance};
    /// # #[tokio::main]
    /// # async fn main() -> eidetica::Result<()> {
    /// let instance = Instance::open(Box::new(InMemory::new())).await?;
    /// let weak = instance.downgrade();
    ///
    /// // Upgrade works while instance exists
    /// assert!(weak.upgrade().is_some());
    ///
    /// drop(instance);
    /// // Upgrade fails after instance is dropped
    /// assert!(weak.upgrade().is_none());
    /// # Ok(())
    /// # }
    /// ```
    pub fn upgrade(&self) -> Option<Instance> {
        self.inner.upgrade().map(|inner| Instance { inner })
    }
}
