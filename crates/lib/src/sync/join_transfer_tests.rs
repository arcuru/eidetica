//! Attempt-local transfer safety: a response is not evidence of a complete replica.

use std::{
    any::Any,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use tokio::sync::mpsc;

use super::{
    Sync,
    background::SyncCommand,
    protocol::{BootstrapResponse, IncrementalResponse, SyncResponse, SyncTreeRequest},
};
use crate::{
    Database, Entry, Instance, NewUser, Result, Snapshot,
    auth::{Permission, crypto::generate_keypair},
    backend::{
        BackendImpl, InstanceMetadata, InstanceSecrets, RecordMutations, RecordPage, RecordRange,
        RecordView, StagingToken, StoreStateRequest, VerificationStatus, database::InMemory,
    },
    crdt::Doc,
    entry::ID,
    store::DocStore,
    sync::Address,
};

#[derive(Clone, Copy)]
enum Fault {
    Put,
    Promote,
}

/// Only the named write fails; all reads use the real backend, including caches.
struct FaultBackend {
    inner: Arc<dyn BackendImpl>,
    fault: Arc<Mutex<Option<(Fault, ID)>>>,
}

#[async_trait]
impl BackendImpl for FaultBackend {
    fn as_any(&self) -> &dyn Any {
        self
    }
    async fn put(&self, entry: Entry) -> Result<()> {
        if self
            .fault
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(op, id)| matches!(op, Fault::Put) && *id == entry.id())
        {
            return Err(std::io::Error::other("injected put failure").into());
        }
        self.inner.put(entry).await
    }
    async fn update_verification_status(&self, id: &ID, status: VerificationStatus) -> Result<()> {
        if self
            .fault
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|(op, target)| matches!(op, Fault::Promote) && target == id)
        {
            return Err(std::io::Error::other("injected promotion failure").into());
        }
        self.inner.update_verification_status(id, status).await
    }
    async fn get(&self, id: &ID) -> Result<Entry> {
        self.inner.get(id).await
    }
    async fn get_verification_status(&self, id: &ID) -> Result<VerificationStatus> {
        self.inner.get_verification_status(id).await
    }
    async fn get_entries_by_verification_status(
        &self,
        status: VerificationStatus,
    ) -> Result<Vec<ID>> {
        self.inner.get_entries_by_verification_status(status).await
    }
    async fn snapshot(&self, tree: &ID) -> Result<Snapshot> {
        self.inner.snapshot(tree).await
    }
    async fn store_snapshot(&self, tree: &ID, store: &str) -> Result<Snapshot> {
        self.inner.store_snapshot(tree, store).await
    }
    async fn store_snapshot_at(
        &self,
        tree: &ID,
        store: &str,
        main_snapshot: &Snapshot,
    ) -> Result<Snapshot> {
        self.inner
            .store_snapshot_at(tree, store, main_snapshot)
            .await
    }
    async fn all_roots(&self) -> Result<Vec<ID>> {
        self.inner.all_roots().await
    }
    async fn find_merge_base(
        &self,
        tree: &ID,
        store: &str,
        entry_ids: &[ID],
    ) -> Result<Option<ID>> {
        self.inner.find_merge_base(tree, store, entry_ids).await
    }
    async fn get_tree(&self, tree: &ID) -> Result<Vec<Entry>> {
        self.inner.get_tree(tree).await
    }
    async fn get_store(&self, tree: &ID, store: &str) -> Result<Vec<Entry>> {
        self.inner.get_store(tree, store).await
    }
    async fn get_tree_from_tips(&self, tree: &ID, tips: &[ID]) -> Result<Vec<Entry>> {
        self.inner.get_tree_from_tips(tree, tips).await
    }
    async fn store_at(&self, tree: &ID, store: &str, snapshot: &Snapshot) -> Result<Vec<Entry>> {
        self.inner.store_at(tree, store, snapshot).await
    }
    async fn get_sorted_store_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        store: &str,
    ) -> Result<Vec<ID>> {
        self.inner
            .get_sorted_store_parents(tree_id, entry_id, store)
            .await
    }
    async fn get_path_from_to(
        &self,
        tree_id: &ID,
        store: &str,
        from_id: Option<&ID>,
        to_ids: &[ID],
    ) -> Result<Vec<ID>> {
        self.inner
            .get_path_from_to(tree_id, store, from_id, to_ids)
            .await
    }
    async fn get_instance_metadata(&self) -> Result<Option<InstanceMetadata>> {
        self.inner.get_instance_metadata().await
    }
    async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> Result<()> {
        self.inner.set_instance_metadata(metadata).await
    }
    async fn get_instance_secrets(&self) -> Result<Option<InstanceSecrets>> {
        self.inner.get_instance_secrets().await
    }
    async fn set_instance_secrets(&self, secrets: &InstanceSecrets) -> Result<()> {
        self.inner.set_instance_secrets(secrets).await
    }
    async fn resolve_store_state(&self, request: &StoreStateRequest) -> Result<Option<RecordView>> {
        self.inner.resolve_store_state(request).await
    }
    async fn begin_store_state_staging(&self, request: StoreStateRequest) -> Result<StagingToken> {
        self.inner.begin_store_state_staging(request).await
    }
    async fn stage_store_state_records(
        &self,
        token: &StagingToken,
        records: RecordMutations,
    ) -> Result<()> {
        self.inner.stage_store_state_records(token, records).await
    }
    async fn publish_store_state(&self, token: StagingToken) -> Result<RecordView> {
        self.inner.publish_store_state(token).await
    }
    async fn abort_store_state(&self, token: StagingToken) -> Result<()> {
        self.inner.abort_store_state(token).await
    }
    async fn store_state_record_get(
        &self,
        view: &RecordView,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        self.inner.store_state_record_get(view, key).await
    }
    async fn store_state_record_scan(
        &self,
        view: &RecordView,
        range: &RecordRange,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<RecordPage> {
        self.inner
            .store_state_record_scan(view, range, after, limit)
            .await
    }
    async fn clear_derived_store_state(&self) -> Result<()> {
        self.inner.clear_derived_store_state().await
    }
}

