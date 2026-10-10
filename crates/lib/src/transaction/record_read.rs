//! Record-shaped source assistance; warm reads fetch only requested physical rows.
use crate::store::query_records::encode_record;

use std::collections::BTreeMap;

use crate::{
    Result, Transaction,
    backend::{BackendError, RecordPage, RecordRange},
    crdt::{CRDT, Codec},
    entry::Entry,
    store::{
        RecordProjection,
        assistance::PrivateRepresentation,
        query::QueryOutcome,
        query_records::{binding, decode_record, select, validate_page},
    },
};

#[derive(Default)]
pub(crate) struct RecordRead {
    pub(super) budget: super::private_read::ReadBudget,
    rebuilt: Option<ReconstructedRecords>,
}

struct ReconstructedRecords {
    identity: blake3::Hash,
    records: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl Transaction {
    #[allow(clippy::too_many_arguments)] // Fixed source/query/projection boundaries.
    pub(crate) async fn query_record_page<D: CRDT + Codec>(
        &self,
        store: &str,
        outer: &str,
        query: Vec<u8>,
        representation: PrivateRepresentation,
        projection: &dyn RecordProjection<D>,
        range: &RecordRange,
        after: Option<&[u8]>,
        limit: usize,
        read: &mut RecordRead,
        decode: impl Fn(&Entry, &[u8]) -> Result<Option<D>>,
    ) -> Result<RecordPage> {
        let reply = self.query_store(store, outer, query).await?;
        if let QueryOutcome::Result(bytes) = reply.outcome {
            let result: crate::store::query_records::DerivedRecordPage =
                serde_json::from_slice(&bytes)?;
            validate_page(&result.page, range, after, limit)?;
            if result.reconstructed {
                read.budget.reconstruct()?;
            }
            return Ok(result.page);
        }
        let source = reply.raw_source.ok_or(BackendError::InvalidRawSource)?;
        self.validate_private_read(&source, &representation)?;
        let identity = binding(&source, &representation)?;
        if let Some(rebuilt) = &read.rebuilt {
            if rebuilt.identity != identity {
                return Err(BackendError::InvalidRawSource.into());
            }
            return Ok(select(&rebuilt.records, range, after, limit));
        }
        // Local client-side materializations use the same Derived staging and
        // framed physical records. They never enter daemon handler/auth caches.
        let local_engine = self.db.ops().local_engine();
        let local_target = crate::backend::StoreStateRequest {
            database: source.database.clone(),
            store: source.store.clone(),
            lifecycle: crate::backend::StoreStateLifecycle::Derived,
            scope: crate::backend::CacheScope::User(
                blake3::hash(&serde_json::to_vec(&self.db.auth_identity())?)
                    .to_hex()
                    .to_string(),
            ),
            projection: crate::backend::ProjectionDescriptor {
                name: format!(
                    "eidetica/query-records/client/{}",
                    representation.format.name
                ),
                version: 0,
            },
            source_key: identity.as_bytes().to_vec(),
        };
        if let Some(engine) = &local_engine {
            let response = match engine.resolve_store_state(&local_target).await {
                Ok(Some(view)) => crate::store::query_records::fetch_page(
                    engine.as_ref(),
                    &view,
                    range,
                    after,
                    limit,
                )
                .await
                .map(Some),
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            };
            match response {
                Ok(Some(page)) => {
                    if let Some(page) =
                        self.decode_record_page(&source, &identity, page, range, after, limit)?
                    {
                        return Ok(page);
                    }
                }
                Ok(None) => {}
                Err(error)
                    if error.is_invalid_store_state_view()
                        || error.is_unsupported_store_state() => {}
                Err(error) => return Err(error),
            }
        }
        #[cfg(all(unix, feature = "service"))]
        if let Some(connection) = self.db.ops().remote_connection() {
            use crate::service::protocol::{DatabaseOp, ServiceResponse};
            let response = connection
                .private_assistance(
                    source.database.clone(),
                    self.db
                        .auth_identity()
                        .cloned()
                        .or_else(|| connection.session_identity())
                        .unwrap_or_default(),
                    DatabaseOp::LookupPrivateMaterialization {
                        source: source.clone(),
                        representation: representation.clone(),
                        range: range.clone(),
                        after: after.map(ToOwned::to_owned),
                    },
                )
                .await;
            match response {
                Ok(ServiceResponse::PrivateMaterialization(Some(page))) => {
                    if let Some(mut page) =
                        self.decode_record_page(&source, &identity, page, range, after, 128)?
                    {
                        if page.records.len() > limit {
                            page.records.truncate(limit);
                            page.next = page.records.last().map(|(key, _)| key.clone());
                        }
                        return Ok(page);
                    }
                }
                Ok(ServiceResponse::PrivateMaterialization(None)) => {}
                Err(error)
                    if error.is_invalid_store_state_view()
                        || error.is_unsupported_store_state() => {}
                Err(error) => return Err(error),
                Ok(_) => return Err(BackendError::InvalidRawPage.into()),
            }
        }
        read.budget.reconstruct()?;
        let state = self
            .fold_raw_source_with_decoder(&source, &mut read.budget, decode)
            .await?;
        let physical = self.project_state(store, projection, &state)?;
        let page = select(&physical, range, after, limit);
        validate_page(&page, range, after, limit)?;
        let records = physical
            .iter()
            .map(|(key, value)| {
                let (logical, value) = self.decrypt_record(store, key, value)?;
                Ok((logical.clone(), encode_record(&identity, &logical, &value)))
            })
            .collect::<Result<Vec<_>>>();
        match records {
            Ok(records) => {
                if let Some(engine) = local_engine {
                    let framed = records
                        .into_iter()
                        .map(|(key, value)| {
                            Ok((
                                self.physical_record_key(store, &key)?,
                                self.encrypt_record(store, &key, &value)?,
                            ))
                        })
                        .collect::<Result<BTreeMap<_, _>>>();
                    let publication = match framed {
                        Ok(framed) => {
                            crate::store::query_records::publish(
                                engine.as_ref(),
                                local_target,
                                framed,
                            )
                            .await
                        }
                        Err(error) => Err(error),
                    };
                    if publication.is_err() {
                        tracing::warn!(store, "optional local Derived record publication failed");
                    }
                } else {
                    #[cfg(all(unix, feature = "service"))]
                    {
                        let mut assistance = self.private_assistance.lock().await;
                        self.cache_private_records_best_effort(
                            &mut assistance,
                            (source, representation),
                            records,
                            (),
                        )
                        .await;
                    }
                }
            }
            Err(_) => tracing::warn!(store, "optional private record preparation failed"),
        }
        read.rebuilt = Some(ReconstructedRecords {
            identity,
            records: physical,
        });
        Ok(page)
    }
    fn decode_record_page(
        &self,
        source: &crate::store::source::StoreSource,
        identity: &blake3::Hash,
        mut page: RecordPage,
        range: &RecordRange,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<Option<RecordPage>> {
        validate_page(&page, range, after, limit)?;
        for (physical, value) in &mut page.records {
            let Ok((logical, plaintext)) = self.decrypt_record(&source.store, physical, value)
            else {
                tracing::warn!(store = %source.store, "unusable derived record after unlock; reconstructing original source once");
                return Ok(None);
            };
            if self.physical_record_key(&source.store, &logical)? != *physical {
                return Err(BackendError::InvalidRawPage.into());
            }
            let Some(bytes) = decode_record(identity, &logical, &plaintext)? else {
                tracing::warn!(store = %source.store, "unusable derived record; reconstructing original source once");
                return Ok(None);
            };
            // Only bounded returned rows are rewrapped for the existing overlay
            // scanner. No opaque-state hydration or complete record scan.
            *value = self.encrypt_record(&source.store, &logical, bytes)?;
        }
        Ok(Some(page))
    }
}
