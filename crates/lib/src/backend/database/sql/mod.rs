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

mod cache;
mod storage;
mod traversal;

/// Schema definition and migration system.
pub mod schema;

use std::any::Any;
use std::time::Duration;

use async_trait::async_trait;
use sqlx::AnyPool;
use sqlx::Executor;
use sqlx::any::AnyPoolOptions;

use crate::Result;
use crate::backend::errors::BackendError;
use crate::backend::{BackendImpl, InstanceMetadata, VerificationStatus};
use crate::entry::{Entry, ID};

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
/// # Thread Safety
///
/// `SqlxBackend` is `Send + Sync` as required by `BackendImpl`. The underlying
/// sqlx pool handles connection pooling and thread safety.
///
/// # Test Isolation
///
/// For PostgreSQL, each backend instance can use its own schema for test isolation.
/// Use `connect_postgres_isolated()` to create an isolated backend for testing.
pub struct SqlxBackend {
    pool: AnyPool,
    kind: DbKind,
}

impl SqlxBackend {
    /// Get a reference to the underlying pool.
    pub fn pool(&self) -> &AnyPool {
        &self.pool
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
        let url = format!("sqlite:{}?mode=rwc", path.as_ref().display());
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

        // Detect if this is an in-memory database
        let is_in_memory = url.contains("mode=memory");

        // For SQLite in-memory databases with shared cache, we must prevent
        // all connections from being closed. When the last connection closes,
        // the in-memory database is destroyed and all data is lost.
        let pool = if is_in_memory {
            AnyPoolOptions::new()
                .max_connections(5)
                .min_connections(1)
                .idle_timeout(None)
                .max_lifetime(None)
                .connect(url)
                .await
                .sql_context("Failed to connect to SQLite")?
        } else {
            AnyPoolOptions::new()
                .max_connections(5)
                .connect(url)
                .await
                .sql_context("Failed to connect to SQLite")?
        };

        // Configure SQLite pragmas
        if is_in_memory {
            // In-memory databases don't need WAL mode (all in RAM)
            sqlx::query("PRAGMA busy_timeout = 5000;")
                .execute(&pool)
                .await
                .sql_context("Failed to configure SQLite")?;
        } else {
            // File-based SQLite:
            // - journal_mode=WAL: Write-Ahead Logging for better concurrency
            // - synchronous=NORMAL: Balanced durability (safe with WAL)
            // - busy_timeout=5000: Wait up to 5s for locks before failing
            sqlx::query(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA busy_timeout = 5000;",
            )
            .execute(&pool)
            .await
            .sql_context("Failed to configure SQLite")?;
        }

        let backend = Self {
            pool,
            kind: DbKind::Sqlite,
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

        let pool = pool_options
            .after_connect(move |conn, _meta| {
                let schema = schema_for_hook.clone();
                Box::pin(async move {
                    if let Some(ref s) = schema {
                        let set_path = format!("SET search_path TO {s}");
                        conn.execute(set_path.as_str()).await?;
                    }
                    Ok(())
                })
            })
            .connect(url)
            .await
            .sql_context("Failed to connect to PostgreSQL")?;

        let backend = Self {
            pool,
            kind: DbKind::Postgres,
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
}

#[async_trait]
impl BackendImpl for SqlxBackend {
    async fn get(&self, id: &ID) -> Result<Entry> {
        storage::get(self, id).await
    }

    async fn get_verification_status(&self, id: &ID) -> Result<VerificationStatus> {
        storage::get_verification_status(self, id).await
    }

    async fn put(&self, verification_status: VerificationStatus, entry: Entry) -> Result<()> {
        storage::put(self, verification_status, entry).await
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

    async fn get_tips(&self, tree: &ID) -> Result<Vec<ID>> {
        traversal::get_tips(self, tree).await
    }

    async fn get_store_tips(&self, tree: &ID, store: &str) -> Result<Vec<ID>> {
        traversal::get_store_tips(self, tree, store).await
    }

    async fn get_store_tips_up_to_entries(
        &self,
        tree: &ID,
        store: &str,
        main_entries: &[ID],
    ) -> Result<Vec<ID>> {
        traversal::get_store_tips_up_to_entries(self, tree, store, main_entries).await
    }

    async fn all_roots(&self) -> Result<Vec<ID>> {
        storage::all_roots(self).await
    }

    async fn find_merge_base(&self, tree: &ID, store: &str, entry_ids: &[ID]) -> Result<ID> {
        traversal::find_merge_base(self, tree, store, entry_ids).await
    }

    async fn collect_root_to_target(
        &self,
        tree: &ID,
        store: &str,
        target_entry: &ID,
    ) -> Result<Vec<ID>> {
        traversal::collect_root_to_target(self, tree, store, target_entry).await
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

    async fn get_store_from_tips(&self, tree: &ID, store: &str, tips: &[ID]) -> Result<Vec<Entry>> {
        traversal::get_store_from_tips(self, tree, store, tips).await
    }

    async fn get_cached_crdt_state(&self, entry_id: &ID, store: &str) -> Result<Option<String>> {
        storage::get_cached_crdt_state(self, entry_id, store).await
    }

    async fn cache_crdt_state(&self, entry_id: &ID, store: &str, state: String) -> Result<()> {
        storage::cache_crdt_state(self, entry_id, store, state).await
    }

    async fn clear_crdt_cache(&self) -> Result<()> {
        storage::clear_crdt_cache(self).await
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
        from_id: &ID,
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
