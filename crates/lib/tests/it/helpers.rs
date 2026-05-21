use std::sync::Arc;
use std::time::Duration;

use eidetica::{
    Database, Error, FixedClock, Instance, NewUser,
    auth::{crypto::PublicKey, types::AuthKey},
    backend::BackendImpl,
    backend::database::InMemory,
    crdt::{Doc, doc::Value},
    store::DocStore,
    user::User,
};

#[cfg(all(unix, feature = "service"))]
use {eidetica::service::ServiceServer, tokio::sync::watch};

// Re-export tokio test macro for convenience
pub use tokio;

// Re-export TestContext for convenience
pub use crate::context::TestContext;

// ==========================
// CORE TEST FACTORIES
// ==========================
// These are the foundation for all test setup. They provide a single point of change
// for backend matrix testing via TEST_BACKEND env var.

/// Creates a test backend based on TEST_BACKEND env var.
///
/// Supported values:
/// - "inmemory" or unset: InMemory backend (default)
/// - "sqlite": SQLite in-memory backend (requires `sqlite` feature)
/// - "postgres": PostgreSQL backend (requires `postgres` feature and TEST_POSTGRES_URL)
/// - "service": RemoteBackend over Unix socket to an in-process daemon
///   (requires `service` feature and unix)
///
/// # Panics
/// Panics if TEST_BACKEND=sqlite but the `sqlite` feature is not enabled.
/// Panics if TEST_BACKEND=postgres but the `postgres` feature is not enabled.
/// Panics if TEST_BACKEND=service but the `service` feature is not enabled or not on unix.
///
/// # Example
/// ```bash
/// # Run tests with InMemory (default)
/// cargo test
///
/// # Run tests with SQLite
/// TEST_BACKEND=sqlite cargo test --features sqlite
///
/// # Run tests with PostgreSQL
/// TEST_BACKEND=postgres TEST_POSTGRES_URL="host=localhost dbname=eidetica_test" \
///   cargo test --features postgres
///
/// # Run tests through the service (daemon) RPC layer
/// TEST_BACKEND=service cargo test --features service
/// ```
/// Creates a test backend based on TEST_BACKEND env var
pub async fn test_backend() -> Box<dyn BackendImpl> {
    match std::env::var("TEST_BACKEND").as_deref() {
        Ok("sqlite") => {
            #[cfg(feature = "sqlite")]
            {
                use eidetica::backend::database::Sqlite;
                Box::new(
                    Sqlite::in_memory()
                        .await
                        .expect("Failed to create SQLite backend"),
                )
            }
            #[cfg(not(feature = "sqlite"))]
            {
                panic!("TEST_BACKEND=sqlite requires the 'sqlite' feature to be enabled")
            }
        }
        Ok("postgres") => {
            #[cfg(feature = "postgres")]
            {
                use eidetica::backend::database::Postgres;
                let url = std::env::var("TEST_POSTGRES_URL")
                    .unwrap_or_else(|_| "postgres://localhost/eidetica_test".to_string());
                Box::new(
                    Postgres::connect_isolated(&url)
                        .await
                        .expect("Failed to connect to PostgreSQL"),
                )
            }
            #[cfg(not(feature = "postgres"))]
            {
                panic!("TEST_BACKEND=postgres requires the 'postgres' feature to be enabled")
            }
        }
        Ok("service") => {
            // Service mode routes top-level tests (those using
            // `test_instance()` and friends) through a real daemon; direct
            // `test_backend()` callers, however, are raw-backend subsystem
            // tests that construct an `Instance` themselves and need a
            // local `BackendImpl`. A `RemoteConnection` can't be returned
            // as one, and these tests wouldn't be exercising the wire path
            // anyway. Fall back to `InMemory` so backend internals (tips,
            // verification status, store layout) can be tested while the
            // service test backend is otherwise active.
            Box::new(InMemory::new())
        }
        Ok("inmemory") | Ok("") | Err(_) => Box::new(InMemory::new()),
        Ok(other) => {
            panic!(
                "Unknown TEST_BACKEND value: {other}. Supported: inmemory, sqlite, postgres, service"
            )
        }
    }
}

