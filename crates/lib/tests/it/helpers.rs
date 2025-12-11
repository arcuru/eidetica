use std::sync::Arc;

use eidetica::{
    Instance, backend::BackendImpl, backend::database::InMemory, crdt::doc::Value,
    instance::LegacyInstanceOps, store::DocStore, user::User,
};

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
pub fn test_backend() -> Box<dyn BackendImpl> {
    match std::env::var("TEST_BACKEND").as_deref() {
        Ok("sqlite") => {
            #[cfg(feature = "sqlite")]
            {
                use eidetica::backend::database::sql::Sqlite;
                Box::new(Sqlite::in_memory().expect("Failed to create SQLite backend"))
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
                // Use connect_isolated for test isolation - each test gets its own schema
                // This always creates its own runtime internally, which works whether
                // we're in a tokio context or not (the runtime is contained in the backend)
                Box::new(Postgres::connect_isolated(&url).expect("Failed to connect to PostgreSQL"))
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

/// Creates a basic Instance with no users or keys
pub fn test_instance() -> Instance {
    Instance::open(test_backend()).expect("Failed to create test instance")
}

/// Creates an Instance wrapped in Arc (common for sync tests)
#[allow(dead_code)]
pub fn test_instance_arc() -> Arc<Instance> {
    Arc::new(test_instance())
}

/// Creates an Instance with a passwordless user (most common test pattern)
///
/// Returns (Instance, User) for immediate use with User API
pub fn test_instance_with_user(username: &str) -> (Instance, User) {
    let instance = test_instance();
    instance
        .create_user(username, None)
        .expect("Failed to create user");
    let user = instance
        .login_user(username, None)
        .expect("Failed to login user");
    (instance, user)
}

/// Creates an Instance with deprecated key management (MIGRATION ONLY)
///
/// **DEPRECATED**: This helper exists only for migrating old tests. New tests should
/// use `test_instance_with_user()` and the User API for key management.
pub fn test_instance_with_legacy_key(key_name: &str) -> Instance {
    let instance = test_instance();
    instance
        .add_private_key(key_name)
        .expect("Failed to add legacy key");
    instance
}

// ==========================
// COMPATIBILITY HELPERS
// ==========================
// These maintain compatibility with existing tests while using the new User API

const DEFAULT_TEST_USER: &str = "test_user";

/// Creates a basic authenticated database with User API and default key
///
/// This replaces the old `setup_db()` pattern. Uses a default test user.
pub fn setup_db() -> (Instance, User) {
    test_instance_with_user(DEFAULT_TEST_USER)
}

/// Creates an instance without any users (for tests that manage users manually)
pub fn setup_empty_db() -> Instance {
    test_instance()
}

/// Creates an authenticated database with a specific key name (DEPRECATED PATTERN)
///
/// **DEPRECATED**: New tests should use `test_instance_with_user()` and User API.
/// This helper maintains compatibility with tests not yet migrated to User API.
pub fn setup_db_with_key(key_name: &str) -> Instance {
    test_instance_with_legacy_key(key_name)
}

/// Creates a basic tree using User API with default key
///
/// Note: Returns the Instance along with the Database because Database holds a weak reference.
/// If the Instance is dropped, operations on the Database will fail with InstanceDropped.
pub fn setup_tree() -> (Instance, eidetica::Database) {
    let (instance, mut user) = setup_db();
    let default_key = user.get_default_key().expect("Failed to get default key");

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "test_tree");

    let tree = user
        .create_database(settings, &default_key)
        .expect("Failed to create tree for testing");
    (instance, tree)
}

/// Creates a tree with a specific key (DEPRECATED PATTERN)
///
/// **DEPRECATED**: New tests should use User API for key management.
///
/// Note: Returns the Instance along with the Database because Database holds a weak reference.
/// If the Instance is dropped, operations on the Database will fail with InstanceDropped.
pub fn setup_tree_with_key(key_name: &str) -> (Instance, eidetica::Database) {
    let db = setup_db_with_key(key_name);
    let tree = db
        .new_database_default(key_name)
        .expect("Failed to create tree for testing");
    (db, tree)
}

/// Creates a tree and database with a specific key (DEPRECATED PATTERN)
///
/// **DEPRECATED**: New tests should use User API for key management.
pub fn setup_db_and_tree_with_key(key_name: &str) -> (Instance, eidetica::Database) {
    let db = setup_db_with_key(key_name);
    let tree = db
        .new_database_default(key_name)
        .expect("Failed to create tree for testing");
    (db, tree)
}

/// Creates a tree with initial settings using User API
///
/// Note: Returns the Instance along with the Database because Database holds a weak reference.
/// If the Instance is dropped, operations on the Database will fail with InstanceDropped.
pub fn setup_tree_with_settings(settings: &[(&str, &str)]) -> (Instance, eidetica::Database) {
    let (instance, mut user) = setup_db();
    let default_key = user.get_default_key().expect("Failed to get default key");

    let mut db_settings = eidetica::crdt::Doc::new();
    db_settings.set("name", "test_tree_with_settings");

    let tree = user
        .create_database(db_settings, &default_key)
        .expect("Failed to create tree");

    // Add the user settings through an operation
    let op = tree.new_transaction().expect("Failed to create operation");
    {
        let settings_store = op
            .get_store::<DocStore>("_settings")
            .expect("Failed to get settings subtree");

        for (key, value) in settings {
            settings_store
                .set(*key, *value)
                .expect("Failed to set setting");
        }
    }
    op.commit().expect("Failed to commit settings");

    (instance, tree)
}

// ==========================
// ASSERTION HELPERS
// ==========================

/// Helper for common assertions around DocStore value retrieval
pub fn assert_dict_value(store: &DocStore, key: &str, expected: &str) {
    match store
        .get(key)
        .unwrap_or_else(|_| panic!("Failed to get key {key}"))
    {
        Value::Text(value) => assert_eq!(value, expected),
        _ => panic!("Expected text value for key {key}"),
    }
}

/// Helper for checking NotFound errors
pub fn assert_key_not_found(result: Result<Value, eidetica::Error>) {
    match result {
        Err(ref err) if err.is_not_found() => (), // Expected
        other => panic!("Expected NotFound error, got {other:?}"),
    }
}
