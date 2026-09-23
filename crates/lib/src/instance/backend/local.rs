//! [`LocalBackend`]: the seam backed by a concrete in-process storage engine.

use std::sync::Arc;

use async_trait::async_trait;

use super::{Backend, MergeSlice};
use crate::{
    Result,
    backend::{
        BackendImpl, InstanceMetadata, RecordMutations, RecordPage, RecordRange, RecordView,
        StagingToken, StoreStateRequest, VerificationStatus,
    },
    entry::{Entry, ID},
    instance::WriteSource,
    snapshot::Snapshot,
};

/// A [`Backend`] backed by a local [`BackendImpl`] (e.g. `InMemory`, SQLx).
///
/// Seam methods forward directly to the engine. The CRDT-state cache serves the
/// trusted [`CacheScope::Shared`] scope (the daemon's own in-process path); the
/// scope-keyed variants used by the service handlers reach the engine directly
/// via [`Backend::local_engine`].
#[derive(Clone)]
pub struct LocalBackend(Arc<dyn BackendImpl>);

impl LocalBackend {
    pub fn new(engine: Arc<dyn BackendImpl>) -> Self {
        Self(engine)
    }
}

impl std::fmt::Debug for LocalBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("LocalBackend").finish()
    }
}

#[async_trait]
impl Backend for LocalBackend {
    async fn resolve_store_state(&self, request: &StoreStateRequest) -> Result<Option<RecordView>> {
        self.0.resolve_store_state(request).await
    }
    async fn begin_store_state_staging(&self, request: StoreStateRequest) -> Result<StagingToken> {
        self.0.begin_store_state_staging(request).await
    }
    async fn store_state_staging_status(
        &self,
        token: &StagingToken,
    ) -> Result<Option<crate::backend::StagingStatus>> {
        self.0.store_state_staging_status(token).await
    }
    async fn store_state_staging_token(
        &self,
        id: &str,
    ) -> Result<Option<(StagingToken, crate::backend::StagingStatus)>> {
        self.0.store_state_staging_token(id).await
    }
    async fn stage_store_state_chunk(
        &self,
        token: &StagingToken,
        sequence: u64,
        digest: &[u8],
        records: RecordMutations,
    ) -> Result<()> {
        self.0
            .stage_store_state_chunk(token, sequence, digest, records)
            .await
    }
    async fn stage_store_state_ordered_chunk(
        &self,
        token: &StagingToken,
        sequence: u64,
        digest: &[u8],
        mutations: Vec<crate::backend::RecordMutation>,
    ) -> Result<()> {
        self.0
            .stage_store_state_ordered_chunk(token, sequence, digest, mutations)
            .await
    }
    async fn reclaim_expired_store_state(&self) -> Result<u64> {
        self.0.reclaim_expired_store_state().await
    }
    #[cfg(feature = "testing")]
    async fn testing_age_store_state_staging(
        &self,
        token: &StagingToken,
        seconds: i64,
    ) -> Result<()> {
        self.0.testing_age_store_state_staging(token, seconds).await
    }
    async fn stage_store_state_records(
        &self,
        token: &StagingToken,
        records: RecordMutations,
    ) -> Result<()> {
        self.0.stage_store_state_records(token, records).await
    }
    async fn publish_store_state(&self, token: StagingToken) -> Result<RecordView> {
        self.0.publish_store_state(token).await
    }
    async fn abort_store_state(&self, token: StagingToken) -> Result<()> {
        self.0.abort_store_state(token).await
    }
    async fn store_state_record_get(
        &self,
        view: &RecordView,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        self.0.store_state_record_get(view, key).await
    }
    async fn store_state_record_scan(
        &self,
        view: &RecordView,
        range: &RecordRange,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<RecordPage> {
        self.0
            .store_state_record_scan(view, range, after, limit)
            .await
    }
    async fn clear_derived_store_state(&self) -> Result<()> {
        self.0.clear_derived_store_state().await
    }
    async fn get(&self, id: &ID) -> Result<Entry> {
        self.0.get(id).await
    }

    async fn snapshot(&self, tree: &ID) -> Result<Snapshot> {
        self.0.snapshot(tree).await
    }

    async fn store_snapshot(&self, tree: &ID, store: &str) -> Result<Snapshot> {
        self.0.store_snapshot(tree, store).await
    }

    async fn store_snapshot_at(
        &self,
        tree: &ID,
        store: &str,
        main_snapshot: &Snapshot,
    ) -> Result<Snapshot> {
        self.0.store_snapshot_at(tree, store, main_snapshot).await
    }

    async fn store_at(&self, tree: &ID, store: &str, snapshot: &Snapshot) -> Result<Vec<Entry>> {
        self.0.store_at(tree, store, snapshot).await
    }

    async fn compute_merge_state(
        &self,
        tree: &ID,
        store: &str,
        entry_ids: &[ID],
    ) -> Result<MergeSlice> {
        let merge_base = self.0.find_merge_base(tree, store, entry_ids).await?;
        // Entries are immutable and parents precede children, so for a local
        // engine the path is a pure function of (base, tips) — the two
        // engine calls cannot disagree the way two remote RPCs can. With no
        // base the caller batch-fetches the full ancestry instead of
        // walking a path, so none is computed.
        let path = match &merge_base {
            Some(base) => {
                self.0
                    .get_path_from_to(tree, store, Some(base), entry_ids)
                    .await?
            }
            None => Vec::new(),
        };
        Ok(MergeSlice { merge_base, path })
    }

    async fn put(&self, entry: Entry) -> Result<()> {
        self.0.put(entry).await
    }

    async fn write_entry(
        &self,
        verification: VerificationStatus,
        entry: Entry,
        _source: WriteSource,
    ) -> Result<()> {
        let entry_id = entry.id();
        self.0.put(entry).await?;
        if verification != VerificationStatus::Unverified {
            self.0
                .update_verification_status(&entry_id, verification)
                .await?;
        }
        Ok(())
    }

    async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>> {
        self.0.get_instance_metadata().await
    }

    async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()> {
        self.0.set_instance_metadata(metadata).await
    }

    fn local_engine(&self) -> Option<Arc<dyn BackendImpl>> {
        Some(Arc::clone(&self.0))
    }
}
