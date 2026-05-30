//! [`LocalBackend`]: the seam backed by a concrete in-process storage engine.

use std::sync::Arc;

use async_trait::async_trait;

use super::Backend;
use crate::{
    Result,
    backend::{BackendImpl, CacheScope, InstanceMetadata, VerificationStatus},
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

    async fn find_merge_base(&self, tree: &ID, store: &str, entry_ids: &[ID]) -> Result<ID> {
        self.0.find_merge_base(tree, store, entry_ids).await
    }

    async fn get_path_from_to(
        &self,
        tree: &ID,
        store: &str,
        from_id: &ID,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        self.0.get_path_from_to(tree, store, from_id, to_ids).await
    }

    async fn get_cached_crdt_state(
        &self,
        _tree: &ID,
        entry_id: &ID,
        store: &str,
    ) -> Result<Option<Vec<u8>>> {
        self.0
            .get_cached_crdt_state(&CacheScope::Shared, entry_id, store)
            .await
    }

    async fn cache_crdt_state(
        &self,
        _tree: &ID,
        entry_id: &ID,
        store: &str,
        state: Vec<u8>,
    ) -> Result<()> {
        self.0
            .cache_crdt_state(CacheScope::Shared, entry_id, store, state)
            .await
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
