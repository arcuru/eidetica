//! `DatabaseOps` — the storage seam `Transaction`/`Store` operate through.
//!
//! Phase 1 of the service-API raise: `Transaction` and the `Store` types
//! funnel every storage read/write through their `Database` handle.
//! `DatabaseOps` names exactly that subset so the backing implementation can
//! later be swapped (a future remote implementor answering Database-level
//! RPCs) without touching `Transaction`/`Store`.
//!
//! [`LocalDatabaseOps`] forwards verbatim to the owning `Instance`'s
//! [`Backend`](crate::instance::backend::Backend). [`RemoteDatabaseOps`]
//! translates each method to a wire RPC (DatabaseOp where available,
//! BackendOp for the rest) and serves CRDT-state caching from a two-tier
//! stack: a connection-scoped process-lifetime LRU on the
//! [`RemoteConnection`](crate::service::client::RemoteConnection), plus the
//! daemon's unified scope-keyed cache (see
//! [`crate::backend::CacheScope`]) reached via `GetCachedCrdtState` /
//! `CacheCrdtState` RPCs.
//!
//! Database-level verification (the Verified frontier,
//! `Database::get_tips`/`verified_frontier`) is deliberately *not* part of
//! this trait and stays in `Database`.

use async_trait::async_trait;

use crate::{
    Error, Instance, Result, WeakInstance,
    auth::types::SigKey,
    backend::CacheScope,
    entry::{Entry, ID},
    instance::errors::InstanceError,
    service::client::RemoteConnection,
    service::protocol::ReadScope,
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
    async fn get_store_from_tips(&self, tree: &ID, store: &str, tips: &[ID]) -> Result<Vec<Entry>>;

    /// Cached materialized CRDT state for `(entry_id, store)`, if present.
    async fn get_cached_crdt_state(&self, entry_id: &ID, store: &str) -> Result<Option<Vec<u8>>>;

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

    async fn get_store_from_tips(&self, tree: &ID, store: &str, tips: &[ID]) -> Result<Vec<Entry>> {
        let instance = self.instance()?;
        instance
            .backend()
            .get_store_from_tips(tree, store, tips)
            .await
    }

    async fn get_cached_crdt_state(&self, entry_id: &ID, store: &str) -> Result<Option<Vec<u8>>> {
        let instance = self.instance()?;
        // LocalDatabaseOps is the daemon's own in-process path; cache reads
        // come from the trusted Shared scope.
        instance
            .backend()
            .get_cached_crdt_state(&CacheScope::Shared, entry_id, store)
            .await
    }

    async fn cache_crdt_state(&self, entry_id: &ID, store: &str, state: Vec<u8>) -> Result<()> {
        let instance = self.instance()?;
        // Daemon-computed bytes are trusted: write to Shared scope so any
        // other authorized user reading the same store can dedup.
        instance
            .backend()
            .cache_crdt_state(CacheScope::Shared, entry_id, store, state)
            .await
    }

    async fn put(&self, entry: Entry) -> Result<()> {
        let instance = self.instance()?;
        instance.backend().put(entry).await
    }
}

/// Remote implementor: translates every [`DatabaseOps`] method to a wire
/// RPC through a shared [`RemoteConnection`].
///
/// CRDT-state caching is two-tiered. Tier 1 is a connection-scoped
/// process-lifetime LRU on the `RemoteConnection`, shared across every
/// `Database` handle on that connection. Tier 2 is the daemon's unified
/// CRDT-state cache (lives in `BackendImpl`, scope-keyed via
/// [`CacheScope::User`] for client uploads with fallback to
/// [`CacheScope::Shared`] for daemon-computed entries), reached via
/// `GetCachedCrdtState` / `CacheCrdtState` RPCs — durable for the
/// lifetime of the backend (in-memory for `InMemory`, on-disk for the
/// SQLx backend). `get_cached_crdt_state` checks tier 1, falls through
/// to tier 2 on miss, and on a tier-2 hit populates tier 1 so a follow-up
/// read short-circuits. `cache_crdt_state` double-writes to both tiers.
///
/// The `identity` is the session's auth identity for gating RPCs; it must
/// match the database's auth settings for the caller's key.
#[derive(Debug)]
pub struct RemoteDatabaseOps {
    conn: RemoteConnection,
    root_id: ID,
    identity: SigKey,
}

