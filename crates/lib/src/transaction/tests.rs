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

#[derive(Default, Debug, PartialEq, Eq)]
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

async fn test_database() -> (Instance, Database) {
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        crate::NewAccount::passwordless("admin"),
    )
    .await
    .unwrap();
    let (private_key, _) = generate_keypair();
    let database = Database::create(&instance, private_key, Doc::new())
        .await
        .unwrap();
    (instance, database)
}

/// Test that corrupted auth configuration prevents commit
///
/// Validates that transactions reject changes that would corrupt the auth configuration,
/// preventing corrupted entries from entering the Merkle DAG.
#[tokio::test]
async fn test_prevent_auth_corruption() {
    let backend = InMemory::new();
    let (instance, _admin) =
        Instance::create_backend(Box::new(backend), crate::NewAccount::passwordless("admin"))
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
        crate::NewAccount::passwordless("admin"),
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
    let (_instance, database) = test_database().await;

    let tx = database.new_transaction().await.unwrap();
    tx.update_subtree("counter", MaxCounter(7).encode().unwrap())
        .await
        .unwrap();
    assert_eq!(
        tx.get_local_data::<MaxCounter>("counter").unwrap(),
        Some(MaxCounter(7))
    );
    let id = tx.commit().await.unwrap();
    assert_eq!(
        database
            .get_entry(id)
            .await
            .unwrap()
            .data("counter")
            .unwrap(),
        &7u64.to_le_bytes()
    );

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
    let view = backend
        .resolve_store_state(&request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        backend
            .store_state_record_get(&view, crate::store::OPAQUE_STATE_KEY)
            .await
            .unwrap(),
        Some(7u64.to_le_bytes().to_vec())
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
    let (_instance, database) = test_database().await;

    let reader = database.new_transaction().await.unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
    *reader.snapshot_pause.lock().unwrap() = Some((entered_tx, resume_rx));
    let read_task =
        tokio::spawn(async move { reader.get_full_state::<MaxCounter>("counter").await });

    entered_rx.await.unwrap();
    let writer = database.new_transaction().await.unwrap();
    writer
        .update_subtree("counter", MaxCounter(7).encode().unwrap())
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
        crate::NewAccount::passwordless("admin"),
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

#[tokio::test]
async fn test_codec_non_clone_counter_multi_tip_cold_and_warm() {
    let (_instance, database) = test_database().await;
    let tx = database.new_transaction().await.unwrap();
    tx.update_subtree("counter", MaxCounter(1).encode().unwrap())
        .await
        .unwrap();
    let base = tx.commit().await.unwrap();
    let mut tips = Vec::new();
    for value in [7, 9] {
        let tx = database
            .new_transaction_at(&Snapshot::from([base.clone()]))
            .await
            .unwrap();
        tx.update_subtree("counter", MaxCounter(value).encode().unwrap())
            .await
            .unwrap();
        tips.push(tx.commit().await.unwrap());
    }
    let tx = database
        .new_transaction_at(&Snapshot::from(tips))
        .await
        .unwrap();
    assert_eq!(
        tx.get_full_state::<MaxCounter>("counter").await.unwrap(),
        MaxCounter(9)
    );
    assert_eq!(
        tx.get_full_state::<MaxCounter>("counter").await.unwrap(),
        MaxCounter(9)
    );
    database
        .backend()
        .unwrap()
        .clear_derived_store_state()
        .await
        .unwrap();
    assert_eq!(
        tx.get_full_state::<MaxCounter>("counter").await.unwrap(),
        MaxCounter(9)
    );
}

// This encoding-only witness needs neither Clone, Serde, Default nor merging.
struct CodecOnly(u8);
impl Codec for CodecOnly {
    fn encode(&self) -> Result<Vec<u8>> {
        Ok(vec![self.0])
    }
    fn decode(bytes: &[u8]) -> Result<Self> {
        match bytes {
            [byte] => Ok(Self(*byte)),
            _ => Err(crate::crdt::CRDTError::DeserializationFailed {
                reason: "expected one byte".into(),
            }
            .into()),
        }
    }
}

#[tokio::test]
async fn test_codec_only_staged_reads() {
    let (_instance, database) = test_database().await;
    let tx = database.new_transaction().await.unwrap();
    assert!(tx.get_local_data::<CodecOnly>("bytes").unwrap().is_none());
    tx.update_subtree("bytes", CodecOnly(255).encode().unwrap())
        .await
        .unwrap();
    assert_eq!(
        tx.get_local_data::<CodecOnly>("bytes").unwrap().unwrap().0,
        255
    );
    tx.update_subtree("bytes", vec![]).await.unwrap();
    assert!(tx.get_local_data::<CodecOnly>("bytes").unwrap().is_none());
    tx.update_subtree("bytes", vec![1, 2]).await.unwrap();
    assert!(
        matches!(tx.get_local_data::<CodecOnly>("bytes"), Err(crate::Error::Transaction(error))
        if matches!(error.as_ref(), TransactionError::StoreDeserializationFailed { store, .. } if store == "bytes"))
    );
}

#[tokio::test]
async fn test_codec_staged_json_errors_unchanged() {
    let (_instance, database) = test_database().await;
    let tx = database.new_transaction().await.unwrap();
    for bytes in [b" ".as_slice(), b"not JSON", b"{\"children\":{}} trailing"] {
        tx.update_subtree("malformed", bytes.to_vec())
            .await
            .unwrap();
        let legacy = serde_json::from_slice::<Doc>(bytes)
            .unwrap_err()
            .to_string();
        match tx.get_local_data::<Doc>("malformed").unwrap_err() {
            crate::Error::Transaction(error) => match error.as_ref() {
                TransactionError::StoreDeserializationFailed { store, reason } => {
                    assert_eq!(store, "malformed");
                    assert_eq!(reason, &legacy);
                }
                error => panic!("changed transaction error: {error}"),
            },
            error => panic!("changed error variant: {error}"),
        }
    }
}
