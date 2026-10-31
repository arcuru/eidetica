//! Shared helpers for benchmark tests

use criterion::{BenchmarkGroup, measurement::Measurement};
use eidetica::{
    Database, Instance,
    account::AccountSession,
    backend::{BackendImpl, database::InMemory},
    crdt::Doc,
};

/// Creates a test backend based on TEST_BACKEND env var.
///
/// Supported values:
/// - "inmemory": InMemory backend
/// - "sqlite" or unset: SQLite in-memory backend (requires `sqlite` feature)
///
/// This mirrors the pattern used in integration tests for consistency.
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
        Ok("inmemory") => Box::new(InMemory::new()),
        #[cfg(feature = "sqlite")]
        Ok("") | Err(_) => {
            use eidetica::backend::database::Sqlite;
            Box::new(
                Sqlite::in_memory()
                    .await
                    .expect("Failed to create SQLite backend"),
            )
        }
        #[cfg(not(feature = "sqlite"))]
        Ok("") | Err(_) => Box::new(InMemory::new()),
        Ok(other) => {
            panic!("Unknown TEST_BACKEND value: {other}. Supported: inmemory, sqlite")
        }
    }
}

/// Creates a fresh empty tree with configurable backend for benchmarking.
///
/// Uses TEST_BACKEND env var to select backend (default: sqlite).
/// Returns (Instance, AccountSession, Database) tuple.
#[allow(dead_code)]
pub async fn setup_tree_async() -> (Instance, AccountSession, Database) {
    let backend = test_backend().await;
    setup_tree_with_backend(backend).await
}

/// Creates a fresh empty tree with explicit InMemory backend.
///
/// Use this for benchmarks that test InMemory-specific functionality
/// (e.g., is_tip method which is not on the BackendImpl trait).
#[allow(dead_code)]
pub async fn setup_tree_inmemory() -> (Instance, AccountSession, Database) {
    let backend = Box::new(InMemory::new());
    setup_tree_with_backend(backend).await
}

/// Sample size used by groups whose per-iteration setup builds a large tree.
///
/// Well below Criterion's default because each sample rebuilds a tree of hundreds
/// of entries; measurements taken at this size are correspondingly noisy.
pub const LARGE_SETUP_SAMPLE_SIZE: usize = 10;

/// Applies [`LARGE_SETUP_SAMPLE_SIZE`] to a group unless `--sample-size` was passed.
///
/// A group calling `sample_size` unconditionally overrides the runner's own
/// configuration, so `--sample-size` has no effect on it. This yields to the flag
/// when it is present, and reports the reduced default on stderr when it is not.
#[allow(dead_code)]
pub fn apply_large_setup_sample_size<M: Measurement>(
    group: &mut BenchmarkGroup<'_, M>,
    name: &str,
) {
    let flagged =
        std::env::args().any(|arg| arg == "--sample-size" || arg.starts_with("--sample-size="));
    if flagged {
        return;
    }
    group.sample_size(LARGE_SETUP_SAMPLE_SIZE);
    eprintln!(
        "note: group `{name}` samples {LARGE_SETUP_SAMPLE_SIZE} iterations; \
         pass --sample-size N for a statistically stronger run"
    );
}

/// Creates a fresh empty tree with the given backend.
async fn setup_tree_with_backend(
    backend: Box<dyn BackendImpl>,
) -> (Instance, AccountSession, Database) {
    use eidetica::NewAccount;

    // Bootstrap "bench_user" as the initial admin. They double as the
    // benchmark's only AccountSession and (since they're the first user)
    // hold Admin on the system DBs by construction.
    let (instance, mut user) =
        Instance::create_backend(backend, NewAccount::passwordless("bench_user"))
            .await
            .expect("Benchmark setup failed");

    let key_id = user.get_default_key().expect("Failed to get default key");
    let db = user
        .create_database(Doc::new(), &key_id)
        .await
        .expect("Failed to create database");

    (instance, user, db)
}
