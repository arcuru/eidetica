//! Source-bound record assistance shared by installed handlers and SDK plans.

use std::collections::BTreeMap;

#[derive(serde::Serialize, serde::Deserialize)]
/// One bounded handler record result, including whether canonical repair was used.
pub struct DerivedRecordPage {
    pub page: crate::backend::RecordPage,
    pub reconstructed: bool,
}

use crate::{
    Result,
    backend::{
        BackendError, CacheScope, RecordPage, RecordRange, StoreStateLifecycle, StoreStateRequest,
    },
    store::{
        RecordProjection, Store, assistance::PrivateRepresentation, query::StoreQueryContext,
        source::StoreSource,
    },
};

const RECORD_ENCODING: &[u8] = b"eidetica/query-record/v0\0";

pub(crate) fn binding(
    source: &StoreSource,
    representation: &PrivateRepresentation,
) -> Result<blake3::Hash> {
    // A view selects/admit-checks the Snapshot before cache access. Neither
    // the view's verification scope nor its seal changes this immutable value.
    Ok(blake3::hash(&serde_json::to_vec(&(
        &source.database,
        &source.store,
        &source.type_id,
        &source.snapshot,
        &source.registration,
        representation,
    ))?))
}

pub(crate) fn encode_record(binding: &blake3::Hash, key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut digest = blake3::Hasher::new();
    digest.update(&(key.len() as u64).to_le_bytes());
    digest.update(key);
    digest.update(value);
    [
        RECORD_ENCODING,
        binding.as_bytes(),
        digest.finalize().as_bytes(),
        value,
    ]
    .concat()
}

/// Envelope/source mismatches are hard. Only damaged derived bytes are repairable;
/// application decoding is intentionally outside this boundary.
pub(crate) fn decode_record<'a>(
    binding: &blake3::Hash,
    key: &[u8],
    value: &'a [u8],
) -> Result<Option<&'a [u8]>> {
    let Some(bytes) = value.strip_prefix(RECORD_ENCODING) else {
        return Ok(None);
    };
    let Some((identity, bytes)) = bytes.split_at_checked(32) else {
        return Ok(None);
    };
    if identity != binding.as_bytes() {
        return Err(BackendError::InvalidRawSource.into());
    }
    let Some((checksum, value)) = bytes.split_at_checked(32) else {
        return Ok(None);
    };
    let mut digest = blake3::Hasher::new();
    digest.update(&(key.len() as u64).to_le_bytes());
    digest.update(key);
    digest.update(value);
    Ok((checksum == digest.finalize().as_bytes()).then_some(value))
}

pub(crate) fn validate_page(
    page: &RecordPage,
    range: &RecordRange,
    after: Option<&[u8]>,
    limit: usize,
) -> Result<()> {
    let mut previous = after;
    if page.records.len() > limit
        || page
            .next
            .as_ref()
            .is_some_and(|next| page.records.last().is_none_or(|(key, _)| key != next))
    {
        return Err(BackendError::InvalidRawPage.into());
    }
    for (key, _) in &page.records {
        if previous.is_some_and(|previous| key.as_slice() <= previous)
            || range.start.as_ref().is_some_and(|start| key < start)
            || range.end.as_ref().is_some_and(|end| key >= end)
        {
            return Err(BackendError::InvalidRawPage.into());
        }
        previous = Some(key);
    }
    crate::store::source::encoded_size(page, crate::store::source::Limits::default().page_bytes)?;
    Ok(())
}

pub(crate) fn select(
    records: &BTreeMap<Vec<u8>, Vec<u8>>,
    range: &RecordRange,
    after: Option<&[u8]>,
    limit: usize,
) -> RecordPage {
    if limit == 0 {
        return RecordPage::default();
    }
    let mut rows = records
        .iter()
        .filter(|(key, _)| {
            after.is_none_or(|after| key.as_slice() > after)
                && range.start.as_ref().is_none_or(|start| *key >= start)
                && range.end.as_ref().is_none_or(|end| *key < end)
        })
        .take(limit.saturating_add(1))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();
    let next = (rows.len() > limit).then(|| rows[limit - 1].0.clone());
    rows.truncate(limit);
    RecordPage {
        records: rows,
        next,
    }
}

pub(crate) async fn fetch_page(
    engine: &dyn crate::backend::BackendImpl,
    view: &crate::backend::RecordView,
    range: &RecordRange,
    after: Option<&[u8]>,
    limit: usize,
) -> Result<RecordPage> {
    if after.is_none()
        && limit == 1
        && let (Some(key), Some(end)) = (&range.start, &range.end)
        && end.len() == key.len() + 1
        && end.starts_with(key)
        && end.last() == Some(&0)
    {
        let value = engine.store_state_record_get(view, key).await?;
        return Ok(RecordPage {
            records: value
                .into_iter()
                .map(|value| (key.clone(), value))
                .collect(),
            next: None,
        });
    }
    engine
        .store_state_record_scan(view, range, after, limit)
        .await
}

