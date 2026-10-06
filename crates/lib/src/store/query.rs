//! Store-owned typed execution and source-bound opaque dispatch.
//!
//! Only delegated messages have a wire contract. Public queries may borrow
//! application state and need not be encoded, cloned, or sent between threads.

use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};

use crate::{
    Result, Snapshot,
    backend::{BackendImpl, VerificationStatus},
    constants::INDEX,
    crdt::{CRDT, Codec, Doc},
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

async fn validate_posture(
    engine: &dyn BackendImpl,
    request: &StoreQueryRequest,
    entries: &[Entry],
) -> Result<()> {
    for entry in entries {
        let status = engine.get_verification_status(entry.id_ref()).await?;
        if status == VerificationStatus::Failed
            || (request.source.scope == ReadScope::Verified
                && status != VerificationStatus::Verified)
        {
            return Err(StoreError::InvalidOperation {
                store: request.store.clone(),
                operation: "query".into(),
                reason: format!(
                    "source entry {} does not permit {:?}",
                    entry.id(),
                    request.source.scope
                ),
            }
            .into());
        }
    }
    Ok(())
}

pub(crate) async fn execute(
    engine: &dyn BackendImpl,
    tree: &ID,
    request: &StoreQueryRequest,
    handlers: &[QueryHandler],
) -> Result<StoreQueryReply> {
    if request.source.main.is_empty() {
        return Err(crate::transaction::TransactionError::EmptyTipsNotAllowed.into());
    }
    // The historical traversal rejects foreign and incomplete ancestry even
    // when a raw tip cache claims this is the latest boundary.
    let entries = engine
        .get_tree_from_tips(tree, request.source.main.tips())
        .await?;
    if !entries.iter().any(|e| e.id_ref() == tree && e.is_root()) {
        return Err(StoreError::InvalidOperation {
            store: request.store.clone(),
            operation: "query".into(),
            reason: "source does not reach the database root".into(),
        }
        .into());
    }
    validate_posture(engine, request, &entries).await?;
    let index_snapshot = engine
        .store_snapshot_at(tree, INDEX, &request.source.main)
        .await?;
    let index_entries = engine.store_at(tree, INDEX, &index_snapshot).await?;
    validate_posture(engine, request, &index_entries).await?;
    let index = fold_entries::<Doc>(INDEX, &index_entries)?;
    let metadata = index
        .get(&request.store)
        .and_then(|v| v.as_doc())
        .ok_or_else(|| StoreError::InvalidConfiguration {
            store: request.store.clone(),
            reason: "Store is not registered at the requested source".into(),
        })?;
    let actual = metadata
        .get("type")
        .and_then(|v| v.as_text())
        .ok_or_else(|| StoreError::InvalidConfiguration {
            store: request.store.clone(),
            reason: "source registration has no Store type".into(),
        })?;
    if actual != request.expected_type {
        return Err(StoreError::TypeMismatch {
            store: request.store.clone(),
            expected: actual.into(),
            actual: request.expected_type.clone(),
        }
        .into());
    }
    // In particular, an unlocked inner handle cannot select plaintext code
    // for encrypted:password:v0 by asserting its hidden inner type.
    let handler = handlers
        .iter()
        .find(|h| h.type_id == actual && !is_encrypted(actual));
    let outcome = if let Some(handler) = handler {
        let type_id = actual.to_string();
        let snapshot = engine
            .store_snapshot_at(tree, &request.store, &request.source.main)
            .await?;
        // Store ancestry is distinct from main ancestry. Use the same pinned
        // Store tips and canonical replay order as established Store reads.
        let entries = engine.store_at(tree, &request.store, &snapshot).await?;
        validate_posture(engine, request, &entries).await?;
        let context = StoreQueryContext {
            store: request.store.clone(),
            type_id,
            source: request.source.clone(),
            entries,
            snapshot,
        };
        (handler.dispatch)(&context, &request.query).await?
    } else {
        QueryOutcome::Unavailable
    };
    Ok(StoreQueryReply {
        source: request.source.clone(),
        outcome,
    })
}

#[cfg(test)]
mod tests;
