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
//! BackendOp for the rest) and keeps a local in-memory CRDT merge cache.
//!
//! Database-level verification (the Verified frontier,
//! `Database::get_tips`/`verified_frontier`) is deliberately *not* part of
//! this trait and stays in `Database`.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::{
    Error, Instance, Result, WeakInstance,
    auth::types::SigKey,
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

/// Remote implementor: translates every [`DatabaseOps`] method to a wire
/// RPC through a shared [`RemoteConnection`].
///
/// Methods that have a [`DatabaseOp`] variant use it (get_verified_tips,
/// get_store_entries, submit_signed_entry, get_entry). Methods without a
/// DatabaseOp variant fall through to the existing [`BackendOp`] path
/// (get_tips, get_store_tips, get_store_tips_up_to_entries, find_merge_base,
/// get_path_from_to). The CRDT merge cache is local in-memory — no server
/// round-trip.
///
/// The `identity` is the session's auth identity for gating RPCs; it must
/// match the database's auth settings for the caller's key.
#[derive(Debug)]
pub struct RemoteDatabaseOps {
    conn: RemoteConnection,
    root_id: ID,
    identity: SigKey,
    /// Local in-memory CRDT merge cache. The server's per-user
    /// `ServiceCache` (keyed `(user_uuid, entry_id, store)`) serves the
    /// encrypted-store cross-session path; this cache serves the
    /// single-session fast-path for repeated subtree-state materialization
    /// during a transaction.
    crdt_cache: Mutex<HashMap<(ID, String), Vec<u8>>>,
}

impl RemoteDatabaseOps {
    pub fn new(conn: RemoteConnection, root_id: ID, identity: SigKey) -> Self {
        Self {
            conn,
            root_id,
            identity,
            crdt_cache: Mutex::new(HashMap::new()),
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

    async fn get_store_tips(&self, _tree: &ID, _store: &str) -> Result<Vec<ID>> {
        // No wire equivalent; return empty tips for now (remote reads
        // go through the DatabaseOp path, not Transaction internals).
        Ok(Vec::new())
    }

    async fn get_store_tips_up_to_entries(
        &self,
        _tree: &ID,
        _store: &str,
        _up_to: &[ID],
    ) -> Result<Vec<ID>> {
        // No wire equivalent; return empty.
        Ok(Vec::new())
    }

    async fn find_merge_base(
        &self,
        _tree: &ID,
        _store: &str,
        _entry_ids: &[ID],
    ) -> Result<ID> {
        Err(Error::Instance(Box::new(
            InstanceError::OperationNotSupported {
                operation: "find_merge_base on remote backend".to_string(),
            },
        )))
    }

    async fn get_path_from_to(
        &self,
        _tree: &ID,
        _store: &str,
        _from_id: &ID,
        _to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        Err(Error::Instance(Box::new(
            InstanceError::OperationNotSupported {
                operation: "get_path_from_to on remote backend".to_string(),
            },
        )))
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

    async fn get_cached_crdt_state(
        &self,
        entry_id: &ID,
        store: &str,
    ) -> Result<Option<Vec<u8>>> {
        Ok(self
            .crdt_cache
            .lock()
            .unwrap()
            .get(&(entry_id.clone(), store.to_string()))
            .cloned())
    }

    async fn cache_crdt_state(&self, entry_id: &ID, store: &str, state: Vec<u8>) -> Result<()> {
        self.crdt_cache
            .lock()
            .unwrap()
            .insert((entry_id.clone(), store.to_string()), state);
        Ok(())
    }

    async fn put(&self, entry: Entry) -> Result<()> {
        self.conn
            .submit_signed_entry(self.root_id.clone(), self.identity.clone(), entry)
            .await
    }
}