/// Publish only the caller's already-bound Derived target through existing
/// atomic staging. No alternate outcomes, retry engine or authoritative target.
pub(crate) async fn publish(
    engine: &dyn crate::backend::BackendImpl,
    target: StoreStateRequest,
    records: BTreeMap<Vec<u8>, Vec<u8>>,
) -> Result<()> {
    if target.lifecycle != StoreStateLifecycle::Derived {
        return Err(BackendError::InvalidRawSource.into());
    }
    crate::store::source::encoded_size(&records.iter().collect::<Vec<_>>(), 16 * 1024 * 1024)?;
    let token = engine.begin_store_state_staging(target).await?;
    let result = async {
        let mut records = records.into_iter();
        loop {
            let chunk = records
                .by_ref()
                .take(128)
                .map(|(key, value)| (key, Some(value)))
                .collect::<crate::backend::RecordMutations>();
            if chunk.is_empty() {
                break;
            }
            engine.stage_store_state_records(&token, chunk).await?;
        }
        engine.publish_store_state(token.clone()).await?;
        Ok::<_, crate::Error>(())
    }
    .await;
    if result.is_err() {
        let _ = engine.abort_store_state(token).await;
    }
    result
}

impl StoreQueryContext<'_> {
    /// Installed code may use only this validated plaintext source, fixed Shared
    /// Derived target and its own projection. No authoritative writer is exposed.
    pub async fn records<S: Store>(
        &self,
        projection: &dyn RecordProjection<S::Data>,
        range: &RecordRange,
        after: Option<&[u8]>,
        limit: usize,
        repair: bool,
    ) -> Result<DerivedRecordPage> {
        if limit > 128 {
            return Err(BackendError::SourceTooLarge.into());
        }
        if self.type_id != S::type_id() || self.type_id.starts_with("encrypted:") {
            return Err(BackendError::InvalidRawSource.into());
        }
        let representation = PrivateRepresentation {
            format: projection.descriptor(),
            configuration: S::type_id().as_bytes().to_vec(),
        };
        let identity = binding(&self.raw_source, &representation)?;
        let target = StoreStateRequest {
            database: self.raw_source.database.clone(),
            store: self.store.clone(),
            lifecycle: StoreStateLifecycle::Derived,
            scope: CacheScope::Shared,
            projection: crate::backend::ProjectionDescriptor {
                name: format!("eidetica/query-records/{}", representation.format.name),
                version: 0,
            },
            source_key: identity.as_bytes().to_vec(),
        };
        match self.engine.resolve_store_state(&target).await {
            Ok(Some(view)) => match fetch_page(self.engine, &view, range, after, limit).await {
                Ok(mut page) => {
                    validate_page(&page, range, after, limit)?;
                    let mut usable = true;
                    for (key, value) in &mut page.records {
                        match decode_record(&identity, key, value)? {
                            Some(bytes) => *value = bytes.to_vec(),
                            None => {
                                usable = false;
                                break;
                            }
                        }
                    }
                    if usable {
                        return Ok(DerivedRecordPage {
                            page,
                            reconstructed: false,
                        });
                    }
                    tracing::warn!(store = %self.store, "unusable derived record; reconstructing original source once");
                }
                Err(error) if error.is_invalid_store_state_view() => {}
                Err(error) => return Err(error),
            },
            Ok(None) => {}
            // Explicit capability refusal lets the SDK retain one bounded
            // reconstruction across all pages of the logical read.
            Err(error) if error.is_unsupported_store_state() => return Err(error),
            Err(error) => return Err(error),
        }
        if !repair {
            return Err(BackendError::InvalidStoreStateView.into());
        }
        let state = self.fold::<S>()?;
        let mut records = BTreeMap::new();
        for mutation in projection.mutations(&state)? {
            match mutation? {
                crate::backend::RecordMutation::Put { key, value } => {
                    records.insert(key, value);
                }
                crate::backend::RecordMutation::Delete { key } => {
                    records.remove(&key);
                }
            }
        }
        let page = select(&records, range, after, limit);
        validate_page(&page, range, after, limit)?;
        // Publication is optional and bounded by the admitted canonical source.
        // A concurrent/corrupt winner is never consumed as the reconstructed result.
        let framed = records
            .into_iter()
            .map(|(key, value)| {
                let framed = encode_record(&identity, &key, &value);
                (key, framed)
            })
            .collect();
        let publication = publish(self.engine, target, framed).await;
        if publication.is_err() {
            tracing::warn!(store = %self.store, "optional derived record publication failed");
        }
        Ok(DerivedRecordPage {
            page,
            reconstructed: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn damaged_record_is_not_an_application_codec_failure() {
        let identity = blake3::hash(b"source");
        let value = encode_record(&identity, b"key", b"not-json");
        assert_eq!(
            decode_record(&identity, b"key", &value).unwrap(),
            Some(b"not-json".as_slice())
        );
        assert!(
            decode_record(&identity, b"other", &value)
                .unwrap()
                .is_none()
        );
        assert!(decode_record(&blake3::hash(b"other-source"), b"key", &value).is_err());
        let mut damaged = value;
        *damaged.last_mut().unwrap() ^= 1;
        assert!(
            decode_record(&identity, b"key", &damaged)
                .unwrap()
                .is_none()
        );
    }
}
