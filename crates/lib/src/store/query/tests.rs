use super::*;
use crate::{
    Instance, NewUser, Transaction, backend::database::InMemory, instance::backend::LocalBackend,
    store::GetValue,
};
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
    async fn handle_query(context: &StoreQueryContext, query: &[u8]) -> Result<QueryOutcome> {
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
