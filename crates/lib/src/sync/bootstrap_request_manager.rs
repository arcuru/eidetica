//! Bootstrap request management for the sync module.
//!
//! This module handles storing and managing bootstrap requests that require manual approval.
//! Bootstrap requests are stored in the sync database as an Instance-level concern.

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::peer_types::Address;
use crate::{Error, Result, Transaction, auth::Permission, entry::ID, store::Table};

/// Private constant for bootstrap request subtree name
pub(super) const BOOTSTRAP_REQUESTS_SUBTREE: &str = "bootstrap_requests";

/// Internal bootstrap request manager for the sync module.
///
/// This struct manages all bootstrap request operations for the sync module,
/// operating on a Transaction to stage changes.
pub(super) struct BootstrapRequestManager<'a> {
    op: &'a Transaction,
}

/// A bootstrap request awaiting manual approval
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BootstrapRequest {
    /// The tree ID being requested for access
    pub tree_id: ID,
    /// Public key of the requesting device
    pub requesting_pubkey: String,
    /// Key name identifier for the requesting key
    pub requesting_key_name: String,
    /// Permission level being requested
    pub requested_permission: Permission,
    /// When the request was made (ISO 8601 timestamp)
    pub timestamp: String,
    /// Current status of the request
    pub status: RequestStatus,
    /// Address of the requesting peer (for future notifications)
    pub peer_address: Address,
}

/// Status of a bootstrap request
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RequestStatus {
    /// Request is pending approval
    Pending,
    /// Request has been approved
    Approved {
        /// Who approved the request
        approved_by: String,
        /// When it was approved
        approval_time: String,
    },
    /// Request has been rejected
    Rejected {
        /// Who rejected the request
        rejected_by: String,
        /// When it was rejected
        rejection_time: String,
    },
}

impl<'a> BootstrapRequestManager<'a> {
    /// Create a new BootstrapRequestManager that operates on the given Transaction.
    pub(super) fn new(op: &'a Transaction) -> Self {
        Self { op }
    }

    /// Store a new bootstrap request in the sync database.
    ///
    /// # Arguments
    /// * `request` - The bootstrap request to store
    ///
    /// # Returns
    /// The generated UUID for the request.
    pub(super) async fn store_request(&self, request: BootstrapRequest) -> Result<String> {
        let requests = self
            .op
            .get_store::<Table<BootstrapRequest>>(BOOTSTRAP_REQUESTS_SUBTREE)
            .await?;

        debug!(tree_id = %request.tree_id, "Storing bootstrap request");

        // Insert request and get generated UUID
        let request_id = requests.insert(request.clone()).await?;

        info!(request_id = %request_id, tree_id = %request.tree_id, "Successfully stored bootstrap request");
        Ok(request_id)
    }

