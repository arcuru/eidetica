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

#[derive(Clone, Copy, Debug)]
enum Fault {
    Put,
    Promote,
    PutTree,
    PromoteTree,
    AckTree,
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
        if self.fault.lock().unwrap().as_ref().is_some_and(|(op, id)| {
            (matches!(op, Fault::Put) && *id == entry.id())
                || (matches!(op, Fault::PutTree) && entry.root().as_ref() == Some(id))
        }) {
            return Err(std::io::Error::other("injected put failure").into());
        }
        self.inner.put(entry).await
    }
    async fn update_verification_status(&self, id: &ID, status: VerificationStatus) -> Result<()> {
        let root = self
            .inner
            .get(id)
            .await?
            .root()
            .unwrap_or_else(|| id.clone());
        let fault = self.fault.lock().unwrap().clone();
        if fault.as_ref().is_some_and(|(op, target)| {
            (matches!(op, Fault::Promote) && target == id)
                || (matches!(op, Fault::PromoteTree) && *target == root)
        }) {
            return Err(std::io::Error::other("injected promotion failure").into());
        }
        self.inner.update_verification_status(id, status).await?;
        if fault
            .as_ref()
            .is_some_and(|(op, target)| matches!(op, Fault::AckTree) && *target == root)
        {
            return Err(std::io::Error::other("injected lost commit acknowledgement").into());
        }
        Ok(())
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
    exchange_with_access(sync, tree, response, None).await
}