async fn source() -> Result<(Instance, ID, Vec<Entry>)> {
    let (instance, mut owner) =
        Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("owner")).await?;
    let key = owner.get_default_key()?;
    let db = owner.create_database(Doc::new(), &key).await?;
    for value in ["ancestor", "final"] {
        db.with_transaction(|tx| async move {
            tx.get_store::<DocStore>("content")
                .await?
                .set_string("message", value)
                .await
        })
        .await?;
    }
    let tree = db.root_id().clone();
    let entries = instance.require_local_engine()?.get_tree(&tree).await?;
    Ok((instance, tree, entries))
}

async fn destination(
    inner: Arc<dyn BackendImpl>,
) -> Result<(Instance, Sync, Arc<Mutex<Option<(Fault, ID)>>>)> {
    let fault = Arc::new(Mutex::new(None));
    let (instance, _) = Instance::create_backend(
        Box::new(FaultBackend {
            inner,
            fault: fault.clone(),
        }),
        NewUser::passwordless("client"),
    )
    .await?;
    let sync = Sync::new(instance.clone()).await?;
    Ok((instance, sync, fault))
}

fn bootstrap(tree: &ID, entries: &[Entry]) -> SyncResponse {
    SyncResponse::Bootstrap(BootstrapResponse {
        tree_id: tree.clone(),
        root_entry: entries.iter().find(|e| e.id() == *tree).unwrap().clone(),
        all_entries: entries
            .iter()
            .filter(|e| e.id() != *tree)
            .cloned()
            .collect(),
        key_approved: true,
        granted_permission: Some(Permission::Read),
    })
}

/// Exercise the real exchange response handler, without a remote response oracle.
/// Unexpected send-back fails loudly instead of hiding a confidentiality leak.
async fn exchange(sync: &Sync, tree: &ID, response: SyncResponse) -> Result<()> {
    let (tx, mut rx) = mpsc::channel(1);
    let mut sync = sync.clone();
    sync.background_tx = std::sync::OnceLock::from(tx);
    let task = tokio::spawn(async move {
        let SyncCommand::SendRequest {
            response: reply, ..
        } = rx.recv().await.unwrap()
        else {
            panic!("expected tree request")
        };
        reply.send(Ok(response)).unwrap();
        while let Some(command) = rx.recv().await {
            if let SyncCommand::SendRequest { response, .. } = command {
                response
                    .send(Err(std::io::Error::other("unexpected send-back").into()))
                    .unwrap();
            }
        }
    });
    let (_, peer) = generate_keypair();
    sync.register_peer(&peer, None).await?;
    let result = sync
        .exchange_tree_request_at(
            &Address::http("unused"),
            &peer,
            SyncTreeRequest {
                tree_id: tree.clone(),
                our_tips: Snapshot::EMPTY,
                peer_pubkey: None,
                requesting_key: None,
                requesting_key_name: None,
                requested_permission: None,
                metadata: None,
                auth: None,
            },
        )
        .await;
    if result.is_err() {
        assert!(
            sync.get_tree_peers(tree).await?.is_empty(),
            "failed transfer published routing"
        );
    }
    drop(sync);
    task.await.unwrap();
    result
}

