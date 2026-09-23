//! [`RemoteBackend`]: the seam backed by a service connection.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex, RwLock},
};

use async_trait::async_trait;

use super::{Backend, MergeSlice};
use crate::{
    Result,
    auth::SigKey,
    backend::{
        BackendError, InstanceMetadata, RecordMutations, RecordPage, RecordRange, RecordView,
        StagingStatus, StagingToken, StoreStateRequest, VerificationStatus,
    },
    entry::{Entry, ID},
    instance::WriteSource,
    service::{
        client::RemoteConnection,
        protocol::{DatabaseOp, ReadScope},
    },
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
    conn: Arc<RwLock<RemoteConnection>>,
    identity: Option<SigKey>,
    views: Arc<Mutex<BTreeMap<String, ID>>>,
    uploads: Arc<tokio::sync::Mutex<HashMap<String, Upload>>>,
    #[cfg(feature = "testing")]
    stage_pause: Arc<Mutex<Option<StagePause>>>,
}

#[cfg(feature = "testing")]
type StagePause = (
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);

#[derive(Debug)]
struct Upload {
    next_sequence: u64,
    identity: SigKey,
    session_identity: Option<SigKey>,
    target: StoreStateRequest,
    // Only the ambiguous request is retained; acknowledged chunks are discarded.
    pending: VecDeque<Vec<u8>>,
    ambiguous: bool,
    publishing: bool,
    aborting: bool,
}

impl RemoteBackend {
    pub fn new(conn: RemoteConnection, identity: Option<SigKey>) -> Self {
        Self {
            conn: Arc::new(RwLock::new(conn)),
            identity,
            views: Arc::new(Mutex::new(BTreeMap::new())),
            uploads: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            #[cfg(feature = "testing")]
            stage_pause: Arc::new(Mutex::new(None)),
        }
    }

    /// Pause one high-level upload after retaining its encoded request, before
    /// sending it. Used to cancel at the ambiguous transport boundary.
    #[cfg(feature = "testing")]
    pub fn testing_pause_next_stage(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *self.stage_pause.lock().unwrap() = Some((entered_tx, release_rx));
        (entered_rx, release_tx)
    }

    /// The acting identity for authenticated RPCs: the bound per-handle
    /// identity, else the connection's current session identity.
    fn identity(&self) -> SigKey {
        self.identity
            .clone()
            .or_else(|| self.connection().session_identity())
            .unwrap_or_default()
    }

    fn connection(&self) -> RemoteConnection {
        self.conn.read().unwrap().clone()
    }

    /// Resume a lost upload on a caller-supplied authenticated connection. The
    /// daemon authorizes status and replay against the original database/user;
    /// this handle retains neither credentials nor a way to manufacture login.
    /// Unknown or terminal failures leave the original build unresolved.
    pub async fn resume_staging(
        &self,
        token: &StagingToken,
        authenticated: RemoteConnection,
    ) -> Result<Option<RecordView>> {
        let mut uploads = self.uploads.lock().await;
        let upload = uploads
            .get_mut(&token.namespace_id)
            .ok_or(BackendError::InvalidStoreStateStagingToken)?;
        if !upload.ambiguous || upload.target != token.target {
            return Err(BackendError::InvalidStoreStateStagingToken.into());
        }
        let identity = upload.identity.clone();
        let root = upload.target.database.clone();
        if authenticated.session_identity() != upload.session_identity {
            return Err(BackendError::InvalidStoreStateStagingToken.into());
        }
        let status = authenticated
            .store_state_staging_status(root.clone(), identity.clone(), token.namespace_id.clone())
            .await?;
        match status {
            Some(StagingStatus::Active) => {
                if upload.aborting {
                    authenticated
                        .abort_store_state(root.clone(), identity, token.namespace_id.clone())
                        .await?;
                    uploads.remove(&token.namespace_id);
                    *self.conn.write().unwrap() = authenticated;
                    return Ok(None);
                }
                while let Some(payload) = upload.pending.front() {
                    authenticated.send_staging_chunk(payload).await?;
                    upload.pending.pop_front();
                    upload.next_sequence += 1;
                }
                if upload.publishing {
                    let view = authenticated
                        .publish_store_state(root.clone(), identity, token.namespace_id.clone())
                        .await?;
                    self.views.lock().unwrap().insert(view.clone(), root);
                    uploads.remove(&token.namespace_id);
                    *self.conn.write().unwrap() = authenticated;
                    return Ok(Some(RecordView { namespace_id: view }));
                }
            }
            Some(StagingStatus::Aborted) => {
                if !upload.aborting {
                    return Err(BackendError::InvalidStoreStateStagingToken.into());
                }
                uploads.remove(&token.namespace_id);
                *self.conn.write().unwrap() = authenticated;
                return Ok(None);
            }
            Some(StagingStatus::Published(_)) | Some(StagingStatus::Adopted(_)) => {
                let view = authenticated
                    .publish_store_state(root.clone(), identity, token.namespace_id.clone())
                    .await?;
                self.views.lock().unwrap().insert(view.clone(), root);
                uploads.remove(&token.namespace_id);
                *self.conn.write().unwrap() = authenticated;
                return Ok(Some(RecordView { namespace_id: view }));
            }
            _ => return Err(BackendError::InvalidStoreStateStagingToken.into()),
        }
        upload.ambiguous = false;
        *self.conn.write().unwrap() = authenticated;
        Ok(None)
    }

