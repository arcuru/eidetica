//! SQL-based backend implementations for Eidetica storage.
//!
//! This module provides SQL database backends that implement the `BackendImpl` trait,
//! allowing Eidetica entries to be stored in relational databases.
//!
//! ## Available Backends
//!
//! - **SQLite** (feature: `sqlite`): Embedded database
//! - **PostgreSQL** (feature: `postgres`): PostgreSQL database
//!
//! ## Architecture
//!
//! The SQL backend uses sqlx with `AnyPool` for multi-database support.
//! All methods are async to match the async `BackendImpl` trait.
//!
//! ## Schema and Migrations
//!
//! The database schema is defined in the [`schema`] module and automatically
//! initialized when connecting. Migrations are handled via code-based functions
//! rather than SQL files to support dialect differences between SQLite and PostgreSQL.
//!
//! See [`schema`] module documentation for details on adding migrations.

mod storage;
mod traversal;

/// Schema definition and migration system.
pub mod schema;

use std::any::Any;
#[cfg(feature = "sqlite")]
use std::fs::{File, OpenOptions, TryLockError};
#[cfg(feature = "sqlite")]
use std::io;
#[cfg(feature = "sqlite")]
use std::path::{Path, PathBuf};
#[cfg(feature = "sqlite")]
use std::str::FromStr;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
use std::time::Duration;

use async_trait::async_trait;
#[cfg(feature = "postgres")]
use sqlx::AnyConnection;
#[cfg(feature = "postgres")]
use sqlx::Connection;
use sqlx::any::AnyPoolOptions;
#[cfg(feature = "sqlite")]
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{AnyPool, Executor};

use crate::Result;
use crate::backend::errors::BackendError;
use crate::backend::{
    BackendImpl, InstanceMetadata, InstanceSecrets, RecordMutations, RecordPage, RecordRange,
    RecordView, StagingToken, StoreStateRequest, VerificationStatus,
};
use crate::entry::{Entry, ID};
use crate::snapshot::Snapshot;

/// Extension trait for sqlx Result types to simplify error handling.
///
/// Similar to `anyhow::Context`, this trait adds a method to convert
/// sqlx errors to `BackendError::SqlxError` with a context message.
pub(crate) trait SqlxResultExt<T> {
    /// Convert sqlx error to BackendError with context message.
    fn sql_context(self, context: &str) -> Result<T>;
}

impl<T> SqlxResultExt<T> for std::result::Result<T, sqlx::Error> {
    fn sql_context(self, context: &str) -> Result<T> {
        self.map_err(|e| {
            BackendError::SqlxError {
                reason: format!("{context}: {e}"),
                source: Some(e),
            }
            .into()
        })
    }
}

/// Database backend kind for SQL dialect selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbKind {
    /// SQLite database
    Sqlite,
    /// PostgreSQL database
    Postgres,
}

/// SQL-based backend implementing `BackendImpl` using sqlx.
///
/// This backend supports both SQLite and PostgreSQL through sqlx's `AnyPool`.
///
/// # Concurrency
///
/// `SqlxBackend` is `Send + Sync` as required by `BackendImpl`. The underlying
/// sqlx pool handles connection pooling and thread safety. Each backend owns its
/// persistent storage namespace exclusively for its lifetime. Share one backend
/// through the Eidetica service rather than opening the same storage directly.
///
/// # Test Isolation
///
/// For PostgreSQL, each backend instance can use its own schema for test isolation.
/// Use `connect_postgres_isolated()` to create an isolated backend for testing.
///
/// ```compile_fail
/// use eidetica::backend::database::SqlxBackend;
///
/// fn cannot_escape_pool(backend: SqlxBackend) {
///     let _ = backend.pool();
/// }
/// ```
pub struct SqlxBackend {
    pool: Option<AnyPool>,
    kind: DbKind,
    _owner: Option<StorageOwner>,
    #[cfg(all(feature = "postgres", feature = "testing"))]
    postgres_token: Option<String>,
}

impl Drop for SqlxBackend {
    fn drop(&mut self) {
        // Mark every pool handle closed before releasing the ownership lock. Creating this
        // future performs the close transition; waiting is unnecessary for the fence.
        // Checked-out SQLx connections remain valid until returned, so PostgreSQL retains
        // their shared advisory locks until then.
        if let Some(pool) = self.pool.take() {
            drop(pool.close());
        }
    }
}

enum StorageOwner {
    #[cfg(feature = "sqlite")]
    Sqlite { _lock: File },
    #[cfg(feature = "postgres")]
    Postgres {
        _connection: tokio::sync::Mutex<AnyConnection>,
    },
}

