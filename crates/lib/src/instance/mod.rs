//!
//! Provides the main database structures (`Instance` and `Database`).
//!
//! `Instance` manages multiple `Database` instances and interacts with the storage `Database`.
//! `Database` represents a single, independent history of data entries, analogous to a table or branch.

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
};

use ed25519_dalek::{SigningKey, VerifyingKey};
use handle_trait::Handle;

use crate::{
    Clock, Database, Entry, Result, SystemClock,
    auth::crypto::format_public_key,
    backend::{BackendImpl, InstanceMetadata},
    entry::ID,
    sync::Sync,
    user::User,
};

pub mod backend;
pub mod errors;
pub mod settings_merge;

// Re-export main types for easier access
use backend::Backend;
pub use errors::InstanceError;

/// Indicates whether an entry write originated locally or from a remote source (e.g., sync).
///
/// This distinction allows different callbacks to be triggered based on the write source,
/// enabling behaviors like "only trigger sync for local writes" or "only update UI for remote writes".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriteSource {
    /// Write originated from a local transaction commit
    Local,
    /// Write originated from a remote source (e.g., sync, replication)
    Remote,
}

/// Type alias for async write callback return type.
///
/// We use a boxed future for callbacks. The future is `Send` since internal
/// operations use `Arc`/`Mutex` for thread-safety.
pub type AsyncWriteCallbackFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// Async callback function type for write operations.
///
/// Receives the entry that was written, the database it was written to, and the instance.
/// Returns a boxed future that resolves to a Result.
/// Used for both local and remote write callbacks.
pub type AsyncWriteCallback = Arc<
    dyn for<'a> Fn(&'a Entry, &'a Database, &'a Instance) -> AsyncWriteCallbackFuture<'a>
        + Send
        + std::marker::Sync,
>;

/// Type alias for a collection of write callbacks
type CallbackVec = Vec<AsyncWriteCallback>;

/// Type alias for the per-database callback map key
type CallbackKey = (WriteSource, ID);

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
    /// Root ID of the _users system database
    users_db_id: ID,
    /// Root ID of the _databases system database
    databases_db_id: ID,
    /// Device signing key - the instance's cryptographic identity
    device_key: SigningKey,
    /// Per-database callbacks keyed by (WriteSource, tree_id)
    write_callbacks: Mutex<HashMap<CallbackKey, CallbackVec>>,
    /// Global callbacks keyed by WriteSource (triggered regardless of database)
    global_write_callbacks: Mutex<HashMap<WriteSource, CallbackVec>>,
}

