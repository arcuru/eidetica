//! A narrow DocStore query vocabulary, separate from its public query object.

use serde::{Deserialize, Serialize};

use crate::{
    Result,
    crdt::{Codec, doc::Value},
    store::query::{QueryOutcome, StoreQueryContext, StoreQueryHandler},
    store::{DocStore, ExecuteQuery, Registered, Store, StoreError},
};

/// Borrowed public point query. Existing DocStore convenience methods remain
/// unchanged; queries need not expose the delegated encoding to applications.
pub struct GetValue<'a>(pub &'a str);

#[derive(Serialize, Deserialize)]
enum DocQuery {
    Get { key: String },
}

impl Codec for DocQuery {
    fn encode(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

impl<'q> ExecuteQuery<GetValue<'q>> for DocStore {
    type Output = Option<Value>;

    async fn execute<'a>(&'a self, query: GetValue<'q>) -> Result<Self::Output>
    where
        GetValue<'q>: 'a,
    {
        // A narrow local answer preserves staged deletions as well as values;
        // unrelated staged keys do not force whole-Store local execution.
        if let Some(local) = self.local_data()? {
            if local.is_tombstone(query.0) {
                return Ok(None);
            }
            if let Some(value) = local.get(query.0) {
                return Ok(Some(value.clone()));
            }
        }
        let reply = self
            .transaction()
            .query_store(
                self.name(),
                Self::type_id(),
                DocQuery::Get {
                    key: query.0.into(),
                }
                .encode()?,
            )
            .await?;
        match reply.outcome {
            QueryOutcome::Result(bytes) => Ok(serde_json::from_slice(&bytes)?),
            QueryOutcome::Unavailable => Err(StoreError::InvalidOperation {
                store: self.name().into(),
                operation: "query".into(),
                reason: "Doc point-query capability unavailable at the requested source".into(),
            }
            .into()),
        }
    }
}

impl StoreQueryHandler for DocStore {
    async fn handle_query(context: &StoreQueryContext, query: &[u8]) -> Result<QueryOutcome> {
        let DocQuery::Get { key } = DocQuery::decode(query)?;
        let state = context.fold::<Self>()?;
        Ok(QueryOutcome::Result(serde_json::to_vec(&state.get(&key))?))
    }
}
