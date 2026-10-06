use super::*;
use crate::{
    Instance, NewUser, Transaction, backend::database::InMemory, instance::backend::LocalBackend,
    store::GetValue,
};
use crate::{backend::VerificationStatus, constants::INDEX, crdt::Doc};
use std::{rc::Rc, sync::Arc};

#[derive(Clone)]
struct Counter(u64);
impl Default for Counter {
    fn default() -> Self {
        Self(7)
    }
}
impl CRDT for Counter {
    fn merge(&self, other: &Self) -> Result<Self> {
        Ok(Self(self.0.max(other.0)))
    }
}
impl Codec for Counter {
    fn encode(&self) -> Result<Vec<u8>> {
        Ok(self.0.to_le_bytes().to_vec())
    }
    fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(Self(u64::from_le_bytes(bytes.try_into().map_err(
            |_| crate::crdt::CRDTError::DeserializationFailed {
                reason: "expected exactly eight counter bytes".into(),
            },
        )?)))
    }
}
struct CounterStore {
    name: String,
    txn: Transaction,
}
impl Registered for CounterStore {
    fn type_id() -> &'static str {
        "test:query-counter:v0"
    }
}
#[async_trait::async_trait]
impl Store for CounterStore {
    type Data = Counter;
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
impl StoreQueryHandler for CounterStore {
    async fn handle_query(context: &StoreQueryContext<'_>, query: &[u8]) -> Result<QueryOutcome> {
        if query == b"RECORDS" {
            let page = context
                .records::<Self>(
                    &CounterRecords,
                    &crate::backend::RecordRange::default(),
                    None,
                    1,
                    true,
                )
                .await?;
            return Ok(QueryOutcome::Result(serde_json::to_vec(&page.page)?));
        }
        assert_eq!(query, b"MAX\0", "only the delegated message is encoded");
        Ok(QueryOutcome::Result(context.fold::<Self>()?.encode()?))
    }
}
struct AtLeast<'q>(&'q Rc<u64>);
impl<'q> ExecuteQuery<AtLeast<'q>> for CounterStore {
    type Output = bool;
    async fn execute<'a>(&'a self, query: AtLeast<'q>) -> Result<bool>
    where
        AtLeast<'q>: 'a,
    {
        let reply = self
            .txn
            .query_store(&self.name, Self::type_id(), b"MAX\0".to_vec())
            .await?;
        let QueryOutcome::Result(bytes) = reply.outcome else {
            panic!("installed handler refused");
        };
        let committed = Counter::decode(&bytes)?;
        let state = committed.merge(&self.local_data()?.unwrap_or_default())?;
        Ok(state.0 >= **query.0)
    }
}

#[tokio::test]
async fn borrowed_non_send_query_composes_actual_binary_staging_without_cache_pollution()
-> Result<()> {
    let (instance, mut owner) =
        Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("owner")).await?;
    let db = owner
        .create_database(Doc::new(), &owner.get_default_key()?)
        .await?;
    let mut backend = LocalBackend::new(instance.require_local_engine()?);
    backend.register_store_query::<CounterStore>()?;
    let db = db.with_test_ops(Arc::new(backend));
    let write = db.new_transaction().await?;
    let store = write.get_store::<CounterStore>("counter").await?;
    write
        .update_subtree(store.name(), Counter(21).encode()?)
        .await?;
    write.commit().await?;
    let txn = db.new_transaction().await?;
    let counter = txn.get_store::<CounterStore>("counter").await?;
    let threshold = Rc::new(22);
    assert!(!counter.query(AtLeast(&threshold)).await?);
    txn.update_subtree(counter.name(), Counter(30).encode()?)
        .await?;
    assert!(counter.query(AtLeast(&threshold)).await?);
    // Delegated bytes describe committed source only, even while staging exists.
    let reply = txn
        .query_store("counter", CounterStore::type_id(), b"MAX\0".to_vec())
        .await?;
    assert_eq!(
        reply.outcome,
        QueryOutcome::Result(21u64.to_le_bytes().to_vec())
    );
    assert!(
        !db.new_transaction()
            .await?
            .get_store::<CounterStore>("counter")
            .await?
            .query(AtLeast(&threshold))
            .await?
    );
    Ok(())
}