impl std::fmt::Debug for InstanceInternal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstanceInternal")
            .field("backend", &"<BackendDB>")
            .field("clock", &self.clock)
            .field("sync", &self.sync)
            .field("users_db_id", &self.users_db_id)
            .field("databases_db_id", &self.databases_db_id)
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
                // Existing backend: load from metadata
                let inner = Arc::new(InstanceInternal {
                    backend: Backend::new(backend),
                    clock,
                    sync: std::sync::OnceLock::new(),
                    users_db_id: metadata.users_db,
                    databases_db_id: metadata.databases_db,
                    device_key: metadata.device_key,
                    write_callbacks: Mutex::new(HashMap::new()),
                    global_write_callbacks: Mutex::new(HashMap::new()),
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
        use crate::{
            auth::crypto::{format_public_key, generate_keypair},
            user::system_databases::{create_databases_tracking, create_users_database},
        };

        // 1. Generate device key
        let (device_key, device_pubkey) = generate_keypair();
        let device_pubkey_str = format_public_key(&device_pubkey);

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
                users_db_id: ID::from(""), // Placeholder - system DBs don't exist yet
                databases_db_id: ID::from(""), // Placeholder - system DBs don't exist yet
                device_key: device_key.clone(), // Use the actual key for signing
                write_callbacks: Mutex::new(HashMap::new()),
                global_write_callbacks: Mutex::new(HashMap::new()),
            }),
        };
        let users_db =
            create_users_database(&temp_instance, &device_key, &device_pubkey_str).await?;
        let databases_db =
            create_databases_tracking(&temp_instance, &device_key, &device_pubkey_str).await?;

        // 3. Save metadata (marks instance as initialized)
        let metadata = InstanceMetadata {
            device_key: device_key.clone(),
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
            users_db_id: users_db.root_id().clone(),
            databases_db_id: databases_db.root_id().clone(),
            device_key,
            write_callbacks: Mutex::new(HashMap::new()),
            global_write_callbacks: Mutex::new(HashMap::new()),
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
    pub(crate) async fn users_db(&self) -> Result<Database> {
        Database::open(
            self.clone(),
            &self.inner.users_db_id,
            self.inner.device_key.clone(),
            "admin".to_string(),
        )
        .await
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
    // The Instance's device identity is stored in InstanceMetadata and cached in memory.

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
    /// Similar to `Database::open_unauthenticated`, this is a controlled escape hatch
    /// for internal library operations. Use with care - prefer User API for normal operations.
    #[cfg(not(any(test, feature = "testing")))]
    pub(crate) fn device_key(&self) -> &SigningKey {
        &self.inner.device_key
    }

    /// Test-only: Get the device signing key.
    ///
    /// This is exposed for testing purposes only. In production, use the User API.
    #[cfg(any(test, feature = "testing"))]
    pub fn device_key(&self) -> &SigningKey {
        &self.inner.device_key
    }

    /// Get the device ID (public key).
    ///
    /// # Returns
    /// The device's public key (device ID).
    pub fn device_id(&self) -> VerifyingKey {
        self.inner.device_key.verifying_key()
    }

    /// Get the device ID as a formatted string.
    ///
    /// This is a convenience method that returns the device ID (public key)
    /// in a standard formatted string representation.
    ///
    /// # Returns
    /// The formatted device ID string.
    pub fn device_id_string(&self) -> String {
        format_public_key(&self.inner.device_key.verifying_key())
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

        // Register global callback for automatic sync on local writes
        let sync_for_callback = Arc::clone(&sync_arc);
        self.register_global_write_callback(
            WriteSource::Local,
            move |entry, database, instance| {
                let sync = Arc::clone(&sync_for_callback);
                let entry = entry.clone();
                let database = database.clone();
                let instance = instance.clone();
                async move { sync.on_local_write(&entry, &database, &instance).await }
            },
        )?;

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

    /// Register a callback to be invoked when entries are written to a database.
    ///
    /// The callback receives the entry, database, and instance as parameters.
    ///
    /// # Arguments
    /// * `source` - The write source to monitor (Local or Remote)
    /// * `tree_id` - The root ID of the database tree to monitor
    /// * `callback` - Function to invoke on writes
    ///
    /// # Returns
    /// A Result indicating success or failure
    pub(crate) fn register_write_callback<F, Fut>(
        &self,
        source: WriteSource,
        tree_id: ID,
        callback: F,
    ) -> Result<()>
    where
        F: for<'a> Fn(&'a Entry, &'a Database, &'a Instance) -> Fut
            + Send
            + std::marker::Sync
            + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let mut callbacks = self.inner.write_callbacks.lock().unwrap();
        callbacks
            .entry((source, tree_id))
            .or_default()
            .push(Arc::new(
                move |entry: &Entry, database: &Database, instance: &Instance| {
                    let fut = callback(entry, database, instance);
                    Box::pin(fut) as AsyncWriteCallbackFuture<'_>
                },
            ));
        Ok(())
    }

    /// Register a global callback to be invoked on all writes of a specific source.
    ///
    /// Global callbacks are invoked for all writes of the specified source across all databases.
    /// This is useful for system-wide operations like synchronization that need to track
    /// changes across all databases.
    ///
    /// # Arguments
    /// * `source` - The write source to monitor (Local or Remote)
    /// * `callback` - Function to invoke on all writes
    ///
    /// # Returns
    /// A Result indicating success or failure
    pub(crate) fn register_global_write_callback<F, Fut>(
        &self,
        source: WriteSource,
        callback: F,
    ) -> Result<()>
    where
        F: for<'a> Fn(&'a Entry, &'a Database, &'a Instance) -> Fut
            + Send
            + std::marker::Sync
            + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let mut callbacks = self.inner.global_write_callbacks.lock().unwrap();
        callbacks.entry(source).or_default().push(Arc::new(
            move |entry: &Entry, database: &Database, instance: &Instance| {
                let fut = callback(entry, database, instance);
                Box::pin(fut) as AsyncWriteCallbackFuture<'_>
            },
        ));
        Ok(())
    }

    /// Write an entry to the backend and dispatch callbacks.
    ///
    /// This is the central coordination point for all entry writes in the system.
    /// All writes must go through this method to ensure:
    /// - Entries are persisted to the backend
    /// - Appropriate callbacks are triggered based on write source
    /// - Hooks have full context (entry, database, instance)
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
        // 1. Persist to backend storage
        self.backend().put(verification, entry.clone()).await?;

        // 2. Look up and execute callbacks based on write source
        // Clone the callbacks to avoid holding the lock while executing callbacks.
        let per_db_callbacks = self
            .inner
            .write_callbacks
            .lock()
            .unwrap()
            .get(&(source, tree_id.clone()))
            .cloned();

        let global_callbacks = self
            .inner
            .global_write_callbacks
            .lock()
            .unwrap()
            .get(&source)
            .cloned();

        // 3. Execute callbacks if any are registered
        let has_callbacks = per_db_callbacks.is_some() || global_callbacks.is_some();
        if has_callbacks {
            // Create a Database handle for the callbacks
            // Use open_readonly since we only need it for callback context
            let database = Database::open_unauthenticated(tree_id.clone(), self)?;

            // Execute per-database callbacks
            if let Some(callbacks) = per_db_callbacks {
                for callback in callbacks {
                    if let Err(e) = callback(&entry, &database, self).await {
                        tracing::error!(
                            tree_id = %tree_id,
                            entry_id = %entry.id(),
                            source = ?source,
                            "Per-database callback failed: {}", e
                        );
                        // Continue executing other callbacks even if one fails
                    }
                }
            }

            // Execute global callbacks
            if let Some(callbacks) = global_callbacks {
                for callback in callbacks {
                    if let Err(e) = callback(&entry, &database, self).await {
                        tracing::error!(
                            tree_id = %tree_id,
                            entry_id = %entry.id(),
                            source = ?source,
                            "Global callback failed: {}", e
                        );
                        // Continue executing other callbacks even if one fails
                    }
                }
            }
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Error, backend::database::InMemory, crdt::Doc};
    use std::path::Path;

    async fn save_in_memory_backend(instance: &Instance, path: &Path) -> Result<(), Error> {
        let backend = instance.backend().as_arc_backend_impl();
        let in_memory = backend
            .as_any()
            .downcast_ref::<InMemory>()
            .expect("Expected in-memory backend");
        in_memory.save_to_file(path).await
    }

    async fn load_in_memory_backend(path: &Path) -> Result<InMemory, Error> {
        InMemory::load_from_file(path).await
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Argon2 password hashing is extremely slow under Miri
    async fn test_create_user() -> Result<(), Error> {
        let backend = InMemory::new();
        let instance = Instance::open(Box::new(backend)).await?;

        // Create user with password
        let user_uuid = instance
            .create_user("alice", Some("password123"))
            .await
            .unwrap();

        assert!(!user_uuid.is_empty());

        // Verify user appears in list
        let users = instance.list_users().await.unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0], "alice");
        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Argon2 password hashing is extremely slow under Miri
    async fn test_login_user() -> Result<(), Error> {
        let backend = InMemory::new();
        let instance = Instance::open(Box::new(backend)).await?;

        // Create user
        instance
            .create_user("alice", Some("password123"))
            .await
            .unwrap();

        // Login user
        let user = instance
            .login_user("alice", Some("password123"))
            .await
            .unwrap();
        assert_eq!(user.username(), "alice");

        // Invalid password should fail
        let result = instance.login_user("alice", Some("wrong_password")).await;
        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_new_database() {
        let backend = InMemory::new();
        let instance = Instance::open(Box::new(backend))
            .await
            .expect("Failed to create test instance");

        // Create database with User API
        instance.create_user("test", None).await.unwrap();
        let mut user = instance.login_user("test", None).await.unwrap();
        let key_id = user.add_private_key(None).await.unwrap();

        let mut settings = Doc::new();
        settings.set("name", "test_db");
        let database = user.create_database(settings, &key_id).await.unwrap();
        assert_eq!(database.get_name().await.unwrap(), "test_db");
    }

    #[tokio::test]
    async fn test_create_database_with_default_settings() {
        let backend = InMemory::new();
        let instance = Instance::open(Box::new(backend))
            .await
            .expect("Failed to create test instance");

        // Create database with User API (default settings via Doc::new())
        instance.create_user("test", None).await.unwrap();
        let mut user = instance.login_user("test", None).await.unwrap();
        let key_id = user.add_private_key(None).await.unwrap();
        let database = user.create_database(Doc::new(), &key_id).await.unwrap();

        // Database should have a valid root_id
        assert!(!database.root_id().is_empty());

        // Database should be loadable via user
        let loaded = user.open_database(database.root_id()).await.unwrap();
        assert_eq!(loaded.root_id(), database.root_id());
    }

    #[tokio::test]
    async fn test_new_database_without_key_fails() -> Result<(), Error> {
        let backend = InMemory::new();
        let instance = Instance::open(Box::new(backend)).await?;

        // Create user but try to use nonexistent key
        instance.create_user("test", None).await?;
        let mut user = instance.login_user("test", None).await?;

        // Create database requires a valid signing key
        let mut settings = Doc::new();
        settings.set("name", "test_db");

        // This should fail with a nonexistent key_id
        let result = user.create_database(settings, "nonexistent_key").await;
        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_instance_load_new_backend() -> Result<(), Error> {
        use crate::clock::FixedClock;

        // Test that Instance::load() creates new system state for empty backend
        let backend = InMemory::new();
        let instance =
            Instance::open_with_clock(Box::new(backend), Arc::new(FixedClock::default())).await?;

        // Verify we can create and login a user
        instance.create_user("alice", None).await?;
        let user = instance.login_user("alice", None).await?;
        assert_eq!(user.username(), "alice");

        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses file I/O which Miri doesn't support
    async fn test_instance_load_existing_backend() -> Result<(), Error> {
        use crate::clock::FixedClock;

        // Use a temporary file path for testing
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("eidetica_test_instance_load.json");

        // Create an instance and user, then save the backend
        let backend1 = InMemory::new();
        let instance1 =
            Instance::open_with_clock(Box::new(backend1), Arc::new(FixedClock::default())).await?;
        instance1.create_user("bob", None).await?;
        let mut user1 = instance1.login_user("bob", None).await?;

        // Get the default key (earliest created key)
        let default_key = user1.get_default_key()?;

        // Create a user database to verify it persists
        let mut settings = Doc::new();
        settings.set("name", "bob_database");
        user1.create_database(settings, &default_key).await?;

        // Save the backend to file
        save_in_memory_backend(&instance1, &path).await?;

        // Drop the first instance
        drop(instance1);
        drop(user1);

        // Load a new backend from the saved file
        let backend2 = load_in_memory_backend(&path).await?;
        let instance2 =
            Instance::open_with_clock(Box::new(backend2), Arc::new(FixedClock::default())).await?;

        // Verify the user still exists
        let users = instance2.list_users().await?;
        assert_eq!(users.len(), 1);
        assert_eq!(users[0], "bob");

        // Verify we can login the existing user
        let user2 = instance2.login_user("bob", None).await?;
        assert_eq!(user2.username(), "bob");

        // Clean up the temporary file
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }

        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses file I/O which Miri doesn't support
    async fn test_instance_load_device_id_persistence() -> Result<(), Error> {
        // Test that device_id remains the same across reloads
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("eidetica_test_device_id.json");

        // Create instance and get device_id
        let backend1 = InMemory::new();
        let instance1 = Instance::open(Box::new(backend1)).await?;
        let device_id1 = instance1.device_id_string();

        // Save backend
        save_in_memory_backend(&instance1, &path).await?;
        drop(instance1);

        // Load backend and verify device_id is the same
        let backend2 = load_in_memory_backend(&path).await?;
        let instance2 = Instance::open(Box::new(backend2)).await?;
        let device_id2 = instance2.device_id_string();

        assert_eq!(
            device_id1, device_id2,
            "Device ID should persist across reloads"
        );

        // Clean up
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }

        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Argon2 password hashing is extremely slow under Miri
    async fn test_instance_load_with_password_protected_users() -> Result<(), Error> {
        // Test that password-protected users work correctly after reload
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("eidetica_test_password_users.json");

        // Create instance with password-protected user
        let backend1 = InMemory::new();
        let instance1 = Instance::open(Box::new(backend1)).await?;
        instance1
            .create_user("secure_alice", Some("secret123"))
            .await?;
        let user1 = instance1
            .login_user("secure_alice", Some("secret123"))
            .await?;
        assert_eq!(user1.username(), "secure_alice");
        drop(user1);

        // Save backend
        save_in_memory_backend(&instance1, &path).await?;
        drop(instance1);

        // Reload and verify password still works
        let backend2 = load_in_memory_backend(&path).await?;
        let instance2 = Instance::open(Box::new(backend2)).await?;

        // Correct password should work
        let user2 = instance2
            .login_user("secure_alice", Some("secret123"))
            .await?;
        assert_eq!(user2.username(), "secure_alice");

        // Wrong password should fail
        let result = instance2
            .login_user("secure_alice", Some("wrong_password"))
            .await;
        assert!(result.is_err(), "Login with wrong password should fail");

        // No password should fail
        let result = instance2.login_user("secure_alice", None).await;
        assert!(
            result.is_err(),
            "Login without password should fail for password-protected user"
        );

        // Clean up
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }

        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Argon2 password hashing is extremely slow under Miri
    async fn test_instance_load_multiple_users() -> Result<(), Error> {
        // Test that multiple users persist correctly
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("eidetica_test_multiple_users.json");

        // Create instance with multiple users (mix of passwordless and password-protected)
        let backend1 = InMemory::new();
        let instance1 = Instance::open(Box::new(backend1)).await?;

        instance1.create_user("alice", None).await?;
        instance1.create_user("bob", Some("bobpass")).await?;
        instance1.create_user("charlie", None).await?;
        instance1.create_user("diana", Some("dianapass")).await?;

        // Verify all users can login
        instance1.login_user("alice", None).await?;
        instance1.login_user("bob", Some("bobpass")).await?;
        instance1.login_user("charlie", None).await?;
        instance1.login_user("diana", Some("dianapass")).await?;

        // Save backend
        save_in_memory_backend(&instance1, &path).await?;
        drop(instance1);

        // Reload and verify all users still exist and can login
        let backend2 = load_in_memory_backend(&path).await?;
        let instance2 = Instance::open(Box::new(backend2)).await?;

        let users = instance2.list_users().await?;
        assert_eq!(users.len(), 4, "All 4 users should be present after reload");
        assert!(users.contains(&"alice".to_string()));
        assert!(users.contains(&"bob".to_string()));
        assert!(users.contains(&"charlie".to_string()));
        assert!(users.contains(&"diana".to_string()));

        // Verify login still works for all users
        instance2.login_user("alice", None).await?;
        instance2.login_user("bob", Some("bobpass")).await?;
        instance2.login_user("charlie", None).await?;
        instance2.login_user("diana", Some("dianapass")).await?;

        // Clean up
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }

        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses file I/O which Miri doesn't support
    async fn test_instance_load_user_databases_persist() -> Result<(), Error> {
        use crate::clock::FixedClock;

        // Test that user-created databases persist across reloads
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("eidetica_test_user_dbs.json");

        // Create instance, user, and multiple databases
        let backend1 = InMemory::new();
        let instance1 =
            Instance::open_with_clock(Box::new(backend1), Arc::new(FixedClock::default())).await?;
        instance1.create_user("eve", None).await?;
        let mut user1 = instance1.login_user("eve", None).await?;

        // Get the default key (earliest created key)
        let default_key = user1.get_default_key()?;

        // Create multiple databases
        let mut settings1 = Doc::new();
        settings1.set("name", "database_one");
        settings1.set("purpose", "testing");
        let db1 = user1.create_database(settings1, &default_key).await?;
        let db1_root = db1.root_id().clone();

        let mut settings2 = Doc::new();
        settings2.set("name", "database_two");
        settings2.set("purpose", "production");
        let db2 = user1.create_database(settings2, &default_key).await?;
        let db2_root = db2.root_id().clone();

        drop(db1);
        drop(db2);
        drop(user1);

        // Save backend
        save_in_memory_backend(&instance1, &path).await?;
        drop(instance1);

        // Reload and verify databases still exist
        let backend2 = load_in_memory_backend(&path).await?;
        let instance2 =
            Instance::open_with_clock(Box::new(backend2), Arc::new(FixedClock::default())).await?;
        let user2 = instance2.login_user("eve", None).await?;

        // Load databases by root_id and verify their settings
        let loaded_db1 = user2.open_database(&db1_root).await?;
        assert_eq!(loaded_db1.get_name().await?, "database_one");
        let settings1_doc = loaded_db1.get_settings().await?;
        assert_eq!(settings1_doc.get_string("purpose").await?, "testing");

        let loaded_db2 = user2.open_database(&db2_root).await?;
        assert_eq!(loaded_db2.get_name().await?, "database_two");
        let settings2_doc = loaded_db2.get_settings().await?;
        assert_eq!(settings2_doc.get_string("purpose").await?, "production");

        // Clean up
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }

        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses file I/O which Miri doesn't support
    async fn test_instance_load_idempotency() -> Result<(), Error> {
        use crate::clock::FixedClock;

        // Test that loading the same backend multiple times gives consistent results
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("eidetica_test_idempotency.json");

        // Create and save initial state
        let backend1 = InMemory::new();
        let instance1 =
            Instance::open_with_clock(Box::new(backend1), Arc::new(FixedClock::default())).await?;
        instance1.create_user("frank", None).await?;
        let device_id1 = instance1.device_id_string();

        save_in_memory_backend(&instance1, &path).await?;
        drop(instance1);

        // Load the same backend multiple times and verify consistency
        for i in 0..3 {
            let backend = load_in_memory_backend(&path).await?;
            let instance =
                Instance::open_with_clock(Box::new(backend), Arc::new(FixedClock::default()))
                    .await?;

            // Device ID should be the same every time
            let device_id = instance.device_id_string();
            assert_eq!(
                device_id, device_id1,
                "Device ID should be consistent on reload {i}"
            );

            // User list should be the same
            let users = instance.list_users().await?;
            assert_eq!(users.len(), 1);
            assert_eq!(users[0], "frank");

            // Should be able to login
            let user = instance.login_user("frank", None).await?;
            assert_eq!(user.username(), "frank");

            drop(user);
            drop(instance);
        }

        // Clean up
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }

        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses file I/O which Miri doesn't support
    async fn test_instance_load_new_vs_existing() -> Result<(), Error> {
        use crate::clock::FixedClock;

        // Test the difference between loading new and existing backends
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("eidetica_test_new_vs_existing.json");

        // Create first instance (new backend)
        let backend1 = InMemory::new();
        let instance1 =
            Instance::open_with_clock(Box::new(backend1), Arc::new(FixedClock::default())).await?;
        let device_id1 = instance1.device_id_string();
        instance1.create_user("grace", None).await?;

        save_in_memory_backend(&instance1, &path).await?;
        drop(instance1);

        // Load existing backend
        let backend2 = load_in_memory_backend(&path).await?;
        let instance2 =
            Instance::open_with_clock(Box::new(backend2), Arc::new(FixedClock::default())).await?;
        let device_id2 = instance2.device_id_string();

        // Device ID should match (existing backend)
        assert_eq!(device_id1, device_id2);

        // User should exist (existing backend)
        let users = instance2.list_users().await?;
        assert_eq!(users.len(), 1);
        assert_eq!(users[0], "grace");
        drop(instance2);

        // Create completely new instance (different backend)
        let backend3 = InMemory::new();
        let instance3 =
            Instance::open_with_clock(Box::new(backend3), Arc::new(FixedClock::default())).await?;
        let device_id3 = instance3.device_id_string();

        // Device ID should be different (new backend)
        assert_ne!(device_id1, device_id3);

        // No users should exist (new backend)
        let users = instance3.list_users().await?;
        assert_eq!(users.len(), 0);

        // Clean up
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }

        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses file I/O which Miri doesn't support
    async fn test_instance_create_strict_fails_on_existing() -> Result<(), Error> {
        // Test that Instance::create() fails on already-initialized backend
        use crate::clock::FixedClock;

        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join("eidetica_test_create_strict.json");

        // Create first instance
        let backend1 = Arc::new(InMemory::new());
        let instance1 =
            Instance::create_internal(backend1, Arc::new(FixedClock::default())).await?;
        instance1.create_user("alice", None).await?;

        // Save backend
        save_in_memory_backend(&instance1, &path).await?;
        drop(instance1);

        // Try to create() on the existing backend - should fail
        // (fails immediately before clock is used, so SystemClock is fine)
        let backend2 = load_in_memory_backend(&path).await?;
        let result = Instance::create(Box::new(backend2)).await;
        assert!(result.is_err(), "create() should fail on existing backend");

        // Verify error type
        if let Err(err) = result {
            if let crate::Error::Instance(instance_err) = err {
                assert!(
                    instance_err.is_already_exists(),
                    "Error should be InstanceAlreadyExists"
                );
            } else {
                panic!("Expected Instance error");
            }
        }

        // Verify open() still works
        let backend3 = load_in_memory_backend(&path).await?;
        let instance3 =
            Instance::open_with_clock(Box::new(backend3), Arc::new(FixedClock::default())).await?;
        let users = instance3.list_users().await?;
        assert_eq!(users.len(), 1);
        assert_eq!(users[0], "alice");

        // Clean up
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }

        Ok(())
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)] // Uses SystemTime for timestamps in create_user
    async fn test_instance_create_on_fresh_backend() -> Result<(), Error> {
        // Test that Instance::create() succeeds on fresh backend
        let backend = InMemory::new();
        let instance = Instance::create(Box::new(backend)).await?;

        // Verify we can create users
        instance.create_user("bob", None).await?;
        let user = instance.login_user("bob", None).await?;
        assert_eq!(user.username(), "bob");

        Ok(())
    }
}