/// Creates a basic Instance with no users or keys.
///
/// Uses a [`FixedClock`] for controllable timestamps in tests.
/// In service mode (TEST_BACKEND=service), spawns an in-process daemon
/// over an InMemory backend and returns a connected remote Instance,
/// authenticated as a bootstrap test user.
pub async fn test_instance() -> Instance {
    #[cfg(all(unix, feature = "service"))]
    {
        if std::env::var("TEST_BACKEND").as_deref() == Ok("service") {
            return test_remote_instance().await;
        }
    }
    test_local_instance().await
}

/// Always-local Instance, regardless of `TEST_BACKEND`.
///
/// Used by tests that exercise process-local subsystems (sync, device
/// keys, verification status, raw backend ops) which can't run client-side
/// against a remote daemon. Bypassing the service-backend env var here
/// keeps those tests running against a real implementation instead of
/// being skipped or failing on `OperationNotSupported`.
///
/// Bootstraps a passwordless `admin` user as the initial admin — matches
/// the historic test convention so existing `users.contains("admin")` style
/// assertions and `login_user("admin", None)` calls keep working. Tests
/// that need a specific bootstrap identity should use
/// [`test_local_instance_with_user`] instead.
#[allow(dead_code)]
pub async fn test_local_instance() -> Instance {
    let clock = Arc::new(FixedClock::default());
    let (instance, _admin) = Instance::create_with_clock(
        Box::new(InMemory::new()),
        clock,
        NewUser::passwordless("admin"),
    )
    .await
    .expect("Failed to create local test instance");
    instance
}

/// Local-only counterpart of `test_instance_with_user`, for subsystem
/// tests that need a logged-in user against a process-local instance
/// regardless of `TEST_BACKEND`. See [`test_local_instance`].
#[allow(dead_code)]
pub async fn test_local_instance_with_user(username: &str) -> (Instance, User) {
    let instance = test_local_instance().await;
    create_user(&instance, username, None)
        .await
        .expect("Failed to create user");
    let user = instance
        .login_user(username, None)
        .await
        .expect("Failed to login user");
    (instance, user)
}

/// Local-only counterpart of `test_instance_with_user_and_key`, for
/// subsystem tests that need a logged-in user against a process-local
/// instance regardless of `TEST_BACKEND`. See [`test_local_instance`].
#[allow(dead_code)]
pub async fn test_local_instance_with_user_and_key(
    username: &str,
    key_display_name: Option<&str>,
) -> (Instance, User, PublicKey) {
    let instance = test_local_instance().await;
    create_user(&instance, username, None)
        .await
        .expect("Failed to create user");
    let mut user = instance
        .login_user(username, None)
        .await
        .expect("Failed to login user");
    let key_id = user
        .add_private_key(key_display_name)
        .await
        .expect("Failed to add key");
    (instance, user, key_id)
}

/// Create a user via the bootstrapped admin session.
///
/// The test bootstrap uses a passwordless `admin` user (see
/// [`test_local_instance`] and [`test_remote_instance`]). This helper logs
/// in as that admin and drives [`InstanceAdmin::create_user`] —
/// the same admin path that production code uses. Returns the new user's
/// UUID, mirroring the old `Instance::create_user` signature so call sites
/// stay one-liners.
pub async fn create_user(
    instance: &Instance,
    username: &str,
    password: Option<&str>,
) -> eidetica::Result<String> {
    let admin = instance.login_user("admin", None).await?;
    let new_user = match password {
        Some(pw) => NewUser::with_password(username, pw),
        None => NewUser::passwordless(username),
    };
    admin.admin().await?.create_user(new_user).await
}

/// List all user IDs via the bootstrapped admin session.
///
/// Replaces the removed `Instance::list_users` — listing users reads `_users`
/// and is an admin operation reached through [`User::admin`].
#[allow(dead_code)]
pub async fn list_users(instance: &Instance) -> eidetica::Result<Vec<String>> {
    let admin = instance.login_user("admin", None).await?;
    admin.admin().await?.list_users().await
}

