//! Bootstrap request management for the sync module.
//!
//! This module handles storing and managing bootstrap requests that require manual approval.
//! Bootstrap requests are stored in the sync database as an Instance-level concern.

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::peer_types::Address;
use crate::{
    Error, Result, Transaction,
    auth::{Permission, crypto::PublicKey},
    crdt::Doc,
    entry::ID,
    store::{StoreError, Table},
};

/// Private constant for bootstrap request subtree name
pub(super) const BOOTSTRAP_REQUESTS_SUBTREE: &str = "bootstrap_requests";

/// Internal bootstrap request manager for the sync module.
///
/// This struct manages all bootstrap request operations for the sync module,
/// operating on a Transaction to stage changes.
pub(super) struct BootstrapRequestManager<'a> {
    txn: &'a Transaction,
}

/// A bootstrap request awaiting manual approval
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BootstrapRequest {
    /// The tree ID being requested for access
    pub tree_id: ID,
    /// Public key of the requesting device
    pub requesting_pubkey: PublicKey,
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
    /// Free-form context supplied by the requester for the approver to inspect
    /// when deciding whether to grant access. Carried verbatim from the request.
    #[serde(default)]
    pub metadata: Option<Doc>,
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

/// Derive the storage ID from the requested access identity.
pub(super) fn request_id_for(
    tree_id: &ID,
    requesting_pubkey: &PublicKey,
    requested_permission: &Permission,
) -> String {
    let identity = serde_ipld_dagcbor::to_vec(&(tree_id, requesting_pubkey, requested_permission))
        .expect("bootstrap request identity is serializable");
    ID::from_dagcbor_bytes(identity).to_string()
}

impl<'a> BootstrapRequestManager<'a> {
    /// Create a new BootstrapRequestManager that operates on the given Transaction.
    pub(super) fn new(txn: &'a Transaction) -> Self {
        Self { txn }
    }

    /// Store a bootstrap request under the ID derived from its identity.
    ///
    /// # Arguments
    /// * `request` - The bootstrap request to store
    ///
    /// # Returns
    /// The stable ID for the request.
    pub(super) async fn store_request(&self, request: BootstrapRequest) -> Result<String> {
        let requests = self
            .txn
            .get_store::<Table<BootstrapRequest>>(BOOTSTRAP_REQUESTS_SUBTREE)
            .await?;

        debug!(tree_id = %request.tree_id, "Storing bootstrap request");

        let request_id = request_id_for(
            &request.tree_id,
            &request.requesting_pubkey,
            &request.requested_permission,
        );
        requests.set(&request_id, request.clone()).await?;

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
            .txn
            .get_store::<Table<BootstrapRequest>>(BOOTSTRAP_REQUESTS_SUBTREE)
            .await?;

        match requests.get(request_id).await {
            Ok(request) => Ok(Some(request)),
            Err(Error::Store(ref e)) if matches!(**e, StoreError::KeyNotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Internal method to filter bootstrap requests by status.
    async fn filter_requests(
        &self,
        status_filter: &RequestStatus,
    ) -> Result<Vec<(String, BootstrapRequest)>> {
        let requests = self
            .txn
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

    /// Find the existing record for the same requested access.
    pub(super) async fn find_existing_request(
        &self,
        tree_id: &ID,
        requesting_pubkey: &PublicKey,
        requested_permission: &Permission,
    ) -> Result<Option<(String, BootstrapRequest)>> {
        let requests = self
            .txn
            .get_store::<Table<BootstrapRequest>>(BOOTSTRAP_REQUESTS_SUBTREE)
            .await?;
        let matches = requests
            .search(|request| {
                &request.tree_id == tree_id
                    && &request.requesting_pubkey == requesting_pubkey
                    && &request.requested_permission == requested_permission
            })
            .await?;
        Ok(matches
            .iter()
            .find(|(_, request)| matches!(request.status, RequestStatus::Rejected { .. }))
            .cloned()
            .or_else(|| {
                matches
                    .iter()
                    .find(|(_, request)| matches!(request.status, RequestStatus::Pending))
                    .cloned()
            })
            .or_else(|| matches.into_iter().next()))
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
            .txn
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

    fn create_test_request(clock: &FixedClock) -> BootstrapRequest {
        BootstrapRequest {
            // Use a valid, prefixed ID so parsing validates correctly
            tree_id: ID::from_bytes("test_tree_id"),
            requesting_pubkey: PublicKey::random(),
            requesting_key_name: "laptop_key".to_string(),
            requested_permission: Permission::Write(5),
            timestamp: clock.now_rfc3339(),
            status: RequestStatus::Pending,
            peer_address: Address {
                transport_type: "http".to_string(),
                address: "127.0.0.1:8080".to_string(),
            },
            metadata: None,
        }
    }

    #[tokio::test]
    async fn test_store_and_get_request() {
        let (_instance, sync_tree, clock) = create_test_sync_tree().await;
        let txn = sync_tree.new_transaction().await.unwrap();
        let manager = BootstrapRequestManager::new(&txn);

        let request = create_test_request(&clock);

        // Store the request and get its identity-derived ID
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
        let txn = sync_tree.new_transaction().await.unwrap();
        let manager = BootstrapRequestManager::new(&txn);

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
        let txn = sync_tree.new_transaction().await.unwrap();
        let manager = BootstrapRequestManager::new(&txn);

        let request = create_test_request(&clock);

        // Store the request and get its identity-derived ID
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

    #[test]
    fn request_identity_is_deterministic_and_distinguishes_semantic_fields() {
        let tree = ID::from_bytes("test_tree_id");
        let other_tree = ID::from_bytes("other_tree_id");
        let key = PublicKey::random();
        let other_key = PublicKey::random();
        let request_id = request_id_for(&tree, &key, &Permission::Write(5));
        let storage_id = ID::parse(&request_id).expect("request ID uses the project ID encoding");

        assert_eq!(storage_id.as_cid().unwrap().codec(), 0x71);
        assert_eq!(storage_id.hash_code(), Some(0x1e));
        assert_eq!(
            request_id,
            request_id_for(&tree, &key, &Permission::Write(5))
        );
        assert_ne!(
            request_id,
            request_id_for(&other_tree, &key, &Permission::Write(5))
        );
        assert_ne!(
            request_id,
            request_id_for(&tree, &other_key, &Permission::Write(5))
        );
        assert_ne!(
            request_id,
            request_id_for(&tree, &key, &Permission::Admin(5))
        );
        assert_ne!(
            request_id,
            request_id_for(&tree, &key, &Permission::Write(6))
        );
        assert_ne!(request_id, request_id_for(&tree, &key, &Permission::Read));
    }

    #[test]
    fn request_identity_ignores_mutable_record_fields() {
        let clock = FixedClock::default();
        let request = create_test_request(&clock);
        let request_id = request_id_for(
            &request.tree_id,
            &request.requesting_pubkey,
            &request.requested_permission,
        );
        let mut changed = request;
        changed.requesting_key_name = "renamed key".to_string();
        changed.timestamp = "2026-09-16T12:34:56Z".to_string();
        changed.status = RequestStatus::Rejected {
            rejected_by: "admin".to_string(),
            rejection_time: "2026-09-16T12:35:00Z".to_string(),
        };
        changed.peer_address = Address {
            transport_type: "iroh".to_string(),
            address: "new-address".to_string(),
        };
        let mut metadata = Doc::new();
        metadata.set("note", "changed");
        changed.metadata = Some(metadata);

        assert_eq!(
            request_id,
            request_id_for(
                &changed.tree_id,
                &changed.requesting_pubkey,
                &changed.requested_permission,
            )
        );
    }

    #[tokio::test]
    async fn concurrent_writers_converge_on_one_request() {
        let (_instance, sync_tree, clock) = create_test_sync_tree().await;
        let request = create_test_request(&clock);
        let first = sync_tree.new_transaction().await.unwrap();
        let second = sync_tree.new_transaction().await.unwrap();
        let first_id = BootstrapRequestManager::new(&first)
            .store_request(request.clone())
            .await
            .unwrap();
        let mut later = request;
        later.timestamp = "2026-09-15T23:00:01Z".to_string();
        let second_id = BootstrapRequestManager::new(&second)
            .store_request(later)
            .await
            .unwrap();
        first.commit().await.unwrap();
        second.commit().await.unwrap();
        assert_eq!(first_id, second_id);
        let txn = sync_tree.new_transaction().await.unwrap();
        let pending = BootstrapRequestManager::new(&txn)
            .pending_requests()
            .await
            .unwrap();
        assert_eq!(pending.len(), 1, "racing writers left {pending:#?}");
    }

    #[tokio::test]
    async fn test_get_nonexistent_request() {
        let (_instance, sync_tree, _clock) = create_test_sync_tree().await;
        let txn = sync_tree.new_transaction().await.unwrap();
        let manager = BootstrapRequestManager::new(&txn);

        let result = manager.get_request("nonexistent").await.unwrap();
        assert!(result.is_none());
    }
}
