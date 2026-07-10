//! Outgoing bootstrap request management for the sync module.
//!
//! This is the client-side mirror of [`bootstrap_request_manager`](super::bootstrap_request_manager),
//! which tracks *incoming* requests an approver must act on. This module tracks
//! the requests *this* node has sent that are still awaiting approval.
//!
//! When a ticket bootstrap comes back pending, everything needed to finish the
//! join once access is granted — the target tree, the addresses to pull from,
//! the requesting key, the requested permission, and the caller's desired
//! [`SyncSettings`] — is captured here in the `_sync` tree. Sync then drives
//! completion locally (pull the now-authorized tree, apply the settings) without
//! calling back into the User layer: the dependency direction stays User -> Sync.

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::peer_types::Address;
#[cfg(test)]
use crate::{Error, store::StoreError};
use crate::{
    Result, Transaction,
    auth::{Permission, crypto::PublicKey},
    entry::ID,
    store::Table,
    user::types::SyncSettings,
};

/// Private constant for the outgoing bootstrap request subtree name.
pub(super) const OUTGOING_BOOTSTRAP_REQUESTS_SUBTREE: &str = "outgoing_bootstrap_requests";

/// Internal outgoing bootstrap request manager for the sync module.
///
/// Mirrors [`BootstrapRequestManager`](super::bootstrap_request_manager::BootstrapRequestManager),
/// operating on a [`Transaction`] to stage changes to the `_sync` tree.
pub(super) struct OutgoingBootstrapRequestManager<'a> {
    txn: &'a Transaction,
}

/// A bootstrap request this node has sent that is awaiting the peer's approval.
///
/// Persisted so completion is restart-safe and does not depend on receiving the
/// approval broadcast: a periodic sweep re-checks every pending record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutgoingBootstrapRequest {
    /// The tree ID access was requested for.
    pub tree_id: ID,
    /// Addresses to pull the tree from once access is granted. Carried from the
    /// ticket so completion can reconnect without further ticket handling.
    pub addresses: Vec<Address>,
    /// Public key this node requested access with.
    pub requesting_pubkey: PublicKey,
    /// Key name identifier for the requesting key.
    pub requesting_key_name: String,
    /// Permission level that was requested.
    pub requested_permission: Permission,
    /// The caller's desired sync settings, applied on completion. Plain shared
    /// data handed into Sync — not a dependency on User state.
    pub sync_settings: SyncSettings,
    /// When the request was recorded (ISO 8601 timestamp).
    pub timestamp: String,
    /// Current status of the request.
    pub status: OutgoingRequestStatus,
}

/// Status of an outgoing bootstrap request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OutgoingRequestStatus {
    /// Awaiting approval; not yet pulled locally.
    Pending,
    /// Access was granted, the tree pulled, and the settings applied.
    Hydrated,
}

impl<'a> OutgoingBootstrapRequestManager<'a> {
    /// Create a new manager that operates on the given [`Transaction`].
    pub(super) fn new(txn: &'a Transaction) -> Self {
        Self { txn }
    }

    /// Store a new outgoing bootstrap request.
    ///
    /// # Returns
    /// The generated UUID for the request.
    pub(super) async fn store_request(&self, request: OutgoingBootstrapRequest) -> Result<String> {
        let requests = self
            .txn
            .get_store::<Table<OutgoingBootstrapRequest>>(OUTGOING_BOOTSTRAP_REQUESTS_SUBTREE)
            .await?;

        debug!(tree_id = %request.tree_id, "Recording outgoing bootstrap request");

        let request_id = requests.insert(request.clone()).await?;

        info!(request_id = %request_id, tree_id = %request.tree_id, "Recorded outgoing bootstrap request");
        Ok(request_id)
    }

