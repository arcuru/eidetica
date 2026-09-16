//! User-scoped database sharing and status management.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use super::{SyncSettings, TrackedDatabase, User, UserError};
use crate::{
    Database, Result,
    auth::{Permission, SigKey, crypto::PrivateKey},
    entry::ID,
    instance::WriteCallback,
    sync::{Address, DatabaseTicket, PeerId},
};

/// Confirmation that a sharing preference was durably accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreferenceWriteReceipt {
    /// ID of the signed preference entry, absent when the value was already current.
    pub entry_id: Option<ID>,
}

/// Outcome of a sharing-preference write.
#[derive(Debug)]
pub enum PreferenceWriteOutcome {
    /// The signed preference entry was acknowledged as durable.
    Written(PreferenceWriteReceipt),
    /// The connection was lost after submission may have reached the owner.
    /// Read the current preference or retry the idempotent write.
    Unknown { source: crate::Error },
}

impl PreferenceWriteOutcome {
    /// Receipt for an acknowledged write, or the original error when unknown.
    pub fn into_result(self) -> std::result::Result<PreferenceWriteReceipt, crate::Error> {
        match self {
            Self::Written(receipt) => Ok(receipt),
            Self::Unknown { source } => Err(source),
        }
    }
}

/// Whether owner-side reconciliation includes the current preference snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppliedState {
    /// The owner has processed the caller's current preference state.
    Current,
    /// The owner has not processed the caller's current preference state yet.
    Pending,
    /// Applied state cannot currently be established.
    Unknown,
}

/// Runtime freshness relative to the connection that produced the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeFreshness {
    /// Runtime observations belong to the current owner run.
    Current,
    /// The cached observation belongs to a disconnected or replaced run.
    Stale,
    /// No owner runtime observation is available.
    Unknown,
}

/// Per-peer observation filtered to the managed database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerObservation {
    /// Peer identity.
    pub peer: PeerId,
    /// When this owner run last completed a sync round involving this database.
    pub last_success_ms: Option<u64>,
    /// When this peer observation was assembled.
    pub observed_at_ms: u64,
}

/// Owner runtime state visible through a database-scoped management view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseObservation {
    /// Opaque identity for one owner process run.
    pub owner_run: String,
    /// Monotonic generation for this database within `owner_run`.
    pub generation: u64,
    /// Freshness of the runtime fields in this observation.
    pub freshness: RuntimeFreshness,
    /// Whether the owner currently has a sync engine.
    pub engine_running: bool,
    /// Addresses currently advertised by running transports.
    pub listen_addresses: Vec<Address>,
    /// Only peers registered for this database.
    pub peers: Vec<PeerObservation>,
    /// Time the owner assembled this observation.
    pub observed_at_ms: u64,
}

/// Current desired, applied and observed state for one tracked database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseManagementSnapshot {
    /// This user's durable desired setting.
    pub desired: SyncSettings,
    /// Owner-combined effective setting across all users.
    pub effective: Option<SyncSettings>,
    /// Whether the current desired snapshot has been processed by the owner.
    pub applied: AppliedState,
    /// Current owner runtime observation.
    pub observed: DatabaseObservation,
}

/// Reason a ticket cannot currently be produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TicketNotReady {
    /// No owner sync engine is running.
    EngineUnavailable,
    /// The caller's current desired state has not been applied.
    DesiredNotApplied,
    /// Effective owner configuration does not serve the database.
    SharingDisabled,
    /// No running transport is advertising an address.
    NoLiveAddress,
    /// Runtime state is stale or unknown.
    RuntimeUnknown,
}

/// Result of a ticket readiness query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TicketStatus {
    /// The owner is serving the database at the ticket's current addresses.
    Ready(DatabaseTicket),
    /// The owner has not reached a ticket-ready state.
    NotReady(TicketNotReady),
}

/// Stream of current database-management state.
pub struct DatabaseManagementWatch {
    receiver: watch::Receiver<DatabaseManagementSnapshot>,
    _callbacks: Vec<WriteCallback>,
    #[cfg(all(unix, feature = "service"))]
    _management_subscription: Option<crate::service::client::ManagementSubscription>,
    _task: tokio::task::JoinHandle<()>,
}

impl DatabaseManagementWatch {
    /// Current snapshot, available immediately after watch creation.
    pub fn current(&self) -> DatabaseManagementSnapshot {
        self.receiver.borrow().clone()
    }

    /// Wait for and return the next coalesced snapshot.
    pub async fn changed(&mut self) -> Result<DatabaseManagementSnapshot> {
        self.receiver.changed().await.map_err(|_| {
            crate::Error::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "database management watch ended",
            ))
        })?;
        Ok(self.current())
    }
}

