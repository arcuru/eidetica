//!
//! Provides the main database structures (`Instance` and `Database`).
//!
//! `Instance` manages multiple `Database` instances and interacts with the storage `Database`.
//! `Database` represents a single, independent history of data entries, analogous to a table or branch.

use std::{
    collections::HashMap,
    future::Future,
    path::PathBuf,
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
    backend::{BackendImpl, InstanceMetadata, InstanceSecrets, VerificationStatus},
    entry::ID,
    sync::Sync,
    user::User,
};
#[cfg(all(unix, feature = "service"))]
use crate::{auth::SigKey, service::client::RemoteConnection};

pub mod backend;
pub mod errors;
pub mod new_user;
pub mod settings_merge;
pub mod url;

#[cfg(test)]
mod tests;

// Re-export main types for easier access
#[cfg(all(unix, feature = "service"))]
use backend::RemoteBackend;
use backend::{Backend, LocalBackend};
pub use errors::InstanceError;
pub use new_user::NewUser;

/// Indicates whether an entry write originated locally or from a remote source (e.g., sync).
///
/// This distinction allows different callbacks to be triggered based on the write source,
/// enabling behaviors like "only trigger sync for local writes" or "only update UI for remote writes".
///
/// Marked `#[non_exhaustive]` so additional source variants can be added in the
/// future (e.g. a distinct `Promoted` for verify-pass fires that surface
/// already-stored entries) without breaking exhaustive `match` arms in user
/// code. Always include a wildcard arm when matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum WriteSource {
    /// Write originated from a local transaction commit
    Local,
    /// Write originated from a remote source (e.g., sync, replication)
    Remote,
}

/// A cursor-advance notification delivered to a write callback.
///
/// A `WriteEvent` carries *no entry payloads* — only the cursor brackets
/// (`previous_tips` → `post_tips`) and the [`WriteSource`]. Callbacks that
/// only care that *something* changed (cache invalidation, UI wake-ups)
/// can act on the event directly without touching the wire or the DAG.
/// Callbacks that need to enumerate or fetch the new entries call
/// [`Database::ids_added`](crate::Database::ids_added) with the brackets:
///
/// ```rust,no_run
/// # use eidetica::{instance::WriteEvent, Database, Result};
/// # async fn example(event: &WriteEvent, db: &Database) -> Result<()> {
/// // Enumerate the IDs added between the two cursors
/// let new_ids = db.ids_added(event.previous_tips(), event.post_tips()).await?;
/// for id in new_ids {
///     // … fetch entry bodies via db.get_entry(id) if needed …
/// }
/// # Ok(()) }
/// ```
///
/// Cursor semantics: `previous_tips` is this callback's frontier *before*
/// this fire — the user-supplied initial tips on the first fire, then the
/// preceding fire's `post_tips` on each subsequent fire. The cursor
/// advances to `post_tips` synchronously *before* the user closure is
/// awaited, so the next fire is guaranteed to bracket against the latest
/// observed frontier even if the closure is slow.
///
/// Fires only on settled-state (Verified) writes. See
/// [`Notification::DatabaseWrite`](crate::service::protocol::Notification::DatabaseWrite)
/// rustdoc for the verification contract.
#[derive(Debug, Clone)]
pub struct WriteEvent {
    /// The DAG tips this callback was last delivered at — its cursor
    /// before this fire. Subsequent fires for the same callback will have
    /// `previous_tips = this fire's post_tips`.
    previous_tips: Vec<ID>,
    /// The DAG tips after this write. Equal to this callback's cursor
    /// *after* the fire. Useful for "what's the frontier I'm now caught
    /// up to" without an extra `get_tips` call.
    post_tips: Vec<ID>,
    /// Whether this write originated locally or from a remote sync.
    source: WriteSource,
}

impl WriteEvent {
    /// Get the DAG tips at this callback's cursor *before* this fire.
    ///
    /// The first fire on a freshly-registered callback returns the
    /// initial tips passed at registration time. Subsequent fires
    /// return the previous fire's `post_tips`.
    pub fn previous_tips(&self) -> &[ID] {
        &self.previous_tips
    }

    /// Get the DAG tips at this callback's cursor *after* this fire.
    ///
    /// The cursor advances to this value before the callback is awaited,
    /// so the next fire on the same callback will have
    /// `previous_tips() == this fire's post_tips()`.
    pub fn post_tips(&self) -> &[ID] {
        &self.post_tips
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

/// One per-database callback registration plus the cursor that tracks the
/// frontier this specific callback has observed.
///
/// Each fire reads the cursor, builds a [`WriteEvent`] with the cursor
/// as `previous_tips`, advances the cursor to the post-write tips, and
/// then invokes the callback. The cursor mutex is held synchronously
/// during the read/advance — never across the user callback's `.await`.
///
/// Stored in `Vec<Arc<PerDbCallbackEntry>>` on each tree so
/// `fire_write_callbacks` can snapshot Arcs under the registry mutex
/// and run the dispatches outside it.
pub(crate) struct PerDbCallbackEntry {
    pub(crate) id: CallbackId,
    /// Cursor — the post-write tips of the most recent event this
    /// callback has been delivered, or the user-provided initial tips
    /// from the registration call before any event has fired.
    pub(crate) last_tips: std::sync::Mutex<Vec<ID>>,
    pub(crate) callback: AsyncWriteCallbackFn,
}

/// Type alias for the per-database callback list on a tree.
type PerDbCallbackVec = Vec<Arc<PerDbCallbackEntry>>;

/// Type alias for the global write callback list. Globals fire for every
/// write on every tree (used by sync today). No cursor — globals are
/// tree-agnostic and don't have a meaningful per-tree frontier to track,
/// so they continue to use whatever `previous_tips` the caller passes in.
type GlobalCallbackVec = Vec<(CallbackId, AsyncWriteCallbackFn)>;

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
            // On a connected instance, dropping the last local callback
            // for this tree transitions the wire-side subscription to
            // `Idle`. Daemon-side stays subscribed through a grace
            // window so a quick re-registration is a no-op; the sweep
            // task in the connection unsubscribes after the window
            // elapses.
            //
            // The local-instance build doesn't read the `was_last`
            // signal; bind it under the `cfg` so the inactive build
            // doesn't carry a dead variable.
            #[cfg(all(unix, feature = "service"))]
            {
                let was_last = instance.remove_write_callback(&self.tree_id, self.id);
                if was_last
                    && let Some(conn) = instance.remote_connection()
                {
                    conn.transition_to_idle(&self.tree_id);
                }
            }
            #[cfg(not(all(unix, feature = "service")))]
            {
                let _ = instance.remove_write_callback(&self.tree_id, self.id);
            }
        }
    }
}

