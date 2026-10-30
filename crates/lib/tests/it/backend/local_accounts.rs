//! Native account records must never enter the replicated Entry DAG or caches.
use std::sync::Arc;

use eidetica::backend::{BackendImpl, database::InMemory};

use super::helpers::{create_and_store_root, test_backend};

#[tokio::test]
async fn test_local_account_records_are_atomic_and_outside_entry_history() {
    let backend = test_backend().await;
    let root = create_and_store_root(&*backend).await;
    let entries = backend.get_tree(&root).await.unwrap();
    assert_eq!(backend.local_account_record("alice").await.unwrap(), None);
    assert!(backend.local_account_names().await.unwrap().is_empty());

    assert!(backend.compare_exchange_local_account_record("alice", None, b"encrypted-bundle-1").await.unwrap());
    assert!(!backend.compare_exchange_local_account_record("alice", None, b"lost-racing-insert").await.unwrap());
    assert!(!backend.compare_exchange_local_account_record("alice", Some(b"stale"), b"lost-update").await.unwrap());
    assert!(backend.compare_exchange_local_account_record("alice", Some(b"encrypted-bundle-1"), b"encrypted-bundle-2").await.unwrap());
    assert!(backend.compare_exchange_local_account_record("bob", None, b"separate-scoped-bundle").await.unwrap());
    assert_eq!(backend.local_account_record("alice").await.unwrap().unwrap(), b"encrypted-bundle-2");
    assert_eq!(backend.local_account_record("bob").await.unwrap().unwrap(), b"separate-scoped-bundle");
    assert_eq!(backend.local_account_names().await.unwrap(), ["alice", "bob"]);
    assert_eq!(backend.get_tree(&root).await.unwrap(), entries);
    assert_eq!(backend.all_roots().await.unwrap(), [root]);
    // Cache clearing and an explicit trust reset must not erase durable custody.
    backend.clear_derived_store_state().await.unwrap();
    backend.reset_local_verification().await.unwrap();
    assert_eq!(backend.local_account_record("alice").await.unwrap().unwrap(), b"encrypted-bundle-2");
}

#[tokio::test]
async fn test_local_account_record_racing_insert_has_one_winner() {
    let backend: Arc<dyn BackendImpl> = Arc::from(test_backend().await);
    let a = Arc::clone(&backend);
    let b = Arc::clone(&backend);
    let (a, b) = tokio::join!(
        a.compare_exchange_local_account_record("app", None, b"installation-key-a"),
        b.compare_exchange_local_account_record("app", None, b"installation-key-b"),
    );
    assert_ne!(a.unwrap(), b.unwrap());
    let value = backend.local_account_record("app").await.unwrap().unwrap();
    assert!(value == b"installation-key-a" || value == b"installation-key-b");
}

#[tokio::test]
async fn test_local_account_records_survive_inmemory_persistence() {
    let backend = InMemory::new();
    backend.compare_exchange_local_account_record("app", None, b"encrypted-scoped-keys").await.unwrap();
    let encoded = serde_json::to_vec(&backend).unwrap();
    let reopened: InMemory = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(reopened.local_account_record("app").await.unwrap().unwrap(), b"encrypted-scoped-keys");
    assert!(reopened.all_roots().await.unwrap().is_empty());
}

#[tokio::test]
#[cfg(feature = "sqlite")]
async fn test_local_account_records_survive_sqlite_reopen() {
    use eidetica::backend::database::Sqlite;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("local.db");
    let backend = Sqlite::open(&path).await.unwrap();
    backend.compare_exchange_local_account_record("app", None, b"encrypted-scoped-keys").await.unwrap();
    drop(backend);
    let reopened = Sqlite::open(&path).await.unwrap();
    assert_eq!(reopened.local_account_record("app").await.unwrap().unwrap(), b"encrypted-scoped-keys");
    assert!(reopened.all_roots().await.unwrap().is_empty());
    drop(reopened);
}