#[tokio::test]
async fn join_transfer_confines_every_response_before_storage() -> Result<()> {
    let (_source, tree, entries) = source().await?;
    let (_foreign, foreign_tree, foreign) = source().await?;
    let foreign_entry = foreign.last().unwrap().clone();
    let mut envelope = bootstrap(&foreign_tree, &foreign);
    let mut root = bootstrap(&tree, &entries);
    if let SyncResponse::Bootstrap(response) = &mut root {
        response.root_entry = foreign[0].clone();
    }
    let mut member = bootstrap(&tree, &entries);
    if let SyncResponse::Bootstrap(response) = &mut member {
        response.all_entries.push(foreign_entry.clone());
    }
    let incremental = SyncResponse::Incremental(IncrementalResponse {
        tree_id: tree.clone(),
        their_tips: vec![],
        missing_entries: vec![foreign_entry.clone()],
    });
    let incremental_envelope = SyncResponse::Incremental(IncrementalResponse {
        tree_id: foreign_tree.clone(),
        their_tips: vec![],
        missing_entries: foreign.clone(),
    });
    // Keep the claimed tree different from the root even if the member is valid elsewhere.
    if let SyncResponse::Bootstrap(response) = &mut envelope {
        response.key_approved = true;
    }
    for response in [envelope, root, member, incremental, incremental_envelope] {
        let (local, sync, _) = destination(Arc::new(InMemory::new())).await?;
        assert!(
            exchange(&sync, &tree, response).await.is_err(),
            "foreign response accepted"
        );
        assert!(
            local.backend().get(&foreign_entry.id()).await.is_err(),
            "foreign entry stored"
        );
        assert!(
            local.backend().get(&foreign_tree).await.is_err(),
            "foreign root stored"
        );
        assert!(
            local.backend().get(&tree).await.is_err(),
            "batch partially stored before confinement check"
        );
    }
    Ok(())
}

#[tokio::test]
async fn join_transfer_requires_every_supplied_entry_verified() -> Result<()> {
    let (_source, tree, entries) = source().await?;
    let mut incomplete = entries.clone();
    incomplete.remove(1);
    let (local, sync, _) = destination(Arc::new(InMemory::new())).await?;
    assert!(
        exchange(&sync, &tree, bootstrap(&tree, &incomplete))
            .await
            .is_err(),
        "missing ancestor reported complete"
    );
    assert_eq!(
        local
            .require_local_engine()?
            .get_verification_status(&entries.last().unwrap().id())
            .await?,
        VerificationStatus::Unverified
    );
    exchange(&sync, &tree, bootstrap(&tree, &entries)).await?;
    assert_eq!(
        local
            .require_local_engine()?
            .get_verification_status(&entries.last().unwrap().id())
            .await?,
        VerificationStatus::Verified
    );

    let (local, sync, _) = destination(Arc::new(InMemory::new())).await?;
    for entry in &entries {
        local.backend().put(entry.clone()).await?;
    }
    local
        .require_local_engine()?
        .update_verification_status(&entries.last().unwrap().id(), VerificationStatus::Failed)
        .await?;
    assert!(
        exchange(&sync, &tree, bootstrap(&tree, &entries))
            .await
            .is_err(),
        "Failed entry reported complete"
    );
    Ok(())
}

#[tokio::test]
async fn join_transfer_propagates_put_and_promotion_failures() -> Result<()> {
    let (_source, tree, entries) = source().await?;
    for op in [Fault::Put, Fault::Promote] {
        for entry in &entries {
            let (local, sync, fault) = destination(Arc::new(InMemory::new())).await?;
            *fault.lock().unwrap() = Some((op, entry.id()));
            assert!(
                exchange(&sync, &tree, bootstrap(&tree, &entries))
                    .await
                    .is_err(),
                "failed write reported complete"
            );
            *fault.lock().unwrap() = None;
            exchange(&sync, &tree, bootstrap(&tree, &entries)).await?;
            for entry in &entries {
                assert_eq!(
                    local
                        .require_local_engine()?
                        .get_verification_status(&entry.id())
                        .await?,
                    VerificationStatus::Verified
                );
            }
            let db = Database::open(&local, &tree).await?;
            assert_eq!(
                db.get_store_viewer::<DocStore>("content")
                    .await?
                    .get_string("message")
                    .await?,
                "final"
            );
        }
    }
    Ok(())
}
