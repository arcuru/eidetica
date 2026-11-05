//! Deterministic checkpoint and send-boundary tests without a running queue timer.
use super::*;
use crate::{
    Entry, NewAccount, Snapshot,
    backend::{VerificationStatus, database::InMemory},
    crdt::Doc,
    store::DocStore,
};

#[tokio::test]
async fn verified_sync_owns_per_peer_checkpoints_independently_of_application_view() -> Result<()> {
    let (instance, mut owner) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("owner"))
            .await?;
    let key = owner.get_default_key()?;
    let db = owner.create_database(Doc::new(), &key).await?;
    let sync = Sync::new(instance.clone()).await?;
    // Install only the command sender: the test owns queue draining; no timer races.
    let (commands, _receiver) = mpsc::channel(1);
    sync.background_tx.set(commands).unwrap();
    owner
        .track_database(
            db.root_id().clone(),
            &key,
            crate::account::types::SyncSettings::on_commit(),
        )
        .await?;
    sync.sync_account(owner.login_id(), owner.account().id())
        .await?;
    let peer = crate::auth::crypto::PublicKey::random();
    sync.register_peer(&peer, None).await?;
    sync.add_tree_sync(&peer, db.root_id()).await?;
    let engine = instance.require_local_engine()?;
    let pending = Entry::builder(db.root_id().clone())
        .add_parent(db.root_id().clone())
        .set_height(1)
        .set_subtree_data("test", b"pending")
        .build()?;
    let failed = Entry::builder(db.root_id().clone())
        .add_parent(db.root_id().clone())
        .set_height(1)
        .set_subtree_data("test", b"failed")
        .build()?;
    engine.put(pending.clone()).await?;
    engine.put(failed.clone()).await?;
    engine
        .update_verification_status(&failed.id(), VerificationStatus::Failed)
        .await?;
    let opt_in = db.clone().allow_unverified();
    sync.on_local_write(&opt_in).await?;
    let first = sync.queue.drain();
    let ids: Vec<_> = first.values().flatten().map(|(id, _)| id.clone()).collect();
    assert!(
        ids.contains(db.root_id()),
        "first queueing includes the Verified ancestry"
    );
    assert!(!ids.contains(&pending.id()));
    assert!(!ids.contains(&failed.id()));
    assert_eq!(
        sync.write_cursors
            .lock()
            .await
            .get(&(PeerId::new(peer.clone()), db.root_id().clone())),
        Some(&engine.verified_snapshot(db.root_id()).await?)
    );
    sync.on_local_write(&db).await?;
    assert!(
        sync.queue.is_empty(),
        "no requeue for an unchanged Verified frontier"
    );

    // Promotion is detected even though structural tips have not moved.
    let raw_before = engine.snapshot(db.root_id()).await?;
    engine
        .update_verification_status(&pending.id(), VerificationStatus::Verified)
        .await?;
    sync.on_local_write(&opt_in).await?;
    let promoted = sync.queue.drain();
    assert_eq!(
        promoted[&PeerId::new(peer)]
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        vec![pending.id()]
    );
    assert_eq!(engine.snapshot(db.root_id()).await?, raw_before);

    // A newly configured peer owns a fresh checkpoint, not the older peer's.
    let later_peer = crate::auth::crypto::PublicKey::random();
    sync.register_peer(&later_peer, None).await?;
    sync.add_tree_sync(&later_peer, db.root_id()).await?;
    sync.on_local_write(&db).await?;
    let later = sync.queue.drain();
    assert_eq!(later.len(), 1);
    assert!(
        later[&PeerId::new(later_peer)]
            .iter()
            .any(|(id, _)| id == db.root_id())
    );
    assert!(!sync.write_cursors.lock().await.is_empty());
    let resumed = Sync::load(instance.clone(), sync.sync_tree_root_id()).await?;
    assert!(
        resumed.write_cursors.lock().await.is_empty(),
        "restart does not invent acknowledged delivery"
    );
    let (commands, _receiver) = mpsc::channel(1);
    resumed.background_tx.set(commands).unwrap();
    resumed.on_local_write(&opt_in).await?;
    let replay = resumed.queue.drain();
    assert_eq!(replay.len(), 2);
    for entries in replay.values() {
        assert!(entries.iter().any(|(id, _)| id == db.root_id()));
        assert!(!entries.iter().any(|(id, _)| id == &failed.id()));
    }
    Ok(())
}

#[tokio::test]
async fn verified_sync_rechecks_retained_retry_payloads() -> Result<()> {
    let (instance, mut owner) =
        Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("owner"))
            .await?;
    let key = owner.get_default_key()?;
    let db = owner.create_database(Doc::new(), &key).await?;
    let tx = db.new_transaction().await?;
    tx.get_store::<DocStore>("test")
        .await?
        .set_string("value", "hello")
        .await?;
    let id = tx.commit().await?;
    let engine = instance.require_local_engine()?;
    let payload = vec![engine.get(&id).await?];
    utils::require_verified_entries(engine.as_ref(), &payload).await?;
    instance.demote_to_unverified(db.root_id(), &id).await?;
    assert!(
        utils::require_verified_entries(engine.as_ref(), &payload)
            .await
            .is_err()
    );
    engine
        .update_verification_status(&id, VerificationStatus::Failed)
        .await?;
    assert!(
        utils::require_verified_entries(engine.as_ref(), &payload)
            .await
            .is_err()
    );
    assert!(
        utils::require_verified_entries(engine.as_ref(), &[Entry::root_builder().build()?])
            .await
            .is_err()
    );
    utils::require_verified_entries(engine.as_ref(), &[]).await?;
    assert_eq!(
        engine.verified_snapshot(db.root_id()).await?,
        Snapshot::new(vec![db.root_id().clone()])
    );
    Ok(())
}
