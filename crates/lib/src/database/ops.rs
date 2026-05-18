//! `DatabaseOps` — the storage seam `Transaction`/`Store` operate through.
//!
//! Phase 1 of the service-API raise: `Transaction` and the `Store` types
//! funnel every storage read/write through their `Database` handle.
//! `DatabaseOps` names exactly that subset so the backing implementation can
//! later be swapped (a future remote implementor answering Database-level
//! RPCs) without touching `Transaction`/`Store`.
//!
//! [`LocalDatabaseOps`] is the only implementor today and forwards verbatim
//! to the existing [`Backend`](crate::instance::backend::Backend) (via the
//! owning `Instance`), exactly as `Database::backend()?` did before the seam,
//! so introducing it is a zero-behavior-change refactor.
//!
//! Database-level verification (the Verified frontier,
//! `Database::get_tips`/`verified_frontier`) is deliberately *not* part of
//! this trait and stays in `Database`; raising it is later-phase work.

use async_trait::async_trait;

use crate::{
    Error, Instance, Result, WeakInstance,
    entry::{Entry, ID},
    instance::errors::InstanceError,
};

/// The storage operations `Transaction` and `Store` perform through their
/// `Database` handle. The method set and signatures mirror the corresponding
/// [`Backend`](crate::instance::backend::Backend) methods exactly; see the
/// module docs for the seam rationale.
#[async_trait]
pub trait DatabaseOps: Send + Sync + std::fmt::Debug {
    /// Retrieve an entry by ID.
    async fn get(&self, id: &ID) -> Result<Entry>;

    /// Raw tips of `tree` (no Verified-frontier filtering — that stays in
    /// `Database`).
    async fn get_tips(&self, tree: &ID) -> Result<Vec<ID>>;

    /// Raw tips of `store` within `tree`.
    async fn get_store_tips(&self, tree: &ID, store: &str) -> Result<Vec<ID>>;

    /// Store tips reachable up to the given main-tree entries.
    async fn get_store_tips_up_to_entries(
        &self,
        tree: &ID,
        store: &str,
        up_to: &[ID],
    ) -> Result<Vec<ID>>;

    /// Lowest common ancestor of `entry_ids` within `store`.
    async fn find_merge_base(&self, tree: &ID, store: &str, entry_ids: &[ID]) -> Result<ID>;

    /// Every entry on the path from `from_id` to each of `to_ids` within
    /// `store`.
    async fn get_path_from_to(
        &self,
        tree: &ID,
        store: &str,
        from_id: &ID,
        to_ids: &[ID],
    ) -> Result<Vec<ID>>;

    /// Every entry of `store` reachable from `tips`.
    async fn get_store_from_tips(
        &self,
        tree: &ID,
        store: &str,
        tips: &[ID],
    ) -> Result<Vec<Entry>>;

    /// Cached materialized CRDT state for `(entry_id, store)`, if present.
    async fn get_cached_crdt_state(
        &self,
        entry_id: &ID,
        store: &str,
    ) -> Result<Option<Vec<u8>>>;

    /// Cache materialized CRDT state for `(entry_id, store)`.
    async fn cache_crdt_state(&self, entry_id: &ID, store: &str, state: Vec<u8>) -> Result<()>;

    /// Persist an entry. Always stored `Unverified` (see `Backend::put`).
    async fn put(&self, entry: Entry) -> Result<()>;
}

/// Local implementor: forwards every call to the owning `Instance`'s
/// `Backend`, exactly as `Database::backend()?` did before the seam.
///
/// Holds a [`WeakInstance`] only — no `key`/`allow_unverified` — so a
/// `Database` rebuilt via `with_key`/`allow_unverified` (`..self`) keeps a
/// correct `ops` rather than carrying a stale per-handle field.
#[derive(Clone, Debug)]
pub struct LocalDatabaseOps {
    instance: WeakInstance,
}

impl LocalDatabaseOps {
    pub fn new(instance: WeakInstance) -> Self {
        Self { instance }
    }

    /// Upgrade the weak instance reference, mirroring `Database::instance()`'s
    /// error so behavior is identical to the pre-seam path.
    fn instance(&self) -> Result<Instance> {
        self.instance
            .upgrade()
            .ok_or_else(|| Error::Instance(Box::new(InstanceError::InstanceDropped)))
    }
}

#[async_trait]
impl DatabaseOps for LocalDatabaseOps {
    async fn get(&self, id: &ID) -> Result<Entry> {
        let instance = self.instance()?;
        instance.backend().get(id).await
    }

    async fn get_tips(&self, tree: &ID) -> Result<Vec<ID>> {
        let instance = self.instance()?;
        instance.backend().get_tips(tree).await
    }

    async fn get_store_tips(&self, tree: &ID, store: &str) -> Result<Vec<ID>> {
        let instance = self.instance()?;
        instance.backend().get_store_tips(tree, store).await
    }

    async fn get_store_tips_up_to_entries(
        &self,
        tree: &ID,
        store: &str,
        up_to: &[ID],
    ) -> Result<Vec<ID>> {
        let instance = self.instance()?;
        instance
            .backend()
            .get_store_tips_up_to_entries(tree, store, up_to)
            .await
    }

    async fn find_merge_base(&self, tree: &ID, store: &str, entry_ids: &[ID]) -> Result<ID> {
        let instance = self.instance()?;
        instance
            .backend()
            .find_merge_base(tree, store, entry_ids)
            .await
    }

    async fn get_path_from_to(
        &self,
        tree: &ID,
        store: &str,
        from_id: &ID,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        let instance = self.instance()?;
        instance
            .backend()
            .get_path_from_to(tree, store, from_id, to_ids)
            .await
    }

    async fn get_store_from_tips(
        &self,
        tree: &ID,
        store: &str,
        tips: &[ID],
    ) -> Result<Vec<Entry>> {
        let instance = self.instance()?;
        instance
            .backend()
            .get_store_from_tips(tree, store, tips)
            .await
    }

    async fn get_cached_crdt_state(
        &self,
        entry_id: &ID,
        store: &str,
    ) -> Result<Option<Vec<u8>>> {
        let instance = self.instance()?;
        instance
            .backend()
            .get_cached_crdt_state(entry_id, store)
            .await
    }

    async fn cache_crdt_state(&self, entry_id: &ID, store: &str, state: Vec<u8>) -> Result<()> {
        let instance = self.instance()?;
        instance
            .backend()
            .cache_crdt_state(entry_id, store, state)
            .await
    }

    async fn put(&self, entry: Entry) -> Result<()> {
        let instance = self.instance()?;
        instance.backend().put(entry).await
    }
}
