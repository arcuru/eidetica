//! Join automatic sync policy with the existing v0 routing inventory. The
//! registration matrix selects candidates, not authorization: the signed
//! SyncTree exchange still goes through the remote live read checks.

use std::collections::{HashMap, HashSet};

use tokio::time::Instant;
use tracing::warn;

use super::{
    RoundContext,
    schedule::{DEFAULT_INTERVAL, Periods, Schedule},
};
use crate::{
    Result, Transaction,
    account::SyncSettings,
    crdt::{Doc, doc::path},
    entry::ID,
    store::DocStore,
    sync::{
        account_sync_manager::DATABASE_USERS_SUBTREE,
        error::SyncError,
        peer_manager::PeerManager,
        peer_types::{Address, PeerId, PeerStatus},
    },
};

impl RoundContext {
    pub(super) async fn refresh_schedule(&self, schedule: &mut Schedule) {
        match self.periodic_snapshot().await {
            Ok(snapshot) => schedule.refresh(snapshot, Instant::now()),
            Err(error) => {
                warn!(%error, "Could not refresh automatic sync schedule; retaining prior due-times")
            }
        }
    }

    async fn periodic_snapshot(&self) -> Result<HashMap<PeerId, Periods>> {
        let txn = self.get_sync_tree().await?.new_transaction().await?;
        let peers = PeerManager::new(&txn);
        let active: HashSet<_> = peers
            .list_peers()
            .await?
            .into_iter()
            .filter(|peer| peer.status == PeerStatus::Active)
            .map(|peer| peer.id)
            .collect();
        let settings = settings_snapshot(&txn).await?;
        let mut snapshot: HashMap<_, Periods> = HashMap::new();
        for key in settings.keys() {
            if let Ok(tree) = ID::parse(key)
                && let Some(interval) = configured_interval(&settings, &tree)?
            {
                for peer in peers.get_tree_peers(&tree).await? {
                    if active.contains(&peer) {
                        snapshot
                            .entry(peer)
                            .or_default()
                            .insert(tree.clone(), interval);
                    }
                }
            }
        }
        Ok(snapshot)
    }

    /// Revalidate only the candidate peer against one current transaction
    /// before admitting a network round. A removed due pair cannot wake a
    /// peer just because it still has another, not-yet-due database.
    pub(super) async fn periodic_plan(&self, peer: &PeerId) -> Result<(Vec<Address>, Periods)> {
        let txn = self.get_sync_tree().await?.new_transaction().await?;
        let peers = PeerManager::new(&txn);
        let Some(info) = peers
            .get_peer_info(peer.public_key())
            .await?
            .filter(|info| info.status == PeerStatus::Active)
        else {
            return Ok((Vec::new(), Periods::new()));
        };
        let settings = settings_snapshot(&txn).await?;
        let mut periods = Periods::new();
        for tree in peers.get_peer_trees(peer.public_key()).await? {
            if let Some(interval) = configured_interval(&settings, &tree)? {
                periods.insert(tree, interval);
            }
        }
        Ok((info.addresses, periods))
    }
}

async fn settings_snapshot(txn: &Transaction) -> Result<Doc> {
    // Read the Store once, preserving all read errors. Only an actual missing
    // combined row means no settings; an error must not clear the schedule.
    txn.get_store::<DocStore>(DATABASE_USERS_SUBTREE)
        .await?
        .get_all()
        .await
}

fn configured_interval(settings: &Doc, tree: &ID) -> Result<Option<u64>> {
    let Some(value) = settings.get(path!(tree.to_string(), "combined_settings")) else {
        return Ok(None);
    };
    let settings: SyncSettings =
        serde_json::from_str(&String::try_from(value)?).map_err(|error| {
            SyncError::SerializationError(format!("Failed to parse combined settings: {error}"))
        })?;
    if !settings.sync_enabled {
        return Ok(None);
    }
    Ok(Some(settings.interval_seconds.unwrap_or(DEFAULT_INTERVAL)))
}