/// Spawn an in-process daemon over an InMemory backend, then connect and
/// authenticate as the auto-bootstrapped `admin`/`admin` user.
///
/// Each test gets its own daemon + socket directory for isolation.
/// Cleanup is handled by `Box::leak` — the server, socket dir, and
/// shutdown channel live for the process lifetime. Acceptable for test
/// code; memory is reclaimed at process exit.
///
/// Logging in as the bootstrap admin (rather than spinning up a
/// separate `test_bootstrap` user) keeps the user count identical to
/// `TEST_BACKEND=inmemory|sqlite`, so user-count assertions don't skew
/// in service mode.
#[cfg(all(unix, feature = "service"))]
async fn test_remote_instance() -> Instance {
    let dir = Box::leak(Box::new(
        tempfile::tempdir().expect("Failed to create temp dir for test daemon"),
    ));
    let socket_path = dir.path().join("test.sock");

    let (server, _admin) =
        Instance::create(Box::new(InMemory::new()), NewUser::passwordless("admin"))
            .await
            .expect("Failed to create server-side Instance");
    let service = ServiceServer::new(server.clone(), socket_path.clone());
    let (tx, rx) = watch::channel(());
    // Keep the shutdown channel alive so the server doesn't exit.
    let _tx = Box::leak(Box::new(tx));
    tokio::spawn(async move {
        let _ = service.run(rx).await;
    });

    // Wait for the socket to appear (server binds asynchronously).
    for _ in 0..50 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        socket_path.exists(),
        "daemon socket did not appear within 500ms"
    );

    let instance = Instance::connect(&socket_path)
        .await
        .expect("Failed to connect to test daemon");
    instance
        .login_user("admin", None)
        .await
        .expect("Failed to login as bootstrapped admin");

    instance
}

/// Creates an Instance wrapped in Arc (common for sync tests)
#[allow(dead_code)]
pub async fn test_instance_arc() -> Arc<Instance> {
    Arc::new(test_instance().await)
}

/// Creates an Instance with a passwordless user (most common test pattern)
///
/// Returns (Instance, User) for immediate use with User API
pub async fn test_instance_with_user(username: &str) -> (Instance, User) {
    #[cfg(all(unix, feature = "service"))]
    {
        if std::env::var("TEST_BACKEND").as_deref() == Ok("service") {
            return test_remote_instance_with_user(username).await;
        }
    }
    let instance = test_instance().await;
    create_user(&instance, username, None)
        .await
        .expect("Failed to create user");
    let user = instance
        .login_user(username, None)
        .await
        .expect("Failed to login user");
    (instance, user)
}

/// Service-mode variant: spawns a daemon, creates the named user server-side,
/// connects and authenticates as that user.
#[cfg(all(unix, feature = "service"))]
async fn test_remote_instance_with_user(username: &str) -> (Instance, User) {
    let dir = Box::leak(Box::new(
        tempfile::tempdir().expect("Failed to create temp dir for test daemon"),
    ));
    let socket_path = dir.path().join("test.sock");

    let (server, _admin) =
        Instance::create(Box::new(InMemory::new()), NewUser::passwordless("admin"))
            .await
            .expect("Failed to create server-side Instance");
    let service = ServiceServer::new(server.clone(), socket_path.clone());
    let (tx, rx) = watch::channel(());
    let _tx = Box::leak(Box::new(tx));
    tokio::spawn(async move {
        let _ = service.run(rx).await;
    });

    for _ in 0..50 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        socket_path.exists(),
        "daemon socket did not appear within 500ms"
    );

    // create_user is not available over the wire — create user server-side.
    create_user(&server, username, None)
        .await
        .expect("Failed to create user server-side");

    let instance = Instance::connect(&socket_path)
        .await
        .expect("Failed to connect to test daemon");
    let user = instance
        .login_user(username, None)
        .await
        .expect("Failed to login user");

    (instance, user)
}

/// Creates an Instance with a user and key, returning user and key_id for User API tests.
///
/// The key_id is the public key (e.g., "ed25519:abc123...") which is used
/// as the SigKey when creating databases via User API.
///
/// # Returns
/// - Instance: The database instance
/// - User: Logged-in user session
/// - PublicKey: The key_id for database operations
pub async fn test_instance_with_user_and_key(
    username: &str,
    key_display_name: Option<&str>,
) -> (Instance, User, PublicKey) {
    #[cfg(all(unix, feature = "service"))]
    {
        if std::env::var("TEST_BACKEND").as_deref() == Ok("service") {
            return test_remote_instance_with_user_and_key(username, key_display_name).await;
        }
    }
    let instance = test_instance().await;
    create_user(&instance, username, None)
        .await
        .expect("Failed to create user");
    let mut user = instance
        .login_user(username, None)
        .await
        .expect("Failed to login user");

    let key_id = user
        .add_private_key(key_display_name)
        .await
        .expect("Failed to add key");

    (instance, user, key_id)
}