    /// Get a specific bootstrap request by ID.
    ///
    /// # Arguments
    /// * `request_id` - The ID of the request to retrieve
    ///
    /// # Returns
    /// The bootstrap request if found, None otherwise.
    pub(super) async fn get_request(&self, request_id: &str) -> Result<Option<BootstrapRequest>> {
        let requests = self
            .op
            .get_store::<Table<BootstrapRequest>>(BOOTSTRAP_REQUESTS_SUBTREE)
            .await?;

        match requests.get(request_id).await {
            Ok(request) => Ok(Some(request)),
            Err(Error::Store(crate::store::StoreError::KeyNotFound { .. })) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Internal method to filter bootstrap requests by status.
    async fn filter_requests(
        &self,
        status_filter: &RequestStatus,
    ) -> Result<Vec<(String, BootstrapRequest)>> {
        let requests = self
            .op
            .get_store::<Table<BootstrapRequest>>(BOOTSTRAP_REQUESTS_SUBTREE)
            .await?;

        let results = requests
            .search(|request| {
                std::mem::discriminant(status_filter) == std::mem::discriminant(&request.status)
            })
            .await?;

        Ok(results)
    }

    /// Get all pending bootstrap requests.
    ///
    /// # Returns
    /// A vector of (request_id, bootstrap_request) pairs for pending requests.
    pub(super) async fn pending_requests(&self) -> Result<Vec<(String, BootstrapRequest)>> {
        self.filter_requests(&RequestStatus::Pending).await
    }

    /// Get all approved bootstrap requests.
    ///
    /// # Returns
    /// A vector of (request_id, bootstrap_request) pairs for approved requests.
    pub(super) async fn approved_requests(&self) -> Result<Vec<(String, BootstrapRequest)>> {
        self.filter_requests(&RequestStatus::Approved {
            approved_by: String::new(),
            approval_time: String::new(),
        })
        .await
    }

    /// Get all rejected bootstrap requests.
    ///
    /// # Returns
    /// A vector of (request_id, bootstrap_request) pairs for rejected requests.
    pub(super) async fn rejected_requests(&self) -> Result<Vec<(String, BootstrapRequest)>> {
        self.filter_requests(&RequestStatus::Rejected {
            rejected_by: String::new(),
            rejection_time: String::new(),
        })
        .await
    }

    /// Update the status of a bootstrap request.
    ///
    /// # Arguments
    /// * `request_id` - The ID of the request to update
    /// * `new_status` - The new status to set
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub(super) async fn update_status(
        &self,
        request_id: &str,
        new_status: RequestStatus,
    ) -> Result<()> {
        let requests = self
            .op
            .get_store::<Table<BootstrapRequest>>(BOOTSTRAP_REQUESTS_SUBTREE)
            .await?;

        // Get the existing request
        let mut request = requests.get(request_id).await?;

        // Update the status
        request.status = new_status;

        // Store the updated request
        requests.set(request_id, request).await?;

        debug!(request_id = %request_id, "Updated bootstrap request status");
        Ok(())
    }
}

#[cfg(test)]
#[allow(deprecated)] // Uses LegacyInstanceOps
mod tests {
    use super::*;
    use crate::{
        Clock, Database, Instance, auth::types::Permission, backend::database::InMemory,
        clock::FixedClock, instance::LegacyInstanceOps, sync::DEVICE_KEY_NAME,
    };
    use std::sync::Arc;

    async fn create_test_sync_tree() -> (Instance, Database, Arc<FixedClock>) {
        let clock = Arc::new(FixedClock::default());
        let backend = Box::new(InMemory::new());
        let instance = Instance::open_with_clock(backend, clock.clone())
            .await
            .expect("Failed to create test instance");

        // Create sync tree similar to how Sync::new does it
        let mut sync_settings = crate::crdt::Doc::new();
        sync_settings.set("name", "_sync");
        sync_settings.set("type", "sync_settings");

        let database = instance
            .new_database(sync_settings, DEVICE_KEY_NAME)
            .await
            .unwrap();

        (instance, database, clock)
    }

    fn create_test_request(clock: &FixedClock) -> BootstrapRequest {
        BootstrapRequest {
            // Use a valid, prefixed ID so parsing validates correctly
            tree_id: ID::from_bytes("test_tree_id"),
            requesting_pubkey: "ed25519:test_public_key".to_string(),
            requesting_key_name: "laptop_key".to_string(),
            requested_permission: Permission::Write(5),
            timestamp: clock.now_rfc3339(),
            status: RequestStatus::Pending,
            peer_address: Address {
                transport_type: "http".to_string(),
                address: "127.0.0.1:8080".to_string(),
            },
        }
    }

    #[tokio::test]
    async fn test_store_and_get_request() {
        let (_instance, sync_tree, clock) = create_test_sync_tree().await;
        let op = sync_tree.new_transaction().await.unwrap();
        let manager = BootstrapRequestManager::new(&op);

        let request = create_test_request(&clock);

        // Store the request and get the generated UUID
        let request_id = manager.store_request(request.clone()).await.unwrap();

        // Retrieve the request
        let retrieved = manager.get_request(&request_id).await.unwrap().unwrap();
        assert_eq!(retrieved.tree_id, request.tree_id);
        assert_eq!(retrieved.requesting_pubkey, request.requesting_pubkey);
        assert_eq!(retrieved.requesting_key_name, request.requesting_key_name);
        assert_eq!(retrieved.requested_permission, request.requested_permission);
        assert_eq!(retrieved.status, request.status);
        assert_eq!(retrieved.peer_address, request.peer_address);
    }

    #[tokio::test]
    async fn test_list_requests() {
        let (_instance, sync_tree, clock) = create_test_sync_tree().await;
        let op = sync_tree.new_transaction().await.unwrap();
        let manager = BootstrapRequestManager::new(&op);

        // Store multiple requests
        let request1 = create_test_request(&clock);

        let mut request2 = create_test_request(&clock);
        request2.status = RequestStatus::Approved {
            approved_by: "admin".to_string(),
            approval_time: clock.now_rfc3339(),
        };

        manager.store_request(request1).await.unwrap();
        manager.store_request(request2).await.unwrap();

        // Get pending requests
        let pending_requests = manager.pending_requests().await.unwrap();
        assert_eq!(pending_requests.len(), 1);

        // Get approved requests
        let approved_requests = manager.approved_requests().await.unwrap();
        assert_eq!(approved_requests.len(), 1);

        // Verify statuses
        assert!(matches!(
            pending_requests[0].1.status,
            RequestStatus::Pending
        ));
        assert!(matches!(
            approved_requests[0].1.status,
            RequestStatus::Approved { .. }
        ));
    }

    #[tokio::test]
    async fn test_update_status() {
        let (_instance, sync_tree, clock) = create_test_sync_tree().await;
        let op = sync_tree.new_transaction().await.unwrap();
        let manager = BootstrapRequestManager::new(&op);

        let request = create_test_request(&clock);

        // Store the request and get the generated UUID
        let request_id = manager.store_request(request).await.unwrap();

        // Update status to approved
        let new_status = RequestStatus::Approved {
            approved_by: "admin".to_string(),
            approval_time: clock.now_rfc3339(),
        };
        manager
            .update_status(&request_id, new_status.clone())
            .await
            .unwrap();

        // Verify status was updated
        let updated_request = manager.get_request(&request_id).await.unwrap().unwrap();
        assert_eq!(updated_request.status, new_status);
    }

    #[tokio::test]
    async fn test_get_nonexistent_request() {
        let (_instance, sync_tree, _clock) = create_test_sync_tree().await;
        let op = sync_tree.new_transaction().await.unwrap();
        let manager = BootstrapRequestManager::new(&op);

        let result = manager.get_request("nonexistent").await.unwrap();
        assert!(result.is_none());
    }
}