/// Internal state for Instance
///
/// This structure holds the actual implementation data for Instance.
/// Instance itself is just a cheap-to-clone handle wrapping Arc<InstanceInternal>.
pub(crate) struct InstanceInternal {
    /// The database storage backend
    backend: Arc<dyn Backend>,
    /// Time provider for timestamps
    clock: Arc<dyn Clock>,
    /// Synchronization module for this database instance
    /// TODO: Overengineered, Sync can be created by default but disabled
    sync: std::sync::OnceLock<Arc<Sync>>,
    /// Public instance metadata (device identity, system database IDs)
    metadata: InstanceMetadata,
    /// Private instance secrets (None for remote instances without key access)
    secrets: Option<InstanceSecrets>,
    /// JSON snapshot file path for an in-memory backend constructed via
    /// `memory:///path.json` (or set explicitly through
    /// [`Instance::snapshot_to_path`]). [`Instance::flush`] and the
    /// [`Drop`] safety net write through this. `None` on any non-snapshot
    /// backend.
    ///
    /// The mutex serves double duty: it guards the path slot itself (so
    /// `set_snapshot_path` doesn't race with readers) AND serializes the
    /// actual write so concurrent callers from `flush` / `snapshot_to_path`
    /// / `Drop` don't race on the shared `<path>.tmp` staging file in
    /// [`InMemory::save_to_file`]. Held across sync I/O only — never
    /// across an `.await`. Poison-tolerant: a panic mid-write leaves the
    /// on-disk snapshot unchanged but must not strand the [`Instance`].
    snapshot_path: Mutex<Option<PathBuf>>,
    /// Per-database callbacks keyed by tree_id. Each entry carries its own
    /// cursor (`last_tips`) so fires can build a callback-specific
    /// `previous_tips` regardless of when the callback registered or when
    /// the most recent fire actually advanced its frontier. Consumers
    /// branch on [`WriteEvent::source`] if they only care about one
    /// source.
    write_callbacks: Mutex<HashMap<ID, PerDbCallbackVec>>,
    /// Global callbacks fired for every write across every database.
    /// Tree-agnostic — no per-callback cursor.
    global_write_callbacks: Mutex<GlobalCallbackVec>,
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
                    self.write_callbacks
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .len()
                ),
            )
            .field(
                "global_write_callbacks",
                &format!(
                    "<{} global callbacks>",
                    self.global_write_callbacks
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .len()
                ),
            )
            .field(
                "next_callback_id",
                &self.next_callback_id.load(Ordering::Relaxed),
            )
            .finish()
    }
}

impl InstanceInternal {
    /// Synchronously write a JSON snapshot of the underlying backend to `path`.
    ///
    /// Returns [`InstanceError::SnapshotNotSupported`] for any backend other
    /// than the local in-memory backend. Shared by [`Instance::snapshot_to_path`],
    /// [`Instance::flush`], and the [`Drop`] fallback so the three can't drift.
    ///
    /// **Caller must hold the [`snapshot_path`](Self::snapshot_path) mutex.**
    /// That lock serializes the write — without it, concurrent callers
    /// would race on the shared `<path>.tmp` staging file in
    /// [`InMemory::save_to_file`]. The critical section is fully sync; no
    /// `.await` happens while the lock is held.
    fn save_snapshot_locked(&self, path: &std::path::Path) -> Result<()> {
        use crate::backend::database::InMemory;
        let engine = self
            .backend
            .local_engine()
            .ok_or(InstanceError::SnapshotNotSupported)?;
        let in_memory = engine
            .as_any()
            .downcast_ref::<InMemory>()
            .ok_or(InstanceError::SnapshotNotSupported)?;
        in_memory.save_to_file(path)
    }
}

/// Best-effort snapshot save on the *last* `InstanceInternal` drop.
///
/// Fires when the `Arc<InstanceInternal>` reaches refcount 0 and a snapshot
/// path is armed (i.e. the `Instance` was constructed via a
/// `memory:///path.json` URL). [`Instance::flush`] does **not** clear the
/// snapshot path, so Drop fires even after a successful `flush()` — the
/// write is idempotent (same atomic tmp+rename), so the worst case is one
/// extra write of unchanged JSON.
///
/// **Errors are logged via `tracing::error!`, not surfaced** — `Drop` can't
/// return a `Result` and panicking would be worse than logging. Apps that
/// care about snapshot durability should call [`Instance::flush`] at
/// well-defined checkpoints and inspect its `Result`; Drop is a safety net,
/// not the primary persistence path. If `flush()` failed with a permanent
/// error (e.g. nonexistent parent directory), Drop will fail the same way
/// and emit a second log line — accept this redundancy as the cost of a
/// best-effort fallback.
///
/// **Blocking I/O warning:** the snapshot write is synchronous
/// (`std::fs::write` + `rename`). If the `Instance` is dropped on a tokio
/// worker thread, this blocks that worker for the duration of the write —
/// negligible for small snapshots, but pathological for very large ones.
/// Prefer `flush().await` (which still blocks briefly, but does so under
/// explicit caller control).
impl Drop for InstanceInternal {
    fn drop(&mut self) {
        // Drop runs at Arc refcount 0, so no other handle can race here —
        // any in-flight `flush()` future holds `&self` and thus an Arc
        // clone, which would have prevented Drop from firing. We still
        // acquire the lock for the write so the locking discipline in
        // `save_snapshot_locked`'s doc-comment holds uniformly. The lock
        // is uncontended at this point.
        let mut guard = match self.snapshot_path.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let Some(path) = guard.take() else { return };

        if let Err(e) = self.save_snapshot_locked(&path) {
            tracing::error!(
                snapshot_path = %path.display(),
                error = %e,
                "Drop: snapshot save failed. Call `Instance::flush().await` at \
                 checkpoints to inspect the error via Result; Drop is a safety net only.",
            );
        }
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
/// # use eidetica::{Instance, NewUser, crdt::Doc};
/// # #[tokio::main]
/// # async fn main() -> eidetica::Result<()> {
/// // Bootstrap a fresh instance with an initial admin user. The first user
/// // created on an instance is automatically granted Admin on the system
/// // databases.
/// let (instance, maybe_user) = Instance::connect_or_create(
///     "memory://",
///     NewUser::passwordless("alice"),
/// ).await?;
/// let mut user = maybe_user.expect("memory:// is always fresh");
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
    /// Open a connection to an eidetica instance described by a connection URL.
    ///
    /// Strict load: returns [`InstanceError::NotInitialized`] when the URL
    /// points at an embedded backend (`sqlite://`, `postgres://`, `memory://`)
    /// that has no eidetica metadata yet. Use
    /// [`Instance::connect_or_create`] to bootstrap an embedded backend on
    /// first run.
    ///
    /// Supported URL schemes:
    /// - `sqlite://./app.db` — embedded sqlite backend; URL is passed through
    ///   to `sqlx::sqlite`, so any sqlx-accepted form works
    ///   (`?mode=rwc&journal_mode=WAL` etc.).
    /// - `postgres://user:pwd@host/db` — embedded postgres backend; URL is
    ///   passed through to `sqlx::postgres`.
    /// - `unix:///run/eidetica/sock` — thin client to a running daemon.
    /// - `memory://` — empty in-memory backend. Strict load against an
    ///   empty in-memory backend always errors `NotInitialized`; use
    ///   `connect_or_create` for a fresh in-memory instance.
    /// - `memory:///path/to/snap.json` — in-memory backend with a JSON
    ///   snapshot file (load-on-start; snapshot writes via
    ///   [`Instance::flush`] / [`Instance::snapshot_to_path`] / Drop fallback).
    ///
    /// See [`crate::instance::url`] for the full URL grammar.
    ///
    /// # Example
    ///
    /// `connect()` only succeeds against an already-initialised backend.
    /// The two-phase pattern below bootstraps once, then re-opens with
    /// the strict load:
    ///
    /// ```
    /// # #[tokio::main]
    /// # async fn main() -> eidetica::Result<()> {
    /// use eidetica::{Instance, NewUser};
    ///
    /// let temp = tempfile::tempdir()?;
    /// let snapshot = temp.path().join("app.json");
    /// let url = format!("memory://{}", snapshot.display());
    ///
    /// // First run: bootstrap and flush a snapshot to disk.
    /// {
    ///     let (instance, maybe_user) =
    ///         Instance::connect_or_create(&url, NewUser::passwordless("alice")).await?;
    ///     let _user = maybe_user.expect("fresh bootstrap on first run");
    ///     instance.flush()?;
    /// }
    ///
    /// // Later: strict connect against the persisted snapshot.
    /// let instance = Instance::connect(&url).await?;
    /// let _user = instance.login_user("alice", None).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Calling `connect()` on a backend with no eidetica metadata returns
    /// [`InstanceError::NotInitialized`]; reach for
    /// [`Instance::connect_or_create`] when first-run bootstrap is part
    /// of the expected lifecycle.
    pub async fn connect(url: impl AsRef<str>) -> Result<Self> {
        Self::connect_impl(url.as_ref(), Arc::new(SystemClock)).await
    }

    /// Open or initialise an eidetica instance described by a connection URL.
    ///
    /// On the load arm: identical to [`Instance::connect`]; `initial` is
    /// silently ignored and the second tuple element is `None`.
    ///
    /// On the bootstrap arm: initialises the backend at the URL with the
    /// supplied [`NewUser`] as the first admin and returns
    /// `(Instance, Some(User))`. Only embedded backends
    /// (sqlite/postgres/memory) ever take the bootstrap arm — `unix://` URLs
    /// degrade to `connect` (the daemon owns its own initialisation), so the
    /// returned `Option<User>` is always `None` for `unix://`.
    ///
    /// # Example
    /// ```
    /// # use eidetica::{Instance, NewUser};
    /// # #[tokio::main]
    /// # async fn main() -> eidetica::Result<()> {
    /// let (instance, maybe_user) = Instance::connect_or_create(
    ///     "memory://",
    ///     NewUser::passwordless("alice"),
    /// ).await?;
    /// let mut user = match maybe_user {
    ///     Some(u) => u,
    ///     None => instance.login_user("alice", None).await?,
    /// };
    /// # let _ = user.get_default_key()?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect_or_create(
        url: impl AsRef<str>,
        initial: NewUser,
    ) -> Result<(Self, Option<User>)> {
        Self::connect_or_create_impl(url.as_ref(), initial, Arc::new(SystemClock)).await
    }

