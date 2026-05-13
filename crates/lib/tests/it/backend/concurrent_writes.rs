//! Regression test for SQLite write-transaction contention.
//!
//! When two or more transactions opened via `pool.begin()` (BEGIN DEFERRED)
//! race to upgrade to a write transaction, SQLite returns SQLITE_BUSY /
//! SQLITE_BUSY_SNAPSHOT immediately and skips the busy_handler — so
//! `busy_timeout` cannot rescue them. The storage layer must instead start
//! write transactions as BEGIN IMMEDIATE so the write lock is acquired up
//! front and busy_timeout actually applies to contending transactions.
//!
//! This test exercises that path by spawning concurrent writers against a
//! file-backed SQLite backend (in-memory has different lock semantics).
//! Before the fix at `backend/database/sql/storage.rs`, this test failed
//! reliably with several "database is locked" errors per run; after the
//! fix it should pass with zero errors.

#![cfg(feature = "sqlite")]

use std::sync::Arc;

use eidetica::backend::BackendImpl;
use eidetica::backend::database::Sqlite;
use eidetica::entry::Entry;

const N_WORKERS: usize = 5;
const N_ENTRIES_EACH: usize = 5;

#[tokio::test]
async fn concurrent_writers_do_not_collide_on_sqlite_busy() {
    // File-backed SQLite (WAL mode) — the in-memory variant uses a different
    // pool configuration and does not reproduce the bug.
    let tmp = tempfile::tempdir().expect("create tempdir");
    let db_path = tmp.path().join("eidetica.db");
    let backend: Arc<dyn BackendImpl> = Arc::new(
        Sqlite::open(&db_path)
            .await
            .expect("open file-based SQLite backend"),
    );

    // Each worker creates a fresh tree and inserts a chain of root + N children.
    // Different trees per worker so they don't logically conflict — the goal is
    // to expose the SQLite-level write-lock race, not application-level conflicts.
    let mut handles = Vec::new();
    for w in 0..N_WORKERS {
        let backend = backend.clone();
        handles.push(tokio::spawn(async move {
            let root = Entry::root_builder().build().expect("root build");
            let root_id = root.id();
            backend
                .put_verified(root)
                .await
                .unwrap_or_else(|e| panic!("worker {w} root put failed: {e}"));

            let mut parent = root_id.clone();
            for i in 0..N_ENTRIES_EACH {
                let child = Entry::builder(root_id.clone())
                    .add_parent(parent.clone())
                    .build()
                    .expect("child build");
                let child_id = child.id();
                backend
                    .put_verified(child)
                    .await
                    .unwrap_or_else(|e| panic!("worker {w} entry {i} put failed: {e}"));
                parent = child_id;
            }
        }));
    }

    for h in handles {
        h.await.expect("worker task panic");
    }

    // Sanity: we should have N_WORKERS roots stored.
    let roots = backend.all_roots().await.expect("all_roots");
    assert_eq!(
        roots.len(),
        N_WORKERS,
        "expected {N_WORKERS} roots, found {}",
        roots.len()
    );
}