    async fn send_upload(&self, token: &StagingToken, upload: &mut Upload) -> Result<()> {
        while let Some(payload) = upload.pending.front() {
            // Cancellation can occur during the await too; require status
            // resolution rather than silently retrying on the next call.
            upload.ambiguous = true;
            #[cfg(feature = "testing")]
            let pause = { self.stage_pause.lock().unwrap().take() };
            #[cfg(feature = "testing")]
            if let Some((entered, release)) = pause {
                let _ = entered.send(());
                let _ = release.await;
            }
            match self.connection().send_staging_chunk(payload).await {
                Ok(()) => {
                    upload.pending.pop_front();
                    upload.next_sequence += 1;
                    upload.ambiguous = false;
                }
                Err(crate::Error::Io(source)) => {
                    upload.ambiguous = true;
                    return Err(crate::Error::AmbiguousStaging {
                        token: token.namespace_id.clone(),
                        chunk: Some(upload.next_sequence),
                        source,
                    });
                }
                Err(error) => {
                    upload.pending.clear();
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Backend for RemoteBackend {
    async fn resolve_store_state(&self, request: &StoreStateRequest) -> Result<Option<RecordView>> {
        let view = self
            .connection()
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
        let mut uploads = self.uploads.lock().await;
        let identity = self.identity();
        let session_identity = self.connection().session_identity();
        let namespace_id = self
            .connection()
            .begin_store_state_staging(identity.clone(), request.clone())
            .await?;
        uploads.insert(
            namespace_id.clone(),
            Upload {
                identity,
                session_identity,
                target: request.clone(),
                next_sequence: 0,
                pending: VecDeque::new(),
                ambiguous: false,
                publishing: false,
                aborting: false,
            },
        );
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
        // coding: one handle-wide lock serializes chunks; per-token locks if throughput matters.
        let mut uploads = self.uploads.lock().await;
        let upload = uploads
            .get_mut(&token.namespace_id)
            .filter(|u| u.target == token.target)
            .ok_or(BackendError::InvalidStoreStateStagingToken)?;
        if upload.ambiguous || upload.publishing || upload.aborting {
            return Err(BackendError::InvalidStoreStateStagingToken.into());
        }
        // Validate all sizes before the first send so a later oversized row
        // cannot leave a partially accepted batch that callers retry as new.
        let mut chunks = Vec::new();
        let mut chunk = RecordMutations::new();
        let mut encoded = 0usize;
        for (key, value) in records {
            let size = serde_json::to_vec(&(key.clone(), value.clone()))?.len();
            if size > crate::service::protocol::MAX_RECORD_CHUNK_BYTES as usize {
                return Err(BackendError::RecordTooLarge {
                    encoded_bytes: size,
                }
                .into());
            }
            if !chunk.is_empty()
                && encoded + size > crate::service::protocol::MAX_RECORD_CHUNK_BYTES as usize
            {
                chunks.push(std::mem::take(&mut chunk));
                encoded = 0;
            }
            encoded += size;
            chunk.insert(key, value);
        }
        if !chunk.is_empty() {
            chunks.push(chunk);
        }
        // Encode all requests once before uploading; retain the current request
        // if the transport fails, never rebuilding its wire mutations.
        for (offset, chunk) in chunks.into_iter().enumerate() {
            upload
                .pending
                .push_back(RemoteConnection::encode_staging_chunk(
                    token.target.database.clone(),
                    upload.identity.clone(),
                    DatabaseOp::StageStoreStateRecords {
                        token: token.namespace_id.clone(),
                        chunk_id: upload.next_sequence + offset as u64,
                        records: chunk.into_iter().collect(),
                    },
                )?);
        }
        self.send_upload(token, upload).await
    }

    async fn stage_store_state_ordered_chunk(
        &self,
        token: &StagingToken,
        sequence: u64,
        _digest: &[u8],
        mutations: Vec<crate::backend::RecordMutation>,
    ) -> Result<()> {
        let mut uploads = self.uploads.lock().await;
        let upload = uploads
            .get_mut(&token.namespace_id)
            .filter(|u| u.target == token.target)
            .ok_or(BackendError::InvalidStoreStateStagingToken)?;
        if upload.ambiguous
            || upload.publishing
            || upload.aborting
            || upload.next_sequence != sequence
        {
            return Err(BackendError::InvalidStoreStateStagingToken.into());
        }
        let encoded = serde_json::to_vec(&mutations)?;
        if encoded.len() > crate::service::protocol::MAX_RECORD_CHUNK_BYTES as usize {
            return Err(BackendError::RecordTooLarge {
                encoded_bytes: encoded.len(),
            }
            .into());
        }
        upload
            .pending
            .push_back(RemoteConnection::encode_staging_chunk(
                token.target.database.clone(),
                upload.identity.clone(),
                DatabaseOp::StageStoreStateOrdered {
                    token: token.namespace_id.clone(),
                    chunk_id: sequence,
                    mutations,
                },
            )?);
        self.send_upload(token, upload).await
    }

    async fn publish_store_state(&self, token: StagingToken) -> Result<RecordView> {
        let mut uploads = self.uploads.lock().await;
        let upload = uploads
            .get_mut(&token.namespace_id)
            .filter(|u| u.target == token.target)
            .ok_or(BackendError::InvalidStoreStateStagingToken)?;
        if upload.ambiguous || upload.aborting || !upload.pending.is_empty() {
            return Err(BackendError::InvalidStoreStateStagingToken.into());
        }
        upload.ambiguous = true;
        upload.publishing = true;
        let result = self
            .connection()
            .publish_store_state(
                token.target.database.clone(),
                upload.identity.clone(),
                token.namespace_id.clone(),
            )
            .await;
        match result {
            Ok(namespace_id) => {
                self.views
                    .lock()
                    .unwrap()
                    .insert(namespace_id.clone(), token.target.database);
                uploads.remove(&token.namespace_id);
                Ok(RecordView { namespace_id })
            }
            Err(crate::Error::Io(source)) => {
                upload.ambiguous = true;
                upload.publishing = true;
                Err(crate::Error::AmbiguousStaging {
                    token: token.namespace_id,
                    chunk: None,
                    source,
                })
            }
            Err(error) => {
                upload.ambiguous = false;
                upload.publishing = false;
                Err(error)
            }
        }
    }

    async fn abort_store_state(&self, token: StagingToken) -> Result<()> {
        let mut uploads = self.uploads.lock().await;
        let upload = uploads
            .get_mut(&token.namespace_id)
            .filter(|u| u.target == token.target)
            .ok_or(BackendError::InvalidStoreStateStagingToken)?;
        if upload.ambiguous {
            return Err(BackendError::InvalidStoreStateStagingToken.into());
        }
        upload.ambiguous = true;
        upload.aborting = true;
        match self
            .connection()
            .abort_store_state(
                token.target.database,
                upload.identity.clone(),
                token.namespace_id.clone(),
            )
            .await
        {
            Ok(()) => {
                uploads.remove(&token.namespace_id);
                Ok(())
            }
            Err(crate::Error::Io(source)) => {
                upload.ambiguous = true;
                Err(crate::Error::AmbiguousStaging {
                    token: token.namespace_id,
                    chunk: None,
                    source,
                })
            }
            Err(error) => {
                upload.ambiguous = false;
                upload.aborting = false;
                Err(error)
            }
        }
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
        self.connection()
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
        self.connection()
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
        self.connection()
            .db_get_entry(ID::default(), self.identity(), id.clone())
            .await
    }

    async fn snapshot(&self, tree: &ID) -> Result<Snapshot> {
        match self
            .connection()
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
            .connection()
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
            .connection()
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
            .connection()
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
        self.connection()
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
            .connection()
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
        self.connection()
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
        self.connection()
            .submit_signed_entry(tree_root, self.identity(), entry)
            .await
    }

    async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>> {
        self.connection().get_instance_metadata().await
    }

    async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()> {
        self.connection().set_instance_metadata(metadata).await
    }

    fn remote_connection(&self) -> Option<RemoteConnection> {
        Some(self.connection())
    }
}
