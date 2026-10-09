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
    store::query::{StoreQueryReply, StoreQueryRequest},
};

/// A [`Backend`] backed by a local [`BackendImpl`] (e.g. `InMemory`, SQLx).
///
/// Seam methods forward directly to the engine. The CRDT-state cache serves the
/// trusted [`CacheScope::Shared`] scope (the daemon's own in-process path); the
/// scope-keyed variants used by the service handlers reach the engine directly
/// via [`Backend::local_engine`].
#[derive(Clone)]
pub struct LocalBackend {
    engine: Arc<dyn BackendImpl>,
    query_handlers: Vec<crate::store::query::QueryHandler>,
    sources: Arc<crate::store::source::Sources>,
}

impl LocalBackend {
    /// Install a plaintext Store's query capability on this local seam.
    pub fn register_store_query<S: crate::store::query::StoreQueryHandler>(
        &mut self,
    ) -> Result<()> {
        crate::store::query::register_handler::<S>(&mut self.query_handlers)
    }

    pub fn new(engine: Arc<dyn BackendImpl>) -> Self {
        Self {
            engine,
            query_handlers: crate::store::query::default_handlers(),
            sources: Arc::new(crate::store::source::Sources::default()),
        }
    }
}

impl std::fmt::Debug for LocalBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("LocalBackend").finish()
    }
}

#[async_trait]
impl Backend for LocalBackend {
    async fn query_store(&self, tree: &ID, request: &StoreQueryRequest) -> Result<StoreQueryReply> {
        let reader = crate::store::source::Reader::local();
        let _work = self
            .sources
            .admit_serialized(&reader, tree, request)
            .await?;
        crate::store::query::execute(
            self.engine.as_ref(),
            tree,
            request,
            &self.query_handlers,
            &self.sources,
            &reader,
        )
        .await
    }

    async fn store_source(
        &self,
        tree: &ID,
        store: &str,
        expected_type: &str,
        source: &crate::store::query::QuerySource,
    ) -> Result<crate::store::source::StoreSource> {
        let reader = crate::store::source::Reader::local();
        let query = StoreQueryRequest {
            store: store.into(),
            expected_type: expected_type.into(),
            source: source.clone(),
            query: Vec::new(),
        };
        let _work = self.sources.admit_serialized(&reader, tree, &query).await?;
        Ok(self
            .sources
            .resolve(self.engine.as_ref(), &reader, tree, &query)
            .await?
            .source)
    }

    async fn raw_store_page(
        &self,
        tree: &ID,
        request: &crate::store::source::RawStoreRequest,
    ) -> Result<crate::store::source::RawStorePage> {
        let reader = crate::store::source::Reader::local();
        self.sources.check_source(&reader, tree, &request.source)?;
        let query = crate::store::query::StoreQueryRequest {
            store: request.source.store.clone(),
            expected_type: request.source.type_id.clone(),
            source: request.source.source.clone(),
            query: Vec::new(),
        };
        let _work = self.sources.admit_serialized(&reader, tree, &query).await?;
        self.sources
            .page(self.engine.as_ref(), &reader, request)
            .await
    }

    async fn resolve_store_state(&self, request: &StoreStateRequest) -> Result<Option<RecordView>> {
        self.engine.resolve_store_state(request).await
    }
    async fn begin_store_state_staging(&self, request: StoreStateRequest) -> Result<StagingToken> {
        self.engine.begin_store_state_staging(request).await
    }
    async fn store_state_staging_status(
        &self,
        token: &StagingToken,
    ) -> Result<Option<crate::backend::StagingStatus>> {
        self.engine.store_state_staging_status(token).await
    }
    async fn store_state_staging_token(
        &self,
        id: &str,
    ) -> Result<Option<(StagingToken, crate::backend::StagingStatus)>> {
        self.engine.store_state_staging_token(id).await
    }
    async fn stage_store_state_chunk(
        &self,
        token: &StagingToken,
        sequence: u64,
        digest: &[u8],
        records: RecordMutations,
    ) -> Result<()> {
        self.engine
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
        self.engine
            .stage_store_state_ordered_chunk(token, sequence, digest, mutations)
            .await
    }
    async fn reclaim_expired_store_state(&self) -> Result<u64> {
        self.engine.reclaim_expired_store_state().await
    }
    async fn stage_store_state_records(
        &self,
        token: &StagingToken,
        records: RecordMutations,
    ) -> Result<()> {
        self.engine.stage_store_state_records(token, records).await
    }
    async fn publish_store_state(&self, token: StagingToken) -> Result<RecordView> {
        self.engine.publish_store_state(token).await
    }
    async fn abort_store_state(&self, token: StagingToken) -> Result<()> {
        self.engine.abort_store_state(token).await
    }
    async fn store_state_record_get(
        &self,
        view: &RecordView,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        self.engine.store_state_record_get(view, key).await
    }
    async fn store_state_record_scan(
        &self,
        view: &RecordView,
        range: &RecordRange,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<RecordPage> {
        self.engine
            .store_state_record_scan(view, range, after, limit)
            .await
    }
    async fn clear_derived_store_state(&self) -> Result<()> {
        self.engine.clear_derived_store_state().await
    }
    async fn get(&self, id: &ID) -> Result<Entry> {
        self.engine.get(id).await
    }

    async fn snapshot(&self, tree: &ID) -> Result<Snapshot> {
        self.engine.snapshot(tree).await
    }

    async fn store_snapshot(&self, tree: &ID, store: &str) -> Result<Snapshot> {
        self.engine.store_snapshot(tree, store).await
    }

    async fn store_snapshot_at(
        &self,
        tree: &ID,
        store: &str,
        main_snapshot: &Snapshot,
    ) -> Result<Snapshot> {
        self.engine
            .store_snapshot_at(tree, store, main_snapshot)
            .await
    }

    async fn store_at(&self, tree: &ID, store: &str, snapshot: &Snapshot) -> Result<Vec<Entry>> {
        self.engine.store_at(tree, store, snapshot).await
    }

    async fn compute_merge_state(
        &self,
        tree: &ID,
        store: &str,
        entry_ids: &[ID],
    ) -> Result<MergeSlice> {
        let merge_base = self.engine.find_merge_base(tree, store, entry_ids).await?;
        // Entries are immutable and parents precede children, so for a local
        // engine the path is a pure function of (base, tips) — the two
        // engine calls cannot disagree the way two remote RPCs can. With no
        // base the caller batch-fetches the full ancestry instead of
        // walking a path, so none is computed.
        let path = match &merge_base {
            Some(base) => {
                self.engine
                    .get_path_from_to(tree, store, Some(base), entry_ids)
                    .await?
            }
            None => Vec::new(),
        };
        Ok(MergeSlice { merge_base, path })
    }

    async fn put(&self, entry: Entry) -> Result<()> {
        self.engine.put(entry).await
    }

    async fn write_entry(
        &self,
        verification: VerificationStatus,
        entry: Entry,
        _source: WriteSource,
    ) -> Result<()> {
        let entry_id = entry.id();
        self.engine.put(entry).await?;
        if verification != VerificationStatus::Unverified {
            self.engine
                .update_verification_status(&entry_id, verification)
                .await?;
        }
        Ok(())
    }

    async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>> {
        self.engine.get_instance_metadata().await
    }

    async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()> {
        self.engine.set_instance_metadata(metadata).await
    }

    fn local_engine(&self) -> Option<Arc<dyn BackendImpl>> {
        Some(Arc::clone(&self.engine))
    }
}
