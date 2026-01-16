//! Shared helpers for benchmark tests

use eidetica::{Database, Instance, backend::database::InMemory, crdt::Doc, user::User};

/// Creates a fresh empty tree with in-memory backend for benchmarking.
///
/// Returns (Instance, User, Database) tuple.
pub async fn setup_tree_async() -> (Instance, User, Database) {
    let backend = Box::new(InMemory::new());
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