#[cfg(feature = "sqlite")]
fn prepare_sqlite(url: &str) -> Result<Option<StorageOwner>> {
    let normalized_url = normalize_sqlite_url(url);
    let options = SqliteConnectOptions::from_str(&normalized_url).map_err(|error| {
        BackendError::SqlxError {
            reason: format!("Failed to parse SQLite connection URL: {error}"),
            source: Some(error),
        }
    })?;
    if sqlite_is_in_memory(&normalized_url) {
        return Ok(None);
    }

    let database_path = canonical_database_path(options.get_filename()).map_err(|error| {
        BackendError::SqlxError {
            reason: format!(
                "Failed to identify SQLite database `{}`: {error}",
                options.get_filename().display()
            ),
            source: None,
        }
    })?;
    let lock_path = sqlite_owner_lock_path(&database_path);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| BackendError::SqlxError {
            reason: format!(
                "Failed to open SQLite ownership sidecar `{}`: {error}",
                lock_path.display()
            ),
            source: None,
        })?;
    match lock.try_lock() {
        Ok(()) => Ok(Some(StorageOwner::Sqlite { _lock: lock })),
        Err(TryLockError::WouldBlock) => Err(BackendError::StorageAlreadyOwned {
            namespace: database_path.display().to_string(),
        }
        .into()),
        Err(TryLockError::Error(error)) => Err(BackendError::SqlxError {
            reason: format!(
                "Failed to claim SQLite database ownership `{}`: {error}",
                database_path.display()
            ),
            source: None,
        }
        .into()),
    }
}

#[cfg(feature = "sqlite")]
fn sqlite_owner_lock_path(database_path: &Path) -> PathBuf {
    let mut path = database_path.as_os_str().to_owned();
    path.push(".eidetica-owner");
    PathBuf::from(path)
}

#[cfg(feature = "sqlite")]
fn normalize_sqlite_url(url: &str) -> String {
    let Some(rest) = url.strip_prefix("sqlite:file:") else {
        return url.to_owned();
    };
    if rest == ":memory:" || rest.starts_with(":memory:?") {
        return url.to_owned();
    }
    format!("sqlite:{rest}")
}

#[cfg(feature = "sqlite")]
fn sqlite_is_in_memory(url: &str) -> bool {
    let url = url
        .trim_start_matches("sqlite://")
        .trim_start_matches("sqlite:");
    let (database, _) = url.split_once('?').unwrap_or((url, ""));
    database == ":memory:" || database == "file::memory:" || sqlite_file_mode(url)
}

#[cfg(feature = "sqlite")]
fn sqlite_file_mode(url: &str) -> bool {
    let query = url.split_once('?').map_or("", |(_, query)| query);
    url::form_urlencoded::parse(query.as_bytes())
        .any(|(key, value)| key == "mode" && value == "memory")
}

#[cfg(feature = "sqlite")]
fn canonical_database_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };

    match absolute.symlink_metadata() {
        Ok(_) => return absolute.canonicalize(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let file_name = absolute.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "database path has no file name",
        )
    })?;
    let parent = absolute.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "database path has no parent")
    })?;
    Ok(parent.canonicalize()?.join(file_name))
}

#[cfg(feature = "sqlite")]
fn sqlite_path_url(path: &Path) -> Result<String> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|error| BackendError::SqlxError {
                reason: format!("Failed to resolve SQLite database path: {error}"),
                source: None,
            })?
            .join(path)
    };
    let file_url = url::Url::from_file_path(&absolute).map_err(|()| BackendError::SqlxError {
        reason: format!(
            "Failed to encode SQLite database path `{}` as a file URL",
            absolute.display()
        ),
        source: None,
    })?;
    Ok(format!(
        "sqlite:{}?mode=rwc",
        &file_url.as_str()["file:".len()..]
    ))
}

impl SqlxBackend {
    /// Get a reference to the underlying pool for SQL backend modules.
    pub(crate) fn pool(&self) -> &AnyPool {
        self.pool.as_ref().expect("SQL pool must exist until drop")
    }

    #[cfg(all(feature = "postgres", feature = "testing"))]
    #[doc(hidden)]
    pub async fn test_postgres_checked_out_connection(
        &self,
    ) -> Result<sqlx::pool::PoolConnection<sqlx::Any>> {
        self.pool()
            .acquire()
            .await
            .sql_context("Failed to acquire PostgreSQL test connection")
    }

    /// Get the database kind.
    pub fn kind(&self) -> DbKind {
        self.kind
    }

    /// Check if this backend is using SQLite.
    pub fn is_sqlite(&self) -> bool {
        self.kind == DbKind::Sqlite
    }

    /// Check if this backend is using PostgreSQL.
    pub fn is_postgres(&self) -> bool {
        self.kind == DbKind::Postgres
    }

    #[cfg(all(feature = "postgres", feature = "testing"))]
    #[doc(hidden)]
    pub async fn test_postgres_owner_pid(&self) -> Result<i32> {
        let _connection = match self
            ._owner
            .as_ref()
            .expect("PostgreSQL backend must hold an ownership connection")
        {
            StorageOwner::Postgres { _connection } => _connection,
            #[cfg(feature = "sqlite")]
            StorageOwner::Sqlite { .. } => unreachable!("only PostgreSQL ownership is queried"),
        };
        let mut connection = _connection.lock().await;
        let (pid,): (i32,) = sqlx::query_as("SELECT pg_backend_pid()")
            .fetch_one(&mut *connection)
            .await
            .sql_context("Failed to read PostgreSQL ownership session")?;
        Ok(pid)
    }

