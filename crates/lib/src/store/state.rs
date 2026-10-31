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

/// Resolve an optional cached record. Only explicit unsupported/refusal or
/// invalidated-view conditions are misses; data, proof and decode errors propagate.
pub(crate) async fn resolve_cached(
    backend: &dyn Backend,
    request: &StoreStateRequest,
) -> Result<Option<RecordView>> {
    match backend.resolve_store_state(request).await {
        Err(err)
            if err.is_optional_store_state_unavailable() || err.is_invalid_store_state_view() =>
        {
            Ok(None)
        }
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
            Err(err) if err.is_optional_store_state_unavailable() => return Ok(None),
            result => return result,
        }
    }
    Ok(None)
}

/// Cache a computed value if optional admission is available. Begin precedes
/// encoding, so disabled policies avoid serialization/encryption/upload entirely.
/// Actual oversize refusal preserves the computed value, not an oversized view.
pub(crate) async fn store_cached<T, F>(
    backend: &dyn Backend,
    request: StoreStateRequest,
    value: T,
    encode: F,
) -> Result<T>
where
    T: Send,
    F: FnOnce(&T) -> Result<Vec<u8>> + Send,
{
    let token = match backend.begin_store_state_staging(request).await {
        Err(error) if error.is_optional_store_state_unavailable() => return Ok(value),
        result => result?,
    };
    let bytes = match encode(&value) {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = backend.abort_store_state(token).await;
            return Err(error);
        }
    };
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
    match result {
        Err(error)
            if error.is_optional_store_state_unavailable()
                || error.is_invalid_store_state_view() =>
        {
            Ok(value)
        }
        result => {
            result?;
            Ok(value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Instance, NewAccount,
        backend::{DerivedCachePolicy, database::InMemory},
    };

    #[tokio::test]
    async fn disabled_opaque_retention_never_calls_encoder() {
        let (instance, _) = Instance::create_backend(
            Box::new(InMemory::new_with_cache_policy(DerivedCachePolicy {
                max_namespaces: 0,
                max_bytes: 0,
            })),
            NewAccount::passwordless("admin"),
        )
        .await
        .unwrap();
        let request = opaque_request(
            &ID::from_bytes("db"),
            "store",
            ProjectionDescriptor {
                name: "test/opaque".into(),
                version: 1,
            },
            crate::Snapshot::EMPTY.cache_key_bytes(),
            CacheScope::Shared,
        );
        let value = store_cached(instance.backend().as_ref(), request, 42u64, |_| {
            panic!("disabled cache must not encode")
        })
        .await
        .unwrap();
        assert_eq!(value, 42);
    }
}
