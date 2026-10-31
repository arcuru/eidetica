//! Advisory, in-memory leases shared by one live [`Instance`].
//!
//! Leases do not fence database writes or external effects. Separate Instances
//! (even on the same storage) and separate daemons do not coordinate. There is
//! no persistence, TTL, renewal, fairness or automatic reacquisition after loss.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use uuid::Uuid;

#[cfg(all(unix, feature = "service"))]
use crate::auth::SigKey;
use crate::{Database, Instance, Result, WeakInstance, entry::ID};

/// Maximum UTF-8 byte length of an opaque lease name (also enforced on the wire).
pub const MAX_LEASE_NAME_BYTES: usize = 256;

/// The cooperative holder, not a grant token or an external authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LeaseOwner {
    /// The Instance's one implicit embedded client.
    Embedded,
    /// One authenticated connection, independent of its username.
    Connection(Uuid),
}

/// Live observation only. Neither the holder nor generation proves ownership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseStatus {
    /// Current cooperative holder, if any.
    pub holder: Option<LeaseOwner>,
    /// Registry-local observation generation; not a durable fencing term.
    pub generation: u64,
}

/// Result of a non-blocking acquisition.
#[derive(Debug)]
pub enum LeaseAcquire {
    /// The only current grant for this key.
    Granted(LeaseGuard),
    /// Another client holds the key.
    Held(LeaseStatus),
    /// This client already holds it; no second independently releasable guard.
    AlreadyHeld(LeaseStatus),
}

/// Wire acquisition result. Grant IDs are opaque, random and owner-only.
#[doc(hidden)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LeaseAcquireResponse {
    Granted { grant_id: Uuid },
    Held(LeaseStatus),
    AlreadyHeld(LeaseStatus),
}

/// Handle to one `(database root, opaque name)` lease.
#[derive(Debug, Clone)]
pub struct Lease {
    instance: WeakInstance,
    pub(crate) key: LeaseKey,
    #[cfg(all(unix, feature = "service"))]
    identity: SigKey,
}

impl Lease {
    pub(crate) fn new(database: &Database, name: String) -> Self {
        Self {
            instance: database.weak_instance(),
            key: LeaseKey {
                root: database.root_id().clone(),
                name,
            },
            #[cfg(all(unix, feature = "service"))]
            identity: database.ops().remote_identity(),
        }
    }

    fn instance(&self) -> Result<Instance> {
        validate_name(&self.key.name)?;
        self.instance
            .upgrade()
            .ok_or_else(|| crate::instance::InstanceError::InstanceDropped.into())
    }

    /// Observe current daemon-local state. Read permission is required remotely.
    pub async fn status(&self) -> Result<LeaseStatus> {
        let instance = self.instance()?;
        #[cfg(all(unix, feature = "service"))]
        if let Some(conn) = instance.remote_connection() {
            return conn.lease_status(&self.key, self.identity.clone()).await;
        }
        require_database(&instance, &self.key.root).await?;
        Ok(instance.lease_registry().status(&self.key))
    }

    /// Atomically try once. Remote clients require Write permission.
    pub async fn try_acquire(&self) -> Result<LeaseAcquire> {
        let instance = self.instance()?;
        #[cfg(all(unix, feature = "service"))]
        if let Some(conn) = instance.remote_connection() {
            let response = conn.lease_acquire(&self.key, self.identity.clone()).await?;
            return Ok(self.acquisition(response, Some(conn.lease_loss_receiver())));
        }
        require_database(&instance, &self.key.root).await?;
        let (response, loss) = instance
            .lease_registry()
            .acquire(&self.key, LeaseOwner::Embedded);
        Ok(self.acquisition(response, loss))
    }

    fn acquisition(
        &self,
        response: LeaseAcquireResponse,
        loss: Option<watch::Receiver<bool>>,
    ) -> LeaseAcquire {
        match response {
            LeaseAcquireResponse::Granted { grant_id } => LeaseAcquire::Granted(LeaseGuard {
                lease: Box::new(self.clone()),
                grant_id,
                loss: loss.expect("granted acquisitions have a loss receiver"),
                active: true,
                #[cfg(all(unix, feature = "service"))]
                cleanup: self
                    .instance
                    .upgrade()
                    .and_then(|i| i.remote_connection())
                    .map(|conn| {
                        conn.lease_cleanup_on_drop(
                            self.key.clone(),
                            self.identity.clone(),
                            None,
                            Some(grant_id),
                        )
                    }),
            }),
            LeaseAcquireResponse::Held(status) => LeaseAcquire::Held(status),
            LeaseAcquireResponse::AlreadyHeld(status) => LeaseAcquire::AlreadyHeld(status),
        }
    }