#[tokio::test]
async fn new_query_reads_bypass_ambiguous_legacy_opaque_caches() -> Result<()> {
    let (_instance, mut owner) =
        Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("owner")).await?;
    let db = owner
        .create_database(Doc::new(), &owner.get_default_key()?)
        .await?;
    let txn = db.new_transaction().await?;
    txn.get_store::<DocStore>("docs")
        .await?
        .set("key", "canonical")
        .await?;
    let tip = txn.commit().await?;
    let backend = db.backend()?;
    let request = crate::store::state::opaque_request(
        db.root_id(),
        "docs",
        DocStore::state_model().descriptor(),
        tip.to_string().into_bytes(),
        crate::backend::CacheScope::Shared,
    );
    let mut poison = Doc::new();
    poison.set("key", "incorrect-cache");
    // Clear first because commit may already have populated the old cache.
    backend.clear_derived_store_state().await?;
    crate::store::state::publish_opaque(backend.as_ref(), request, poison.encode()?).await?;
    let docs = db
        .new_transaction()
        .await?
        .get_store::<DocStore>("docs")
        .await?;
    assert_eq!(
        docs.get("key").await?.as_text(),
        Some("incorrect-cache"),
        "poison must actually hit the legacy path"
    );
    assert_eq!(
        docs.query(GetValue("key")).await?.unwrap().as_text(),
        Some("canonical")
    );
    Ok(())
}

