//! Registration errors must stop before Store construction or payload decoding.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use eidetica::{
    Error, Registered, Result, Snapshot, Store, Transaction,
    constants::INDEX,
    crdt::{Doc, doc::Value},
    store::{DocStore, StoreError},
};

use crate::helpers::setup_tree;

// Only test_get_store_registration_boundary uses these probes, including its
// positive controls, so parallel tests cannot change the counters.
static REGISTERS: AtomicUsize = AtomicUsize::new(0);
static LOADS: AtomicUsize = AtomicUsize::new(0);

fn calls() -> [usize; 2] {
    [&REGISTERS, &LOADS].map(|counter| counter.load(Ordering::SeqCst))
}

fn reset_calls() {
    for counter in [&REGISTERS, &LOADS] {
        counter.store(0, Ordering::SeqCst);
    }
}

struct ProbeStore {
    inner: DocStore,
    data: Doc,
}

impl Registered for ProbeStore {
    fn type_id() -> &'static str {
        "registration-probe:current"
    }

    fn supports_type_id(type_id: &str) -> bool {
        // A test-only multi-ID reader, not a production compatibility promise.
        matches!(
            type_id,
            "registration-probe:current" | "registration-probe:old"
        )
    }
}

#[async_trait]
impl Store for ProbeStore {
    type Data = Doc;

    async fn load(txn: &Transaction, name: String) -> Result<Self> {
        LOADS.fetch_add(1, Ordering::SeqCst);
        let inner = DocStore::load(txn, name).await?;
        // Eagerly read/decode the payload: a zero load count proves this path
        // was not reached, unlike a lazy Store that only constructs a handle.
        let data = inner.get_all().await?;
        Ok(Self { inner, data })
    }