    /// Escape hatch: open or initialise an eidetica instance against a
    /// pre-built [`BackendImpl`] (sqlite, postgres, in-memory, or custom).
    ///
    /// Same load-or-bootstrap semantics as [`Instance::connect_or_create`]
    /// but skips URL parsing. Useful for tests, embedded apps that want to
    /// configure the backend's pool/runtime manually, or backends not yet
    /// exposed via a URL scheme.
    pub async fn connect_or_create_backend(
        backend: Box<dyn BackendImpl>,
        initial: NewUser,
    ) -> Result<(Self, Option<User>)> {
        Self::connect_or_create_backend_impl(
            Arc::from(backend),
            initial,
            Arc::new(SystemClock),
            None,
        )
        .await
    }

    /// Strict-load escape hatch: open an eidetica instance against a
    /// pre-built [`BackendImpl`] that's already been initialised. Mirrors
    /// [`Instance::connect`]'s strict semantics for the URL-less case.
    ///
    /// Errors with [`InstanceError::NotInitialized`] if the backend has no
    /// instance metadata; use [`Instance::connect_or_create_backend`] when you want
    /// to bootstrap on an empty backend.
    pub async fn open_backend(backend: Box<dyn BackendImpl>) -> Result<Self> {
        Self::open_impl(backend, Arc::new(SystemClock)).await
    }

    /// Test variant of [`Instance::open_backend`] with an injectable clock.
    #[cfg(any(test, feature = "testing"))]
    pub async fn open_backend_with_clock(
        backend: Box<dyn BackendImpl>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        Self::open_impl(backend, clock).await
    }

    /// Strict-create escape hatch: initialise an eidetica instance on a
    /// fresh pre-built [`BackendImpl`] and bootstrap an initial admin user.
    ///
    /// Errors with [`InstanceError::InstanceAlreadyExists`] if the backend
    /// is already initialised; use [`Instance::connect_or_create_backend`] when the
    /// caller doesn't want to choose between load and create up front.
    pub async fn create_backend(
        backend: Box<dyn BackendImpl>,
        initial: NewUser,
    ) -> Result<(Self, User)> {
        Self::create_backend_impl(backend, initial, Arc::new(SystemClock)).await
    }

    /// Test variant of [`Instance::create_backend`] with an injectable clock.
    ///
    /// Arg order: backend, clock, initial — clock goes in the middle so
    /// migrating from the prior `create_with_clock` is a pure rename.
    #[cfg(any(test, feature = "testing"))]
    pub async fn create_backend_with_clock(
        backend: Box<dyn BackendImpl>,
        clock: Arc<dyn Clock>,
        initial: NewUser,
    ) -> Result<(Self, User)> {
        Self::create_backend_impl(backend, initial, clock).await
    }

    async fn create_backend_impl(
        backend: Box<dyn BackendImpl>,
        initial: NewUser,
        clock: Arc<dyn Clock>,
    ) -> Result<(Self, User)> {
        let backend: Arc<dyn BackendImpl> = Arc::from(backend);
        if backend.get_instance_metadata().await?.is_some() {
            return Err(InstanceError::InstanceAlreadyExists.into());
        }
        Self::create_internal(backend, clock, initial).await
    }

    // Clock injection is exposed only through the pre-built-backend
    // variants ([`open_backend_with_clock`] and [`create_backend_with_clock`]).
    // The URL-based `connect_*` constructors deliberately have no
    // `_with_clock` siblings: every existing test that needs deterministic
    // timestamps already builds an `InMemory` backend directly, so a URL-
    // shaped clock entry point would be dead weight.

    // ============ Internal URL dispatchers ============

    async fn connect_impl(url: &str, clock: Arc<dyn Clock>) -> Result<Self> {
        let parsed = url::parse(url)?;
        match parsed {
            url::ConnectionUrl::Sqlite { url } => Self::connect_sqlite(&url, clock).await,
            url::ConnectionUrl::Postgres { url } => Self::connect_postgres(&url, clock).await,
            url::ConnectionUrl::Unix { socket_path } => {
                Self::connect_unix_socket(socket_path, clock).await
            }
            url::ConnectionUrl::Memory { snapshot_path } => {
                Self::connect_memory(snapshot_path, clock).await
            }
        }
    }