#[tokio::test]
async fn uncommitted_doc_query_preserves_absence_values_and_tombstones() -> Result<()> {
    let (_instance, mut owner) =
        Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("owner")).await?;
    let db = owner
        .create_database(Doc::new(), &owner.get_default_key()?)
        .await?;
    let txn = db.new_transaction().await?;
    let docs = txn.get_store::<DocStore>("new-docs").await?;
    assert!(
        matches!(docs.get("missing").await, Err(crate::Error::Store(e)) if matches!(*e, StoreError::KeyNotFound { .. }))
    );
    assert_eq!(docs.query(GetValue("missing")).await?, None);
    docs.set("key", "staged").await?;
    assert_eq!(
        docs.query(GetValue("key")).await?,
        Some(docs.get("key").await?)
    );
    assert_eq!(docs.query(GetValue("missing")).await?, None);
    docs.delete("key").await?;
    assert!(
        matches!(docs.get("key").await, Err(crate::Error::Store(e)) if matches!(*e, StoreError::KeyNotFound { .. }))
    );
    assert_eq!(docs.query(GetValue("key")).await?, None);
    // The local empty-history answer must not relax daemon registration checks.
    assert!(
        txn.query_store(
            "new-docs",
            DocStore::type_id(),
            br#"{"Get":{"key":"missing"}}"#.to_vec()
        )
        .await
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn query_follows_canonical_store_history_beyond_main_ancestry() -> Result<()> {
    for store in ["docs", INDEX] {
        let (instance, mut owner) =
            Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("owner"))
                .await?;
        let db = owner
            .create_database(Doc::new(), &owner.get_default_key()?)
            .await?;
        let txn = db.new_transaction().await?;
        txn.get_store::<DocStore>("seed")
            .await?
            .set("seed", "value")
            .await?;
        if store == "docs" {
            txn.get_store::<DocStore>("docs")
                .await?
                .set("base", "value")
                .await?;
        }
        let seed = txn.commit().await?;
        let engine = instance.require_local_engine()?;
        let parents = engine
            .store_snapshot_at(db.root_id(), store, &Snapshot::from([seed.clone()]))
            .await?;
        let mut delta = Doc::new();
        if store == INDEX {
            let mut metadata = Doc::new();
            metadata.set("type", DocStore::type_id());
            delta.set("docs", metadata);
        } else {
            delta.set("external", "store-parent");
        }
        // This fixture isolates traversal semantics. Verification labels are
        // injected, as in the source-posture tests, not an auth-proof assertion.
        let external = Entry::builder(db.root_id().clone())
            .set_parents(vec![seed.clone()])
            .set_height(20)
            .set_subtree_parents(store, parents.into_tips())
            .set_subtree_height(store, Some(20))
            .set_subtree_data(store, delta.encode()?)
            .build()?;
        let external_id = external.id();
        engine.put(external).await?;
        engine
            .update_verification_status(&external_id, VerificationStatus::Verified)
            .await?;
        let mut selected = Entry::builder(db.root_id().clone())
            .set_parents(vec![seed.clone()])
            .set_height(21)
            .set_subtree_parents(store, vec![external_id.clone()])
            .set_subtree_height(store, Some(21));
        if store == INDEX {
            let mut value = Doc::new();
            value.set("external", "store-parent");
            selected = selected.set_subtree_data("docs", value.encode()?);
        }
        let selected = selected.build()?;
        let selected_id = selected.id();
        engine.put(selected).await?;
        engine
            .update_verification_status(&selected_id, VerificationStatus::Verified)
            .await?;
        let sibling = Entry::builder(db.root_id().clone())
            .set_parents(vec![seed])
            .set_height(22)
            .build()?;
        let sibling_id = sibling.id();
        engine.put(sibling).await?;
        engine
            .update_verification_status(&sibling_id, VerificationStatus::Verified)
            .await?;
        for main in [
            Snapshot::from([selected_id.clone()]),
            Snapshot::from([selected_id.clone(), sibling_id]),
        ] {
            assert!(
                !engine
                    .get_tree_from_tips(db.root_id(), main.tips())
                    .await?
                    .iter()
                    .any(|e| e.id() == external_id)
            );
            let tips = engine.store_snapshot_at(db.root_id(), store, &main).await?;
            assert!(
                engine
                    .store_at(db.root_id(), store, &tips)
                    .await?
                    .iter()
                    .any(|e| e.id() == external_id)
            );
            let pinned = db.new_transaction_at(&main).await?;
            let docs = pinned.get_store::<DocStore>("docs").await?;
            let raw = pinned.raw_store_source("docs", DocStore::type_id()).await?;
            assert_eq!(
                pinned
                    .fold_raw_source::<Doc>(&raw)
                    .await?
                    .get("external")
                    .unwrap()
                    .as_text(),
                Some("store-parent")
            );
            assert_eq!(docs.get("external").await?.as_text(), Some("store-parent"));
            assert_eq!(
                docs.query(GetValue("external")).await?,
                Some(docs.get("external").await?),
                "canonical {store} ancestry must not be filtered by main ancestry"
            );
        }
        // Newly consumed Store ancestors retain the existing posture check.
        engine
            .update_verification_status(&external_id, VerificationStatus::Unverified)
            .await?;
        let local = LocalBackend::new(engine);
        let request = StoreQueryRequest {
            store: "docs".into(),
            expected_type: DocStore::type_id().into(),
            source: QuerySource {
                main: Snapshot::from([selected_id]),
                scope: ReadScope::Verified,
            },
            query: br#"{"Get":{"key":"external"}}"#.to_vec(),
        };
        use crate::instance::backend::Backend;
        assert!(local.query_store(db.root_id(), &request).await.is_err());
        let request = StoreQueryRequest {
            source: QuerySource {
                scope: ReadScope::AllowUnverified,
                ..request.source.clone()
            },
            ..request
        };
        assert!(matches!(
            local.query_store(db.root_id(), &request).await?.outcome,
            QueryOutcome::Result(_)
        ));
    }
    Ok(())
}