/// User-bound management capability for one tracked database.
#[derive(Clone)]
pub struct DatabaseManagement {
    user_uuid: String,
    user_database: Database,
    target_database: Database,
    target_id: ID,
    signing_key: PrivateKey,
    identity: SigKey,
}

impl DatabaseManagement {
    pub(crate) async fn new(user: &User, database_id: &ID) -> Result<Self> {
        let tracked = user.database(database_id).await?;
        let signing_key = user.get_signing_key(&tracked.key_id)?;
        let identity = user
            .key_mapping(&tracked.key_id, database_id)?
            .ok_or_else(|| UserError::NoSigKeyMapping {
                key_id: tracked.key_id.to_string(),
                database_id: database_id.clone(),
            })?;
        let target_database = user
            .open_database_with_key(database_id, &tracked.key_id)
            .await?;
        if target_database.current_permission().await? < Permission::Read {
            return Err(UserError::InsufficientPermissions.into());
        }

        Ok(Self {
            user_uuid: user.user_uuid().to_string(),
            user_database: user.user_database().clone(),
            target_database,
            target_id: database_id.clone(),
            signing_key,
            identity,
        })
    }

    /// Save this user's sharing enablement without waiting for owner application.
    pub async fn set_sharing(&self, enabled: bool) -> Result<PreferenceWriteOutcome> {
        let tx = self.user_database.new_transaction().await?;
        let table = tx
            .get_store::<crate::store::Table<TrackedDatabase>>("databases")
            .await?;
        let key = self.target_id.to_string();
        let mut tracked = table
            .get(&key)
            .await
            .map_err(|_| UserError::DatabaseNotTracked {
                database_id: self.target_id.clone(),
            })?;

        if tracked.sync_settings.sync_enabled == enabled {
            return Ok(PreferenceWriteOutcome::Written(PreferenceWriteReceipt {
                entry_id: None,
            }));
        }
        tracked.sync_settings.sync_enabled = enabled;
        table.set(&key, tracked).await?;
        match tx.commit().await {
            Ok(entry_id) => Ok(PreferenceWriteOutcome::Written(PreferenceWriteReceipt {
                entry_id: Some(entry_id),
            })),
            Err(source) if source.is_io_error() || source.is_network_error() => {
                Ok(PreferenceWriteOutcome::Unknown { source })
            }
            Err(source) => Err(source),
        }
    }

    /// Enable this user's sharing preference.
    pub async fn share(&self) -> Result<PreferenceWriteOutcome> {
        self.set_sharing(true).await
    }

    /// Withdraw only this user's sharing enablement.
    pub async fn stop_sharing(&self) -> Result<PreferenceWriteOutcome> {
        self.set_sharing(false).await
    }

    /// Read the current authorized snapshot.
    pub async fn snapshot(&self) -> Result<DatabaseManagementSnapshot> {
        self.authorize().await?;
        #[cfg(all(unix, feature = "service"))]
        if let Some(conn) = self.target_database.instance()?.remote_connection() {
            conn.register_session_key(&self.signing_key).await?;
            return conn
                .database_management_snapshot(&self.target_id, self.identity.clone())
                .await;
        }

        let instance = self.target_database.instance()?;
        let desired = self.desired().await?;
        let state = instance
            .database_management_state(
                &self.target_id,
                &self.user_uuid,
                self.user_database.root_id(),
            )
            .await?;
        Ok(state.with_desired(desired))
    }

    /// Query a ticket without mutating preferences.
    pub async fn ticket(&self) -> Result<TicketStatus> {
        ticket_from_snapshot(&self.target_id, self.snapshot().await?)
    }