    #[cfg(all(feature = "postgres", feature = "testing"))]
    #[doc(hidden)]
    pub async fn test_postgres_pool_pids(&self) -> Result<Vec<i32>> {
        let mut connections = Vec::with_capacity(2);
        for _ in 0..2 {
            connections.push(
                self.pool()
                    .acquire()
                    .await
                    .sql_context("Failed to acquire PostgreSQL test connection")?,
            );
        }
        let mut pids = Vec::with_capacity(connections.len());
        for connection in &mut connections {
            let (pid,): (i32,) = sqlx::query_as("SELECT pg_backend_pid()")
                .fetch_one(&mut **connection)
                .await
                .sql_context("Failed to read PostgreSQL pool session")?;
            pids.push(pid);
        }
        Ok(pids)
    }

    #[cfg(all(feature = "postgres", feature = "testing"))]
    #[doc(hidden)]
    pub fn test_postgres_token(&self) -> &str {
        self.postgres_token
            .as_deref()
            .expect("PostgreSQL backend must hold an ownership token")
    }
}

// Test-only Store-state stage pause hook.
//
// A regression test for same-token stage-vs-publish interleavings needs to
// park a `stage_store_state_records` call after token validation (advisory
// locks held) but before any record writes, run a competing publish, then
// release the stage. Production call paths cannot pause mid-transaction, and
// a sleep-based race would be flaky by construction, so this narrow hook
// exists behind the `testing` feature only: it is compiled out of every
// production build, adds no trait surface, and is a no-op (one map miss)
// unless a test registered a gate for the exact staging namespace. Gates are
// one-shot and keyed by namespace UUID, so parallel tests cannot observe or
// disturb each other.
#[cfg(feature = "testing")]
#[derive(Debug)]
pub struct StoreStateStagePause {
    validated: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(feature = "testing")]
impl StoreStateStagePause {
    fn new() -> Self {
        Self {
            validated: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        }
    }

    /// Wait until the staged call reaches the pause point (bounded).
    ///
    /// # Panics
    ///
    /// Panics after 15 seconds: a timeout means the test never drove a stage
    /// through the gate, i.e. a harness bug, not a backend result.
    pub async fn wait_validated(&self) {
        if tokio::time::timeout(
            std::time::Duration::from_secs(15),
            self.validated.notified(),
        )
        .await
        .is_err()
        {
            panic!("Store-state stage pause gate never reached: test harness bug");
        }
    }

    /// Let the paused stage proceed to its writes.
    pub fn release(&self) {
        self.release.notify_one();
    }
}

#[cfg(feature = "testing")]
static STORE_STATE_STAGE_PAUSES: std::sync::OnceLock<
    tokio::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<StoreStateStagePause>>>,
> = std::sync::OnceLock::new();

#[cfg(feature = "testing")]
fn store_state_stage_pauses() -> &'static tokio::sync::Mutex<
    std::collections::HashMap<String, std::sync::Arc<StoreStateStagePause>>,