/// Service-mode variant: spawns daemon, creates user + key server-side,
/// connects and authenticates as that user.
#[cfg(all(unix, feature = "service"))]
async fn test_remote_instance_with_user_and_key(
    username: &str,
    key_display_name: Option<&str>,
) -> (Instance, User, PublicKey) {
    let dir = Box::leak(Box::new(
        tempfile::tempdir().expect("Failed to create temp dir for test daemon"),
    ));
    let socket_path = dir.path().join("test.sock");

    let (server, _admin) =
        Instance::create(Box::new(InMemory::new()), NewUser::passwordless("admin"))
            .await
            .expect("Failed to create server-side Instance");
    let service = ServiceServer::new(server.clone(), socket_path.clone());
    let (tx, rx) = watch::channel(());
    let _tx = Box::leak(Box::new(tx));
    tokio::spawn(async move {
        let _ = service.run(rx).await;
    });

    for _ in 0..50 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        socket_path.exists(),
        "daemon socket did not appear within 500ms"
    );

    // Create user and add key server-side.
    create_user(&server, username, None)
        .await
        .expect("Failed to create user server-side");
    let mut server_user = server
        .login_user(username, None)
        .await
        .expect("Failed to login user server-side");
    server_user
        .add_private_key(key_display_name)
        .await
        .expect("Failed to add key server-side");

    // Connect and authenticate as the user.
    let instance = Instance::connect(&socket_path)
        .await
        .expect("Failed to connect to test daemon");
    let user = instance
        .login_user(username, None)
        .await
        .expect("Failed to login user");

    let key_id = user
        .get_default_key()
        .expect("User should have a default key");

    (instance, user, key_id)
}

/// Creates a tree using User API and returns (Instance, Database, key_id).
///
/// This is the preferred pattern for new tests. The key_id should be used
/// in assertions like `is_signed_by(&key_id)`.
pub async fn setup_tree_with_user_key() -> (Instance, Database, PublicKey) {
    let (instance, mut user, key_id) =
        test_instance_with_user_and_key("test_user", Some("test_key")).await;

    let mut settings = Doc::new();
    settings.set("name", "test_tree");

    let tree = user
        .create_database(settings, &key_id)
        .await
        .expect("Failed to create tree");

    (instance, tree, key_id)
}

/// Local-only variant of [`setup_tree_with_user_key`], for tests that
/// poke at backend internals (`get_verification_status`,
/// `update_verification_status`, `all_roots`, etc.) which have no wire
/// equivalent. See [`test_local_instance`].
#[allow(dead_code)]
pub async fn setup_tree_with_user_key_local() -> (Instance, Database, PublicKey) {
    let (instance, mut user, key_id) =
        test_local_instance_with_user_and_key("test_user", Some("test_key")).await;

    let mut settings = Doc::new();
    settings.set("name", "test_tree");

    let tree = user
        .create_database(settings, &key_id)
        .await
        .expect("Failed to create tree");

    (instance, tree, key_id)
}

// ==========================
// COMPATIBILITY HELPERS
// ==========================
// These maintain compatibility with existing tests while using the new User API

const DEFAULT_TEST_USER: &str = "test_user";

/// Creates a basic authenticated database with User API and default key
///
/// This replaces the old `setup_db()` pattern. Uses a default test user.
pub async fn setup_db() -> (Instance, User) {
    test_instance_with_user(DEFAULT_TEST_USER).await
}

/// Creates a basic tree using User API with default key
///
/// Note: Returns the Instance along with the Database because Database holds a weak reference.
/// If the Instance is dropped, operations on the Database will fail with InstanceDropped.
pub async fn setup_tree() -> (Instance, Database) {
    let (instance, mut user) = setup_db().await;
    let default_key = user.get_default_key().expect("Failed to get default key");

    let mut settings = Doc::new();
    settings.set("name", "test_tree");

    let tree = user
        .create_database(settings, &default_key)
        .await
        .expect("Failed to create tree for testing");
    (instance, tree)
}

