//! Opaque committed-source reads. Record-shaped Store plans may keep using the
//! private physical point/page transport without hydrating this representation.

use crate::{
    Result, Transaction,
    backend::BackendError,
    crdt::{CRDT, Codec},
    store::{StoreError, assistance::PrivateRepresentation, source::StoreSource},
};

/// Shared by cache reconstruction and raw continuation, not reset by helpers.
#[derive(Default)]
pub(super) struct ReadBudget {
    pub replayed: bool,
    reconstructed: bool,
}
impl ReadBudget {
    #[allow(dead_code)] // Consumed by record-shaped Store plans.
    pub(super) fn can_reconstruct(&self) -> bool {
        !self.reconstructed
    }

    pub(super) fn reconstruct(&mut self) -> Result<()> {
        if self.reconstructed {
            return Err(BackendError::InvalidStoreStateView.into());
        }
        self.reconstructed = true;
        Ok(())
    }
}

impl Transaction {
    /// Resolve the daemon-visible type without pretending an unlocked inner
    /// Store is registered as plaintext. Unlock validated both identities.
    pub(crate) fn query_outer_type(&self, store: &str, inner: &str) -> Result<String> {
        let encryptors = self.encryptors.lock().unwrap();
        match encryptors.get(store) {
            None => Ok(inner.into()),
            Some(encryptor) => match encryptor.store_type_ids() {
                Some((outer, actual)) if actual == inner => Ok(outer.into()),
                _ => Err(StoreError::InvalidOperation {
                    store: store.into(),
                    operation: "query".into(),
                    reason: "unlock does not validate this inner Store type".into(),
                }
                .into()),
            },
        }
    }

    /// Store-owned delegation with optional opaque private reuse. Only explicit
    /// capability refusal enters the cached source fold; successful registered
    /// answers and their decode errors never request canonical payloads.
    pub async fn query_store_or_cached_fold<D: CRDT + Codec, T>(
        &self,
        store: &str,
        outer_type: &str,
        query: Vec<u8>,
        representation: PrivateRepresentation,
        remote: impl FnOnce(&[u8]) -> Result<T>,
        local: impl FnOnce(D) -> Result<T>,
    ) -> Result<T> {
        let reply = self.query_store(store, outer_type, query).await?;
        match reply.outcome {
            crate::store::query::QueryOutcome::Result(bytes) => remote(&bytes),
            crate::store::query::QueryOutcome::Unavailable => {
                let source = reply.raw_source.ok_or(BackendError::InvalidRawSource)?;
                // Mapping application values is deliberately outside the cache
                // decoder: a RowCodec error cannot trigger reconstruction.
                local(
                    self.cached_fold_raw_source::<D>(&source, representation)
                        .await?,
                )
            }
        }
    }

    /// Fold one committed source using a Store-selected opaque representation.
    /// Construct its identity with `PrivateRepresentation::opaque`, identifying D's Codec;
    /// daemon binding additionally fixes registration, outer type and source.
    /// This is not a generic point/page plan or a Table row decoding policy.
    pub async fn cached_fold_raw_source<D: CRDT + Codec>(
        &self,
        source: &StoreSource,
        representation: PrivateRepresentation,
    ) -> Result<D> {
        let mut budget = ReadBudget::default();
        if !representation
            .configuration
            .starts_with(crate::store::assistance::OPAQUE_ENCODING)
        {
            return Err(BackendError::InvalidRawSource.into());
        }
        self.validate_private_read(source, &representation)?;
        #[cfg(all(unix, feature = "service"))]
        if let Some(connection) = self.db.ops().remote_connection() {
            use crate::service::protocol::{DatabaseOp, ServiceResponse};
            let identity = self
                .db
                .auth_identity()
                .cloned()
                .or_else(|| connection.session_identity())
                .unwrap_or_default();
            let key = self.physical_record_key(&source.store, crate::store::OPAQUE_STATE_KEY)?;
            let mut end = key.clone();
            end.push(0);
            let response = connection
                .private_assistance(
                    source.database.clone(),
                    identity,
                    DatabaseOp::LookupPrivateMaterialization {
                        source: source.clone(),
                        representation: representation.clone(),
                        range: crate::backend::RecordRange {
                            start: Some(key.clone()),
                            end: Some(end),
                        },
                        after: None,
                    },
                )
                .await;
            match response {
                Ok(ServiceResponse::PrivateMaterialization(Some(page))) => {
                    // FIFO response correlation binds this page to the exact
                    // authorized lookup. Check its physical range/shape before
                    // treating only its derived payload as disposable.
                    crate::store::source::encoded_size(
                        &page,
                        crate::store::source::Limits::default().page_bytes,
                    )?;
                    if page.next.is_some()
                        || page.records.len() > 1
                        || page.records.first().is_some_and(|(k, _)| k != &key)
                    {
                        return Err(BackendError::InvalidRawPage.into());
                    }
                    if let Some((key, value)) = page.records.first() {
                        if let Some(state) =
                            self.decode_cached_state::<D>(source, &representation, key, value)?
                        {
                            return Ok(state);
                        }
                        tracing::warn!(store = %source.store, "unusable private state; reconstructing the original source once");
                    } else {
                        // An existing but incomplete opaque materialization is
                        // not a genuinely empty Store (which has encoded D).
                        tracing::warn!(store = %source.store, "private state record missing; reconstructing the original source once");
                    }
                }
                Ok(ServiceResponse::PrivateMaterialization(None)) => {}
                Err(crate::Error::Backend(error))
                    if matches!(*error, BackendError::InvalidStoreStateView) =>
                {
                    tracing::debug!(store = %source.store, "private view expired; reconstructing the original source once");
                }
                Err(error) => return Err(error),
                Ok(_) => return Err(BackendError::InvalidRawPage.into()),
            }
            budget.reconstruct()?;
            let state = self
                .fold_raw_source_with_budget::<D>(source, &mut budget)
                .await?;
            // Encoding/publication is optional after valid reconstruction. The
            // bounded transaction owner retains ambiguous exact encrypted bytes
            // across cloned handles; no unrelated new admission replaces them.
            match encode_cached_state(source, &representation, &state) {
                Ok(bytes) => {
                    let mut assistance = self.private_assistance.lock().await;
                    return Ok(self
                        .cache_private_records_best_effort(
                            &mut assistance,
                            (source.clone(), representation),
                            vec![(crate::store::OPAQUE_STATE_KEY.to_vec(), bytes)],
                            state,
                        )
                        .await);
                }
                Err(error) => tracing::warn!(%error, "optional private state encoding failed"),
            }
            return Ok(state);
        }
        budget.reconstruct()?;
        self.fold_raw_source_with_budget(source, &mut budget).await
    }