    async fn connect_or_create_impl(
        url: &str,
        initial: NewUser,
        clock: Arc<dyn Clock>,
    ) -> Result<(Self, Option<User>)> {
        let parsed = url::parse(url)?;
        match parsed {
            url::ConnectionUrl::Sqlite { url } => {
                let backend = open_sqlite_backend(&url).await?;
                Self::connect_or_create_backend_impl(Arc::from(backend), initial, clock, None).await
            }
            url::ConnectionUrl::Postgres { url } => {
                let backend = open_postgres_backend(&url).await?;
                Self::connect_or_create_backend_impl(Arc::from(backend), initial, clock, None).await
            }
            url::ConnectionUrl::Unix { socket_path } => {
                // Daemons own their own initialisation. connect_or_create
                // against `unix://` degrades to a plain connect; `initial`
                // is unused on this arm. Log it so the silent drop is
                // discoverable when debugging "why didn't my initial user
                // get created?" on a remote URL.
                tracing::debug!(
                    socket_path = %socket_path.display(),
                    username = %initial.username,
                    "connect_or_create against `unix://` is degrading to `connect`; \
                     `initial` is ignored — daemons own their own initialisation. \
                     Run `eidetica daemon init` to bootstrap a daemon-side instance."
                );
                let instance = Self::connect_unix_socket(socket_path, clock).await?;
                Ok((instance, None))
            }
            url::ConnectionUrl::Memory { snapshot_path } => {
                use crate::backend::database::InMemory;
                // Build the backend from a single `try_load_from_file` call.
                // `Ok(None)` means the file didn't exist at read time (the
                // bootstrap-friendly "first run" case → empty backend).
                // `Ok(Some(loaded))` means the file existed and parsed; if
                // it carries no instance metadata it's foreign data that
                // happened to satisfy the `SerializableDatabase` shape, and
                // we refuse to bootstrap over it (the next snapshot would
                // silently overwrite the caller's file). Doing the existence
                // test and the read in one call removes the TOCTOU window a
                // separate `path.exists()` check would open.
                let backend: Box<dyn BackendImpl> = match snapshot_path.as_deref() {
                    None => Box::new(InMemory::new()),
                    Some(path) => {
                        let loaded = InMemory::try_load_from_file(path).await.map_err(|e| {
                            InstanceError::InvalidSnapshot {
                                path: path.to_path_buf(),
                                reason: e.to_string(),
                            }
                        })?;
                        match loaded {
                            None => Box::new(InMemory::new()),
                            Some(loaded) => {
                                let boxed: Box<dyn BackendImpl> = Box::new(loaded);
                                if boxed.get_instance_metadata().await?.is_none() {
                                    return Err(InstanceError::InvalidSnapshot {
                                        path: path.to_path_buf(),
                                        reason: "snapshot file exists but contains no instance \
                                             metadata; refusing to bootstrap on top of foreign \
                                             data. Delete or move the file to create a fresh \
                                             instance at this path."
                                            .into(),
                                    }
                                    .into());
                                }
                                boxed
                            }
                        }
                    }
                };
                Self::connect_or_create_backend_impl(
                    Arc::from(backend),
                    initial,
                    clock,
                    snapshot_path,
                )
                .await
            }
        }
    }

    /// Internal: load-or-bootstrap against a pre-built backend, optionally
    /// remembering a snapshot path so Drop / flush can write to it.
    async fn connect_or_create_backend_impl(
        backend: Arc<dyn BackendImpl>,
        initial: NewUser,
        clock: Arc<dyn Clock>,
        snapshot_path: Option<PathBuf>,
    ) -> Result<(Self, Option<User>)> {
        if let Some(metadata) = backend.get_instance_metadata().await? {
            let instance = Self::open_impl_arc_with_metadata(backend, clock, metadata).await?;
            instance.set_snapshot_path(snapshot_path);
            Ok((instance, None))
        } else {
            let (instance, user) = Self::create_internal(backend, clock, initial).await?;
            instance.set_snapshot_path(snapshot_path);
            Ok((instance, Some(user)))
        }
    }

    // ============ Backend connection helpers ============

    #[cfg(all(unix, feature = "service"))]
    async fn connect_unix_socket(socket_path: PathBuf, clock: Arc<dyn Clock>) -> Result<Self> {
        let conn = crate::service::client::RemoteConnection::connect(&socket_path).await?;
        // Keep a clone for the post-construction `attach_instance` call:
        // the reader task spawned inside `RemoteConnection::connect` needs a
        // `WeakInstance` to route push notifications into, and the Instance
        // doesn't exist yet here.
        let conn_for_attach = conn.clone();
        let backend: Arc<dyn Backend> = Arc::new(RemoteBackend::new(conn, None));

        // Load metadata from the remote backend
        let metadata = backend
            .get_instance_metadata()
            .await?
            .ok_or(InstanceError::DeviceKeyNotFound)?;

        // No local secrets — keys are held server-side after login.
        let inner = Arc::new(InstanceInternal {
            backend,
            clock,
            sync: std::sync::OnceLock::new(),
            metadata,
            secrets: None,
            snapshot_path: Mutex::new(None),
            write_callbacks: Mutex::new(HashMap::new()),
            global_write_callbacks: Mutex::new(Vec::new()),
            next_callback_id: AtomicU64::new(0),
            tree_locks: Mutex::new(HashMap::new()),
        });
        let instance = Self { inner };
        // Hand the reader task a Weak reference so it can dispatch
        // server-pushed `Notification::DatabaseWrite` frames into this
        // Instance's callback registry. Must run *after* the Instance is
        // constructed; until then the reader task drops notifications
        // (none can arrive because the client only subscribes lazily on
        // the first `Database::on_write` call).
        conn_for_attach.attach_instance(instance.downgrade());
        Ok(instance)
    }

    #[cfg(not(all(unix, feature = "service")))]
    async fn connect_unix_socket(_socket_path: PathBuf, _clock: Arc<dyn Clock>) -> Result<Self> {
        Err(InstanceError::BackendUnavailable {
            scheme: "unix",
            missing_feature: "service",
        }
        .into())
    }

    async fn connect_sqlite(url: &str, clock: Arc<dyn Clock>) -> Result<Self> {
        let backend = open_sqlite_backend(url).await?;
        Self::open_impl(backend, clock).await
    }

    async fn connect_postgres(url: &str, clock: Arc<dyn Clock>) -> Result<Self> {
        let backend = open_postgres_backend(url).await?;
        Self::open_impl(backend, clock).await
    }

    async fn connect_memory(snapshot_path: Option<PathBuf>, clock: Arc<dyn Clock>) -> Result<Self> {
        use crate::backend::database::InMemory;
        // Strict load: a snapshot URL that points at a non-existent file
        // cannot satisfy `connect`'s "must already be initialised" contract.
        // `try_load_from_file` returns `Ok(None)` when the file doesn't
        // exist, which we translate into a pointed `InvalidSnapshot`. Using
        // the same call for the existence test and the read removes the
        // TOCTOU window a separate `path.exists()` check would open: if the
        // file vanishes mid-call, the underlying `read_to_string` surfaces
        // `NotFound` and lands us in the same `None` arm.
        let backend: Box<dyn BackendImpl> = match snapshot_path.as_deref() {
            None => Box::new(InMemory::new()),
            Some(path) => {
                let loaded = InMemory::try_load_from_file(path).await.map_err(|e| {
                    InstanceError::InvalidSnapshot {
                        path: path.to_path_buf(),
                        reason: e.to_string(),
                    }
                })?;
                match loaded {
                    Some(loaded) => Box::new(loaded),
                    None => {
                        return Err(InstanceError::InvalidSnapshot {
                            path: path.to_path_buf(),
                            reason: "snapshot file does not exist; \
                                     use `Instance::connect_or_create` to bootstrap a new instance \
                                     at this path, or pass `memory://` for an ephemeral instance"
                                .into(),
                        }
                        .into());
                    }
                }
            }
        };
        let instance = Self::open_impl(backend, clock).await?;
        instance.set_snapshot_path(snapshot_path);
        Ok(instance)
    }

