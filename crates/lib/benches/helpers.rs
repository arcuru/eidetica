//! Shared helpers for benchmark tests

use eidetica::{
    Database, Instance,
    backend::{BackendImpl, database::InMemory},
    crdt::Doc,
    user::User,
};

/// Creates a test backend based on TEST_BACKEND env var.
///
/// Supported values:
/// - "inmemory" or unset: InMemory backend (default)
/// - "sqlite": SQLite in-memory backend (requires `sqlite` feature)
///
/// This mirrors the pattern used in integration tests for consistency.
async fn test_backend() -> Box<dyn BackendImpl> {
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
        Ok("inmemory") | Ok("") | Err(_) => Box::new(InMemory::new()),
        Ok(other) => {
            panic!("Unknown TEST_BACKEND value: {other}. Supported: inmemory, sqlite")
        }
    }
}

/// Creates a fresh empty tree with configurable backend for benchmarking.
///
/// Uses TEST_BACKEND env var to select backend (default: inmemory).
/// Returns (Instance, User, Database) tuple.
pub async fn setup_tree_async() -> (Instance, User, Database) {
    let backend = test_backend().await;
    let instance = Instance::open(backend)
        .await
        .expect("Benchmark setup failed");

    // Create and login user
    instance
        .create_user("bench_user", None)
        .await
        .expect("Failed to create user");
    let mut user = instance
        .login_user("bench_user", None)
        .await
        .expect("Failed to login user");

    let key_id = user.get_default_key().expect("Failed to get default key");
    let db = user
        .create_database(Doc::new(), &key_id)
        .await
        .expect("Failed to create database");

    (instance, user, db)
}
