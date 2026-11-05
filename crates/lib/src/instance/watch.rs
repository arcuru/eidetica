//! Per-registration latest-Snapshot dispatch. Only mailbox updates run under
//! the tree lock; even the synchronous prefix of a user closure runs here.

use std::sync::{Arc, Mutex};

use tokio::{
    sync::{oneshot, watch},
    task::{AbortHandle, JoinSet},
};

use super::{AsyncWriteCallbackFn, CallbackId};
use crate::{Database, Snapshot};

pub(super) struct PerDbCallbackEntry {
    pub(super) id: CallbackId,
    pub(super) database: Database,
    state: Mutex<Mailbox>,
    task: AbortHandle,
}

struct Mailbox {
    last: Option<Snapshot>,
    initial: Option<oneshot::Sender<Snapshot>>,
    latest: watch::Sender<Snapshot>,
}

impl PerDbCallbackEntry {
    pub(super) fn new(
        id: CallbackId,
        database: Database,
        callback: AsyncWriteCallbackFn,
    ) -> Arc<Self> {
        let (initial, first) = oneshot::channel();
        let (latest, updates) = watch::channel(Snapshot::EMPTY);
        let task = tokio::spawn(run(database.clone(), callback, first, updates)).abort_handle();
        Arc::new(Self {
            id,
            database,
            state: Mutex::new(Mailbox {
                last: None,
                initial: Some(initial),
                latest,
            }),
            task,
        })
    }

    pub(super) fn cancel(&self) {
        self.task.abort();
    }

    /// Called in tree order (or wire order for a remote registration).
    /// The first Snapshot is never replaced by a subsequent update. Thereafter
    /// only one pending Snapshot is retained, irrespective of callback speed.
    pub(super) fn offer(&self, snapshot: Snapshot) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.last.as_ref() == Some(&snapshot) {
            return;
        }
        state.last = Some(snapshot.clone());
        if let Some(initial) = state.initial.take() {
            let _ = initial.send(snapshot);
        } else {
            state.latest.send_replace(snapshot);
        }
    }
}

impl Drop for PerDbCallbackEntry {
    fn drop(&mut self) {
        self.cancel();
    }
}

async fn run(
    database: Database,
    callback: AsyncWriteCallbackFn,
    initial: oneshot::Receiver<Snapshot>,
    mut updates: watch::Receiver<Snapshot>,
) {
    let Ok(mut snapshot) = initial.await else {
        return;
    };
    // JoinSet both contains user panics and aborts an in-flight closure if
    // cancellation drops this worker. No callback tasks outlive registration.
    let mut calls = JoinSet::new();
    loop {
        let Ok(instance) = database.instance() else {
            return;
        };
        let lock = instance.tree_lock(database.root_id());
        // Publication wakes us under this lock. Cross the lock boundary before
        // user work, without holding a guard while invoking or awaiting it.
        drop(lock.lock().await);
        drop(instance);
        let db = database.clone();
        let cb = callback.clone();
        calls.spawn(async move { cb(&snapshot, &db).await });
        match calls.join_next().await {
            Some(Ok(Ok(()))) => {}
            result => {
                tracing::error!(tree_id = %database.root_id(), ?result, "Snapshot callback failed")
            }
        }
        if updates.changed().await.is_err() {
            return;
        }
        snapshot = updates.borrow_and_update().clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Instance, NewAccount, auth::crypto::PrivateKey, backend::database::InMemory, crdt::Doc,
        entry::ID,
    };

    #[tokio::test]
    async fn snapshot_watch_mailbox_suppresses_unchanged_and_retains_only_latest() {
        let (instance, _) =
            Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("watch"))
                .await
                .unwrap();
        let db = Database::create(&instance, PrivateKey::generate(), Doc::new())
            .await
            .unwrap();
        let initial = db.snapshot().await.unwrap();
        let entry = PerDbCallbackEntry::new(
            CallbackId(1),
            db,
            Arc::new(|_, _| Box::pin(std::future::pending())),
        );
        let mut pending = entry.state.lock().unwrap().latest.subscribe();
        entry.offer(initial.clone());
        entry.offer(initial);
        assert!(
            !pending.has_changed().unwrap(),
            "unchanged selected frontier queued an event"
        );
        let mut latest = Snapshot::EMPTY;
        for i in 0..4096 {
            latest = Snapshot::new(vec![ID::from_bytes(i.to_string())]);
            entry.offer(latest.clone());
        }
        assert_eq!(
            *pending.borrow_and_update(),
            latest,
            "pending storage is a single replacement slot, not a FIFO"
        );
        assert!(!pending.has_changed().unwrap());
        entry.offer(latest);
        assert!(!pending.has_changed().unwrap());
    }

    #[tokio::test]
    async fn snapshot_watch_initial_cannot_be_replaced_before_first_poll() {
        let (instance, _) =
            Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("watch"))
                .await
                .unwrap();
        let db = Database::create(&instance, PrivateKey::generate(), Doc::new())
            .await
            .unwrap();
        let initial = db.snapshot().await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let entry = PerDbCallbackEntry::new(
            CallbackId(1),
            db,
            Arc::new(move |snapshot, _| {
                let _ = tx.send(snapshot.clone());
                Box::pin(async { Ok(()) })
            }),
        );
        entry.offer(initial.clone());
        let mut latest = Snapshot::EMPTY;
        // Single-thread runtime and no yield: every offer precedes the first
        // poll of the callback worker, not merely a slow in-flight callback.
        for i in 0..64 {
            latest = Snapshot::new(vec![ID::from_bytes(i.to_string())]);
            entry.offer(latest.clone());
        }
        let timeout = std::time::Duration::from_secs(5);
        assert_eq!(
            tokio::time::timeout(timeout, rx.recv())
                .await
                .unwrap()
                .unwrap(),
            initial
        );
        assert_eq!(
            tokio::time::timeout(timeout, rx.recv())
                .await
                .unwrap()
                .unwrap(),
            latest
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn snapshot_watch_sync_prefix_and_future_cross_tree_lock_boundary() {
        let (instance, _) =
            Instance::create_backend(Box::new(InMemory::new()), NewAccount::passwordless("watch"))
                .await
                .unwrap();
        let db = Database::create(&instance, PrivateKey::generate(), Doc::new())
            .await
            .unwrap();
        let lock = instance.tree_lock(db.root_id());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let _cb = db
            .on_write(move |snapshot, db| {
                let lock = db.instance().unwrap().tree_lock(db.root_id());
                assert!(
                    lock.try_lock().is_ok(),
                    "user prefix owns the publisher's tree lock"
                );
                let tx = tx.clone();
                let snapshot = snapshot.clone();
                let db = db.clone();
                async move {
                    db.verify().await?;
                    tx.send(snapshot).unwrap();
                    Ok(())
                }
            })
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let guard = lock.lock().await;
        let selected = Snapshot::new(vec![ID::from_bytes("next")]);
        let callbacks = instance
            .inner
            .write_callbacks
            .lock()
            .unwrap()
            .get(db.root_id())
            .unwrap()
            .clone();
        callbacks[0].offer(selected.clone());
        // Explicitly hold publication's lock across a scheduling opportunity.
        tokio::task::yield_now().await;
        assert!(rx.try_recv().is_err());
        drop(guard);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap(),
            selected
        );
    }
}
