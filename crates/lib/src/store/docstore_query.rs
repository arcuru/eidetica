//! Doc-owned point and state plans. Only committed data is delegated or cached.

use serde::{Deserialize, Serialize};

use crate::{
    Result,
    crdt::{
        CRDT, Codec, Doc,
        doc::{Path, Value},
    },
    store::query::{QueryOutcome, StoreQueryContext, StoreQueryHandler},
    store::{DocStore, ExecuteQuery, Registered, Store},
};

/// Borrowed public point query, using the same key/path semantics as Doc.
pub struct GetValue<'a>(pub &'a str);

/// Borrowed path query. Paths retain Doc's normalization and list navigation.
pub struct GetPath<'a>(pub &'a Path);

/// Complete Doc view, including tombstones and the transaction's staged delta.
pub struct GetAll;

#[derive(Serialize, Deserialize)]
enum DocQuery {
    Get { key: String },
    All,
}

impl Codec for DocQuery {
    fn encode(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }
    fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

impl DocStore {
    // Concrete plans keep Send conveniences without constraining ExecuteQuery<Q>.
    async fn committed_query<T>(
        &self,
        query: DocQuery,
        remote: impl FnOnce(&[u8]) -> Result<T>,
        local: impl FnOnce(Doc) -> Result<T>,
    ) -> Result<T> {
        if self
            .transaction()
            .store_snapshot(self.name())
            .await?
            .is_empty()
        {
            return local(Doc::default());
        }
        let outer = self
            .transaction()
            .query_outer_type(self.name(), Self::type_id())?;
        // The raw/cache plan has large bounded paging state. Keep it off the
        // caller's inline future, including callers with many convenience reads.
        Box::pin(self.transaction().query_store_or_cached_fold::<Doc, _>(
            self.name(),
            &outer,
            query.encode()?,
            super::assistance::PrivateRepresentation::opaque(
                Self::state_model().descriptor(),
                Self::type_id(),
            ),
            remote,
            local,
        ))
        .await
    }

    pub(crate) async fn get_value_plan(&self, key: &str) -> Result<Option<Value>> {
        let local = self.local_data()?;
        // Root-atomic deltas replace all historical keys. Otherwise compose only
        // the affected top-level value, not the whole state of a dirty Store.
        if let Some(local) = &local
            && local.is_atomic()
        {
            return Ok(local.get(key).cloned());
        }
        let top = key.split('.').find(|s| !s.is_empty()).unwrap_or(key);
        let staged = local.as_ref().and_then(|d| d.get(top));
        if local.as_ref().is_some_and(|d| d.is_tombstone(top)) {
            return Ok(None);
        }
        let needs_history = matches!(staged, Some(Value::Doc(d)) if !d.is_atomic());
        if staged.is_some() && !needs_history {
            return Ok(local.as_ref().and_then(|d| d.get(key)).cloned());
        }
        // Fetch the ancestor so nested staged tombstones, siblings and atomic
        // replacements have exactly the same semantics as Doc::merge.
        let historical: Option<Value> = self
            .committed_query(
                DocQuery::Get { key: top.into() },
                |bytes| Ok(serde_json::from_slice(bytes)?),
                |state| Ok(state.get(top).cloned()),
            )
            .await?;
        let mut selected = Doc::new();
        if let Some(value) = historical {
            selected.set(top, value);
        }
        if let Some(staged) = staged {
            let mut delta = Doc::new();
            delta.set(top, staged.clone());
            selected = selected.merge(&delta)?;
        }
        Ok(selected.get(key).cloned())
    }

    pub(crate) async fn committed_doc(&self) -> Result<Doc> {
        self.committed_query(DocQuery::All, Doc::decode, Ok).await
    }

    pub(crate) async fn get_all_plan(&self) -> Result<Doc> {
        let committed = self.committed_doc().await?;
        match self.local_data()? {
            Some(local) => committed.merge(&local),
            None => Ok(committed),
        }
    }
}

impl<'q> ExecuteQuery<GetValue<'q>> for DocStore {
    type Output = Option<Value>;
    async fn execute<'a>(&'a self, query: GetValue<'q>) -> Result<Self::Output>
    where
        GetValue<'q>: 'a,
    {
        self.get_value_plan(query.0).await
    }
}
impl<'q> ExecuteQuery<GetPath<'q>> for DocStore {
    type Output = Option<Value>;
    async fn execute<'a>(&'a self, query: GetPath<'q>) -> Result<Self::Output>
    where
        GetPath<'q>: 'a,
    {
        self.get_value_plan(query.0.as_str()).await
    }
}
impl ExecuteQuery<GetAll> for DocStore {
    type Output = Doc;
    async fn execute<'a>(&'a self, _: GetAll) -> Result<Doc>
    where
        GetAll: 'a,
    {
        self.get_all_plan().await
    }
}

impl StoreQueryHandler for DocStore {
    async fn handle_query(context: &StoreQueryContext<'_>, query: &[u8]) -> Result<QueryOutcome> {
        let state = context.fold::<Self>()?;
        let bytes = match DocQuery::decode(query)? {
            DocQuery::Get { key } => serde_json::to_vec(&state.get(&key))?,
            DocQuery::All => state.encode()?,
        };
        Ok(QueryOutcome::Result(bytes))
    }
}