    /// Subscribe and obtain initial status in the same registry critical section.
    /// Dropping the watch unregisters it (best-effort over a live socket).
    pub async fn watch(&self) -> Result<LeaseWatch> {
        let instance = self.instance()?;
        let id = Uuid::new_v4();
        let (tx, rx) = watch::channel(None);
        // Construct cleanup before the first wire await, including cancellation.
        #[allow(unused_mut)]
        let mut watcher = LeaseWatch {
            lease: self.clone(),
            id,
            rx,
            #[cfg(all(unix, feature = "service"))]
            cleanup: None,
        };
        #[cfg(all(unix, feature = "service"))]
        if let Some(conn) = instance.remote_connection() {
            watcher.cleanup = Some(conn.lease_cleanup_on_drop(
                self.key.clone(),
                self.identity.clone(),
                Some(id),
                None,
            ));
            conn.lease_watch(&self.key, self.identity.clone(), id, tx)
                .await?;
            watcher.rx.borrow_and_update();
            return Ok(watcher);
        }
        require_database(&instance, &self.key.root).await?;
        instance.lease_registry().watch(
            &self.key,
            LeaseOwner::Embedded,
            id,
            Arc::new(move |status| {
                tx.send_replace(Some(status));
            }),
        );
        watcher.rx.borrow_and_update();
        Ok(watcher)
    }

    /// Subscribe, try, and retry on changes on the client. Contenders race;
    /// this never blocks the daemon's per-connection dispatch loop.
    pub async fn wait_acquire(&self) -> Result<LeaseGuard> {
        let mut watcher = self.watch().await?;
        loop {
            match self.try_acquire().await? {
                LeaseAcquire::Granted(guard) => return Ok(guard),
                LeaseAcquire::AlreadyHeld(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "this client already holds the lease",
                    )
                    .into());
                }
                LeaseAcquire::Held(_) => {
                    watcher.changed().await?;
                }
            }
        }
    }

    async fn release(&self, grant_id: Uuid) -> Result<()> {
        let instance = self.instance()?;
        #[cfg(all(unix, feature = "service"))]
        if let Some(conn) = instance.remote_connection() {
            return conn
                .lease_release(&self.key, self.identity.clone(), grant_id)
                .await;
        }
        if instance
            .lease_registry()
            .release(&self.key, LeaseOwner::Embedded, grant_id)
        {
            Ok(())
        } else {
            Err(invalid_grant())
        }
    }
}

/// A grant-specific advisory owner guard. Not cloneable.
///
/// Stop side-effecting work when [`Self::lost`] resolves or [`Self::is_lost`]
/// becomes true. Remote Drop is best-effort, not a synchronous wire release;
/// call [`Self::release`] to await acknowledgment. Connection cleanup is the
/// authoritative fallback. A stalled but connected holder is never evicted.
#[must_use = "dropping the guard releases the advisory lease"]
pub struct LeaseGuard {
    lease: Box<Lease>,
    grant_id: Uuid,
    loss: watch::Receiver<bool>,
    active: bool,
    #[cfg(all(unix, feature = "service"))]
    cleanup: Option<Box<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for LeaseGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseGuard")
            .field("key", &self.lease.key)
            .field("lost", &self.is_lost())
            .finish_non_exhaustive()
    }
}

impl LeaseGuard {
    /// Whether this guard was released, its Instance vanished, or transport died.
    pub fn is_lost(&self) -> bool {
        !self.active
            || self.lease.instance.upgrade().is_none()
            || *self.loss.borrow()
            || self.loss.has_changed().is_err()
    }

    /// Wait for loss. This does not renew, reconnect or reacquire the lease.
    pub async fn lost(&mut self) {
        while !self.is_lost() {
            if self.loss.changed().await.is_err() {
                break;
            }
        }
    }

    /// Release this exact grant and await acknowledgment. A revoked database
    /// permission does not prevent the owning connection from releasing it.
    pub async fn release(mut self) -> Result<()> {
        self.lease.release(self.grant_id).await?;
        self.active = false;
        Ok(())
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        #[cfg(all(unix, feature = "service"))]
        if let Some(cleanup) = &self.cleanup {
            cleanup();
            return;
        }
        let Some(instance) = self.lease.instance.upgrade() else {
            return;
        };
        instance
            .lease_registry()
            .release(&self.lease.key, LeaseOwner::Embedded, self.grant_id);
    }
}