    /// Flush deferred persistence state to disk.
    ///
    /// For an `Instance` constructed via a `memory:///path.json` URL, this
    /// writes the current backend state to the snapshot path (atomic on
    /// POSIX — `<path>.tmp` then rename). For sqlite/postgres/unix
    /// backends this is a no-op; those storage layers handle persistence
    /// inline.
    ///
    /// Idempotent and reentrant — call it as often as you like at
    /// well-defined checkpoints. The snapshot path stays armed, so the
    /// [`Drop`] fallback continues to fire on the last handle as a safety
    /// net. The `Instance` (and any clones) remain fully usable after
    /// `flush()` returns; this is not a shutdown.
    ///
    /// If `flush()` fails (e.g. nonexistent parent directory), the error
    /// surfaces in the `Result`. Drop will later try the same write and
    /// fail the same way, logging via `tracing::error!`. The duplicate
    /// signal is intentional — Drop must report what it sees.
    ///
    /// **Blocking I/O note:** the snapshot write is synchronous
    /// (`std::fs::write` + `rename`) and runs inline on the caller. Hence
    /// the sync signature — there is no `.await` inside. If you're calling
    /// from a tokio task, this briefly blocks the runtime worker;
    /// negligible for small snapshots.
    pub fn flush(&self) -> Result<()> {
        // Acquire the snapshot_path lock once and hold it across the
        // write — the lock both gates the path slot and serializes the
        // sync I/O so concurrent flushes don't clobber each other's
        // staging tempfile. The path stays armed (we read, don't take)
        // so subsequent flushes and the Drop safety net keep working.
        let guard = self
            .inner
            .snapshot_path
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(path) = guard.as_deref() {
            self.inner.save_snapshot_locked(path)?;
        }
        Ok(())
    }

    /// Write a JSON snapshot of the in-memory backend to `path`.
    ///
    /// The write goes to `<path>.tmp` and then renames into place. On POSIX
    /// the rename is atomic; on Windows it is not atomic when the
    /// destination already exists. Returns
    /// [`InstanceError::SnapshotNotSupported`] on any backend other than
    /// the in-memory backend.
    pub fn snapshot_to_path(&self, path: impl AsRef<std::path::Path>) -> Result<()> {
        let _guard = self
            .inner
            .snapshot_path
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        self.inner.save_snapshot_locked(path.as_ref())
    }

    /// Stash the snapshot path on the InstanceInternal so Drop / close can
    /// find it. Only meaningful for in-memory backends — no-op for others.
    fn set_snapshot_path(&self, path: Option<PathBuf>) {
        if path.is_none() {
            return;
        }
        // Poison-tolerant: a panic in another holder must not strand the
        // Instance — the snapshot path is a simple swappable Option.
        let mut guard = self
            .inner
            .snapshot_path
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        *guard = path;
    }

