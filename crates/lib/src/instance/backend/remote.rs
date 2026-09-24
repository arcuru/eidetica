//! [`RemoteBackend`]: the seam backed by a service connection.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;

use super::{Backend, MergeSlice};
use crate::{
    Result,
    auth::SigKey,
    backend::{
        InstanceMetadata, RecordMutations, RecordPage, RecordRange, RecordView, StagingToken,
        StoreStateRequest, VerificationStatus,
    },
    entry::{Entry, ID},
    instance::WriteSource,
    service::{client::RemoteConnection, protocol::ReadScope},
    snapshot::Snapshot,
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
/// Views onto published record sets are server-issued opaque tokens that carry no database of
/// their own, so `views` remembers which database each token was resolved
/// against and every later record read is routed back to it.
#[derive(Debug, Clone)]
pub struct RemoteBackend {
    conn: RemoteConnection,
    identity: Option<SigKey>,
    views: Arc<Mutex<BTreeMap<String, ID>>>,
    staging_sequences: Arc<tokio::sync::Mutex<HashMap<String, u64>>>,
}

impl RemoteBackend {
    pub fn new(conn: RemoteConnection, identity: Option<SigKey>) -> Self {
        Self {
            conn,
            identity,
            views: Arc::new(Mutex::new(BTreeMap::new())),
            staging_sequences: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
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
    async fn resolve_store_state(&self, request: &StoreStateRequest) -> Result<Option<RecordView>> {
        let view = self
            .conn
            .resolve_store_state(self.identity(), request.clone())
            .await?;
        if let Some(token) = &view {
            self.views
                .lock()
                .unwrap()
                .insert(token.clone(), request.database.clone());
        }
        Ok(view.map(|namespace_id| RecordView { namespace_id }))
    }

    async fn begin_store_state_staging(&self, request: StoreStateRequest) -> Result<StagingToken> {
        let namespace_id = self
            .conn
            .begin_store_state_staging(self.identity(), request.clone())
            .await?;
        self.staging_sequences
            .lock()
            .await
            .insert(namespace_id.clone(), 0);
        Ok(StagingToken {
            namespace_id,
            target: request,
        })
    }

    async fn stage_store_state_records(
        &self,
        token: &StagingToken,
        records: RecordMutations,
    ) -> Result<()> {
        // coding: one handle-wide upload lock keeps token sequences ordered;
        // use per-token locks if concurrent builds need more throughput.
        let mut sequences = self.staging_sequences.lock().await;
        let chunk_id = sequences
            .get_mut(&token.namespace_id)
            .ok_or(crate::backend::BackendError::InvalidStoreStateStagingToken)?;
        let mut chunk = RecordMutations::new();
        let mut encoded = 0usize;
        for (key, value) in records {
            let size = serde_json::to_vec(&(key.clone(), value.clone()))?.len();
            if size > crate::service::protocol::MAX_RECORD_CHUNK_BYTES as usize {
                return Err(crate::backend::BackendError::RecordTooLarge {
                    encoded_bytes: size,
                }
                .into());
            }
            if !chunk.is_empty()
                && encoded + size > crate::service::protocol::MAX_RECORD_CHUNK_BYTES as usize
            {
                self.conn
                    .stage_store_state_records(
                        token.target.database.clone(),
                        self.identity(),
                        token.namespace_id.clone(),
                        *chunk_id,
                        std::mem::take(&mut chunk),
                    )
                    .await?;
                *chunk_id += 1;
                encoded = 0;
            }
            encoded += size;
            chunk.insert(key, value);
        }
        if !chunk.is_empty() {
            self.conn
                .stage_store_state_records(
                    token.target.database.clone(),
                    self.identity(),
                    token.namespace_id.clone(),
                    *chunk_id,
                    chunk,
                )
                .await?;
            *chunk_id += 1;
        }
        Ok(())
    }

    async fn stage_store_state_ordered_chunk(
        &self,
        token: &StagingToken,
        sequence: u64,
        _digest: &[u8],
        mutations: Vec<crate::backend::RecordMutation>,
    ) -> Result<()> {
        // The service computes the digest of its encoded wire representation.
        // This adapter does not yet recover an ambiguous transport response.
        self.conn
            .stage_store_state_ordered_chunk(
                token.target.database.clone(),
                self.identity(),
                token.namespace_id.clone(),
                sequence,
                mutations,
            )
            .await
    }

    async fn publish_store_state(&self, token: StagingToken) -> Result<RecordView> {
        let database = token.target.database.clone();
        let token_id = token.namespace_id.clone();
        let namespace_id = self
            .conn
            .publish_store_state(token.target.database, self.identity(), token.namespace_id)
            .await?;
        self.staging_sequences.lock().await.remove(&token_id);
        self.views
            .lock()
            .unwrap()
            .insert(namespace_id.clone(), database);
        Ok(RecordView { namespace_id })
    }

    async fn abort_store_state(&self, token: StagingToken) -> Result<()> {
        let token_id = token.namespace_id.clone();
        self.conn
            .abort_store_state(token.target.database, self.identity(), token.namespace_id)
            .await?;
        self.staging_sequences.lock().await.remove(&token_id);
        Ok(())
    }

    async fn store_state_record_get(
        &self,
        view: &RecordView,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        let database = self
            .views
            .lock()
            .unwrap()
            .get(&view.namespace_id)
            .cloned()
            .ok_or(crate::backend::BackendError::InvalidStoreStateView)?;
        self.conn
            .store_state_record_get(
                database,
                self.identity(),
                view.namespace_id.clone(),
                key.to_vec(),
            )
            .await
    }

    async fn store_state_record_scan(
        &self,
        view: &RecordView,
        range: &RecordRange,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<RecordPage> {
        let database = self
            .views
            .lock()
            .unwrap()
            .get(&view.namespace_id)
            .cloned()
            .ok_or(crate::backend::BackendError::InvalidStoreStateView)?;
        self.conn
            .store_state_record_scan(
                database,
                self.identity(),
                view.namespace_id.clone(),
                range.clone(),
                after.map(ToOwned::to_owned),
                u32::try_from(limit).unwrap_or(u32::MAX),
                crate::service::protocol::MAX_RECORD_PAGE_BYTES,
            )
            .await
    }

    async fn clear_derived_store_state(&self) -> Result<()> {
        // Clearing is an administrative operation on daemon-owned derived
        // state. A connected client cannot safely clear published record sets used by
        // other sessions, so a client-side clear is a no-op; natural
        // descriptor/source misses rebuild through the record seam.
        Ok(())
    }

    async fn get(&self, id: &ID) -> Result<Entry> {
        // `ID::default()` is never a real database, so the pre-dispatch gate
        // waves it through; the server then gates post-fetch against the
        // fetched entry's owning tree using our identity.
        self.conn
            .db_get_entry(ID::default(), self.identity(), id.clone())
            .await
    }

    async fn snapshot(&self, tree: &ID) -> Result<Snapshot> {
        match self
            .conn
            .get_verified_tips(tree.clone(), self.identity())
            .await
        {
            Ok(snapshot) => Ok(snapshot),
            Err(e) if e.is_not_found() => Ok(Snapshot::EMPTY),
            Err(e) => Err(e),
        }
    }

    async fn store_snapshot(&self, tree: &ID, store: &str) -> Result<Snapshot> {
        let tree_tips = match self
            .conn
            .get_verified_tips(tree.clone(), self.identity())
            .await
        {
            Ok(tips) => tips,
            Err(e) if e.is_not_found() => return Ok(Snapshot::EMPTY),
            Err(e) => return Err(e),
        };
        if tree_tips.is_empty() {
            return Ok(Snapshot::EMPTY);
        }
        match self
            .conn
            .store_snapshot_at(
                tree.clone(),
                self.identity(),
                store.to_string(),
                tree_tips.into_tips(),
            )
            .await
        {
            Ok(snapshot) => Ok(snapshot),
            Err(e) if e.is_not_found() => Ok(Snapshot::EMPTY),
            Err(e) => Err(e),
        }
    }

    async fn store_snapshot_at(
        &self,
        tree: &ID,
        store: &str,
        main_snapshot: &Snapshot,
    ) -> Result<Snapshot> {
        match self
            .conn
            .store_snapshot_at(
                tree.clone(),
                self.identity(),
                store.to_string(),
                main_snapshot.tips().to_vec(),
            )
            .await
        {
            Ok(snapshot) => Ok(snapshot),
            Err(e) if e.is_not_found() => Ok(Snapshot::EMPTY),
            Err(e) => Err(e),
        }
    }

    async fn store_at(&self, tree: &ID, store: &str, snapshot: &Snapshot) -> Result<Vec<Entry>> {
        self.conn
            .get_store_entries(
                tree.clone(),
                self.identity(),
                store.to_string(),
                snapshot.tips().to_vec(),
                ReadScope::Verified,
            )
            .await
    }

    async fn compute_merge_state(
        &self,
        tree: &ID,
        store: &str,
        entry_ids: &[ID],
    ) -> Result<MergeSlice> {
        // One RPC resolves base and path against a single server-side view;
        // see the trait doc for why they must not be two round-trips.
        let state = self
            .conn
            .compute_merge_state(
                tree.clone(),
                self.identity(),
                store.to_string(),
                entry_ids.to_vec(),
            )
            .await?;
        Ok(MergeSlice {
            merge_base: state.merge_base,
            path: state.path,
        })
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