> {
    STORE_STATE_STAGE_PAUSES
        .get_or_init(|| tokio::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Register a one-shot pause gate for the given staging namespace.
///
/// The next `stage_store_state_records` call for this namespace signals the
/// gate after validating its token and waits (bounded) for [`StoreStateStagePause::release`]
/// before writing any records. Returns the gate the test drives.
#[cfg(feature = "testing")]
impl SqlxBackend {
    pub async fn testing_register_stage_pause(
        namespace_id: &str,
    ) -> std::sync::Arc<StoreStateStagePause> {
        let gate = std::sync::Arc::new(StoreStateStagePause::new());
        store_state_stage_pauses()
            .lock()
            .await
            .insert(namespace_id.to_string(), gate.clone());
        gate
    }
}

/// Fire the pause gate for a namespace, if a test registered one.
///
/// Called from `stage_store_state_records` after token validation, before
/// record writes. One-shot: the gate is removed before signalling, so a late
/// duplicate stage for the same namespace proceeds unpaused.
#[cfg(feature = "testing")]
pub(crate) async fn fire_store_state_stage_pause(namespace_id: &str) {
    let gate = store_state_stage_pauses().lock().await.remove(namespace_id);
    let Some(gate) = gate else { return };
    gate.validated.notify_one();
    if tokio::time::timeout(std::time::Duration::from_secs(15), gate.release.notified())
        .await
        .is_err()
    {
        panic!(
            "Store-state stage pause gate for namespace {namespace_id} was never released: test harness bug"
        );
    }
}

// SQLite-specific implementations
#[cfg(feature = "sqlite")]
impl SqlxBackend {
    /// Open a SQLite database at the given path.
    ///
    /// Creates the database file and schema if they don't exist.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the SQLite database file
    ///
    /// # Example
    ///
    /// ```ignore
    /// use eidetica::backend::database::sql::SqlxBackend;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let backend = SqlxBackend::open_sqlite("my_database.db").await.unwrap();
    /// }
    /// ```
    pub async fn open_sqlite<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        // mode=rwc: read-write-create (create file if it doesn't exist)
        let url = sqlite_path_url(path.as_ref())?;
        Self::connect_sqlite(&url).await
    }

    /// Connect to a SQLite database using a connection URL.
    ///
    /// # Arguments
    ///
    /// * `url` - SQLite connection URL (e.g., "sqlite:./my.db")
    pub async fn connect_sqlite(url: &str) -> Result<Self> {
        // Install any driver support
        sqlx::any::install_default_drivers();

        let storage = prepare_sqlite(url)?;
        let is_in_memory = storage.is_none();
        let normalized_url = normalize_sqlite_url(url);

        // For SQLite in-memory databases with shared cache, we must prevent
        // all connections from being closed. When the last connection closes,
        // the in-memory database is destroyed and all data is lost.
        //
        // IMPORTANT: SQLite pragmas like busy_timeout and synchronous are per-connection
        // settings. We use after_connect to ensure every connection in the pool has
        // these configured, not just one.
        let pool = if is_in_memory {
            AnyPoolOptions::new()
                .max_connections(1)
                .min_connections(1)
                .idle_timeout(None)
                .max_lifetime(None)
                .after_connect(|conn, _meta| {
                    Box::pin(async move {
                        // In-memory databases don't need WAL mode (all in RAM)
                        // but still need busy_timeout for lock contention
                        conn.execute("PRAGMA busy_timeout = 5000;").await?;
                        Ok(())
                    })
                })
                .connect(&normalized_url)
                .await
                .sql_context("Failed to connect to SQLite")?
        } else {
            AnyPoolOptions::new()
                .max_connections(5)
                .after_connect(|conn, _meta| {
                    Box::pin(async move {
                        // File-based SQLite per-connection settings:
                        // - synchronous=NORMAL: Balanced durability (safe with WAL)
                        // - busy_timeout=5000: Wait up to 5s for locks before failing
                        //
                        // Note: journal_mode=WAL is a database-level setting that persists,
                        // so we only set it once after pool creation, not per-connection.
                        conn.execute("PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000;")
                            .await?;
                        Ok(())
                    })
                })
                .connect(&normalized_url)
                .await
                .sql_context("Failed to connect to SQLite")?
        };

        // Set WAL mode once (database-level setting that persists in the file)
        if !is_in_memory {
            sqlx::query("PRAGMA journal_mode = WAL;")
                .execute(&pool)
                .await
                .sql_context("Failed to set SQLite WAL mode")?;
        }

        let backend = Self {
            pool: Some(pool),
            kind: DbKind::Sqlite,
            _owner: storage,
            #[cfg(all(feature = "postgres", feature = "testing"))]
            postgres_token: None,
        };

        // Initialize schema
        schema::initialize(&backend).await?;

        Ok(backend)
    }

    /// Create an in-memory SQLite database (async).
    ///
    /// The database exists only for the lifetime of this backend instance.
    /// Useful for testing.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use eidetica::backend::database::sql::SqlxBackend;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let backend = SqlxBackend::sqlite_in_memory().await.unwrap();
    /// }
    /// ```
    pub async fn sqlite_in_memory() -> Result<Self> {
        // Use shared cache mode for in-memory SQLite so all connections in the pool
        // share the same database. Without this, each connection gets its own
        // isolated in-memory database.
        // Use a unique name per instance to avoid sharing between tests.
        let unique_id = uuid::Uuid::new_v4();
        let url = format!("sqlite:file:mem_{unique_id}?mode=memory&cache=shared");
        Self::connect_sqlite(&url).await
    }
}

// PostgreSQL-specific implementations
#[cfg(feature = "postgres")]
const POSTGRES_OWNERSHIP_TABLE: &str = "_eidetica_storage_owner";

#[cfg(feature = "postgres")]
const POSTGRES_NAMESPACE_LOCK: &str = "hashtextextended(format('eidetica-storage-v1:%s/%s:%s/%s', octet_length(current_database()), current_database(), octet_length(current_schema()), current_schema()), 0)";

#[cfg(feature = "postgres")]
impl SqlxBackend {
    /// Connect to a PostgreSQL database using a connection URL.
    ///
    /// This connects to the default (public) schema. For test isolation,
    /// use `connect_postgres_isolated()` instead.
    ///
    /// # Arguments
    ///
    /// * `url` - PostgreSQL connection URL (e.g., "postgres://user:pass@localhost/dbname")
    ///
    /// # Example
    ///
    /// ```ignore
    /// use eidetica::backend::database::sql::SqlxBackend;
    ///
    /// let backend = SqlxBackend::connect_postgres("postgres://localhost/eidetica").await.unwrap();
    /// ```
    pub async fn connect_postgres(url: &str) -> Result<Self> {
        Self::connect_postgres_with_schema(url, None).await
    }

    /// Connect to a PostgreSQL database with a specific schema for isolation.
    ///
    /// Creates a unique schema if `schema_name` is provided, providing test isolation.
    /// Each test can use its own schema so they don't interfere with each other.
    ///
    /// # Arguments
    ///
    /// * `url` - PostgreSQL connection URL
    /// * `schema_name` - Optional schema name. If None, uses the default (public) schema.
    async fn connect_postgres_with_schema(url: &str, schema_name: Option<String>) -> Result<Self> {
        // Install any driver support
        sqlx::any::install_default_drivers();

        // If schema_name is provided, first create the schema, then use after_connect
        // to set search_path on each connection. This is more reliable than URL options
        // which don't work consistently across all network configurations.
        if let Some(ref schema) = schema_name {
            // First connect to create the schema if needed
            let temp_pool = AnyPoolOptions::new()
                .max_connections(1)
                .connect(url)
                .await
                .sql_context("Failed to connect to PostgreSQL")?;

            // Create schema if it doesn't exist
            let create_schema = format!("CREATE SCHEMA IF NOT EXISTS {schema}");
            sqlx::query(&create_schema)
                .execute(&temp_pool)
                .await
                .sql_context(&format!("Failed to create schema {schema}"))?;

            temp_pool.close().await;
        }

        // Build pool with after_connect hook to set search_path on each connection
        // For isolated (test) connections, use smaller pool to avoid exhausting
        // PostgreSQL's max_connections when running many tests in parallel.
        let mut owner = AnyConnection::connect(url)
            .await
            .sql_context("Failed to connect to PostgreSQL")?;
        if let Some(ref schema) = schema_name {
            let set_path = format!("SET search_path TO {schema}");
            owner
                .execute(set_path.as_str())
                .await
                .sql_context("Failed to select PostgreSQL storage namespace")?;
        }

        let (database, schema, acquired): (String, String, bool) = sqlx::query_as(&format!(
            "SELECT current_database()::text, current_schema()::text, pg_try_advisory_lock({POSTGRES_NAMESPACE_LOCK})"
        ))
        .fetch_one(&mut owner)
        .await
        .sql_context("Failed to claim PostgreSQL storage ownership")?;
        if !acquired {
            return Err(BackendError::StorageAlreadyOwned {
                namespace: format!("PostgreSQL database `{database}` schema `{schema}`"),
            }
            .into());
        }

        owner
            .execute(format!(
                "CREATE TABLE IF NOT EXISTS {POSTGRES_OWNERSHIP_TABLE} (id SMALLINT PRIMARY KEY CHECK (id = 1), token TEXT NOT NULL)"
            ).as_str())
            .await
            .sql_context("Failed to initialize PostgreSQL storage ownership metadata")?;
        let token = uuid::Uuid::new_v4().to_string();
        sqlx::query(&format!(
            "INSERT INTO {POSTGRES_OWNERSHIP_TABLE} (id, token) VALUES (1, $1) ON CONFLICT (id) DO UPDATE SET token = EXCLUDED.token"
        ))
        .bind(&token)
        .execute(&mut owner)
        .await
        .sql_context("Failed to publish PostgreSQL storage ownership token")?;
        owner
            .execute(format!("SELECT pg_advisory_lock_shared({POSTGRES_NAMESPACE_LOCK})").as_str())
            .await
            .sql_context("Failed to fence PostgreSQL storage ownership")?;
        owner
            .execute(format!("SELECT pg_advisory_unlock({POSTGRES_NAMESPACE_LOCK})").as_str())
            .await
            .sql_context("Failed to finish PostgreSQL storage ownership claim")?;

        let schema_for_hook = schema_name.clone();
        let is_isolated = schema_name.is_some();
        let mut pool_options = AnyPoolOptions::new();

        if is_isolated {
            // Test isolation: 2 connections is enough, with longer timeout to wait
            // rather than fail when many tests run in parallel
            pool_options = pool_options
                .max_connections(2)
                .acquire_timeout(Duration::from_secs(30));
        } else {
            // Production: 5 connections for real concurrency needs
            pool_options = pool_options.max_connections(5);
        }

        let token_for_hook = token.clone();
        let pool = pool_options
            .after_connect(move |conn, _meta| {
                let schema = schema_for_hook.clone();
                let token = token_for_hook.clone();
                Box::pin(async move {
                    if let Some(ref schema) = schema {
                        let set_path = format!("SET search_path TO {schema}");
                        conn.execute(set_path.as_str()).await?;
                    }
                    conn.execute(
                        format!("SELECT pg_advisory_lock_shared({POSTGRES_NAMESPACE_LOCK})")
                            .as_str(),
                    )
                    .await?;
                    let valid: (bool,) = sqlx::query_as(&format!(
                        "SELECT token = $1 FROM {POSTGRES_OWNERSHIP_TABLE} WHERE id = 1"
                    ))
                    .bind(token)
                    .fetch_one(&mut *conn)
                    .await?;
                    if !valid.0 {
                        return Err(sqlx::Error::Protocol(
                            "PostgreSQL storage ownership changed".to_string(),
                        ));
                    }
                    Ok(())
                })
            })
            .connect(url)
            .await
            .sql_context("Failed to connect to PostgreSQL")?;

        let backend = Self {
            pool: Some(pool),
            kind: DbKind::Postgres,
            _owner: Some(StorageOwner::Postgres {
                _connection: tokio::sync::Mutex::new(owner),
            }),
            #[cfg(feature = "testing")]
            postgres_token: Some(token),
        };

        // Initialize schema (tables will be created in the current search_path)
        schema::initialize(&backend).await?;

        Ok(backend)
    }

    /// Connect to a PostgreSQL database with test isolation.
    ///
    /// Creates a unique schema for this backend instance, ensuring tests
    /// don't interfere with each other when run in parallel.
    ///
    /// # Arguments
    ///
    /// * `url` - PostgreSQL connection URL (e.g., "postgres://user:pass@localhost/dbname")
    ///
    /// # Example
    ///
    /// ```ignore
    /// use eidetica::backend::database::sql::SqlxBackend;
    ///
    /// let backend = SqlxBackend::connect_postgres_isolated("postgres://localhost/eidetica").await.unwrap();
    /// // This backend uses its own isolated schema
    /// ```
    pub async fn connect_postgres_isolated(url: &str) -> Result<Self> {
        // Generate a unique schema name using UUID
        // PostgreSQL schema names must start with a letter and be lowercase
        let unique_id = uuid::Uuid::new_v4().simple().to_string();
        let schema_name = format!("test_{unique_id}");
        Self::connect_postgres_with_schema(url, Some(schema_name)).await
    }

    #[cfg(feature = "testing")]
    #[doc(hidden)]
    pub async fn test_connect_postgres_schema(url: &str, schema: String) -> Result<Self> {
        Self::connect_postgres_with_schema(url, Some(schema)).await
    }
}

#[async_trait]
impl BackendImpl for SqlxBackend {
    async fn resolve_store_state(&self, request: &StoreStateRequest) -> Result<Option<RecordView>> {
        storage::resolve_store_state(self, request).await
    }

    async fn begin_store_state_staging(&self, request: StoreStateRequest) -> Result<StagingToken> {
        storage::begin_store_state_staging(self, request).await
    }

    async fn stage_store_state_records(
        &self,
        token: &StagingToken,
        records: RecordMutations,
    ) -> Result<()> {
        storage::stage_store_state_records(self, token, records).await
    }

    async fn publish_store_state(&self, token: StagingToken) -> Result<RecordView> {
        storage::publish_store_state(self, token).await
    }

    async fn abort_store_state(&self, token: StagingToken) -> Result<()> {
        storage::abort_store_state(self, token).await
    }

    async fn store_state_record_get(
        &self,
        view: &RecordView,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        storage::store_state_record_get(self, view, key).await
    }

    async fn store_state_record_scan(
        &self,
        view: &RecordView,
        range: &RecordRange,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<RecordPage> {
        storage::store_state_record_scan(self, view, range, after, limit).await
    }

    async fn clear_derived_store_state(&self) -> Result<()> {
        storage::clear_derived_store_state(self).await
    }
    async fn get(&self, id: &ID) -> Result<Entry> {
        storage::get(self, id).await
    }

    async fn get_verification_status(&self, id: &ID) -> Result<VerificationStatus> {
        storage::get_verification_status(self, id).await
    }

    async fn put(&self, entry: Entry) -> Result<()> {
        storage::put(self, entry).await
    }

    async fn update_verification_status(
        &self,
        id: &ID,
        verification_status: VerificationStatus,
    ) -> Result<()> {
        storage::update_verification_status(self, id, verification_status).await
    }

    async fn get_entries_by_verification_status(
        &self,
        status: VerificationStatus,
    ) -> Result<Vec<ID>> {
        storage::get_entries_by_verification_status(self, status).await
    }

    async fn snapshot(&self, tree: &ID) -> Result<Snapshot> {
        traversal::snapshot(self, tree).await.map(Snapshot::new)
    }

    async fn store_snapshot(&self, tree: &ID, store: &str) -> Result<Snapshot> {
        traversal::store_snapshot(self, tree, store)
            .await
            .map(Snapshot::new)
    }

    async fn store_snapshot_at(
        &self,
        tree: &ID,
        store: &str,
        main_snapshot: &Snapshot,
    ) -> Result<Snapshot> {
        traversal::store_snapshot_at(self, tree, store, main_snapshot.tips())
            .await
            .map(Snapshot::new)
    }

    async fn all_roots(&self) -> Result<Vec<ID>> {
        storage::all_roots(self).await
    }

    async fn find_merge_base(
        &self,
        tree: &ID,
        store: &str,
        entry_ids: &[ID],
    ) -> Result<Option<ID>> {
        traversal::find_merge_base(self, tree, store, entry_ids).await
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    async fn get_tree(&self, tree: &ID) -> Result<Vec<Entry>> {
        storage::get_tree(self, tree).await
    }

    async fn get_store(&self, tree: &ID, store: &str) -> Result<Vec<Entry>> {
        storage::get_store(self, tree, store).await
    }

    async fn get_tree_from_tips(&self, tree: &ID, tips: &[ID]) -> Result<Vec<Entry>> {
        traversal::get_tree_from_tips(self, tree, tips).await
    }

    async fn store_at(&self, tree: &ID, store: &str, snapshot: &Snapshot) -> Result<Vec<Entry>> {
        traversal::store_at(self, tree, store, snapshot.tips()).await
    }

    async fn get_sorted_store_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        store: &str,
    ) -> Result<Vec<ID>> {
        traversal::get_sorted_store_parents(self, tree_id, entry_id, store).await
    }

    async fn get_path_from_to(
        &self,
        tree_id: &ID,
        store: &str,
        from_id: Option<&ID>,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        traversal::get_path_from_to(self, tree_id, store, from_id, to_ids).await
    }

    async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>> {
        storage::get_instance_metadata(self).await
    }

    async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()> {
        storage::set_instance_metadata(self, metadata).await
    }

    async fn get_instance_secrets(&self) -> Result<Option<InstanceSecrets>> {
        storage::get_instance_secrets(self).await
    }

    async fn set_instance_secrets(&self, secrets: &InstanceSecrets) -> Result<()> {
        storage::set_instance_secrets(self, secrets).await
    }
}

/// Namespace for SQLite database constructors.
///
/// Provides ergonomic factory methods for creating SQLite-backed storage.
/// All methods return `SqlxBackend` which implements `BackendImpl`.
///
/// # Example
///
/// ```ignore
/// use eidetica::backend::database::Sqlite;
///
/// // File-based storage
/// let backend = Sqlite::open("my_data.db").await?;
///
/// // In-memory (for testing)
/// let backend = Sqlite::in_memory().await?;
/// ```
#[cfg(feature = "sqlite")]
pub struct Sqlite;

#[cfg(feature = "sqlite")]
impl Sqlite {
    /// Open a SQLite database at the given path.
    ///
    /// Creates the database file and schema if they don't exist.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the SQLite database file
    pub async fn open<P: AsRef<std::path::Path>>(path: P) -> Result<SqlxBackend> {
        SqlxBackend::open_sqlite(path).await
    }

    /// Create an in-memory SQLite database.
    ///
    /// The database exists only for the lifetime of the returned backend.
    /// Useful for testing.
    pub async fn in_memory() -> Result<SqlxBackend> {
        SqlxBackend::sqlite_in_memory().await
    }

    /// Connect to a SQLite database using a connection URL.
    ///
    /// # Arguments
    ///
    /// * `url` - SQLite connection URL (e.g., "sqlite:./my.db")
    pub async fn connect(url: &str) -> Result<SqlxBackend> {
        SqlxBackend::connect_sqlite(url).await
    }
}

/// Namespace for PostgreSQL database constructors.
///
/// Provides ergonomic factory methods for creating PostgreSQL-backed storage.
/// All methods return `SqlxBackend` which implements `BackendImpl`.
///
/// # Example
///
/// ```ignore
/// use eidetica::backend::database::Postgres;
///
/// // Connect to PostgreSQL
/// let backend = Postgres::connect("postgres://user:pass@localhost/mydb").await?;
///
/// // With test isolation (unique schema per instance)
/// let backend = Postgres::connect_isolated("postgres://localhost/test").await?;
/// ```
#[cfg(feature = "postgres")]
pub struct Postgres;

#[cfg(feature = "postgres")]
impl Postgres {
    /// Connect to a PostgreSQL database using a connection URL.
    ///
    /// This connects to the default (public) schema. For test isolation,
    /// use `connect_isolated()` instead.
    ///
    /// # Arguments
    ///
    /// * `url` - PostgreSQL connection URL (e.g., "postgres://user:pass@localhost/dbname")
    pub async fn connect(url: &str) -> Result<SqlxBackend> {
        SqlxBackend::connect_postgres(url).await
    }

    /// Connect to a PostgreSQL database with test isolation.
    ///
    /// Creates a unique schema for this backend instance, ensuring tests
    /// don't interfere with each other when run in parallel.
    ///
    /// # Arguments
    ///
    /// * `url` - PostgreSQL connection URL
    pub async fn connect_isolated(url: &str) -> Result<SqlxBackend> {
        SqlxBackend::connect_postgres_isolated(url).await
    }
}

#[cfg(all(test, feature = "postgres"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_postgres_initialization_releases_ownership() {
        if std::env::var("TEST_BACKEND").as_deref() != Ok("postgres") {
            return;
        }

        let url = std::env::var("TEST_POSTGRES_URL")
            .unwrap_or_else(|_| "postgres://localhost/eidetica_test".to_string());
        sqlx::any::install_default_drivers();
        let schema = format!("test_{}", uuid::Uuid::new_v4().simple());
        let setup = AnyPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&setup)
            .await
            .unwrap();
        sqlx::query(&format!(
            "CREATE VIEW {schema}.entries AS SELECT 1 AS value"
        ))
        .execute(&setup)
        .await
        .unwrap();

        assert!(
            SqlxBackend::connect_postgres_with_schema(&url, Some(schema.clone()))
                .await
                .is_err(),
            "the conflicting view must make schema initialization fail"
        );
        sqlx::query(&format!("DROP VIEW {schema}.entries"))
            .execute(&setup)
            .await
            .unwrap();

        SqlxBackend::connect_postgres_with_schema(&url, Some(schema.clone()))
            .await
            .expect("failed initialization must release PostgreSQL ownership");
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&setup)
            .await
            .unwrap();
        setup.close().await;
    }
}

