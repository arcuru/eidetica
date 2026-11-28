//! Filtered host sync observations for the local service.

use serde::{Deserialize, Serialize};

use super::{
    account_sync_manager::DATABASE_USERS_SUBTREE,
    peer_manager::{PEERS_SUBTREE, TREES_SUBTREE},
};
use crate::{
    Instance, Transaction, account::types::SyncSettings, auth::crypto::PublicKey, crdt::doc::path,
    entry::ID, store::DocStore,
};

/// Point-in-time view of one database's persisted sync state on this daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostSyncObservation {
    /// Exact database root requested by the caller.
    pub root_id: ID,
    /// The serving instance's device public key.
    pub daemon_id: PublicKey,
    /// Daemon wall clock at observation start, in Unix milliseconds.
    pub observed_at_ms: u64,
    /// Combined host registration, independent of the caller's preferences.
    pub registration: HostSyncRegistration,
    /// Mapped peer public keys only. `None` means unknown; `Some(vec![])` means no peers.
    pub mapped_peers: Option<Vec<PublicKey>>,
}

/// Persisted host registration for a single database, not a replication status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostSyncRegistration {
    /// The daemon could not determine registration from its current accepted state.
    Unknown,
    /// The current accepted state has no registration for this root.
    Unregistered,
    /// The daemon has combined settings for this root. Application properties are omitted.
    Registered {
        /// Whether sync is enabled in the combined settings.
        sync_enabled: bool,
        /// Whether the combined settings request sync on commit.
        sync_on_commit: bool,
        /// Interval override in seconds, if configured.
        interval_seconds: Option<u64>,
    },
}

impl HostSyncObservation {
    pub(crate) async fn read(instance: &Instance, root_id: ID) -> Self {
        let mut observation = Self {
            root_id,
            daemon_id: instance.id(),
            observed_at_ms: instance.clock().now_millis(),
            registration: HostSyncRegistration::Unknown,
            mapped_peers: None,
        };
        if let Some(txn) = observation_transaction(instance).await {
            observation.registration = read_registration(&txn, &observation.root_id)
                .await
                .unwrap_or(HostSyncRegistration::Unknown);
            observation.mapped_peers = read_mapped_peers(&txn, &observation.root_id).await;
        }
        observation
    }
}

async fn observation_transaction(instance: &Instance) -> Option<Transaction> {
    let sync = instance.sync()?;
    let tree = sync.sync_tree();
    let backend = instance.require_local_engine().ok()?;
    let accepted = backend.verified_snapshot(tree.root_id()).await.ok()?;
    // Do not verify or report an older accepted state as current when newer
    // non-failed entries remain unverified. Both facts use this one cut.
    if accepted.is_empty() || accepted != backend.non_failed_snapshot(tree.root_id()).await.ok()? {
        return None;
    }
    tree.new_transaction_at(&accepted).await.ok()
}

async fn read_registration(txn: &Transaction, root: &ID) -> Option<HostSyncRegistration> {
    let registrations = txn
        .get_store::<DocStore>(DATABASE_USERS_SUBTREE)
        .await
        .ok()?;
    let Some(record) = registrations.try_get(root.to_string()).await.ok()? else {
        return Some(HostSyncRegistration::Unregistered);
    };
    let encoded = record.as_doc()?.get("combined_settings")?.as_text()?;
    let settings: SyncSettings = serde_json::from_str(encoded).ok()?;
    Some(HostSyncRegistration::Registered {
        sync_enabled: settings.sync_enabled,
        sync_on_commit: settings.sync_on_commit,
        interval_seconds: settings.interval_seconds,
    })
}

async fn read_mapped_peers(txn: &Transaction, root: &ID) -> Option<Vec<PublicKey>> {
    let trees = txn.get_store::<DocStore>(TREES_SUBTREE).await.ok()?;
    let root = root.to_string();
    let Some(mapping) = trees.try_get(&root).await.ok()? else {
        return Some(Vec::new());
    };
    let mapping = mapping.as_doc()?;
    if mapping.get("tree_id")?.as_text()? != root {
        return None;
    }
    let encoded = mapping.get("peer_pubkeys")?.as_text()?;
    let keys: Vec<String> = serde_json::from_str(encoded).ok()?;
    if keys.is_empty() {
        return Some(Vec::new());
    }
    let peers = txn
        .get_store::<DocStore>(PEERS_SUBTREE)
        .await
        .ok()?
        .get_all()
        .await
        .ok()?;
    let mut mapped = Vec::with_capacity(keys.len());
    for key in keys {
        let pubkey = PublicKey::from_prefixed_string(&key).ok()?;
        if pubkey.to_string() != key || peers.get(path!(&key, "pubkey"))?.as_text()? != key {
            // A dangling or inconsistent relationship is not a partial success.
            return None;
        }
        mapped.push(pubkey);
    }
    Some(mapped)
}