    pub(super) fn validate_private_read(
        &self,
        source: &StoreSource,
        representation: &PrivateRepresentation,
    ) -> Result<()> {
        if source.database != *self.database_id()
            || source.source != self.query_source()?
            || source.seal.is_empty()
            || representation.format.name.is_empty()
        {
            return Err(BackendError::InvalidRawSource.into());
        }
        crate::store::source::encoded_size(representation, 16 * 1024)?;
        // A mismatched persisted identity is not a cache decode failure.
        let registration = crate::crdt::Doc::decode(&source.registration)?;
        if registration.get("type").and_then(|v| v.as_text()) != Some(&source.type_id) {
            return Err(BackendError::InvalidRawSource.into());
        }
        if source.type_id.starts_with("encrypted:")
            && !self.encryptors.lock().unwrap().contains_key(&source.store)
        {
            return Err(StoreError::InvalidOperation {
                store: source.store.clone(),
                operation: "private read".into(),
                reason: "encrypted Store must be unlocked client-side".into(),
            }
            .into());
        }
        Ok(())
    }

    #[cfg(all(unix, feature = "service"))]
    fn decode_cached_state<D: Codec>(
        &self,
        source: &StoreSource,
        representation: &PrivateRepresentation,
        key: &[u8],
        value: &[u8],
    ) -> Result<Option<D>> {
        let (logical, bytes) = match self.decode_private_record(source, key, value) {
            Ok(record) => record,
            Err(error) => {
                // Unlock/source validation preceded lookup. Only these derived
                // encrypted bytes are disposable, never the key or raw source.
                tracing::warn!(%error, "private state ciphertext unusable after unlock");
                return Ok(None);
            }
        };
        if logical != crate::store::OPAQUE_STATE_KEY {
            return Err(BackendError::InvalidRawPage.into());
        }
        let Some(body) = bytes.strip_prefix(crate::store::assistance::OPAQUE_ENCODING) else {
            return Ok(None);
        };
        let Some((binding, payload)) = body.split_at_checked(32) else {
            return Ok(None);
        };
        if binding != private_state_binding(source, representation)?.as_bytes() {
            return Err(BackendError::InvalidRawSource.into());
        }
        match D::decode(payload) {
            Ok(state) => Ok(Some(state)),
            Err(error) => {
                tracing::warn!(%error, "private whole-state Codec payload unusable");
                Ok(None)
            }
        }
    }

    /// Explicit recovery after caller reauthentication, never automatic relogin.
    /// This can resume only this transaction's retained exact in-flight upload.
    #[cfg(all(unix, feature = "service"))]
    pub async fn resume_private_cache_upload(
        &self,
        authenticated: &crate::service::client::RemoteConnection,
    ) -> Result<()> {
        self.private_assistance
            .lock()
            .await
            .resume(authenticated)
            .await
    }
}

// Bind the *consumed bytes* as well as the server lookup, without changing the
// common wire. Seals are connection/daemon-local; canonical identities are not.
#[cfg(all(unix, feature = "service"))]
fn private_state_binding(
    source: &StoreSource,
    representation: &PrivateRepresentation,
) -> Result<blake3::Hash> {
    let mut source = source.clone();
    source.seal.clear();
    Ok(blake3::hash(&serde_json::to_vec(&(
        source,
        representation,
    ))?))
}

#[cfg(all(unix, feature = "service"))]
fn encode_cached_state<D: Codec>(
    source: &StoreSource,
    representation: &PrivateRepresentation,
    state: &D,
) -> Result<Vec<u8>> {
    let mut bytes = crate::store::assistance::OPAQUE_ENCODING.to_vec();
    bytes.extend_from_slice(private_state_binding(source, representation)?.as_bytes());
    bytes.extend(state.encode()?);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_read_budget_cannot_reconstruct_twice() {
        let mut budget = ReadBudget::default();
        budget.reconstruct().unwrap();
        assert!(budget.reconstruct().is_err());
    }
}
