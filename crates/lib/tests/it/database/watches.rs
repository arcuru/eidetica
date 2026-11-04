//! Latest-Snapshot watches on the selected native backend. Wire parity and
//! identity/revocation are exercised separately in service/notifications.rs.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use eidetica::{
    Database, Entry, Instance, Snapshot, WriteCallback, WriteSource,
    auth::crypto::{PrivateKey, sign_entry},
    backend::{VerificationStatus, database::InMemory},
    crdt::Doc,
    store::DocStore,
};
use tokio::sync::{Semaphore, mpsc};

use crate::helpers::test_backend;

async fn setup() -> (Instance, Database) {
    let instance = Instance::create_backend(
        test_backend().await,
        eidetica::NewAccount::passwordless("watch-test"),
    )
    .await
    .unwrap()
    .0;
    let db = Database::create(&instance, PrivateKey::generate(), Doc::new())
        .await
        .unwrap();
    (instance, db)
}

async fn put(db: &Database, value: &str) -> eidetica::ID {
    let tx = db.new_transaction().await.unwrap();
    tx.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("key", value)
        .await
        .unwrap();
    tx.commit().await.unwrap()
}

async fn watch(db: &Database) -> (WriteCallback, mpsc::UnboundedReceiver<Snapshot>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let cb = db
        .on_write(move |snapshot, readonly| {
            assert!(readonly.auth_identity().is_none());
            let _ = tx.send(snapshot.clone());
            async { Ok(()) }
        })
        .await
        .unwrap();
    (cb, rx)
}

async fn next(rx: &mut mpsc::UnboundedReceiver<Snapshot>) -> Snapshot {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("watch stalled")
        .expect("watch closed")
}

/// A real signed source chain. Withholding the first entry leaves the second
/// Unverified even when subscription/snapshot opportunistically verifies.
async fn chain() -> (Entry, Entry, Entry) {
    let source = Instance::create_backend(
        Box::new(InMemory::new()),
        eidetica::NewAccount::passwordless("source"),
    )
    .await
    .unwrap()
    .0;
    let db = Database::create(&source, PrivateKey::generate(), Doc::new())
        .await
        .unwrap();
    let first = put(&db, "first").await;
    let second = put(&db, "second").await;
    (
        source.backend().get(db.root_id()).await.unwrap(),
        source.backend().get(&first).await.unwrap(),
        source.backend().get(&second).await.unwrap(),
    )
}

async fn receiver(root: Entry) -> (Instance, Database) {
    let instance = Instance::create_backend(
        test_backend().await,
        eidetica::NewAccount::passwordless("watch-test"),
    )
    .await
    .unwrap()
    .0;
    let tree = root.id();
    instance
        .put_entry(
            &tree,
            VerificationStatus::Verified,
            root,
            WriteSource::Remote,
        )
        .await
        .unwrap();
    let db = Database::open(&instance, &tree).await.unwrap();
    (instance, db)
}

#[tokio::test]
async fn snapshot_watch_initial_quiet_and_registration_races() {
    let (_instance, db) = setup().await;
    let (_cb, mut rx) = watch(&db).await;
    assert_eq!(next(&mut rx).await, db.snapshot().await.unwrap());
    for i in 0..16 {
        let before = db.snapshot().await.unwrap();
        let value = i.to_string();
        let (registered, id) = tokio::join!(watch(&db), put(&db, &value));
        let (_cb, mut rx) = registered;
        let after = Snapshot::new(vec![id]);
        let initial = next(&mut rx).await;
        assert!(
            initial == before || initial == after,
            "initial must linearize with the write"
        );
        if initial == before {
            assert_eq!(next(&mut rx).await, after);
        }
        assert_eq!(db.snapshot().await.unwrap(), after);
    }
}