    /// Internal load-only implementation that works with any clock.
    async fn open_impl(backend: Box<dyn BackendImpl>, clock: Arc<dyn Clock>) -> Result<Self> {
        let backend: Arc<dyn BackendImpl> = Arc::from(backend);

        // Strict: require existing InstanceMetadata. Initialisation is the
        // caller's responsibility (`connect_or_create` / `connect_or_create_backend`).
        let metadata = backend
            .get_instance_metadata()
            .await?
            .ok_or(InstanceError::NotInitialized)?;

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
            backend: Arc::new(LocalBackend::new(backend)),
            clock,
            sync: std::sync::OnceLock::new(),
            metadata,
            secrets,
            snapshot_path: Mutex::new(None),
            write_callbacks: Mutex::new(HashMap::new()),
            global_write_callbacks: Mutex::new(Vec::new()),
            next_callback_id: AtomicU64::new(0),
            tree_locks: Mutex::new(HashMap::new()),
        });
        Ok(Self { inner })
    }

    /// Load-only helper that accepts an already-arc'd backend and the
    /// already-fetched metadata. Used by `connect_or_create_backend_impl`,
    /// which has already inspected metadata to choose between the load
    /// and bootstrap arms — passing it through avoids a redundant
    /// `get_instance_metadata` round-trip.
    async fn open_impl_arc_with_metadata(
        backend: Arc<dyn BackendImpl>,
        clock: Arc<dyn Clock>,
        metadata: InstanceMetadata,
    ) -> Result<Self> {
        let secrets = backend.get_instance_secrets().await?;
        if let Some(ref secrets) = secrets {
            let derived_id = secrets.signing_key.public_key();
            if derived_id != metadata.id {
                return Err(InstanceError::DeviceKeyMismatch.into());
            }
        }
        let inner = Arc::new(InstanceInternal {
            backend: Arc::new(LocalBackend::new(backend)),
            clock,
            sync: std::sync::OnceLock::new(),
            metadata,
            secrets,
            snapshot_path: Mutex::new(None),
            write_callbacks: Mutex::new(HashMap::new()),
            global_write_callbacks: Mutex::new(Vec::new()),
            next_callback_id: AtomicU64::new(0),
            tree_locks: Mutex::new(HashMap::new()),
        });
        Ok(Self { inner })
    }

    /// Internal create implementation. Returns the new `Instance` along with
    /// the just-bootstrapped initial `User`, materialised directly from the
    /// keys we generated (no redundant login round-trip).
    pub(crate) async fn create_internal(
        backend: Arc<dyn BackendImpl>,
        clock: Arc<dyn Clock>,
        initial: NewUser,
    ) -> Result<(Self, User)> {
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
                backend: Arc::new(LocalBackend::new(Arc::clone(&backend))),
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
                snapshot_path: Mutex::new(None),
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

        // 4. Build real instance
        let inner = Arc::new(InstanceInternal {
            backend: Arc::new(LocalBackend::new(backend)),
            clock,
            sync: std::sync::OnceLock::new(),
            metadata,
            secrets: Some(secrets),
            snapshot_path: Mutex::new(None),
            write_callbacks: Mutex::new(HashMap::new()),
            global_write_callbacks: Mutex::new(Vec::new()),
            next_callback_id: AtomicU64::new(0),
            tree_locks: Mutex::new(HashMap::new()),
        });

        let instance = Self { inner };

        // 5. Bootstrap the initial user. The first user created on an
        // instance is automatically promoted to Admin on the system
        // databases by `system_databases::create_user`'s
        // first-user-becomes-admin logic.
        let users_db = instance.users_db().await?;
        let (user_uuid, user_info, root_key) = crate::user::system_databases::create_user(
            &users_db,
            &instance,
            &initial.username,
            initial.password.as_deref(),
        )
        .await?;

        // 6. Materialise the User session directly from the keys we just
        // generated — skips a redundant `login_user` round-trip that would
        // otherwise re-derive the encryption key from the password.
        let user = crate::user::system_databases::build_user_session(
            &instance,
            &user_uuid,
            &user_info,
            root_key,
            initial.password.as_deref(),
        )
        .await?;

        Ok((instance, user))
    }

    /// Get a reference to the backend seam.
    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.inner.backend
    }

    /// The concrete in-process storage engine, or [`OperationNotSupported`] on
    /// a remote instance.
    ///
    /// Off-seam local-only operations (instance secrets, verification-status
    /// mutation, `all_roots`/`get_tree` raw dumps, scope-keyed cache) are
    /// performed through this accessor, so they are reachable only where a
    /// concrete local backend exists.
    ///
    /// [`OperationNotSupported`]: InstanceError::OperationNotSupported
    pub(crate) fn require_local_engine(&self) -> Result<Arc<dyn BackendImpl>> {
        self.inner.backend.local_engine().ok_or_else(|| {
            InstanceError::OperationNotSupported {
                operation: "local backend engine on remote instance".to_string(),
            }
            .into()
        })
    }

    /// The remote connection backing this instance, if it was created via
    /// [`connect`](Self::connect). Returns `None` for local instances.
    ///
    /// Useful for constructing a [`Database`](crate::Database) that routes
    /// reads through the Database-level wire API while sharing the same
    /// connection and session as the instance's write path.
    #[cfg(all(unix, feature = "service"))]
    pub fn remote_connection(&self) -> Option<RemoteConnection> {
        self.inner.backend.remote_connection()
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

    /// Put an entry into the backend. Always stored Unverified — see
    /// [`crate::backend::BackendImpl::put`].
    pub(crate) async fn put(&self, entry: crate::entry::Entry) -> Result<()> {
        self.inner.backend.put(entry).await
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
    /// locally, so no key is attached — the returned handle is read-only.
    /// Write paths on a remote instance must instead go through
    /// [`Instance::users_db_for_session`], which attaches the caller's
    /// session signing key (e.g. admin's key on the `InstanceAdmin`
    /// `create_user` path) and routes through `Database::open_remote`.
    pub(crate) async fn users_db(&self) -> Result<Database> {
        let db = Database::open(self, &self.inner.metadata.users_db).await?;
        #[cfg(all(unix, feature = "service"))]
        if self.remote_connection().is_some() {
            return Ok(db);
        }
        Ok(db.with_key(self.signing_key()?.clone()))
    }

    /// Open the _users system database with a specific signing key (not the device
    /// key).  Used by the admin-session paths
    /// ([`InstanceAdmin`](crate::user::InstanceAdmin), `User::admin_check`) on
    /// remote instances where the device key is unavailable.
    pub(crate) async fn users_db_for_session(&self, signing_key: &PrivateKey) -> Result<Database> {
        self.open_system_db_for_session(&self.inner.metadata.users_db, signing_key)
            .await
    }

    /// Open a system database for an authenticated session.
    ///
    /// On a remote instance this routes every read through the connection's
    /// Database wire protocol ([`Database::open_remote`], a per-handle
    /// `RemoteBackend`), gated by the session key's identity — the plain
    /// [`Database::open`] path instead clones the instance's session backend,
    /// so on a connected instance its reads carry the connection's login
    /// identity. On a local instance it opens against the local backend as
    /// before. The signing key is attached for writes.
    pub(crate) async fn open_system_db_for_session(
        &self,
        root_id: &ID,
        signing_key: &PrivateKey,
    ) -> Result<Database> {
        #[cfg(all(unix, feature = "service"))]
        if let Some(conn) = self.remote_connection() {
            // The daemon gates per-tree reads against the acting pubkey
            // from the request's identity hint, and the hint here is
            // `signing_key.public_key()` (the caller's chosen identity for
            // this DB). The hint must be in the connection's session keyset
            // — register it now so subsequent reads through the returned
            // `RemoteBackend` are accepted.
            conn.register_session_key(signing_key).await?;
            let identity = SigKey::from_pubkey(&signing_key.public_key());
            return Ok(Database::open_remote(self, conn, root_id, identity)
                .await?
                .with_key(signing_key.clone()));
        }
        Ok(Database::open(self, root_id)
            .await?
            .with_key(signing_key.clone()))
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
        if let Some(conn) = self.remote_connection() {
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
        // sync. Sync is owned by the process that owns the Instance.
        //
        // Return `Ok(())` so callers on a connected instance get the same
        // no-op success they would on a local instance where sync is already
        // running daemon-side. Long-term this should become an admin-gated
        // operation that lets a client ask the daemon to enable its sync
        // subsystem; until that ships, the client-side `enable_sync` is
        // intentionally a silent no-op because the daemon either already
        // has sync running or it doesn't, and the client can't change that.
        //
        // TODO(service): expose an admin-gated `enable_sync` on
        // `InstanceAdmin` so a client can enable sync remotely.
        #[cfg(all(unix, feature = "service"))]
        if self.remote_connection().is_some() {
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

        // Initialize the sync engine (no transports registered yet)
        // Users should call register_transport() to add transports
        sync_arc.start_background_sync()?;

        // Sync wants to observe writes across *every* tree, including
        // trees created after this point — there's no fixed tree set to
        // register per-db callbacks against, so this is one of the few
        // legitimate uses of `register_global_write_callback`. Idempotent
        // because `enable_sync` early-returns at the top if sync is
        // already initialized (`self.inner.sync.get().is_some()`); without
        // that guard this would register a duplicate hook every call.
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

    /// Register a per-database callback. Fires for writes to `tree_id` on
    /// this Instance.
    ///
    /// `initial_tips` seeds the callback's cursor — the first
    /// [`WriteEvent`] this callback receives will have `previous_tips`
    /// equal to `initial_tips`, and the cursor advances on each
    /// subsequent fire to that fire's post-write tips. Callers that
    /// want "tell me about everything after the point I just read at"
    /// pass the tips they just read; callers that want "tell me about
    /// everything from this empty cursor forward" can pass `vec![]`
    /// (the first fire's `previous_tips` will be empty, and the
    /// subscriber walks the DAG to discover the gap).
    ///
    /// Returns the [`CallbackId`] of the registration. Callers wrap
    /// this in a [`WriteCallback`] handle (see
    /// [`Database::on_write_at_tips`]) to manage lifetime.
    pub(crate) fn register_write_callback<F, Fut>(
        &self,
        tree_id: ID,
        initial_tips: Vec<ID>,
        callback: F,
    ) -> CallbackId
    where
        F: for<'a> Fn(&'a WriteEvent, &'a Database) -> Fut + Send + std::marker::Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let id = CallbackId(self.inner.next_callback_id.fetch_add(1, Ordering::Relaxed));
        let cb: AsyncWriteCallbackFn = Arc::new(move |event: &WriteEvent, database: &Database| {
            let fut = callback(event, database);
            Box::pin(fut) as AsyncWriteCallbackFuture<'_>
        });
        let entry = Arc::new(PerDbCallbackEntry {
            id,
            last_tips: std::sync::Mutex::new(initial_tips),
            callback: cb,
        });
        self.inner
            .write_callbacks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(tree_id)
            .or_default()
            .push(entry);
        id
    }

    /// Register a non-removable callback fired for **every** write on **every**
    /// database for the life of the Instance.
    ///
    /// This is purpose-built for hooks that need to observe writes across
    /// the entire Instance — including writes to trees created *after* the
    /// hook is registered. The only legitimate use is something that
    /// genuinely doesn't know its target tree set up front: today, just
    /// sync (which wants to react to every local write so it can propagate
    /// to peers, and registers its hook once during `enable_sync`).
    ///
    /// **Not the right primitive for connection-scoped fan-out.** If you
    /// know up front which trees a consumer cares about (e.g. service
    /// clients subscribing per-tree via `DatabaseOp::SubscribeWrites`),
    /// register per-database callbacks with [`Self::register_write_callback`]
    /// instead. Per-db callbacks have a removal path
    /// ([`Self::remove_write_callback`]) which lets you tear them down on
    /// disconnect; this API does not.
    ///
    /// Callers branch on [`WriteEvent::source`] inside the closure if they
    /// only care about one source. Caller is responsible for idempotency
    /// (registering N times produces N firings); the canonical pattern is
    /// to guard the registration site with a `OnceLock` so it cannot run
    /// twice on the same Instance.
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
            .unwrap_or_else(|p| p.into_inner())
            .push((id, cb));
    }

    /// Remove a per-database callback by id. Returns `true` iff the
    /// removal emptied the per-tree callback list (i.e. this was the
    /// last live callback for `tree_id` on this Instance). No-op if
    /// the id isn't registered.
    ///
    /// Callers use the `true` return to drive lifecycle hooks on the
    /// connection's subscription state: dropping the last local
    /// callback for a tree on a connected instance is the trigger to
    /// transition the wire subscription to `Idle` (see
    /// [`crate::service::client::RemoteConnection::transition_to_idle`]).
    pub(crate) fn remove_write_callback(&self, tree_id: &ID, id: CallbackId) -> bool {
        let mut callbacks = self
            .inner
            .write_callbacks
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(vec) = callbacks.get_mut(tree_id) {
            let before = vec.len();
            vec.retain(|entry| entry.id != id);
            let removed = vec.len() < before;
            if vec.is_empty() {
                callbacks.remove(tree_id);
                return removed;
            }
        }
        false
    }

    /// Acquire (or create) the per-tree async lock that serializes the
    /// `get_tips` → backend write → callback dispatch sequence.
    ///
    /// Without this, two concurrent writers to the same tree both snapshot
    /// `previous_tips` before either writes, so the second callback's
    /// `previous_tips` would not reflect the first write — breaking the
    /// "diff against current tips" contract documented on [`WriteEvent`].
    pub(crate) fn tree_lock(&self, tree_id: &ID) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .inner
            .tree_locks
            .lock()
            .unwrap_or_else(|p| p.into_inner());
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

        // 1. Capture tips before the write so callbacks know what changed.
        //
        // On a connected (remote) instance, the daemon owns the canonical
        // DAG and the client's local backend has nothing to read; reading
        // tips here would also gate against the *connection's* login pubkey
        // (not the per-DB acting identity from the Database handle), which
        // breaks the legitimate "create-a-tree-with-a-non-login-key" flow.
        // The client also skips firing callbacks locally on this path (see
        // step 3 below) — the daemon round-trips a `Notification::DatabaseWrite`
        // back with its own canonical `previous_tips` and we fire from
        // there instead.
        #[cfg(all(unix, feature = "service"))]
        let is_connected = self.remote_connection().is_some();
        #[cfg(not(all(unix, feature = "service")))]
        let is_connected = false;

        let previous_tips = if is_connected {
            Vec::new()
        } else {
            self.get_tips(tree_id).await?
        };

        // 2. Persist to backend storage (and notify server for remote backends)
        self.backend()
            .write_entry(verification, entry.clone(), source)
            .await?;

        // 3. Build event and fire callbacks — but only on a local
        //    instance, and only for entries that arrive `Verified`.
        //
        // **Connected instance**: the daemon is the sole publisher of
        // write events. It fires its own callback registry when it
        // stores the entry, then pushes a `Notification::DatabaseWrite`
        // back to every subscribed client (including this one). Firing
        // here too would double-deliver. See `Database::on_write` for
        // the timing contract.
        //
        // **Unverified path**: skipped on purpose. Subscribers only
        // ever see *settled-state* writes — i.e. Verified. An entry
        // that arrives `Unverified` (over the wire as a
        // `SubmitSignedEntry` body, or via sync as a remote batch) is
        // ingested silently here; the subsequent local verification
        // pass (the caller's responsibility to schedule) decides
        // whether it ever becomes a fire-eligible Verified entry, and
        // if so fires from there. This closes the
        // Failed-in-`previous_tips` semantic hole: a subscriber's
        // accumulated state can only ever rest on entries that have
        // passed local validation.
        if !is_connected && verification == VerificationStatus::Verified {
            // Compute post-write tips for cursor advance. Cheap: just
            // re-read `Backend::get_tips` post-put. Each per-callback
            // cursor advances to this value.
            let post_tips = self.get_tips(tree_id).await.unwrap_or_default();
            self.fire_write_callbacks(tree_id, &previous_tips, &post_tips, source)
                .await;
        }

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
    /// Entries are stored as [`VerificationStatus::Unverified`] without
    /// exception: they arrive from outside this node's local validation pass,
    /// so this node has not verified them and a peer cannot assert that it
    /// did. A later local re-verification pass may promote them.
    ///
    /// # Arguments
    /// * `tree_id` - The root ID of the database receiving the batch
    /// * `entries` - The entries to ingest
    pub(crate) async fn put_remote_entries(
        &self,
        tree_id: &ID,
        entries: Vec<Entry>,
    ) -> Result<usize> {
        if entries.is_empty() {
            return Ok(0);
        }

        // Store the batch under the tree lock; release before calling
        // `verify`, which acquires its own lock for the pass + fire.
        let stored_count = {
            let lock = self.tree_lock(tree_id);
            let _guard = lock.lock().await;
            let mut stored = 0usize;
            for entry in entries {
                match self.backend().put(entry.clone()).await {
                    Ok(_) => stored += 1,
                    Err(e) => tracing::error!(
                        tree_id = %tree_id,
                        entry_id = %entry.id(),
                        "Failed to store remote entry: {}", e
                    ),
                }
            }
            stored
        };

        // Run verify inline. `Database::verify` walks the Unverified
        // region in O(K), promotes whatever can be settled, and fires
        // one batched `Verified` event for the promotions. Sync-ingest
        // subscribers see the promotion without needing to schedule
        // their own verify pass.
        if stored_count > 0 {
            Database::open(self, tree_id).await?.verify().await?;
        }

        Ok(stored_count)
    }

    /// Demote `entry_id` to [`VerificationStatus::Unverified`] and
    /// cascade the demotion to every `Verified` descendant in
    /// `tree_id`'s DAG.
    ///
    /// **Why the cascade.** The `Verified` set on a tree is
    /// prefix-closed: an entry is `Verified` only if every one of its
    /// ancestors is. Demoting an ancestor without also demoting its
    /// `Verified` descendants breaks that invariant, which means
    /// `Database::verify`'s targeted walk-from-tips cannot find the
    /// demoted entry — it's hidden behind a still-`Verified` descendant
    /// and would be stranded. The cascade restores the invariant.
    ///
    /// **Scope.** Today's only callers are tests (and the v0
    /// re-verification scenarios they exercise). Production code does
    /// not demote `Verified` → `Unverified` at all under the current
    /// verify implementation. If a future demotion path appears (e.g.
    /// retroactive settings-change-driven invalidation) it should route
    /// through this method.
    ///
    /// O(N) per call: walks `get_tree` to build a children index. Cheap
    /// for the test sizes this targets; not a hot path.
    ///
    /// Exposed publicly under `cfg(test)` and the `testing` feature so
    /// integration tests in the `it` crate can use it; otherwise
    /// `pub(crate)`-equivalent.
    #[cfg(any(test, feature = "testing"))]
    pub async fn demote_to_unverified(&self, tree_id: &ID, entry_id: &ID) -> Result<()> {
        self.demote_to_unverified_impl(tree_id, entry_id).await
    }

    #[cfg(not(any(test, feature = "testing")))]
    pub(crate) async fn demote_to_unverified(
        &self,
        tree_id: &ID,
        entry_id: &ID,
    ) -> Result<()> {
        self.demote_to_unverified_impl(tree_id, entry_id).await
    }

    async fn demote_to_unverified_impl(
        &self,
        tree_id: &ID,
        entry_id: &ID,
    ) -> Result<()> {
        use std::collections::{HashMap, HashSet, VecDeque};
        let backend = self.require_local_engine()?;
        let entries = backend.get_tree(tree_id).await?;

        // Build children index from each entry's parents.
        let mut children: HashMap<ID, Vec<ID>> = HashMap::new();
        for entry in &entries {
            for p in entry.parents().unwrap_or_default() {
                children.entry(p).or_default().push(entry.id());
            }
        }

        // BFS from the target. The target itself always gets demoted
        // (caller's intent); descendants get demoted only if they are
        // currently `Verified`. `Failed` or `Unverified` descendants
        // are left as-is — `Failed` is terminal, `Unverified` is
        // already at the target state.
        let mut queue: VecDeque<ID> = VecDeque::new();
        queue.push_back(entry_id.clone());
        let mut visited: HashSet<ID> = HashSet::new();
        while let Some(id) = queue.pop_front() {
            if !visited.insert(id.clone()) {
                continue;
            }
            let status = match backend.get_verification_status(&id).await {
                Ok(s) => s,
                Err(e) if e.is_not_found() => continue,
                Err(e) => return Err(e),
            };
            let is_target = id == *entry_id;
            if is_target || status == VerificationStatus::Verified {
                backend
                    .update_verification_status(&id, VerificationStatus::Unverified)
                    .await?;
            }
            if let Some(kids) = children.get(&id) {
                for kid in kids {
                    queue.push_back(kid.clone());
                }
            }
        }
        Ok(())
    }

    /// Dispatch callbacks for a write event.
    ///
    /// Per-database callbacks for `tree_id` get **per-callback events**:
    /// each callback's `previous_tips` is read from its own cursor and
    /// the cursor advances to `post_tips` synchronously around the
    /// fire. The cursor mutex is released before the user callback is
    /// awaited, so a slow callback does not stall other callbacks'
    /// cursor reads on a concurrent fire.
    ///
    /// **Per-callback dispatch is concurrent.** Cursor advancement
    /// happens synchronously in arrival order under each callback's
    /// own mutex, then every callback's closure is spawned on its own
    /// tokio task. A slow callback for one subscriber doesn't stall
    /// other subscribers' callbacks for the same event. The dispatcher
    /// awaits every spawned task before returning, so the per-tree
    /// dispatch worker's "this notification is finished" point still
    /// serialises against the next event on the same tree — the
    /// inter-event ordering contract documented on
    /// [`Database::on_write`](crate::Database::on_write) is preserved.
    ///
    /// Global callbacks fire with a single shared event whose
    /// `previous_tips` is the caller-supplied `previous_tips`
    /// argument — globals don't track per-tree cursors and continue
    /// to receive the pre-write tips view. Globals also dispatch
    /// concurrently across subscribers.
    ///
    /// `pub(crate)` so the service module's reader task can drive this
    /// directly when a `Notification::DatabaseWrite` arrives from the
    /// daemon — that path is the *sole* publisher on a connected
    /// instance.
    pub(crate) async fn fire_write_callbacks(
        &self,
        tree_id: &ID,
        previous_tips: &[ID],
        post_tips: &[ID],
        source: WriteSource,
    ) {
        let per_db_callbacks = self
            .inner
            .write_callbacks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(tree_id)
            .cloned();

        let global_callbacks = self
            .inner
            .global_write_callbacks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();

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

        // Single JoinSet across per-db + global callbacks: phase 1
        // (cursor read+advance under each callback's own std::Mutex)
        // happens synchronously in arrival order before any task is
        // spawned, so cursors commit deterministically; phase 2 runs
        // every closure concurrently and the dispatcher awaits the
        // whole set before returning.
        let mut joins = tokio::task::JoinSet::new();

        if let Some(callbacks) = per_db_callbacks {
            for entry in callbacks {
                let cb_previous = {
                    let mut guard = entry
                        .last_tips
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    std::mem::replace(&mut *guard, post_tips.to_vec())
                };
                let event = WriteEvent {
                    previous_tips: cb_previous,
                    post_tips: post_tips.to_vec(),
                    source,
                };
                let cb = entry.callback.clone();
                let database_for_cb = database.clone();
                let tree_id_for_cb = tree_id.clone();
                let cb_id = entry.id;
                joins.spawn(async move {
                    if let Err(e) = cb(&event, &database_for_cb).await {
                        tracing::error!(
                            tree_id = %tree_id_for_cb,
                            source = ?source,
                            callback_id = ?cb_id,
                            "Per-database callback failed: {}", e
                        );
                    }
                });
            }
        }

        // Globals: every subscriber gets its own owned clone of the
        // shared pre-write-tips event so their closures can run
        // concurrently with each other and with the per-db callbacks
        // above. A WriteEvent is two `Vec<ID>` + a Copy enum; cloning
        // it per global is cheap relative to the spawn cost.
        for (id, callback) in global_callbacks {
            let event = WriteEvent {
                previous_tips: previous_tips.to_vec(),
                post_tips: post_tips.to_vec(),
                source,
            };
            let database_for_cb = database.clone();
            let tree_id_for_cb = tree_id.clone();
            joins.spawn(async move {
                if let Err(e) = callback(&event, &database_for_cb).await {
                    tracing::error!(
                        tree_id = %tree_id_for_cb,
                        source = ?source,
                        callback_id = ?id,
                        "Global callback failed: {}", e
                    );
                }
            });
        }

        while joins.join_next().await.is_some() {}
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
    /// # use eidetica::{Instance, NewUser};
    /// # #[tokio::main]
    /// # async fn main() -> eidetica::Result<()> {
    /// let (instance, maybe_user) = Instance::connect_or_create(
    ///     "memory://",
    ///     NewUser::passwordless("alice"),
    /// ).await?;
    /// let user = maybe_user.expect("memory:// is always fresh");
    /// let weak = instance.downgrade();
    ///
    /// // Upgrade works while instance exists
    /// assert!(weak.upgrade().is_some());
    ///
    /// // User holds its own strong handle to the Instance — drop it too so
    /// // the weak upgrade can fail.
    /// drop(user);
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

// ============ URL-dispatch backend constructors ============

#[cfg(feature = "sqlite")]
async fn open_sqlite_backend(url: &str) -> Result<Box<dyn BackendImpl>> {
    let backend = crate::backend::database::Sqlite::connect(url).await?;
    Ok(Box::new(backend))
}

#[cfg(not(feature = "sqlite"))]
async fn open_sqlite_backend(_url: &str) -> Result<Box<dyn BackendImpl>> {
    Err(InstanceError::BackendUnavailable {
        scheme: "sqlite",
        missing_feature: "sqlite",
    }
    .into())
}

#[cfg(feature = "postgres")]
async fn open_postgres_backend(url: &str) -> Result<Box<dyn BackendImpl>> {
    let backend = crate::backend::database::Postgres::connect(url).await?;
    Ok(Box::new(backend))
}

#[cfg(not(feature = "postgres"))]
async fn open_postgres_backend(_url: &str) -> Result<Box<dyn BackendImpl>> {
    Err(InstanceError::BackendUnavailable {
        scheme: "postgres",
        missing_feature: "postgres",
    }
    .into())
}
