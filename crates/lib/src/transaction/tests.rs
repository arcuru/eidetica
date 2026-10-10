//! Tests for the transaction module.

use super::*;

use crate::{
    Instance,
    auth::crypto::generate_keypair,
    backend::database::InMemory,
    backend::{CacheScope, ProjectionDescriptor, StoreStateLifecycle, StoreStateRequest},
    crdt::{CRDT, Codec},
    store::{DocStore, Registered},
};

#[derive(Clone, Default, Debug, PartialEq, Eq)]
struct MaxCounter(u64);

impl Codec for MaxCounter {
    fn encode(&self) -> Result<Vec<u8>> {
        Ok(self.0.to_le_bytes().to_vec())
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        let bytes =
            bytes
                .try_into()
                .map_err(|_| crate::crdt::CRDTError::DeserializationFailed {
                    reason: "expected exactly eight counter bytes".into(),
                })?;
        Ok(Self(u64::from_le_bytes(bytes)))
    }
}

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
async fn test_docstore_set_preserves_malformed_staged_data() {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (private_key, _) = generate_keypair();
    let database = Database::create(&instance, private_key, Doc::new())
        .await
        .unwrap();
    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("data").await.unwrap();

    // Malformed staged bytes require the transaction's internal injection seam.
    let malformed = b"not JSON".to_vec();
    tx.update_subtree("data", malformed.clone()).await.unwrap();
    let set_error = store.set("name", "Alice").await.unwrap_err();
    assert!(matches!(
        set_error,
        crate::Error::Transaction(err)
            if matches!(err.as_ref(), TransactionError::StoreDeserializationFailed { store, .. } if store == "data")
    ));
    assert_eq!(
        tx.entry_builder
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .data("data")
            .unwrap(),
        &malformed
    );
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
    let store = tx.get_store::<CounterStore>("counter").await.unwrap();
    assert_eq!(store.local_data().unwrap(), None);
    tx.update_subtree("counter", MaxCounter(7).encode().unwrap())
        .await
        .unwrap();
    assert_eq!(store.local_data().unwrap(), Some(MaxCounter(7)));
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

#[tokio::test]
async fn fixed_parent_subtree_read_ignores_concurrent_live_write() {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (private_key, _) = generate_keypair();
    let database = Database::create(&instance, private_key, Doc::new())
        .await
        .unwrap();

    let reader = database.new_transaction().await.unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
    *reader.snapshot_pause.lock().unwrap() = Some((entered_tx, resume_rx));
    let read_task =
        tokio::spawn(async move { reader.get_full_state::<MaxCounter>("counter").await });

    entered_rx.await.unwrap();
    let writer = database.new_transaction().await.unwrap();
    writer
        .update_subtree("counter", serde_json::to_vec(&MaxCounter(7)).unwrap())
        .await
        .unwrap();
    writer.commit().await.unwrap();
    resume_tx.send(()).unwrap();

    assert_eq!(read_task.await.unwrap().unwrap(), MaxCounter(0));
}

#[tokio::test]
async fn fixed_parent_auth_settings_read_ignores_concurrent_grant() {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (private_key, _) = generate_keypair();
    let database = Database::create(&instance, private_key, Doc::new())
        .await
        .unwrap();

    let baseline = database
        .new_transaction()
        .await
        .unwrap()
        .get_full_state::<Doc>(SETTINGS)
        .await
        .unwrap();
    let reader = database.new_transaction().await.unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
    *reader.snapshot_pause.lock().unwrap() = Some((entered_tx, resume_rx));
    let read_task = tokio::spawn(async move { reader.get_full_state::<Doc>(SETTINGS).await });

    entered_rx.await.unwrap();
    let (_, new_key) = generate_keypair();
    let writer = database.new_transaction().await.unwrap();
    writer
        .get_settings()
        .unwrap()
        .set_auth_key(
            &new_key,
            crate::auth::types::AuthKey::active(
                Some("new-key"),
                crate::auth::types::Permission::Write(1),
            ),
        )
        .await
        .unwrap();
    writer.commit().await.unwrap();
    resume_tx.send(()).unwrap();

    assert_eq!(read_task.await.unwrap().unwrap(), baseline);
}