/// A coalescing observation stream. Initial state and changes are reconciled by
/// generation, not by response/notification arrival order. No event is authority.
#[must_use = "dropping a lease watch unregisters it"]
pub struct LeaseWatch {
    lease: Lease,
    id: Uuid,
    rx: watch::Receiver<Option<LeaseStatus>>,
    #[cfg(all(unix, feature = "service"))]
    cleanup: Option<Box<dyn Fn() + Send + Sync>>,
}

impl LeaseWatch {
    /// Latest observed status; it can be stale and never proves ownership.
    pub fn status(&self) -> LeaseStatus {
        self.rx
            .borrow()
            .clone()
            .expect("watch returns only after initial status")
    }

    /// Await a change. Connection or Instance loss returns an error, not a free
    /// status that might incorrectly be interpreted as an ownership grant.
    pub async fn changed(&mut self) -> Result<LeaseStatus> {
        if self.rx.has_changed().is_err() {
            return Err(connection_lost());
        }
        self.rx.changed().await.map_err(|_| connection_lost())?;
        if self.lease.instance.upgrade().is_none() || self.rx.has_changed().is_err() {
            return Err(connection_lost());
        }
        Ok(self
            .rx
            .borrow_and_update()
            .clone()
            .expect("initialized lease watch"))
    }
}

impl Drop for LeaseWatch {
    fn drop(&mut self) {
        #[cfg(all(unix, feature = "service"))]
        if let Some(cleanup) = &self.cleanup {
            cleanup();
            return;
        }
        let Some(instance) = self.lease.instance.upgrade() else {
            return;
        };
        instance
            .lease_registry()
            .unwatch(&self.lease.key, LeaseOwner::Embedded, self.id);
    }
}

pub(crate) fn validate_name(name: &str) -> Result<()> {
    if name.len() > MAX_LEASE_NAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "lease name exceeds 256 bytes",
        )
        .into());
    }
    Ok(())
}

async fn require_database(instance: &Instance, root: &ID) -> Result<()> {
    if !instance.has_database(root).await {
        return Err(crate::instance::InstanceError::DatabaseNotFound {
            name: root.to_string(),
        }
        .into());
    }
    Ok(())
}

pub(crate) fn invalid_grant() -> crate::Error {
    crate::auth::AuthError::PermissionDenied {
        reason: "lease grant or watch does not belong to this connection".into(),
    }
    .into()
}

