//! User-scoped database sharing preferences and ticket lookup.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use super::{SyncSettings, TrackedDatabase, User, UserError};
use crate::{
    Database, Result, Snapshot,
    auth::{Permission, SigKey},
    entry::ID,
    instance::WriteCallback,
    sync::{DatabaseTicket, SyncError},
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

/// This user's sharing preference at a specific user-database snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseManagementSnapshot {
    /// The source database snapshot that pins `settings`.
    pub source: Snapshot,
    /// This user's durable settings at `source`.
    pub settings: SyncSettings,
}

/// Stream of this user's current preference and subsequent database changes.
pub struct DatabaseManagementWatch {
    receiver: watch::Receiver<DatabaseManagementSnapshot>,
    _callbacks: Vec<WriteCallback>,
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
    user_database: Database,
    target_database: Database,
    target_id: ID,
    identity: SigKey,
}

impl DatabaseManagement {
    pub(crate) async fn new(user: &User, database_id: &ID) -> Result<Self> {
        let tracked = user.database(database_id).await?;
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
            user_database: user.user_database().clone(),
            target_database,
            target_id: database_id.clone(),
            identity,
        })
    }

    /// Save this user's sharing enablement without waiting for owner reconciliation.
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

    /// Read this user's settings from one pinned user-database snapshot.
    pub async fn snapshot(&self) -> Result<DatabaseManagementSnapshot> {
        self.authorize().await?;
        let source = self.user_database.snapshot().await?;
        self.snapshot_at(source).await
    }

    /// Return a point-in-time locator when this user's sharing setting is enabled.
    pub async fn ticket(&self) -> Result<DatabaseTicket> {
        self.authorize().await?;
        if !self.snapshot().await?.settings.sync_enabled {
            return Err(UserError::DatabaseNotShared {
                database_id: self.target_id.clone(),
            }
            .into());
        }
        #[cfg(all(unix, feature = "service"))]
        if let Some(conn) = self.target_database.instance()?.remote_connection() {
            return conn
                .database_management_ticket(&self.target_id, self.identity.clone())
                .await;
        }
        ticket_locator(&self.target_database.instance()?, &self.target_id).await
    }

    /// Watch this user's settings from an initial pinned snapshot onward.
    pub async fn watch(&self) -> Result<DatabaseManagementWatch> {
        self.authorize().await?;
        let source = self.user_database.snapshot().await?;
        let initial = self.snapshot_at(source.clone()).await?;
        let (trigger_tx, mut trigger_rx) = mpsc::channel::<Option<Snapshot>>(1);
        let (snapshot_tx, snapshot_rx) = watch::channel(initial);

        let tx = trigger_tx.clone();
        let callback = self
            .user_database
            .on_write_at_tips(source, move |event, _db| {
                let _ = tx.try_send(Some(event.post_tips().clone()));
                async move { Ok(()) }
            })
            .await?;

        let target_source = self.target_database.snapshot().await?;
        let tx = trigger_tx.clone();
        let target_callback = self
            .target_database
            .on_write_at_tips(target_source, move |_event, _db| {
                let _ = tx.try_send(None);
                async move { Ok(()) }
            })
            .await?;

        let view = self.clone();
        let task = tokio::spawn(async move {
            while let Some(trigger) = trigger_rx.recv().await {
                let mut source = trigger;
                while let Ok(newer) = trigger_rx.try_recv() {
                    if newer.is_some() {
                        source = newer;
                    }
                }
                let snapshot = match source {
                    Some(source) => view.snapshot_at(source).await,
                    None => view.snapshot().await,
                };
                let Ok(snapshot) = snapshot else {
                    break;
                };
                if snapshot_tx.send(snapshot).is_err() {
                    break;
                }
            }
        });

        // Fold a write that landed after the initial read but before callback
        // registration into the same native Snapshot timeline.
        let current = self.user_database.snapshot().await?;
        if current != snapshot_rx.borrow().source {
            let _ = trigger_tx.try_send(Some(current));
        }
        // Close the target read/subscription race: a revocation that landed
        // while callbacks were being installed makes watch creation fail;
        // later revocations arrive through the target callback above.
        self.authorize().await?;

        Ok(DatabaseManagementWatch {
            receiver: snapshot_rx,
            _callbacks: vec![callback, target_callback],
            _task: task,
        })
    }

    /// Wait until this user's settings match `predicate`.
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

    async fn snapshot_at(&self, source: Snapshot) -> Result<DatabaseManagementSnapshot> {
        self.authorize().await?;
        let tx = self.user_database.new_transaction_at(&source).await?;
        let settings = tx
            .get_store::<crate::store::Table<TrackedDatabase>>("databases")
            .await?
            .get(&self.target_id.to_string())
            .await
            .map_err(|_| UserError::DatabaseNotTracked {
                database_id: self.target_id.clone(),
            })?
            .sync_settings;
        Ok(DatabaseManagementSnapshot { source, settings })
    }

    async fn authorize(&self) -> Result<()> {
        if self.target_database.current_permission().await? >= Permission::Read {
            Ok(())
        } else {
            Err(UserError::InsufficientPermissions.into())
        }
    }
}

pub(crate) async fn ticket_locator(
    instance: &crate::Instance,
    database_id: &ID,
) -> Result<DatabaseTicket> {
    let sync = instance.sync().ok_or(SyncError::SyncNotEnabled)?;
    sync.create_ticket(database_id).await
}