#[tokio::test]
async fn snapshot_watch_views_and_promotion_with_unchanged_raw_tips() {
    let (root, first, second) = chain().await;
    let (instance, db) = receiver(root).await;
    let opted = db.clone().allow_unverified();
    let (_verified, mut verified) = watch(&db).await;
    let (_opted, mut unverified) = watch(&opted).await;
    let checkpoint = next(&mut verified).await;
    assert_eq!(next(&mut unverified).await, checkpoint);

    assert_eq!(db.insert_raw(second.clone()).await.unwrap(), second.id());
    assert_eq!(
        next(&mut unverified).await,
        Snapshot::new(vec![db.root_id().clone(), second.id()])
    );
    assert_eq!(
        instance
            .backend()
            .local_engine()
            .unwrap()
            .get_verification_status(&second.id())
            .await
            .unwrap(),
        VerificationStatus::Unverified
    );
    assert_eq!(
        opted.snapshot().await.unwrap(),
        Snapshot::new(vec![db.root_id().clone(), second.id()])
    );
    assert_eq!(db.snapshot().await.unwrap(), checkpoint);

    instance
        .put_entry(
            db.root_id(),
            VerificationStatus::Unverified,
            first.clone(),
            WriteSource::Remote,
        )
        .await
        .unwrap();
    assert_eq!(
        next(&mut unverified).await,
        Snapshot::new(vec![second.id()])
    );
    let raw = instance.backend().snapshot(db.root_id()).await.unwrap();
    let report = db.verify().await.unwrap();
    assert_eq!(report.verified, 2);
    let latest = next(&mut verified).await;
    assert_eq!(latest, Snapshot::new(vec![second.id()]));
    assert_eq!(
        instance.backend().snapshot(db.root_id()).await.unwrap(),
        raw
    );
    assert_eq!(
        db.ids_added(&checkpoint, &latest).await.unwrap(),
        vec![first.id(), second.id()]
    );
    assert!(
        unverified.try_recv().is_err(),
        "promotion must not change the opt-in frontier"
    );
}