    /// Watch current state and subsequent scoped invalidations.
    pub async fn watch(&self) -> Result<DatabaseManagementWatch> {
        self.authorize().await?;
        let initial = self.snapshot().await?;
        let (trigger_tx, mut trigger_rx) = mpsc::channel(1);
        let (snapshot_tx, snapshot_rx) = watch::channel(initial);
        let mut callbacks = Vec::new();

        let desired_tips = self.user_database.snapshot().await?;
        let tx = trigger_tx.clone();
        callbacks.push(
            self.user_database
                .on_write_at_tips(desired_tips, move |_event, _db| {
                    let tx = tx.clone();
                    let _ = tx.try_send(());
                    async move { Ok(()) }
                })
                .await?,
        );
        let target_tips = self.target_database.snapshot().await?;
        let tx = trigger_tx.clone();
        callbacks.push(
            self.target_database
                .on_write_at_tips(target_tips, move |_event, _db| {
                    let tx = tx.clone();
                    let _ = tx.try_send(());
                    async move { Ok(()) }
                })
                .await?,
        );

        #[cfg(all(unix, feature = "service"))]
        let remote = self.target_database.instance()?.remote_connection();
        #[cfg(all(unix, feature = "service"))]
        let management_subscription = if let Some(conn) = &remote {
            Some(
                conn.subscribe_management(
                    self.target_id.clone(),
                    self.identity.clone(),
                    trigger_tx.clone(),
                )
                .await?,
            )
        } else {
            None
        };
        #[cfg(all(unix, feature = "service"))]
        let uses_remote = remote.is_some();
        #[cfg(not(all(unix, feature = "service")))]
        let uses_remote = false;

        if !uses_remote {
            let instance = self.target_database.instance()?;
            let mut runtime = instance.subscribe_management_runtime();
            let target = self.target_id.clone();
            let tx = trigger_tx.clone();
            tokio::spawn(async move {
                while runtime.changed().await.is_ok() {
                    if runtime
                        .borrow()
                        .database
                        .as_ref()
                        .is_none_or(|db| db == &target)
                    {
                        let _ = tx.try_send(());
                    }
                }
            });
        }

        let view = self.clone();
        let task = tokio::spawn(async move {
            while trigger_rx.recv().await.is_some() {
                while trigger_rx.try_recv().is_ok() {}
                let Ok(snapshot) = view.snapshot().await else {
                    break;
                };
                if snapshot_tx.send(snapshot).is_err() {
                    break;
                }
            }
        });

        // A change between the initial snapshot and either subscription is folded
        // into this immediate recomputation; callbacks/runtime invalidations cover
        // every change after subscription installation.
        let _ = trigger_tx.try_send(());

        Ok(DatabaseManagementWatch {
            receiver: snapshot_rx,
            _callbacks: callbacks,
            #[cfg(all(unix, feature = "service"))]
            _management_subscription: management_subscription,
            _task: task,
        })
    }

    /// Wait until a snapshot matches `predicate`, without mutating preferences.
    pub async fn wait_for<F>(
        &self,
        timeout: Duration,
        predicate: F,
    ) -> Result<Option<DatabaseManagementSnapshot>>
    where
        F: Fn(&DatabaseManagementSnapshot) -> bool,
    {
        let mut watch = self.watch().await?;
        if predicate(&watch.current()) {
            return Ok(Some(watch.current()));
        }
        let wait = async {
            loop {
                let snapshot = watch.changed().await?;
                if predicate(&snapshot) {
                    return Ok(snapshot);
                }
            }
        };
        match tokio::time::timeout(timeout, wait).await {
            Ok(result) => result.map(Some),
            Err(_) => Ok(None),
        }
    }

    async fn desired(&self) -> Result<SyncSettings> {
        let table = self
            .user_database
            .get_store_viewer::<crate::store::Table<TrackedDatabase>>("databases")
            .await?;
        Ok(table
            .get(&self.target_id.to_string())
            .await
            .map_err(|_| UserError::DatabaseNotTracked {
                database_id: self.target_id.clone(),
            })?
            .sync_settings)
    }

    async fn authorize(&self) -> Result<()> {
        if self.target_database.current_permission().await? >= Permission::Read {
            Ok(())
        } else {
            Err(UserError::InsufficientPermissions.into())
        }
    }
}

pub(crate) fn ticket_from_snapshot(
    database_id: &ID,
    snapshot: DatabaseManagementSnapshot,
) -> Result<TicketStatus> {
    if snapshot.observed.freshness != RuntimeFreshness::Current {
        return Ok(TicketStatus::NotReady(TicketNotReady::RuntimeUnknown));
    }
    if !snapshot.observed.engine_running {
        return Ok(TicketStatus::NotReady(TicketNotReady::EngineUnavailable));
    }
    if snapshot.applied != AppliedState::Current {
        return Ok(TicketStatus::NotReady(TicketNotReady::DesiredNotApplied));
    }
    if !snapshot
        .effective
        .as_ref()
        .is_some_and(|settings| settings.sync_enabled)
    {
        return Ok(TicketStatus::NotReady(TicketNotReady::SharingDisabled));
    }
    if snapshot.observed.listen_addresses.is_empty() {
        return Ok(TicketStatus::NotReady(TicketNotReady::NoLiveAddress));
    }
    Ok(TicketStatus::Ready(DatabaseTicket::with_addresses(
        database_id.clone(),
        snapshot.observed.listen_addresses,
    )))
}

pub(crate) struct OwnerManagementState {
    pub effective: Option<SyncSettings>,
    pub applied: AppliedState,
    pub observed: DatabaseObservation,
}

impl OwnerManagementState {
    pub(crate) fn with_desired(self, desired: SyncSettings) -> DatabaseManagementSnapshot {
        DatabaseManagementSnapshot {
            desired,
            effective: self.effective,
            applied: self.applied,
            observed: self.observed,
        }
    }
}

/// Runtime invalidation emitted by the owner sync engine.
#[derive(Debug, Clone, Default)]
pub(crate) struct ManagementInvalidation {
    pub database: Option<ID>,
}
