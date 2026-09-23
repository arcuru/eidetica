//! Tests for the transaction module.

use super::*;
use serde::{Deserialize, Serialize};

use crate::{
    Instance,
    auth::crypto::generate_keypair,
    backend::database::InMemory,
    backend::{CacheScope, ProjectionDescriptor, StoreStateLifecycle, StoreStateRequest},
    crdt::{CRDT, Data},
    store::{DocStore, Registered},
};

#[derive(Clone, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct MaxCounter(u64);

impl Data for MaxCounter {}

impl CRDT for MaxCounter {
    fn merge(&self, other: &Self) -> Result<Self> {
        Ok(Self(self.0.max(other.0)))
    }
}

struct CounterStore {
    name: String,
    txn: Transaction,
}

impl Registered for CounterStore {
    fn type_id() -> &'static str {
        "test:max-counter"
    }
}

#[async_trait::async_trait]
impl Store for CounterStore {
    type Data = MaxCounter;

    async fn load(txn: &Transaction, name: String) -> Result<Self> {
        Ok(Self {
            name,
            txn: txn.clone(),
        })
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn transaction(&self) -> &Transaction {
        &self.txn
    }
}

#[tokio::test]
async fn typed_store_state_folds_custom_crdt_without_doc_conversion() {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (key, _) = generate_keypair();
    let db = Database::create(&instance, key, Doc::new()).await.unwrap();
    for value in [4, 9, 2] {
        let tx = db.new_transaction().await.unwrap();
        tx.get_store::<CounterStore>("counter").await.unwrap();
        tx.update_subtree("counter", serde_json::to_vec(&MaxCounter(value)).unwrap())
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    assert_eq!(
        db.get_store_state::<CounterStore>("counter").await.unwrap(),
        MaxCounter(9)
    );
    assert!(matches!(
        db.get_store_state::<DocStore>("counter").await.unwrap_err(),
        crate::Error::Store(ref error) if matches!(**error, StoreError::TypeMismatch { .. })
    ));
}

/// Test that corrupted auth configuration prevents commit
///
/// Validates that transactions reject changes that would corrupt the auth configuration,
/// preventing corrupted entries from entering the Merkle DAG.
#[tokio::test]
async fn test_prevent_auth_corruption() {
    let backend = InMemory::new();
    let (instance, _admin) =
        Instance::create_backend(Box::new(backend), crate::NewUser::passwordless("admin"))
            .await
            .unwrap();
    let (private_key, _) = generate_keypair();

    // Create database with the test key
    let database = Database::create(&instance, private_key, Doc::new())
        .await
        .unwrap();

    // Initial operation should work
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("data").await.unwrap();
    store.set("initial", "value").await.unwrap();
    tx.commit().await.expect("Initial operation should succeed");

    // Test corruption path 1: Set auth to wrong type (String instead of Doc)
    let tx = database.new_transaction().await.unwrap();
    let settings = tx.get_store::<DocStore>("_settings").await.unwrap();
    settings.set("auth", "corrupted_string").await.unwrap();

    let result = tx.commit().await;
    assert!(
        result.is_err(),
        "Corruption commit (wrong type) should fail immediately"
    );
    assert!(
        result.unwrap_err().is_authentication_error(),
        "Should be authentication error"
    );

    // Test corruption path 2: Delete auth (creates CRDT tombstone)
    let tx = database.new_transaction().await.unwrap();
    let settings = tx.get_store::<DocStore>("_settings").await.unwrap();
    settings.delete("auth").await.unwrap();

    let result = tx.commit().await;
    assert!(
        result.is_err(),
        "Deletion commit (tombstone) should fail immediately"
    );
    assert!(
        result.unwrap_err().is_authentication_error(),
        "Should be authentication error"
    );

    // Verify database is still functional after preventing corruption
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("data").await.unwrap();
    store
        .set("after_prevented_corruption", "value")
        .await
        .unwrap();
    tx.commit()
        .await
        .expect("Normal operations should still work");
}

#[tokio::test]
async fn opaque_non_doc_state_materializes_cold_warm_and_after_clear() {
    let backend = InMemory::new();
    let (instance, _admin) =
        Instance::create_backend(Box::new(backend), crate::NewUser::passwordless("admin"))
            .await
            .unwrap();
    let (private_key, _) = generate_keypair();
    let database = Database::create(&instance, private_key, Doc::new())
        .await
        .unwrap();

    let tx = database.new_transaction().await.unwrap();
    tx.update_subtree("counter", serde_json::to_vec(&MaxCounter(7)).unwrap())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let tx = database.new_transaction().await.unwrap();
    let cold = tx.get_full_state::<MaxCounter>("counter").await.unwrap();
    let warm = tx.get_full_state::<MaxCounter>("counter").await.unwrap();
    assert_eq!(cold, MaxCounter(7));
    assert_eq!(warm, cold);

    let backend = database.backend().unwrap();
    let entry_id = backend
        .store_snapshot(database.root_id(), "counter")
        .await
        .unwrap()
        .into_tips()
        .pop()
        .unwrap();
    let request = StoreStateRequest {
        database: database.root_id().clone(),
        store: "counter".to_string(),
        lifecycle: StoreStateLifecycle::Derived,
        scope: CacheScope::Shared,
        projection: ProjectionDescriptor {
            name: "eidetica/opaque".to_string(),
            version: 0,
        },
        source_key: entry_id.to_string().into_bytes(),
    };
    assert!(
        backend
            .resolve_store_state(&request)
            .await
            .unwrap()
            .is_some()
    );
    backend.clear_derived_store_state().await.unwrap();
    assert!(
        backend
            .resolve_store_state(&request)
            .await
            .unwrap()
            .is_none()
    );

    let rebuilt = tx.get_full_state::<MaxCounter>("counter").await.unwrap();
    assert_eq!(rebuilt, cold);
    assert!(
        backend
            .resolve_store_state(&request)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        CounterStore::state_model().descriptor().name,
        "eidetica/opaque"
    );
}