fn connection_lost() -> crate::Error {
    std::io::Error::new(
        std::io::ErrorKind::ConnectionAborted,
        "lease observation connection lost",
    )
    .into()
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct LeaseKey {
    pub(crate) root: ID,
    pub(crate) name: String,
}

struct Grant {
    id: Uuid,
    owner: LeaseOwner,
    loss: watch::Sender<bool>,
}

type WatchSender = Arc<dyn Fn(LeaseStatus) + Send + Sync>;

#[derive(Default)]
struct State {
    generation: u64,
    grant: Option<Grant>,
    // Internal nonblocking sends only, never a user callback or an await.
    watchers: HashMap<(LeaseOwner, Uuid), WatchSender>,
}

impl State {
    fn status(&self) -> LeaseStatus {
        LeaseStatus {
            holder: self.grant.as_ref().map(|g| g.owner),
            generation: self.generation,
        }
    }

    fn changed(&mut self) {
        self.generation += 1;
        let status = self.status();
        for send in self.watchers.values() {
            send(status.clone());
        }
    }

    fn release(&mut self) {
        if let Some(grant) = self.grant.take() {
            grant.loss.send_replace(true);
        }
        self.changed();
    }
}

/// One registry per shared Instance. All transitions and watch registration are
/// synchronous under this lock; no CRDT data or per-tree write locks participate.
#[derive(Default)]
pub(crate) struct LeaseRegistry(Mutex<HashMap<LeaseKey, State>>);

impl LeaseRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<LeaseKey, State>> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn status(&self, key: &LeaseKey) -> LeaseStatus {
        self.lock()
            .get(key)
            .map(State::status)
            .unwrap_or(LeaseStatus {
                holder: None,
                generation: 0,
            })
    }

    pub(crate) fn acquire(
        &self,
        key: &LeaseKey,
        owner: LeaseOwner,
    ) -> (LeaseAcquireResponse, Option<watch::Receiver<bool>>) {
        let mut registry = self.lock();
        let state = registry.entry(key.clone()).or_default();
        if let Some(grant) = &state.grant {
            return (
                if grant.owner == owner {
                    LeaseAcquireResponse::AlreadyHeld(state.status())
                } else {
                    LeaseAcquireResponse::Held(state.status())
                },
                None,
            );
        }
        let id = Uuid::new_v4();
        let (loss, rx) = watch::channel(false);
        state.grant = Some(Grant { id, owner, loss });
        state.changed();
        (LeaseAcquireResponse::Granted { grant_id: id }, Some(rx))
    }

    pub(crate) fn release(&self, key: &LeaseKey, owner: LeaseOwner, id: Uuid) -> bool {
        let mut registry = self.lock();
        let Some(state) = registry.get_mut(key) else {
            return false;
        };
        if !state
            .grant
            .as_ref()
            .is_some_and(|g| g.owner == owner && g.id == id)
        {
            return false;
        }
        state.release();
        true
    }

    pub(crate) fn watch(
        &self,
        key: &LeaseKey,
        owner: LeaseOwner,
        id: Uuid,
        send: WatchSender,
    ) -> LeaseStatus {
        let mut registry = self.lock();
        let state = registry.entry(key.clone()).or_default();
        let send = state.watchers.entry((owner, id)).or_insert(send).clone();
        let status = state.status();
        send(status.clone());
        status
    }

    pub(crate) fn unwatch(&self, key: &LeaseKey, owner: LeaseOwner, id: Uuid) -> bool {
        let mut registry = self.lock();
        let Some(state) = registry.get_mut(key) else {
            return false;
        };
        state.watchers.remove(&(owner, id)).is_some()
    }

    #[cfg(all(unix, feature = "service"))]
    pub(crate) fn disconnect(&self, owner: LeaseOwner) {
        for state in self.lock().values_mut() {
            state
                .watchers
                .retain(|(watch_owner, _), _| *watch_owner != owner);
            if state.grant.as_ref().is_some_and(|g| g.owner == owner) {
                state.release();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_lease_registry_watch_coalesces_transitions_and_cleans_disconnect() {
        let registry = LeaseRegistry::default();
        let key = LeaseKey {
            root: ID::from_bytes("db"),
            name: "runtime".into(),
        };
        let owner = LeaseOwner::Connection(Uuid::new_v4());
        let standby = LeaseOwner::Connection(Uuid::new_v4());
        let (tx, mut rx) = watch::channel(None);
        let id = Uuid::new_v4();
        let initial = registry.watch(
            &key,
            standby,
            id,
            Arc::new(move |status| {
                tx.send_replace(Some(status));
            }),
        );
        assert_eq!(initial.generation, 0);
        rx.borrow_and_update();
        let (LeaseAcquireResponse::Granted { grant_id }, _) = registry.acquire(&key, owner) else {
            panic!("missing grant");
        };
        assert!(registry.release(&key, owner, grant_id));
        let (_, loss) = registry.acquire(&key, owner);
        // Release and reacquire before the observer polls still cause a wakeup
        // with the latest state, not a promise that the observer owns anything.
        rx.changed().await.unwrap();
        assert_eq!(rx.borrow_and_update().as_ref().unwrap().generation, 3);
        assert_eq!(rx.borrow().as_ref().unwrap().holder, Some(owner));
        #[cfg(all(unix, feature = "service"))]
        {
            registry.disconnect(owner);
            let mut loss = loss.unwrap();
            loss.changed().await.unwrap();
            assert!(*loss.borrow());
            assert_eq!(registry.status(&key).holder, None);
            registry.disconnect(standby);
            assert!(registry.lock()[&key].watchers.is_empty());
            assert!(rx.has_changed().is_err());
        }
        #[cfg(not(all(unix, feature = "service")))]
        let _ = loss;
    }

    #[tokio::test]
    async fn test_lease_watch_drop_unregisters_embedded() {
        let (instance, user) =
            Instance::connect_or_create("memory://", crate::NewAccount::passwordless("admin"))
                .await
                .unwrap();
        let mut user = user.unwrap();
        let key = user.get_default_key().unwrap();
        let db = user
            .create_database(crate::crdt::Doc::new(), &key)
            .await
            .unwrap();
        let lease = db.lease("runtime");
        let watcher = lease.watch().await.unwrap();
        assert_eq!(
            instance.lease_registry().lock()[&lease.key].watchers.len(),
            1
        );
        drop(watcher);
        assert!(
            instance.lease_registry().lock()[&lease.key]
                .watchers
                .is_empty()
        );
    }
}