/// Replaces the old should_panic regression. No auto-verifying snapshot read
/// can heal this construction: the held entry has a definitively bad signature.
#[tokio::test]
async fn snapshot_watch_excludes_unverified_and_failed_restores_parent() {
    let (root, first, _) = chain().await;
    let bad = first.clone().with_auth(|auth| {
        auth.signature = Some(sign_entry(&first, &PrivateKey::generate()).unwrap());
    });
    let (instance, db) = receiver(root).await;
    let opted = db.clone().allow_unverified();
    let (_v, mut verified) = watch(&db).await;
    let (_u, mut unverified) = watch(&opted).await;
    let checkpoint = next(&mut verified).await;
    assert_eq!(next(&mut unverified).await, checkpoint);
    instance
        .put_entry(
            db.root_id(),
            VerificationStatus::Unverified,
            bad.clone(),
            WriteSource::Remote,
        )
        .await
        .unwrap();
    let historical = next(&mut unverified).await;
    assert_eq!(historical, Snapshot::new(vec![bad.id()]));
    assert_eq!(
        instance
            .backend()
            .local_engine()
            .unwrap()
            .get_verification_status(&bad.id())
            .await
            .unwrap(),
        VerificationStatus::Unverified
    );
    assert_eq!(db.verify().await.unwrap().failed, 1);
    assert_eq!(next(&mut unverified).await, checkpoint);
    assert_eq!(
        opted.snapshot().await.unwrap(),
        checkpoint,
        "rejection restores the surviving parent, not EMPTY"
    );
    // Selection does not change immutable Snapshot materialization or cache identity.
    let tx = opted.new_transaction_at(&historical).await.unwrap();
    assert_eq!(
        tx.get_store::<DocStore>("data")
            .await
            .unwrap()
            .get_string("key")
            .await
            .unwrap(),
        "first"
    );
    assert_eq!(
        instance
            .backend()
            .local_engine()
            .unwrap()
            .get_verification_status(&bad.id())
            .await
            .unwrap(),
        VerificationStatus::Failed
    );
    instance
        .put_entry(
            db.root_id(),
            VerificationStatus::Verified,
            first.clone(),
            WriteSource::Local,
        )
        .await
        .unwrap();
    let latest = next(&mut verified).await;
    assert_eq!(latest, Snapshot::new(vec![first.id()]));
    assert_eq!(
        db.ids_added(&checkpoint, &latest).await.unwrap(),
        vec![first.id()]
    );
    assert!(!latest.tips().contains(&bad.id()));
    assert!(
        verified.try_recv().is_err(),
        "no empty event for insertion or failure"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_watch_slow_initial_latest_wins_and_caller_checkpoint() {
    let (_instance, db) = setup().await;
    let initial = db.snapshot().await.unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let first = Arc::new(AtomicBool::new(true));
    let _slow = db
        .on_write({
            let release = release.clone();
            move |snapshot, _| {
                let snapshot = snapshot.clone();
                let tx = tx.clone();
                let release = release.clone();
                let first = first.swap(false, Ordering::SeqCst);
                async move {
                    tx.send(snapshot).unwrap();
                    if first {
                        release.acquire().await.unwrap().forget();
                    }
                    Ok(())
                }
            }
        })
        .await
        .unwrap();
    assert_eq!(next(&mut rx).await, initial);
    let (_fast, mut fast) = watch(&db).await;
    assert_eq!(next(&mut fast).await, initial);
    let mut ids = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        for i in 0..32 {
            ids.push(put(&db, &i.to_string()).await);
        }
    })
    .await
    .expect("commits awaited a slow application callback");
    let final_snapshot = db.snapshot().await.unwrap();
    while next(&mut fast).await != final_snapshot {}
    release.add_permits(1);
    let latest = next(&mut rx).await;
    assert_eq!(
        latest, final_snapshot,
        "one pending latest Snapshot, not a transition queue"
    );
    assert_eq!(db.ids_added(&initial, &latest).await.unwrap(), ids);
    assert!(rx.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_watch_reentrant_read_and_commit() {
    let (_instance, db) = setup().await;
    let writing_db = db.clone();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let once = Arc::new(AtomicBool::new(true));
    let _cb = db
        .on_write(move |_, readonly| {
            let db = writing_db.clone();
            let readonly = readonly.clone();
            let tx = tx.clone();
            let first = once.swap(false, Ordering::SeqCst);
            async move {
                readonly.verify().await?;
                readonly.snapshot().await?;
                if first {
                    tx.send(put(&db, "from callback").await).unwrap();
                }
                Ok(())
            }
        })
        .await
        .unwrap();
    let id = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(db.snapshot().await.unwrap(), Snapshot::new(vec![id]));
}

#[tokio::test]
async fn snapshot_watch_error_and_panic_are_isolated_and_retry_next_change() {
    for panic in [false, true] {
        let (_instance, db) = setup().await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let first = Arc::new(AtomicBool::new(true));
        let _cb = db
            .on_write(move |snapshot, _| {
                let tx = tx.clone();
                let snapshot = snapshot.clone();
                let first = first.swap(false, Ordering::SeqCst);
                async move {
                    tx.send(snapshot).unwrap();
                    if first {
                        assert!(!panic, "intentional callback panic");
                        return Err(std::io::Error::other("intentional callback error").into());
                    }
                    Ok(())
                }
            })
            .await
            .unwrap();
        next(&mut rx).await;
        let id = put(&db, "after failure").await;
        assert_eq!(next(&mut rx).await, Snapshot::new(vec![id]));
    }
}

struct Cancelled(mpsc::UnboundedSender<()>);
impl Drop for Cancelled {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

#[tokio::test]
async fn snapshot_watch_drop_cancels_inflight_and_only_its_registration() {
    let (_instance, db) = setup().await;
    let (started, mut start) = mpsc::unbounded_channel();
    let (cancelled, mut cancel) = mpsc::unbounded_channel();
    let cb = db
        .on_write(move |_, _| {
            let started = started.clone();
            let guard = Cancelled(cancelled.clone());
            async move {
                let _guard = guard;
                started.send(()).unwrap();
                std::future::pending::<()>().await;
                Ok(())
            }
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), start.recv())
        .await
        .unwrap()
        .unwrap();
    let (keep, mut rx) = watch(&db).await;
    next(&mut rx).await;
    drop(cb);
    tokio::time::timeout(Duration::from_secs(5), cancel.recv())
        .await
        .unwrap()
        .unwrap();
    keep.detach();
    let id = put(&db, "after cancellation").await;
    assert_eq!(next(&mut rx).await, Snapshot::new(vec![id]));
    assert!(start.try_recv().is_err());
}