/// A publish carrying a token whose target does not match the namespace it
/// names must never disturb a ready namespace.
///
/// Unlike `InMemory` (which resolves by target and adopts the ready winner),
/// the SQL publish names the namespace: the `UPDATE` only flips
/// lifecycle/status and keeps the begin-time identity, using `target` for
/// locking and failure-path winner adoption. Either way the ready snapshot
/// and its records always survive a malformed clone.
#[cfg(all(test, feature = "sqlite"))]
mod store_state_token_tests {
    use std::collections::BTreeMap;

    use super::Sqlite;
    use crate::backend::{
        BackendImpl, CacheScope, ProjectionDescriptor, StagingToken, StoreStateLifecycle,
        StoreStateRequest,
    };
    use crate::entry::ID;

    fn request(store: &str) -> StoreStateRequest {
        StoreStateRequest {
            database: ID::from_bytes("db"),
            store: store.to_string(),
            lifecycle: StoreStateLifecycle::Derived,
            scope: CacheScope::Shared,
            projection: ProjectionDescriptor {
                name: "test/opaque".to_string(),
                version: 0,
            },
            source_key: b"snapshot".to_vec(),
        }
    }

    #[tokio::test]
    async fn mismatched_target_publish_preserves_ready_namespace() {
        let backend = Sqlite::in_memory().await.unwrap();

        // Ready namespace for target A.
        let request_a = request("store-a");
        let token_a = backend
            .begin_store_state_staging(request_a.clone())
            .await
            .unwrap();
        backend
            .stage_store_state_records(
                &token_a,
                BTreeMap::from([(b"key".to_vec(), Some(b"value-a".to_vec()))]),
            )
            .await
            .unwrap();
        let view_a = backend.publish_store_state(token_a).await.unwrap();

        // Staging namespace for target B.
        let request_b = request("store-b");
        let token_b = backend
            .begin_store_state_staging(request_b.clone())
            .await
            .unwrap();
        backend
            .stage_store_state_records(
                &token_b,
                BTreeMap::from([(b"key".to_vec(), Some(b"value-b".to_vec()))]),
            )
            .await
            .unwrap();

        // Malformed clone: B's namespace id, A's target. The SQL publish names
        // the namespace (the UPDATE only flips lifecycle/status and keeps the
        // begin-time identity; `target` drives locking and winner adoption),
        // so B becomes ready under its own identity while ready A is
        // untouched: no ready snapshot is ever modified by a mismatched token.
        let bad = StagingToken {
            namespace_id: token_b.namespace_id.clone(),
            target: request_a.clone(),
        };
        let published_b = backend.publish_store_state(bad).await.unwrap();
        assert_eq!(published_b.namespace_id, token_b.namespace_id);
        assert_eq!(
            backend.resolve_store_state(&request_b).await.unwrap(),
            Some(published_b.clone())
        );
        assert_eq!(
            backend
                .store_state_record_get(&published_b, b"key")
                .await
                .unwrap(),
            Some(b"value-b".to_vec())
        );
        assert_eq!(
            backend.resolve_store_state(&request_a).await.unwrap(),
            Some(view_a.clone())
        );
        assert_eq!(
            backend
                .store_state_record_get(&view_a, b"key")
                .await
                .unwrap(),
            Some(b"value-a".to_vec())
        );

        // Malformed clone: A's (ready) namespace id with a target that
        // resolves nowhere. The publish fails and the ready row survives the
        // guarded discard with its records intact.
        let nowhere = request("store-nowhere");
        let bad_ready = StagingToken {
            namespace_id: view_a.namespace_id.clone(),
            target: nowhere.clone(),
        };
        assert!(backend.publish_store_state(bad_ready).await.is_err());
        assert_eq!(
            backend.resolve_store_state(&request_a).await.unwrap(),
            Some(view_a.clone())
        );
        assert_eq!(
            backend
                .store_state_record_get(&view_a, b"key")
                .await
                .unwrap(),
            Some(b"value-a".to_vec())
        );
        assert_eq!(backend.resolve_store_state(&nowhere).await.unwrap(), None);
    }
}