impl RemoteDatabaseOps {
    pub fn new(conn: RemoteConnection, root_id: ID, identity: SigKey) -> Self {
        Self {
            conn,
            root_id,
            identity,
        }
    }
}

#[async_trait]
impl DatabaseOps for RemoteDatabaseOps {
    async fn get(&self, id: &ID) -> Result<Entry> {
        self.conn
            .db_get_entry(self.root_id.clone(), self.identity.clone(), id.clone())
            .await
    }

    async fn get_tips(&self, _tree: &ID) -> Result<Vec<ID>> {
        self.conn
            .get_verified_tips(self.root_id.clone(), self.identity.clone())
            .await
    }

    async fn get_store_tips(&self, _tree: &ID, store: &str) -> Result<Vec<ID>> {
        let tips = self
            .conn
            .get_verified_tips(self.root_id.clone(), self.identity.clone())
            .await?;
        self.conn
            .get_store_tips_up_to_entries(
                self.root_id.clone(),
                self.identity.clone(),
                store.to_string(),
                tips,
            )
            .await
    }

    async fn get_store_tips_up_to_entries(
        &self,
        _tree: &ID,
        store: &str,
        up_to: &[ID],
    ) -> Result<Vec<ID>> {
        self.conn
            .get_store_tips_up_to_entries(
                self.root_id.clone(),
                self.identity.clone(),
                store.to_string(),
                up_to.to_vec(),
            )
            .await
    }

    async fn find_merge_base(&self, _tree: &ID, store: &str, entry_ids: &[ID]) -> Result<ID> {
        let state = self
            .conn
            .compute_merge_state(
                self.root_id.clone(),
                self.identity.clone(),
                store.to_string(),
                entry_ids.to_vec(),
            )
            .await?;
        Ok(state.merge_base)
    }

    async fn get_path_from_to(
        &self,
        _tree: &ID,
        store: &str,
        _from_id: &ID,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        let state = self
            .conn
            .compute_merge_state(
                self.root_id.clone(),
                self.identity.clone(),
                store.to_string(),
                to_ids.to_vec(),
            )
            .await?;
        Ok(state.path)
    }

    async fn get_store_from_tips(
        &self,
        _tree: &ID,
        store: &str,
        tips: &[ID],
    ) -> Result<Vec<Entry>> {
        self.conn
            .get_store_entries(
                self.root_id.clone(),
                self.identity.clone(),
                store.to_string(),
                tips.to_vec(),
                ReadScope::Verified,
            )
            .await
    }

    async fn get_cached_crdt_state(&self, entry_id: &ID, store: &str) -> Result<Option<Vec<u8>>> {
        // Tier 1: connection-shared process-lifetime LRU.
        if let Some(blob) = self.conn.cache_get(&self.root_id, entry_id, store) {
            return Ok(Some(blob));
        }
        // Tier 2: daemon-side unified cache, durable across sessions.
        let blob = self
            .conn
            .get_cached_crdt_state_remote(
                self.root_id.clone(),
                self.identity.clone(),
                store.to_string(),
                entry_id.clone(),
            )
            .await?;
        // On tier-2 hit, populate tier 1 so a follow-up read short-circuits.
        if let Some(b) = &blob {
            self.conn.cache_put(
                self.root_id.clone(),
                entry_id.clone(),
                store.to_string(),
                b.clone(),
            );
        }
        Ok(blob)
    }

    async fn cache_crdt_state(&self, entry_id: &ID, store: &str, state: Vec<u8>) -> Result<()> {
        // Tier 1: stash locally first so a same-session re-read hits without
        // any wire activity even if the tier-2 write later fails.
        self.conn.cache_put(
            self.root_id.clone(),
            entry_id.clone(),
            store.to_string(),
            state.clone(),
        );
        // Tier 2: propagate to the daemon so future sessions / new handles
        // can short-circuit the full recompute. Awaited (not fire-and-forget)
        // so wire errors propagate to the Transaction.
        self.conn
            .cache_crdt_state_remote(
                self.root_id.clone(),
                self.identity.clone(),
                store.to_string(),
                entry_id.clone(),
                state,
            )
            .await
    }

    async fn put(&self, entry: Entry) -> Result<()> {
        self.conn
            .submit_signed_entry(self.root_id.clone(), self.identity.clone(), entry)
            .await
    }
}