#[tokio::test]
async fn raw_sdk_unknown_non_serde_store_folds_same_source_and_composes_staging() -> Result<()> {
    let (instance, mut owner) = Instance::create_backend(
        Box::new(InMemory::new()),
        NewUser::passwordless("raw-owner"),
    )
    .await?;
    let db = owner
        .create_database(Doc::new(), &owner.get_default_key()?)
        .await?;
    let write = db.new_transaction().await?;
    write.get_store::<CounterStore>("counter").await?;
    write
        .update_subtree("counter", Counter(21).encode()?)
        .await?;
    write.commit().await?;
    let tx = db.new_transaction().await?;
    let source = tx.query_source()?;
    instance
        .require_local_engine()?
        .clear_derived_store_state()
        .await?;
    let reply = tx
        .query_store("counter", CounterStore::type_id(), b"MAX\0".to_vec())
        .await?;
    assert_eq!(reply.outcome, QueryOutcome::Unavailable);
    let later = db.new_transaction().await?;
    later
        .update_subtree("counter", Counter(99).encode()?)
        .await?;
    later.commit().await?;
    assert_eq!(
        tx.fold_raw_source::<Counter>(reply.raw_source.as_ref().unwrap())
            .await?
            .0,
        21
    );
    tx.update_subtree("counter", Counter(30).encode()?).await?;
    let committed = tx
        .query_store_or_cached_fold::<Counter, _>(
            "counter",
            CounterStore::type_id(),
            Vec::new(),
            crate::store::assistance::PrivateRepresentation::opaque(
                CounterStore::state_model().descriptor(),
                CounterStore::type_id(),
            ),
            Counter::decode,
            Ok,
        )
        .await?;
    let state = committed.merge(
        &tx.get_store::<CounterStore>("counter")
            .await?
            .local_data()?
            .unwrap(),
    )?;
    assert_eq!(committed.0, 21);
    assert_eq!(state.0, 30);
    assert_eq!(tx.query_source()?, source);
    Ok(())
}

struct CounterRecords;
impl crate::store::RecordProjection<Counter> for CounterRecords {
    fn descriptor(&self) -> crate::store::ProjectionDescriptor {
        crate::store::ProjectionDescriptor {
            name: "test/query/counter-records".into(),
            version: 0,
        }
    }
    fn mutations<'a>(
        &'a self,
        state: &'a Counter,
    ) -> Result<Box<dyn Iterator<Item = Result<crate::backend::RecordMutation>> + Send + 'a>> {
        Ok(Box::new(std::iter::once(Ok(
            crate::backend::RecordMutation::Put {
                key: b"counter".to_vec(),
                value: state.encode()?,
            },
        ))))
    }
}

async fn record_handler_behavior(engine: Box<dyn BackendImpl>) -> Result<()> {
    let (instance, mut owner) =
        Instance::create_backend(engine, NewUser::passwordless("owner")).await?;
    let db = owner
        .create_database(Doc::new(), &owner.get_default_key()?)
        .await?;
    let mut backend = LocalBackend::new(instance.require_local_engine()?);
    backend.register_store_query::<CounterStore>()?;
    let db = db.with_test_ops(Arc::new(backend));
    let write = db.new_transaction().await?;
    write.get_store::<CounterStore>("counter").await?;
    write
        .update_subtree("counter", Counter(21).encode()?)
        .await?;
    write.commit().await?;
    let txn = db.new_transaction().await?;
    for _ in 0..2 {
        let reply = txn
            .query_store("counter", CounterStore::type_id(), b"RECORDS".to_vec())
            .await?;
        let QueryOutcome::Result(bytes) = reply.outcome else {
            panic!("record handler refused")
        };
        let page: crate::backend::RecordPage = serde_json::from_slice(&bytes)?;
        assert_eq!(
            page.records,
            vec![(b"counter".to_vec(), 21u64.to_le_bytes().to_vec())]
        );
    }
    instance
        .require_local_engine()?
        .clear_derived_store_state()
        .await?;
    let reply = txn
        .query_store("counter", CounterStore::type_id(), b"RECORDS".to_vec())
        .await?;
    assert!(matches!(reply.outcome, QueryOutcome::Result(_)));
    Ok(())
}

#[tokio::test]
async fn installed_handler_source_bound_records_memory() -> Result<()> {
    record_handler_behavior(Box::new(InMemory::new())).await
}
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn installed_handler_source_bound_records_sqlite() -> Result<()> {
    record_handler_behavior(Box::new(
        crate::backend::database::Sqlite::in_memory().await?,
    ))
    .await
}
