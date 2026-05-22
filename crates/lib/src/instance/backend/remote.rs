//! [`RemoteBackend`]: the seam backed by a service connection.

use async_trait::async_trait;

use super::Backend;
use crate::{
    Result,
    auth::SigKey,
    backend::{InstanceMetadata, VerificationStatus},
    entry::{Entry, ID},
    instance::WriteSource,
    service::{client::RemoteConnection, protocol::ReadScope},
};

/// A [`Backend`] that translates every storage operation to a wire RPC over a
/// shared [`RemoteConnection`].
///
/// The only per-handle state is the acting identity: `None` means "use the
/// connection's current session identity" (the instance-level backend), and
/// `Some(k)` means "act as `k`" (a `Database` handle opened with key `k`).
/// Every clone shares the same socket and session — additional keys are
/// proof-of-possession registered into the connection's keyset by the handle
/// constructors, not by holding a separate connection.
///
/// Tree-scoped methods use the `tree` argument the caller already supplies
/// (`Transaction` passes the owning database's root), so no root is bound here.
/// `get` derives its gating tree server-side from the fetched entry, so it
/// passes `ID::default()` as the (waved-through) request root.
///
/// CRDT-state caching is two-tiered: a connection-scoped process-lifetime LRU
/// (tier 1) backed by the daemon's unified scope-keyed cache (tier 2) reached
/// via `GetCachedCrdtState` / `CacheCrdtState` RPCs.
#[derive(Debug, Clone)]
pub struct RemoteBackend {
    conn: RemoteConnection,
    identity: Option<SigKey>,
}

impl RemoteBackend {
    pub fn new(conn: RemoteConnection, identity: Option<SigKey>) -> Self {
        Self { conn, identity }
    }

    /// The acting identity for authenticated RPCs: the bound per-handle
    /// identity, else the connection's current session identity.
    fn identity(&self) -> SigKey {
        self.identity
            .clone()
            .or_else(|| self.conn.session_identity())
            .unwrap_or_default()
    }
}

#[async_trait]
impl Backend for RemoteBackend {
    async fn get(&self, id: &ID) -> Result<Entry> {
        // `ID::default()` is never a real database, so the pre-dispatch gate
        // waves it through; the server then gates post-fetch against the
        // fetched entry's owning tree using our identity.
        self.conn
            .db_get_entry(ID::default(), self.identity(), id.clone())
            .await
    }

    async fn get_tips(&self, tree: &ID) -> Result<Vec<ID>> {
        match self
            .conn
            .get_verified_tips(tree.clone(), self.identity())
            .await
        {
            Ok(tips) => Ok(tips),
            Err(e) if e.is_not_found() => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    async fn get_store_tips(&self, tree: &ID, store: &str) -> Result<Vec<ID>> {
        let tree_tips = match self
            .conn
            .get_verified_tips(tree.clone(), self.identity())
            .await
        {
            Ok(tips) => tips,
            Err(e) if e.is_not_found() => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        if tree_tips.is_empty() {
            return Ok(Vec::new());
        }
        match self
            .conn
            .get_store_tips_up_to_entries(
                tree.clone(),
                self.identity(),
                store.to_string(),
                tree_tips,
            )
            .await
        {
            Ok(tips) => Ok(tips),
            Err(e) if e.is_not_found() => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    async fn get_store_tips_up_to_entries(
        &self,
        tree: &ID,
        store: &str,
        up_to: &[ID],
    ) -> Result<Vec<ID>> {
        match self
            .conn
            .get_store_tips_up_to_entries(
                tree.clone(),
                self.identity(),
                store.to_string(),
                up_to.to_vec(),
            )
            .await
        {
            Ok(tips) => Ok(tips),
            Err(e) if e.is_not_found() => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    async fn get_store_from_tips(&self, tree: &ID, store: &str, tips: &[ID]) -> Result<Vec<Entry>> {
        self.conn
            .get_store_entries(
                tree.clone(),
                self.identity(),
                store.to_string(),
                tips.to_vec(),
                ReadScope::Verified,
            )
            .await
    }

    async fn find_merge_base(&self, tree: &ID, store: &str, entry_ids: &[ID]) -> Result<ID> {
        let state = self
            .conn
            .compute_merge_state(
                tree.clone(),
                self.identity(),
                store.to_string(),
                entry_ids.to_vec(),
            )
            .await?;
        Ok(state.merge_base)
    }

    async fn get_path_from_to(
        &self,
        tree: &ID,
        store: &str,
        _from_id: &ID,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        // The server fuses LCA + path against `to_ids` in one round-trip, so a
        // separately-supplied `from_id` LCA isn't replayed.
        let state = self
            .conn
            .compute_merge_state(
                tree.clone(),
                self.identity(),
                store.to_string(),
                to_ids.to_vec(),
            )
            .await?;
        Ok(state.path)
    }

    async fn get_cached_crdt_state(
        &self,
        tree: &ID,
        entry_id: &ID,
        store: &str,
    ) -> Result<Option<Vec<u8>>> {
        // Tier 1: connection-shared process-lifetime LRU.
        if let Some(blob) = self.conn.cache_get(tree, entry_id, store) {
            return Ok(Some(blob));
        }
        // Tier 2: daemon-side unified cache, durable across sessions.
        let blob = self
            .conn
            .get_cached_crdt_state_remote(
                tree.clone(),
                self.identity(),
                store.to_string(),
                entry_id.clone(),
            )
            .await?;
        if let Some(b) = &blob {
            self.conn
                .cache_put(tree.clone(), entry_id.clone(), store.to_string(), b.clone());
        }
        Ok(blob)
    }

    async fn cache_crdt_state(
        &self,
        tree: &ID,
        entry_id: &ID,
        store: &str,
        state: Vec<u8>,
    ) -> Result<()> {
        // Tier 1: stash locally first so a same-session re-read hits even if
        // the tier-2 write later fails.
        self.conn.cache_put(
            tree.clone(),
            entry_id.clone(),
            store.to_string(),
            state.clone(),
        );
        // Tier 2: propagate to the daemon. Awaited so wire errors surface.
        self.conn
            .cache_crdt_state_remote(
                tree.clone(),
                self.identity(),
                store.to_string(),
                entry_id.clone(),
                state,
            )
            .await
    }

    async fn put(&self, entry: Entry) -> Result<()> {
        let tree_root = entry.root().unwrap_or_else(|| entry.id());
        self.conn
            .submit_signed_entry(tree_root, self.identity(), entry)
            .await
    }

    async fn write_entry(
        &self,
        _verification: VerificationStatus,
        entry: Entry,
        _source: WriteSource,
    ) -> Result<()> {
        // The server stores the submitted entry `Unverified` and runs its own
        // verification pass; a client-asserted status is never trusted.
        let tree_root = entry.root().unwrap_or_else(|| entry.id());
        self.conn
            .submit_signed_entry(tree_root, self.identity(), entry)
            .await
    }

    async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>> {
        self.conn.get_instance_metadata().await
    }

    async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()> {
        self.conn.set_instance_metadata(metadata).await
    }

    fn remote_connection(&self) -> Option<RemoteConnection> {
        Some(self.conn.clone())
    }
}
