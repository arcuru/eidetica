//! Store-owned typed execution and source-bound opaque dispatch.
//!
//! Only delegated messages have a wire contract. Public queries may borrow
//! application state and need not be encoded, cloned, or sent between threads.

use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};

use crate::{
    Result, Snapshot,
    backend::BackendImpl,
    crdt::{CRDT, Codec},
    entry::{Entry, ID},
    store::{DocStore, Registered, Store, StoreError},
};

/// The Store decides whether to execute locally, delegate, or combine results
/// with its staged changes. This interface imposes no transport bounds on Q.
pub trait ExecuteQuery<Q>: Store {
    /// The typed result of this Store-specific plan.
    type Output;

    fn execute<'a>(&'a self, query: Q) -> impl Future<Output = Result<Self::Output>> + 'a
    where
        Q: 'a;
}

/// Verification posture of the requested source, not an authorization grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReadScope {
    /// Only an all-Verified, ancestor-closed source is permitted.
    #[default]
    Verified,
    /// Explicit opt-in to Unverified entries; Failed ancestry is still refused.
    AllowUnverified,
}

/// Main-tree boundary retained by a transaction and every delegated result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuerySource {
    pub main: Snapshot,
    pub scope: ReadScope,
}

/// Common dispatch envelope. The type assertion never selects a decoder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreQueryRequest {
    pub store: String,
    pub expected_type: String,
    pub source: QuerySource,
    pub query: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueryOutcome {
    Result(Vec<u8>),
    /// Installed code is absent or explicitly lacks this capability. Malformed
    /// messages, authorization and authoritative-source failures are errors.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreQueryReply {
    pub source: QuerySource,
    pub outcome: QueryOutcome,
    /// Only explicit refusal supplies an authenticated raw source for fallback.
    pub raw_source: Option<super::source::StoreSource>,
}

impl StoreQueryReply {
    pub fn validate(&self, tree: &ID, request: &StoreQueryRequest) -> Result<()> {
        let valid = self.source == request.source
            && match (&self.outcome, &self.raw_source) {
                (QueryOutcome::Result(_), None) => true,
                (QueryOutcome::Unavailable, Some(raw)) => {
                    &raw.database == tree
                        && raw.store == request.store
                        && raw.type_id == request.expected_type
                        && raw.source == request.source
                        && !raw.seal.is_empty()
                }
                _ => false,
            };
        if !valid {
            return Err(crate::backend::BackendError::InvalidRawSource.into());
        }
        Ok(())
    }
}

/// Read-only context created after outer type and complete source validation.
/// It cannot submit writes or expose an unbound backend. No derived cache is
/// consumed here: legacy opaque cache keys do not identify the Store type.
pub struct StoreQueryContext {
    store: String,
    type_id: String,
    source: QuerySource,
    entries: Vec<Entry>,
    snapshot: Snapshot,
}

impl StoreQueryContext {
    pub fn source(&self) -> &QuerySource {
        &self.source
    }

    /// Store tips derived at the validated main-tree boundary.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub fn store(&self) -> &str {
        &self.store
    }

    /// Canonical source deltas, in Store replay order; protected bytes remain
    /// protected. These are never described as merged unknown Store state.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Fold a registered plaintext Store strictly using its installed codec.
    pub fn fold<S: Store>(&self) -> Result<S::Data> {
        if self.type_id != S::type_id() || is_encrypted(&self.type_id) {
            return Err(StoreError::TypeMismatch {
                store: self.store.clone(),
                expected: self.type_id.clone(),
                actual: S::type_id().into(),
            }
            .into());
        }
        fold_entries::<S::Data>(&self.store, &self.entries)
    }
}

/// Installed Store code, not executable code supplied by a request.
pub trait StoreQueryHandler: Store {
    fn handle_query<'a>(
        context: &'a StoreQueryContext,
        query: &'a [u8],
    ) -> impl Future<Output = Result<QueryOutcome>> + Send + 'a;
}

type Dispatch = for<'a> fn(
    &'a StoreQueryContext,
    &'a [u8],
) -> Pin<Box<dyn Future<Output = Result<QueryOutcome>> + Send + 'a>>;

#[derive(Clone)]
pub(crate) struct QueryHandler {
    type_id: &'static str,
    dispatch: Dispatch,
}

fn dispatch<'a, S: StoreQueryHandler>(
    context: &'a StoreQueryContext,
    query: &'a [u8],
) -> Pin<Box<dyn Future<Output = Result<QueryOutcome>> + Send + 'a>> {
    Box::pin(S::handle_query(context, query))
}

pub(crate) fn default_handlers() -> Vec<QueryHandler> {
    vec![QueryHandler {
        type_id: DocStore::type_id(),
        dispatch: dispatch::<DocStore>,
    }]
}

pub(crate) fn register_handler<S: StoreQueryHandler>(
    handlers: &mut Vec<QueryHandler>,
) -> Result<()> {
    let type_id = S::type_id();
    if is_encrypted(type_id) || handlers.iter().any(|h| h.type_id == type_id) {
        return Err(StoreError::InvalidConfiguration {
            store: type_id.into(),
            reason: "query handler already registered or encrypted".into(),
        }
        .into());
    }
    handlers.push(QueryHandler {
        type_id,
        dispatch: dispatch::<S>,
    });
    Ok(())
}

fn is_encrypted(type_id: &str) -> bool {
    type_id.starts_with("encrypted:")
}

fn fold_entries<D: CRDT + Codec>(store: &str, entries: &[Entry]) -> Result<D> {
    let mut state = D::default();
    for entry in entries {
        // A subtree node can carry only parents (no operation payload).
        // Nonempty operation payloads still decode strictly.
        if let Ok(bytes) = entry.data(store)
            && !bytes.is_empty()
        {
            state = state.merge(&D::decode(bytes)?)?;
        }
    }
    Ok(state)
}

pub(crate) async fn execute(
    engine: &dyn BackendImpl,
    tree: &ID,
    request: &StoreQueryRequest,
    handlers: &[QueryHandler],
    sources: &super::source::Sources,
    reader: &super::source::Reader,
) -> Result<StoreQueryReply> {
    let bound = sources.resolve(engine, reader, tree, request).await?;
    let handler = handlers
        .iter()
        .find(|h| h.type_id == bound.source.type_id && !is_encrypted(h.type_id));
    let outcome = if let Some(handler) = handler {
        let context = StoreQueryContext {
            store: request.store.clone(),
            type_id: bound.source.type_id.clone(),
            source: request.source.clone(),
            entries: bound.entries,
            snapshot: bound.source.snapshot.clone(),
        };
        (handler.dispatch)(&context, &request.query).await?
    } else {
        QueryOutcome::Unavailable
    };
    let raw_source = (outcome == QueryOutcome::Unavailable).then_some(bound.source);
    let reply = StoreQueryReply {
        source: request.source.clone(),
        outcome,
        raw_source,
    };
    super::source::encoded_size(&reply, sources.limits.page_bytes.saturating_sub(128))?;
    Ok(reply)
}

#[cfg(test)]
mod tests;
