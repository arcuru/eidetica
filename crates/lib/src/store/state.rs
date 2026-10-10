use std::collections::BTreeMap;

use crate::{
    Result,
    backend::{CacheScope, RecordView, StoreStateLifecycle, StoreStateRequest},
    entry::ID,
    instance::backend::Backend,
};

use super::ProjectionDescriptor;
use super::RecordProjection;
use crate::crdt::{Codec, Doc};

/// Reserved key for the generic opaque whole-state projection.
pub const OPAQUE_STATE_KEY: &[u8] = &[0x00];

pub(crate) fn opaque_request(
    database: &ID,
    store: &str,
    descriptor: ProjectionDescriptor,
    source_key: Vec<u8>,
    scope: CacheScope,
) -> StoreStateRequest {
    StoreStateRequest {
        database: database.clone(),
        store: store.to_string(),
        lifecycle: StoreStateLifecycle::Derived,
        scope,
        projection: descriptor,
        source_key,
    }
}

pub(crate) fn records_request(
    database: &ID,
    store: &str,
    descriptor: ProjectionDescriptor,
    source_key: Vec<u8>,
    scope: CacheScope,
) -> StoreStateRequest {
    opaque_request(database, store, descriptor, source_key, scope)
}

pub(crate) async fn publish_records<'a>(
    backend: &dyn Backend,
    request: StoreStateRequest,
    deltas: impl Iterator<Item = &'a [u8]>,
    projection: &dyn RecordProjection<Doc>,
) -> Result<RecordView> {
    let token = backend.begin_store_state_staging(request).await?;
    let result = async {
        let mut records = BTreeMap::new();
        for bytes in deltas {
            let delta = Doc::decode(bytes)?;
            projection.project_delta(&delta, &mut records)?;
        }
        records.retain(|_, value| value.is_some());
        if !records.is_empty() {
            backend.stage_store_state_records(&token, records).await?;
        }
        backend.publish_store_state(token.clone()).await
    }
    .await;
    if result.is_err() {
        let _ = backend.abort_store_state(token).await;
    }
    result
}

pub(crate) async fn load_opaque(
    backend: &dyn Backend,
    view: &RecordView,
) -> Result<Option<Vec<u8>>> {
    backend.store_state_record_get(view, OPAQUE_STATE_KEY).await
}

pub(crate) async fn publish_opaque(
    backend: &dyn Backend,
    request: StoreStateRequest,
    bytes: Vec<u8>,
) -> Result<RecordView> {
    let token = backend.begin_store_state_staging(request).await?;
    let result = async {
        backend
            .stage_store_state_records(
                &token,
                BTreeMap::from([(OPAQUE_STATE_KEY.to_vec(), Some(bytes))]),
            )
            .await?;
        backend.publish_store_state(token.clone()).await
    }
    .await;
    if result.is_err() {
        let _ = backend.abort_store_state(token).await;
    }
    result
}

/// Resolve a cached opaque record, treating a missing record substrate as a
/// miss. Only `StoreStateStorageUnsupported` maps to `None` — an old custom
/// backend predating the record API. Every other error propagates.
pub(crate) async fn resolve_cached(
    backend: &dyn Backend,
    request: &StoreStateRequest,
) -> Result<Option<RecordView>> {
    match backend.resolve_store_state(request).await {
        Err(err) if err.is_unsupported_store_state() => Ok(None),
        result => result,
    }
}

/// Load a cached opaque record through resolve, with one bounded retry.
///
/// A view minted before a clear races the load: the first load reports
/// `InvalidStoreStateView`, so resolve once more in case another materializer
/// republished meanwhile. Anything still missing afterwards is a genuine miss
/// and the caller recomputes from history — exactly two loads, no retry loop,
/// and a vanished snapshot is never read as empty.
pub(crate) async fn load_cached(
    backend: &dyn Backend,
    request: &StoreStateRequest,
) -> Result<Option<Vec<u8>>> {
    for _ in 0..2 {
        let Some(view) = resolve_cached(backend, request).await? else {
            return Ok(None);
        };
        match load_opaque(backend, &view).await {
            Err(err) if err.is_invalid_store_state_view() => continue,
            result => return result,
        }
    }
    Ok(None)
}

/// Stage and publish an opaque record, skipping backends without the record
/// substrate. An `Unsupported` at any step aborts the attempt and reports
/// "not cached" — `publish_opaque` already aborts the token on error, so no
/// staging namespace leaks. Genuine errors propagate.
pub(crate) async fn store_cached(
    backend: &dyn Backend,
    request: StoreStateRequest,
    bytes: Vec<u8>,
) -> Result<()> {
    match publish_opaque(backend, request, bytes).await {
        Err(err) if err.is_unsupported_store_state() => Ok(()),
        result => result.map(|_| ()),
    }
}