/// Creates a tree with initial settings using User API
///
/// Note: Returns the Instance along with the Database because Database holds a weak reference.
/// If the Instance is dropped, operations on the Database will fail with InstanceDropped.
pub async fn setup_tree_with_settings(settings: &[(&str, &str)]) -> (Instance, Database) {
    let (instance, mut user) = setup_db().await;
    let default_key = user.get_default_key().expect("Failed to get default key");

    let mut db_settings = Doc::new();
    db_settings.set("name", "test_tree_with_settings");

    let tree = user
        .create_database(db_settings, &default_key)
        .await
        .expect("Failed to create tree");

    // Add the user settings through an operation
    let txn = tree
        .new_transaction()
        .await
        .expect("Failed to create transaction");
    {
        let settings_store = txn
            .get_store::<DocStore>("_settings")
            .await
            .expect("Failed to get settings subtree");

        for (key, value) in settings {
            settings_store
                .set(*key, *value)
                .await
                .expect("Failed to set setting");
        }
    }
    txn.commit().await.expect("Failed to commit settings");

    (instance, tree)
}

// ==========================
// ASSERTION HELPERS
// ==========================

/// Helper for common assertions around DocStore value retrieval
pub async fn assert_dict_value(store: &DocStore, key: &str, expected: &str) {
    match store
        .get(key)
        .await
        .unwrap_or_else(|_| panic!("Failed to get key {key}"))
    {
        Value::Text(value) => assert_eq!(value, expected),
        _ => panic!("Expected text value for key {key}"),
    }
}

/// Helper for checking NotFound errors
pub fn assert_key_not_found(result: Result<Value, Error>) {
    match result {
        Err(ref err) if err.is_not_found() => (), // Expected
        other => panic!("Expected NotFound error, got {other:?}"),
    }
}

// ==========================
// AUTH KEY HELPERS
// ==========================

/// Add or overwrite an auth key on a database via a settings transaction.
pub async fn add_auth_key(db: &Database, pubkey: &PublicKey, key: AuthKey) {
    let txn = db.new_transaction().await.unwrap();
    let settings = txn.get_settings().unwrap();
    settings.set_auth_key(pubkey, key).await.unwrap();
    txn.commit().await.unwrap();
}

/// Rename an auth key's display name on a database via a settings transaction.
pub async fn rename_auth_key(db: &Database, pubkey: &PublicKey, name: Option<&str>) {
    let txn = db.new_transaction().await.unwrap();
    let settings = txn.get_settings().unwrap();
    settings.rename_auth_key(pubkey, name).await.unwrap();
    txn.commit().await.unwrap();
}

/// Add or overwrite multiple auth keys on a database in a single transaction.
pub async fn add_auth_keys(db: &Database, keys: &[(&PublicKey, AuthKey)]) {
    let txn = db.new_transaction().await.unwrap();
    let settings = txn.get_settings().unwrap();
    for (pubkey, key) in keys {
        settings.set_auth_key(pubkey, key.clone()).await.unwrap();
    }
    txn.commit().await.unwrap();
}

/// Set the global auth key on a database via a settings transaction.
pub async fn set_global_auth_key(db: &Database, key: AuthKey) {
    let txn = db.new_transaction().await.unwrap();
    let settings = txn.get_settings().unwrap();
    settings.set_global_auth_key(key).await.unwrap();
    txn.commit().await.unwrap();
}

// ==========================
// TEST-ONLY VERIFICATION HELPER
// ==========================

/// Store an entry and immediately promote it to `Verified`, emulating what
/// the local validation pass does (store, then `update_verification_status`).
///
/// The production storage API no longer accepts a caller-asserted
/// verification status — `put` always stores `Unverified`. Tests that need a
/// pre-verified entry in the DAG (DAG/tip/sync fixtures that assume
/// already-validated data) use this extension instead. It exists only in the
/// test crate, so it is not part of the library's API surface.
pub trait TestVerify {
    async fn put_verified(&self, entry: eidetica::entry::Entry) -> eidetica::Result<()>;
}

// Explicit impls per concrete receiver (no blanket — a blanket over
// `BackendImpl` would coherence-conflict with the `Backend` impl). Method
// resolution auto-derefs, so these also cover `Box<_>` / `Arc<_>` wrappers.
macro_rules! impl_test_verify {
    ($ty:ty) => {
        impl TestVerify for $ty {
            async fn put_verified(&self, entry: eidetica::entry::Entry) -> eidetica::Result<()> {
                let id = entry.id();
                self.put(entry).await?;
                self.update_verification_status(
                    &id,
                    eidetica::backend::VerificationStatus::Verified,
                )
                .await
            }
        }
    };
}

impl_test_verify!(dyn BackendImpl);
impl_test_verify!(eidetica::backend::database::InMemory);
impl_test_verify!(eidetica::instance::backend::Backend);