    /// Get a specific outgoing bootstrap request by ID.
    ///
    /// Part of the manager's read surface, mirroring the incoming
    /// [`get_request`](super::bootstrap_request_manager::BootstrapRequestManager::get_request);
    /// currently exercised only by tests, kept for symmetry and direct lookups.
    #[cfg(test)]
    pub(super) async fn get_request(
        &self,
        request_id: &str,
    ) -> Result<Option<OutgoingBootstrapRequest>> {
        let requests = self
            .txn
            .get_store::<Table<OutgoingBootstrapRequest>>(OUTGOING_BOOTSTRAP_REQUESTS_SUBTREE)
            .await?;

        match requests.get(request_id).await {
            Ok(request) => Ok(Some(request)),
            Err(Error::Store(ref e)) if matches!(**e, StoreError::KeyNotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Get all pending outgoing bootstrap requests.
    ///
    /// Symmetric to [`pending_requests`](super::bootstrap_request_manager::BootstrapRequestManager::pending_requests)
    /// on the incoming side.
    ///
    /// # Returns
    /// A vector of (request_id, request) pairs for pending requests.
    pub(super) async fn pending_requests(&self) -> Result<Vec<(String, OutgoingBootstrapRequest)>> {
        let requests = self
            .txn
            .get_store::<Table<OutgoingBootstrapRequest>>(OUTGOING_BOOTSTRAP_REQUESTS_SUBTREE)
            .await?;

        requests
            .search(|request| matches!(request.status, OutgoingRequestStatus::Pending))
            .await
    }

    /// Get every pending request whose tree matches `tree_id`.
    ///
    /// Used by the broadcast-woken reaction: when remote entries land for a
    /// tree, this pulls the matching outgoing record(s) so completion can fire.
    pub(super) async fn pending_requests_for_tree(
        &self,
        tree_id: &ID,
    ) -> Result<Vec<(String, OutgoingBootstrapRequest)>> {
        let requests = self
            .txn
            .get_store::<Table<OutgoingBootstrapRequest>>(OUTGOING_BOOTSTRAP_REQUESTS_SUBTREE)
            .await?;

        requests
            .search(|request| {
                matches!(request.status, OutgoingRequestStatus::Pending)
                    && &request.tree_id == tree_id
            })
            .await
    }

    /// Mark an outgoing request as hydrated (access granted, tree pulled,
    /// settings applied).
    pub(super) async fn mark_hydrated(&self, request_id: &str) -> Result<()> {
        let requests = self
            .txn
            .get_store::<Table<OutgoingBootstrapRequest>>(OUTGOING_BOOTSTRAP_REQUESTS_SUBTREE)
            .await?;

        let mut request = requests.get(request_id).await?;
        request.status = OutgoingRequestStatus::Hydrated;
        requests.set(request_id, request).await?;

        debug!(request_id = %request_id, "Marked outgoing bootstrap request hydrated");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Clock, Database, Instance, auth::types::Permission, backend::database::InMemory,
        clock::FixedClock, crdt::Doc,
    };
    use std::sync::Arc;

    async fn create_test_sync_tree() -> (Instance, Database, Arc<FixedClock>) {
        let clock = Arc::new(FixedClock::default());
        let (instance, mut user) = Instance::create_backend_with_clock(
            Box::new(InMemory::new()),
            clock.clone(),
            crate::NewUser::passwordless("test"),
        )
        .await
        .expect("Failed to create test instance");

        let mut sync_settings = Doc::new();
        sync_settings.set("name", "_sync");
        sync_settings.set("type", "sync_settings");

        let (database, _) = user
            .new_database()
            .settings(sync_settings)
            .build()
            .await
            .unwrap();

        (instance, database, clock)
    }

    fn create_test_request(clock: &FixedClock) -> OutgoingBootstrapRequest {
        OutgoingBootstrapRequest {
            tree_id: ID::from_bytes("test_tree_id"),
            addresses: vec![Address {
                transport_type: "http".to_string(),
                address: "127.0.0.1:8080".to_string(),
            }],
            requesting_pubkey: PublicKey::random(),
            requesting_key_name: "laptop_key".to_string(),
            requested_permission: Permission::Write(5),
            sync_settings: SyncSettings::enabled(),
            timestamp: clock.now_rfc3339(),
            status: OutgoingRequestStatus::Pending,
        }
    }

    #[tokio::test]
    async fn test_store_and_get_request() {
        let (_instance, sync_tree, clock) = create_test_sync_tree().await;
        let txn = sync_tree.new_transaction().await.unwrap();
        let manager = OutgoingBootstrapRequestManager::new(&txn);

        let request = create_test_request(&clock);
        let request_id = manager.store_request(request.clone()).await.unwrap();

        let retrieved = manager.get_request(&request_id).await.unwrap().unwrap();
        assert_eq!(retrieved.tree_id, request.tree_id);
        assert_eq!(retrieved.addresses, request.addresses);
        assert_eq!(retrieved.requesting_pubkey, request.requesting_pubkey);
        assert_eq!(retrieved.requesting_key_name, request.requesting_key_name);
        assert_eq!(retrieved.requested_permission, request.requested_permission);
        assert_eq!(
            retrieved.sync_settings.sync_enabled,
            request.sync_settings.sync_enabled
        );
        assert_eq!(retrieved.status, request.status);
    }

    #[tokio::test]
    async fn test_pending_and_hydrate() {
        let (_instance, sync_tree, clock) = create_test_sync_tree().await;
        let txn = sync_tree.new_transaction().await.unwrap();
        let manager = OutgoingBootstrapRequestManager::new(&txn);

        let request = create_test_request(&clock);
        let tree_id = request.tree_id.clone();
        let request_id = manager.store_request(request).await.unwrap();

        // Pending queries surface it.
        assert_eq!(manager.pending_requests().await.unwrap().len(), 1);
        assert_eq!(
            manager
                .pending_requests_for_tree(&tree_id)
                .await
                .unwrap()
                .len(),
            1
        );

        // A different tree does not match.
        assert!(
            manager
                .pending_requests_for_tree(&ID::from_bytes("other"))
                .await
                .unwrap()
                .is_empty()
        );

        // After hydration it drops out of the pending queries.
        manager.mark_hydrated(&request_id).await.unwrap();
        assert!(manager.pending_requests().await.unwrap().is_empty());
        assert!(
            manager
                .pending_requests_for_tree(&tree_id)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            manager
                .get_request(&request_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            OutgoingRequestStatus::Hydrated
        );
    }

    #[tokio::test]
    async fn test_get_nonexistent_request() {
        let (_instance, sync_tree, _clock) = create_test_sync_tree().await;
        let txn = sync_tree.new_transaction().await.unwrap();
        let manager = OutgoingBootstrapRequestManager::new(&txn);

        assert!(manager.get_request("nonexistent").await.unwrap().is_none());
    }
}