    async fn register(txn: &Transaction, name: String) -> Result<Self> {
        REGISTERS.fetch_add(1, Ordering::SeqCst);
        let store = Self::load(txn, name).await?;
        store.set_config(Self::default_config()).await?;
        Ok(store)
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn transaction(&self) -> &Transaction {
        self.inner.transaction()
    }
}

fn metadata(fields: &[(&str, Value)]) -> Value {
    // Replacement semantics let missing-field fixtures stay missing even over
    // an earlier, valid registration in signed history.
    let mut doc = Doc::atomic();
    for (key, value) in fields {
        doc.set(*key, value.clone());
    }
    Value::Doc(doc)
}

#[tokio::test]
async fn test_get_store_registration_boundary() {
    let current = Value::Text(ProbeStore::type_id().into());
    let malformed = [
        Value::Text("not metadata".into()),
        metadata(&[]),
        metadata(&[("type", Value::Int(7))]),
        metadata(&[("type", current.clone()), ("config", "not empty".into())]),
        metadata(&[("type", current.clone()), ("settings", Value::Int(7))]),
        metadata(&[
            ("type", current.clone()),
            ("settings", metadata(&[("height_strategy", Value::Int(7))])),
        ]),
        metadata(&[
            ("type", current.clone()),
            (
                "settings",
                metadata(&[("height_strategy", "not JSON".into())]),
            ),
        ]),
        metadata(&[("type", "registration-probe:unknown".into())]),
    ];

    for (case, raw_metadata) in malformed.into_iter().enumerate() {
        let (_instance, database) = setup_tree().await;
        let tx = database.new_transaction().await.unwrap();
        tx.get_store::<DocStore>("subject")
            .await
            .unwrap()
            .set("sentinel", "original")
            .await
            .unwrap();
        let valid_id = tx.commit().await.unwrap();
        let valid_entry = database.get_entry(&valid_id).await.unwrap();
        let valid_bytes = valid_entry.canonical_bytes().unwrap();

        let tx = database.new_transaction().await.unwrap();
        // Seed raw _index metadata without going through Registry's writer.
        tx.get_store::<DocStore>(INDEX)
            .await
            .unwrap()
            .set("subject", raw_metadata.clone())
            .await
            .unwrap();
        let malformed_id = tx.commit().await.unwrap();
        let original = database.get_entry(&malformed_id).await.unwrap();
        let original_bytes = original.canonical_bytes().unwrap();
        assert_eq!(original.id(), malformed_id);
        assert!(original.auth().signature.is_some());
        assert!(
            database
                .verify_entry_signature(&malformed_id)
                .await
                .unwrap()
        );

        let tx = database.new_transaction().await.unwrap();
        let index = tx.get_index().await.unwrap();
        let expected = index.get_entry("subject").await;
        reset_calls();
        let result = tx.get_store::<ProbeStore>("subject").await;
        assert_eq!(calls(), [0, 0], "case {case}: reached Store code");
        let error = result.err().expect("invalid registration must refuse");
        match expected {
            Ok(info) => {
                // Unknown IDs parse successfully but must refuse before load.
                assert!(matches!(error, Error::Store(ref e) if matches!(e.as_ref(),
                    StoreError::TypeMismatch { store, expected, actual }
                        if store == "subject" && expected == ProbeStore::type_id()
                            && actual == &info.type_id)));
            }
            Err(expected) => {
                for error in [&expected, &error] {
                    assert!(matches!(error, Error::Store(e) if matches!(e.as_ref(),
                        StoreError::DeserializationFailed { store, .. } if store == INDEX)));
                }
                assert_eq!(error.to_string(), expected.to_string(), "case {case}");
            }
        }

        // A caller can catch the error and still commit. Parent initialization
        // may add bookkeeping, but must not stage replacement metadata or data.
        let caught_id = tx.commit().await.unwrap();
        let caught = database.get_entry(&caught_id).await.unwrap();
        assert!(caught.data(INDEX).is_err());
        assert!(caught.data("subject").is_err());
        let reopened = database.new_transaction().await.unwrap();
        assert_eq!(
            reopened
                .get_store::<DocStore>(INDEX)
                .await
                .unwrap()
                .get("subject")
                .await
                .unwrap(),
            raw_metadata,
            "case {case}: registration changed after caught error"
        );
        assert!(reopened.get_store::<ProbeStore>("subject").await.is_err());
        assert_eq!(calls(), [0, 0]);
        let payload = DocStore::load(&reopened, "subject".into()).await.unwrap();
        assert_eq!(
            payload.get("sentinel").await.unwrap().as_text(),
            Some("original")
        );

        let retained_valid = database.get_entry(&valid_id).await.unwrap();
        assert_eq!(retained_valid.id(), valid_id);
        assert_eq!(retained_valid.canonical_bytes().unwrap(), valid_bytes);
        assert!(database.verify_entry_signature(&valid_id).await.unwrap());
        let retained = database.get_entry(&malformed_id).await.unwrap();
        assert_eq!(retained.id(), malformed_id);
        assert_eq!(retained.canonical_bytes().unwrap(), original_bytes);
        assert_eq!(retained.auth(), original.auth());
        assert!(
            database
                .verify_entry_signature(&malformed_id)
                .await
                .unwrap()
        );
        let historical = database
            .new_transaction_at(&Snapshot::from([malformed_id]))
            .await
            .unwrap();
        assert_eq!(
            historical
                .get_store::<DocStore>(INDEX)
                .await
                .unwrap()
                .get("subject")
                .await
                .unwrap(),
            raw_metadata
        );
        let historical = database
            .new_transaction_at(&Snapshot::from([valid_id]))
            .await
            .unwrap();
        let store = historical.get_store::<DocStore>("subject").await.unwrap();
        assert_eq!(
            store.get("sentinel").await.unwrap().as_text(),
            Some("original")
        );
    }

    // Supported IDs and every existing empty-config normalization still load.
    // Non-Doc/non-Text config currently defaults to empty too; do not tighten it.
    for raw_metadata in [
        metadata(&[("type", current.clone())]),
        metadata(&[
            ("type", current.clone()),
            ("config", Value::Doc(Doc::new())),
        ]),
        metadata(&[("type", current.clone()), ("config", "{}".into())]),
        metadata(&[("type", current.clone()), ("config", Value::Int(7))]),
        metadata(&[("type", "registration-probe:old".into())]),
    ] {
        let (_instance, database) = setup_tree().await;
        let tx = database.new_transaction().await.unwrap();
        DocStore::load(&tx, "subject".into())
            .await
            .unwrap()
            .set("sentinel", "original")
            .await
            .unwrap();
        tx.get_store::<DocStore>(INDEX)
            .await
            .unwrap()
            .set("subject", raw_metadata.clone())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let tx = database.new_transaction().await.unwrap();
        assert!(
            tx.get_index()
                .await
                .unwrap()
                .get_entry("subject")
                .await
                .unwrap()
                .config
                .is_empty()
        );
        reset_calls();
        let store = tx.get_store::<ProbeStore>("subject").await.unwrap();
        assert_eq!(calls(), [0, 1]);
        assert_eq!(
            store.data.get("sentinel").unwrap().as_text(),
            Some("original")
        );
        tx.commit().await.unwrap();
        let tx = database.new_transaction().await.unwrap();
        assert_eq!(
            tx.get_store::<DocStore>(INDEX)
                .await
                .unwrap()
                .get("subject")
                .await
                .unwrap(),
            raw_metadata
        );
    }

    let (_instance, database) = setup_tree().await;
    let tx = database.new_transaction().await.unwrap();
    let missing = tx
        .get_index()
        .await
        .unwrap()
        .get_entry("absent")
        .await
        .unwrap_err();
    assert!(
        matches!(missing, Error::Store(ref e) if matches!(e.as_ref(),
        StoreError::KeyNotFound { store, key } if store == INDEX && key == "absent"))
    );
    reset_calls();
    tx.get_store::<ProbeStore>("absent").await.unwrap();
    assert_eq!(calls(), [1, 1], "genuine absence must register");
    tx.commit().await.unwrap();
    let tx = database.new_transaction().await.unwrap();
    assert_eq!(
        tx.get_index()
            .await
            .unwrap()
            .get_entry("absent")
            .await
            .unwrap()
            .type_id,
        ProbeStore::type_id()
    );
}
