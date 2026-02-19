use std::sync::Arc;

use eidetica::{
    Database, Error, FixedClock, Instance,
    auth::types::AuthKey,
    backend::BackendImpl,
    backend::database::InMemory,
    crdt::{Doc, doc::Value},
    store::DocStore,
    user::User,
};

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
///
/// # Panics
/// Panics if TEST_BACKEND=sqlite but the `sqlite` feature is not enabled.
/// Panics if TEST_BACKEND=postgres but the `postgres` feature is not enabled.
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
        Ok("inmemory") | Ok("") | Err(_) => Box::new(InMemory::new()),
        Ok(other) => {
            panic!("Unknown TEST_BACKEND value: {other}. Supported: inmemory, sqlite, postgres")
        }
    }
}

/// Creates a basic Instance with no users or keys.
///
/// Uses a [`FixedClock`] for controllable timestamps in tests.
pub async fn test_instance() -> Instance {
    let clock = Arc::new(FixedClock::default());
    Instance::open_with_clock(test_backend().await, clock)
        .await
        .expect("Failed to create test instance")
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
    let instance = test_instance().await;
    instance
        .create_user(username, None)
        .await
        .expect("Failed to create user");
    let user = instance
        .login_user(username, None)
        .await
        .expect("Failed to login user");
    (instance, user)
}

/// Creates an Instance with a user and key, returning user and key_id for User API tests.
///
/// The key_id is the public key string (e.g., "ed25519:abc123...") which is used
/// as the SigKey when creating databases via User API.
///
/// # Returns
/// - Instance: The database instance
/// - User: Logged-in user session
/// - String: The key_id (public key string) for database operations
pub async fn test_instance_with_user_and_key(
    username: &str,
    key_display_name: Option<&str>,
) -> (Instance, User, String) {
    let instance = test_instance().await;
    instance
        .create_user(username, None)
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

/// Creates a tree using User API and returns (Instance, Database, key_id).
///
/// This is the preferred pattern for new tests. The key_id should be used
/// in assertions like `is_signed_by(&key_id)`.
pub async fn setup_tree_with_user_key() -> (Instance, Database, String) {
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

/// Creates an instance without any users (for tests that manage users manually)
pub async fn setup_empty_db() -> Instance {
    test_instance().await
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
pub async fn add_auth_key(db: &Database, pubkey: &str, key: AuthKey) {
    let txn = db.new_transaction().await.unwrap();
    let settings = txn.get_settings().unwrap();
    settings.set_auth_key(pubkey, key).await.unwrap();
    txn.commit().await.unwrap();
}

/// Rename an auth key's display name on a database via a settings transaction.
pub async fn rename_auth_key(db: &Database, pubkey: &str, name: Option<&str>) {
    let txn = db.new_transaction().await.unwrap();
    let settings = txn.get_settings().unwrap();
    settings.rename_auth_key(pubkey, name).await.unwrap();
    txn.commit().await.unwrap();
}

/// Add or overwrite multiple auth keys on a database in a single transaction.
pub async fn add_auth_keys(db: &Database, keys: &[(&str, AuthKey)]) {
    let txn = db.new_transaction().await.unwrap();
    let settings = txn.get_settings().unwrap();
    for (pubkey, key) in keys {
        settings.set_auth_key(pubkey, key.clone()).await.unwrap();
    }
    txn.commit().await.unwrap();
}