async fn exchange_with_access(
    sync: &Sync,
    tree: &ID,
    response: SyncResponse,
    access: Option<(crate::auth::crypto::PublicKey, Permission)>,
) -> Result<()> {
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
                requesting_key: access.as_ref().map(|(key, _)| key.clone()),
                requesting_key_name: access.as_ref().map(|_| "requester".to_string()),
                requested_permission: access.as_ref().map(|(_, permission)| *permission),
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

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn join_transfer_reuses_partial_entries_after_sqlite_reopen() -> Result<()> {
    use crate::backend::database::Sqlite;
    let (_source, tree, entries) = source().await?;
    for op in [Fault::Put, Fault::Promote] {
        for entry in &entries {
            let dir = tempfile::tempdir()?;
            let path = dir.path().join("replica.sqlite");
            let (local, sync, fault) = destination(Arc::new(Sqlite::open(&path).await?)).await?;
            *fault.lock().unwrap() = Some((op, entry.id()));
            assert!(
                exchange(&sync, &tree, bootstrap(&tree, &entries))
                    .await
                    .is_err()
            );
            let before = local.require_local_engine()?.get_tree(&tree).await?;
            drop(sync);
            drop(local);
            drop(fault);

            // Reopen the file, not an in-memory snapshot of its apparent state.
            let reopened = Instance::open_backend(Box::new(Sqlite::open(&path).await?)).await?;
            let after = reopened.require_local_engine()?.get_tree(&tree).await?;
            assert_eq!(
                before.iter().map(Entry::id).collect::<Vec<_>>(),
                after.iter().map(Entry::id).collect::<Vec<_>>()
            );
            let sync = Sync::new(reopened.clone()).await?;
            exchange(&sync, &tree, bootstrap(&tree, &entries)).await?;
            for entry in &entries {
                assert_eq!(
                    reopened
                        .require_local_engine()?
                        .get_verification_status(&entry.id())
                        .await?,
                    VerificationStatus::Verified
                );
            }
            let tips = reopened.backend().snapshot(&tree).await?;
            exchange(&sync, &tree, bootstrap(&tree, &entries)).await?;
            assert_eq!(
                tips,
                reopened.backend().snapshot(&tree).await?,
                "duplicate transfer changed target history"
            );
            assert_eq!(
                Database::open(&reopened, &tree)
                    .await?
                    .get_store_viewer::<DocStore>("content")
                    .await?
                    .get_string("message")
                    .await?,
                "final"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn join_user_pending_and_rejected_leave_user_history_unchanged() -> Result<()> {
    use crate::user::SyncSettings;
    let (instance, mut user) =
        Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("client"))
            .await?;
    let key = user.get_default_key()?;
    let tree = ID::from_bytes("not-yet-replicated");
    let before = user.user_database().snapshot().await?;
    for rejected in [false, false, true, true] {
        let error = if rejected {
            super::SyncError::BootstrapRejected {
                request_id: "same".into(),
                message: "rejected".into(),
            }
        } else {
            super::SyncError::BootstrapPending {
                request_id: "same".into(),
                message: "pending".into(),
            }
        };
        assert!(
            user.record_database_access(&tree, &key, SyncSettings::on_commit(), Err(error.into()))
                .await
                .is_err()
        );
        assert_eq!(
            before,
            user.user_database().snapshot().await?,
            "non-success appended User history"
        );
        assert_eq!(user.key_mapping(&key, &tree)?, None);
        assert!(user.database(&tree).await.is_err());
    }
    drop(instance);
    Ok(())
}

async fn wildcard_database(instance: &Instance) -> Result<Database> {
    let (owner, _) = generate_keypair();
    let db = Database::create(instance, owner, Doc::new()).await?;
    db.with_transaction(|tx| async move {
        tx.get_settings()?
            .set_global_auth_key(crate::auth::AuthKey::active(None, Permission::Write(0)))
            .await
    })
    .await?;
    Ok(db)
}

#[tokio::test]
async fn join_user_commit_failure_does_not_publish_speculative_cache() -> Result<()> {
    use crate::user::SyncSettings;
    for op in [Fault::PutTree, Fault::PromoteTree, Fault::AckTree] {
        let (instance, _sync, fault) = destination(Arc::new(InMemory::new())).await?;
        let mut user = instance.login_user("client", None).await?;
        let key = user.get_default_key()?;
        let db = wildcard_database(&instance).await?;
        *fault.lock().unwrap() = Some((op, user.user_database().root_id().clone()));
        assert!(
            user.track_database(db.root_id(), &key, SyncSettings::disabled())
                .await
                .is_err()
        );
        if matches!(op, Fault::AckTree) {
            assert_eq!(
                user.key_mapping(&key, db.root_id())?,
                Some(crate::auth::SigKey::global(&key)),
                "cache did not reconcile verified lost-ack outcome"
            );
        } else {
            assert_eq!(
                user.key_mapping(&key, db.root_id())?,
                None,
                "uncommitted mapping escaped into cache"
            );
        }
        *fault.lock().unwrap() = None;
        user.track_database(db.root_id(), &key, SyncSettings::disabled())
            .await?;
        assert_eq!(
            user.key_mapping(&key, db.root_id())?,
            Some(crate::auth::SigKey::global(&key))
        );
        let before = user.user_database().snapshot().await?;
        user.track_database(db.root_id(), &key, SyncSettings::disabled())
            .await?;
        assert_eq!(
            before,
            user.user_database().snapshot().await?,
            "identical retry appended User history"
        );
        let fresh = instance.login_user("client", None).await?;
        assert_eq!(
            fresh.key_mapping(&key, db.root_id())?,
            user.key_mapping(&key, db.root_id())?
        );
    }
    Ok(())
}

#[tokio::test]
async fn join_user_rediscovers_legacy_mapping_and_preserves_unrelated_intent() -> Result<()> {
    use crate::user::SyncSettings;
    let (instance, mut user) =
        Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("client"))
            .await?;
    let key = user.get_default_key()?;
    let db = wildcard_database(&instance).await?;
    user.track_database(db.root_id(), &key, SyncSettings::disabled())
        .await?;
    // Simulate a legacy provisional direct mapping on an unchanged tracked key.
    user.map_key(&key, db.root_id(), crate::auth::SigKey::from_pubkey(&key))
        .await?;
    let other = wildcard_database(&instance).await?;
    let mut another_session = instance.login_user("client", None).await?;
    another_session
        .track_database(
            other.root_id(),
            &key,
            SyncSettings::on_commit().with_interval(43),
        )
        .await?;
    user.record_database_access(db.root_id(), &key, SyncSettings::disabled(), Ok(()))
        .await?;
    assert_eq!(
        user.key_mapping(&key, db.root_id())?,
        Some(crate::auth::SigKey::global(&key)),
        "successful retry kept provisional identity"
    );
    assert!(
        user.key_mapping(&key, other.root_id())?.is_some(),
        "cache dropped other session's persisted mapping"
    );
    assert_eq!(
        user.database(other.root_id())
            .await?
            .sync_settings
            .interval_seconds,
        Some(43)
    );
    Ok(())
}

async fn pending_request(
    sync: &Sync,
    tree: &ID,
    key: &crate::auth::crypto::PublicKey,
    permission: Permission,
) -> Result<String> {
    use super::{
        BootstrapRequest, RequestStatus, bootstrap_request_manager::BootstrapRequestManager,
    };
    let tx = sync.sync_tree.new_transaction().await?;
    let id = BootstrapRequestManager::new(&tx)
        .store_request(BootstrapRequest {
            tree_id: tree.clone(),
            requesting_pubkey: key.clone(),
            requesting_key_name: "requester".into(),
            requested_permission: permission,
            timestamp: "2026-10-03T18:00:00Z".into(),
            status: RequestStatus::Pending,
            peer_address: Address::http("unused"),
            metadata: None,
        })
        .await?;
    tx.commit().await?;
    Ok(id)
}

#[tokio::test]
async fn join_approval_reconciles_split_commit_without_regrant() -> Result<()> {
    for op in [Fault::PutTree, Fault::PromoteTree, Fault::AckTree] {
        let (instance, sync, fault) = destination(Arc::new(InMemory::new())).await?;
        let mut owner = instance.login_user("client", None).await?;
        let owner_key = owner.get_default_key()?;
        let db = owner.create_database(Doc::new(), &owner_key).await?;
        let (_, requester) = generate_keypair();
        let request =
            pending_request(&sync, db.root_id(), &requester, Permission::Write(5)).await?;
        *fault.lock().unwrap() = Some((op, sync.sync_tree.root_id().clone()));
        assert!(
            owner
                .approve_bootstrap_request(&sync, &request, &owner_key)
                .await
                .is_err()
        );
        assert!(
            Database::can_access(&instance, db.root_id(), &requester, &Permission::Write(5))
                .await?
        );
        let after_grant = db.snapshot().await?;
        *fault.lock().unwrap() = None;
        owner
            .approve_bootstrap_request(&sync, &request, &owner_key)
            .await?;
        assert_eq!(
            after_grant,
            db.snapshot().await?,
            "approval retry reissued active grant"
        );
        let after_decision = sync.sync_tree.snapshot().await?;
        owner
            .approve_bootstrap_request(&sync, &request, &owner_key)
            .await?;
        assert_eq!(
            after_decision,
            sync.sync_tree.snapshot().await?,
            "completed decision not idempotent"
        );
    }
    Ok(())
}

#[tokio::test]
async fn join_approval_retry_refuses_observed_reduction_and_revocation() -> Result<()> {
    for revoke in [false, true] {
        let (instance, sync, _) = destination(Arc::new(InMemory::new())).await?;
        let mut owner = instance.login_user("client", None).await?;
        let owner_key = owner.get_default_key()?;
        let db = owner.create_database(Doc::new(), &owner_key).await?;
        let (_, requester) = generate_keypair();
        let request =
            pending_request(&sync, db.root_id(), &requester, Permission::Write(5)).await?;
        db.with_transaction(|tx| {
            let requester = requester.clone();
            async move {
                let auth = if revoke {
                    crate::auth::AuthKey::new(
                        None,
                        Permission::Write(5),
                        crate::auth::KeyStatus::Revoked,
                    )
                } else {
                    crate::auth::AuthKey::active(None, Permission::Read)
                };
                tx.get_settings()?.set_auth_key(&requester, auth).await
            }
        })
        .await?;
        let before = db.snapshot().await?;
        assert!(
            owner
                .approve_bootstrap_request(&sync, &request, &owner_key)
                .await
                .is_err(),
            "retry restored reduced/revoked grant"
        );
        assert_eq!(before, db.snapshot().await?);
    }
    Ok(())
}

#[tokio::test]
async fn join_transfer_checks_grant_and_rejects_incremental_send_back() -> Result<()> {
    let (source, tree, entries) = source().await?;
    let (local, sync, _) = destination(Arc::new(InMemory::new())).await?;
    let (_, requester) = generate_keypair();
    let access = (requester.clone(), Permission::Write(5));
    assert!(
        exchange_with_access(
            &sync,
            &tree,
            SyncResponse::Incremental(IncrementalResponse {
                tree_id: tree.clone(),
                their_tips: vec![tree.clone()],
                missing_entries: entries.clone(),
            }),
            Some(access.clone())
        )
        .await
        .is_err()
    );
    assert!(
        local.backend().get(&tree).await.is_err(),
        "hostile incremental join wrote before refusing send-back"
    );
    assert!(
        exchange_with_access(
            &sync,
            &tree,
            bootstrap(&tree, &entries),
            Some(access.clone())
        )
        .await
        .is_err(),
        "peer's approved flag substituted for requested authority"
    );
    let owner = source.login_user("owner", None).await?;
    let db = owner.open_database(&tree).await?;
    db.with_transaction(|tx| {
        let requester = requester.clone();
        async move {
            tx.get_settings()?
                .set_auth_key(
                    &requester,
                    crate::auth::AuthKey::active(None, Permission::Write(5)),
                )
                .await
        }
    })
    .await?;
    let entries = source.require_local_engine()?.get_tree(&tree).await?;
    let grant = entries.last().unwrap().id();
    for op in [Fault::Put, Fault::Promote] {
        let (local, sync, fault) = destination(Arc::new(InMemory::new())).await?;
        *fault.lock().unwrap() = Some((op, grant.clone()));
        assert!(
            exchange_with_access(
                &sync,
                &tree,
                bootstrap(&tree, &entries),
                Some(access.clone())
            )
            .await
            .is_err()
        );
        *fault.lock().unwrap() = None;
        exchange_with_access(
            &sync,
            &tree,
            bootstrap(&tree, &entries),
            Some(access.clone()),
        )
        .await?;
        assert!(Database::can_access(&local, &tree, &requester, &Permission::Write(5)).await?);
    }
    Ok(())
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn join_user_and_approval_reconcile_persisted_faults_after_reopen() -> Result<()> {
    use crate::{backend::database::Sqlite, user::SyncSettings};
    for op in [Fault::PutTree, Fault::PromoteTree, Fault::AckTree] {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("join.sqlite");
        let (instance, sync, fault) = destination(Arc::new(Sqlite::open(&file).await?)).await?;
        let mut user = instance.login_user("client", None).await?;
        let key = user.get_default_key()?;
        let target = wildcard_database(&instance).await?;
        let target_id = target.root_id().clone();
        *fault.lock().unwrap() = Some((op, user.user_database().root_id().clone()));
        assert!(
            user.track_database(&target_id, &key, SyncSettings::disabled())
                .await
                .is_err()
        );
        drop(user);
        drop(target);
        drop(sync);
        drop(instance);
        drop(fault);
        let instance = Instance::open_backend(Box::new(Sqlite::open(&file).await?)).await?;
        let mut user = instance.login_user("client", None).await?;
        user.track_database(&target_id, &key, SyncSettings::disabled())
            .await?;
        let tips = user.user_database().snapshot().await?;
        user.track_database(&target_id, &key, SyncSettings::disabled())
            .await?;
        assert_eq!(tips, user.user_database().snapshot().await?);
        assert_eq!(
            user.key_mapping(&key, &target_id)?,
            Some(crate::auth::SigKey::global(&key))
        );
        assert!(user.open_database(&target_id).await.is_ok());
    }
    for target_fault in [false, true] {
        for op in [Fault::PutTree, Fault::PromoteTree, Fault::AckTree] {
            let dir = tempfile::tempdir()?;
            let file = dir.path().join("peer.sqlite");
            let (instance, sync, fault) = destination(Arc::new(Sqlite::open(&file).await?)).await?;
            let mut owner = instance.login_user("client", None).await?;
            let key = owner.get_default_key()?;
            let target = owner.create_database(Doc::new(), &key).await?;
            let tree = target.root_id().clone();
            let (_, requester) = generate_keypair();
            let request = pending_request(&sync, &tree, &requester, Permission::Write(5)).await?;
            let sync_id = sync.sync_tree.root_id().clone();
            *fault.lock().unwrap() = Some((
                op,
                if target_fault {
                    tree.clone()
                } else {
                    sync_id.clone()
                },
            ));
            assert!(
                owner
                    .approve_bootstrap_request(&sync, &request, &key)
                    .await
                    .is_err()
            );
            drop(owner);
            drop(target);
            drop(sync);
            drop(instance);
            drop(fault);
            let instance = Instance::open_backend(Box::new(Sqlite::open(&file).await?)).await?;
            let sync = Sync::load(instance.clone(), &sync_id).await?;
            assert_eq!(sync.sync_tree.root_id(), &sync_id);
            let owner = instance.login_user("client", None).await?;
            owner
                .approve_bootstrap_request(&sync, &request, &key)
                .await?;
            assert!(
                Database::can_access(&instance, &tree, &requester, &Permission::Write(5)).await?
            );
            let target_tips = Database::open(&instance, &tree).await?.snapshot().await?;
            let decision_tips = sync.sync_tree.snapshot().await?;
            owner
                .approve_bootstrap_request(&sync, &request, &key)
                .await?;
            assert_eq!(
                target_tips,
                Database::open(&instance, &tree).await?.snapshot().await?
            );
            assert_eq!(decision_tips, sync.sync_tree.snapshot().await?);
        }
    }
    Ok(())
}

#[tokio::test]
async fn join_approval_refuses_removed_key_and_rejection_is_idempotent() -> Result<()> {
    let (instance, sync, _) = destination(Arc::new(InMemory::new())).await?;
    let mut owner = instance.login_user("client", None).await?;
    let key = owner.get_default_key()?;
    let db = owner.create_database(Doc::new(), &key).await?;
    let (_, requester) = generate_keypair();
    let request = pending_request(&sync, db.root_id(), &requester, Permission::Write(5)).await?;
    db.with_transaction(|tx| {
        let requester = requester.clone();
        async move {
            tx.get_settings()?
                .set_auth_key(
                    &requester,
                    crate::auth::AuthKey::active(None, Permission::Write(5)),
                )
                .await
        }
    })
    .await?;
    let path = format!("auth.keys.{requester}");
    db.with_transaction(|tx| async move {
        tx.get_store::<DocStore>(crate::constants::SETTINGS)
            .await?
            .delete(path)
            .await
    })
    .await?;
    let before = db.snapshot().await?;
    assert!(
        owner
            .approve_bootstrap_request(&sync, &request, &key)
            .await
            .is_err(),
        "removed grant was reactivated"
    );
    assert_eq!(before, db.snapshot().await?);
    owner
        .reject_bootstrap_request(&sync, &request, &key)
        .await?;
    let decision = sync.sync_tree.snapshot().await?;
    owner
        .reject_bootstrap_request(&sync, &request, &key)
        .await?;
    assert_eq!(decision, sync.sync_tree.snapshot().await?);
    assert!(
        owner
            .approve_bootstrap_request(&sync, &request, &key)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn join_transfer_missing_delegated_history_is_not_complete() -> Result<()> {
    use crate::auth::{
        DelegatedTreeRef, DelegationStep, KeyHint, PermissionBounds, SigKey, TreeReference,
    };
    use crate::database::DatabaseKey;
    let (source, _) =
        Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("owner")).await?;
    let (delegate, delegate_pub) = generate_keypair();
    let identity = Database::create(&source, delegate.clone(), Doc::new()).await?;
    let target = Database::create(&source, generate_keypair().0, Doc::new()).await?;
    let delegated_tips = identity.snapshot().await?.into_tips();
    target
        .with_transaction(|tx| {
            let tree = identity.root_id().clone();
            let tips = delegated_tips.clone();
            async move {
                tx.get_settings()?
                    .add_delegated_tree(DelegatedTreeRef {
                        permission_bounds: PermissionBounds {
                            max: Permission::Write(10),
                            min: None,
                        },
                        tree: TreeReference { root: tree, tips },
                    })
                    .await
            }
        })
        .await?;
    let delegated = target.clone().with_key(DatabaseKey::with_identity(
        delegate,
        SigKey::Delegation {
            path: vec![DelegationStep {
                tree: identity.root_id().clone(),
                tips: delegated_tips,
            }],
            hint: KeyHint::from_pubkey(&delegate_pub),
        },
    ));
    delegated
        .with_transaction(|tx| async move {
            tx.get_store::<DocStore>("content")
                .await?
                .set_string("message", "delegated")
                .await
        })
        .await?;
    let entries = source
        .require_local_engine()?
        .get_tree(target.root_id())
        .await?;
    let (local, sync, _) = destination(Arc::new(InMemory::new())).await?;
    assert!(
        exchange(
            &sync,
            target.root_id(),
            bootstrap(target.root_id(), &entries)
        )
        .await
        .is_err(),
        "missing delegated history was reported complete"
    );
    assert_ne!(
        local
            .require_local_engine()?
            .get_verification_status(&entries.last().unwrap().id())
            .await?,
        VerificationStatus::Verified
    );
    // Positive control: the identical transfer verifies when the delegated
    // history is already available. Native Failed-status recovery after missing
    // history is a separate release prerequisite, not repaired by join.
    let (local, sync, _) = destination(Arc::new(InMemory::new())).await?;
    let history = source
        .require_local_engine()?
        .get_tree(identity.root_id())
        .await?;
    exchange(
        &sync,
        identity.root_id(),
        bootstrap(identity.root_id(), &history),
    )
    .await?;
    exchange(
        &sync,
        target.root_id(),
        bootstrap(target.root_id(), &entries),
    )
    .await?;
    assert_eq!(
        local
            .require_local_engine()?
            .get_verification_status(&entries.last().unwrap().id())
            .await?,
        VerificationStatus::Verified
    );
    Ok(())
}

#[tokio::test]
async fn join_concurrent_approval_and_routing_are_idempotent() -> Result<()> {
    let (instance, sync, _) = destination(Arc::new(InMemory::new())).await?;
    let mut owner = instance.login_user("client", None).await?;
    let key = owner.get_default_key()?;
    let db = owner.create_database(Doc::new(), &key).await?;
    let (_, requester) = generate_keypair();
    let request = pending_request(&sync, db.root_id(), &requester, Permission::Read).await?;
    let count = instance
        .require_local_engine()?
        .get_tree(db.root_id())
        .await?
        .len();
    let (a, b) = tokio::join!(
        owner.approve_bootstrap_request(&sync, &request, &key),
        owner.approve_bootstrap_request(&sync, &request, &key)
    );
    a?;
    b?;
    assert_eq!(
        count + 1,
        instance
            .require_local_engine()?
            .get_tree(db.root_id())
            .await?
            .len()
    );
    let (_, peer) = generate_keypair();
    sync.register_peer(&peer, None).await?;
    let count = instance
        .require_local_engine()?
        .get_tree(sync.sync_tree.root_id())
        .await?
        .len();
    let (a, b) = tokio::join!(
        sync.add_tree_sync(&peer, db.root_id()),
        sync.add_tree_sync(&peer, db.root_id())
    );
    a?;
    b?;
    assert_eq!(
        count + 1,
        instance
            .require_local_engine()?
            .get_tree(sync.sync_tree.root_id())
            .await?
            .len()
    );
    let after = sync.sync_tree.snapshot().await?;
    sync.add_tree_sync(&peer, db.root_id()).await?;
    assert_eq!(after, sync.sync_tree.snapshot().await?);
    Ok(())
}
