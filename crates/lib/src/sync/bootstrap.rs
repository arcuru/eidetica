//! Bootstrap sync operations and request management.

use tracing::info;

use super::{
    Address, BootstrapRequest, RequestStatus, Sync, SyncError,
    bootstrap_request_manager::BootstrapRequestManager,
};
use crate::{
    Database, Result,
    auth::{Permission, crypto::PublicKey, types::AuthKey},
    database::DatabaseKey,
    entry::ID,
};

impl Sync {
    // === Bootstrap Sync Methods ===
    //
    // Bootstrap sync allows a device to request access to a database it doesn't
    // have permission to yet. The device sends its public key and requested
    // permission level to the peer, creating a pending bootstrap request that
    // an administrator can approve or reject.
    //
    // Use `sync_with_peer_for_bootstrap_with_key()` with User API managed keys.

    /// Internal helper for bootstrap sync operations.
    ///
    /// This method contains the common logic for bootstrap scenarios where the local
    /// device doesn't have access to the target tree yet and needs to request
    /// permission during the initial sync.
    ///
    /// # Arguments
    /// * `address` - The transport address of the peer to sync with
    /// * `tree_id` - The ID of the tree to sync
    /// * `requesting_public_key` - The formatted public key string for authentication
    /// * `requesting_key_name` - The name/ID of the requesting key
    /// * `requested_permission` - The permission level being requested
    ///
    /// # Returns
    /// A Result indicating success or failure.
    ///
    /// # Errors
    /// * `SyncError::InvalidPublicKey` if the public key is empty or malformed
    /// * `SyncError::InvalidKeyName` if the key name is empty
    async fn sync_with_peer_for_bootstrap_internal(
        &self,
        address: &Address,
        tree_id: &ID,
        requesting_public_key: String,
        requesting_key_name: &str,
        requested_permission: Permission,
    ) -> Result<()> {
        // Validate public key is not empty
        if requesting_public_key.is_empty() {
            return Err(SyncError::InvalidPublicKey {
                reason: "Public key cannot be empty".to_string(),
            }
            .into());
        }

        // Validate public key format by attempting to parse it
        PublicKey::from_prefixed_string(&requesting_public_key).map_err(|e| {
            SyncError::InvalidPublicKey {
                reason: format!("Invalid public key format: {e}"),
            }
        })?;

        // Validate key name is not empty
        if requesting_key_name.is_empty() {
            return Err(SyncError::InvalidKeyName {
                reason: "Key name cannot be empty".to_string(),
            }
            .into());
        }

        // Connect to peer if not already connected
        let peer_pubkey = self.connect_to_peer(address).await?;

        // Store the address for this peer
        self.add_peer_address(&peer_pubkey, address.clone()).await?;

        // Sync tree with authentication
        self.sync_tree_with_peer_auth(
            &peer_pubkey,
            tree_id,
            Some(&requesting_public_key),
            Some(requesting_key_name),
            Some(requested_permission),
        )
        .await?;

        Ok(())
    }

    /// Sync with a peer for bootstrap using a user-provided public key.
    ///
    /// This method is specifically designed for bootstrap scenarios where the local
    /// device doesn't have access to the target tree yet and needs to request
    /// permission during the initial sync. The public key is provided directly
    /// rather than looked up from backend storage, making it compatible with
    /// User API managed keys.
    ///
    /// # Arguments
    /// * `address` - The transport address of the peer to sync with
    /// * `tree_id` - The ID of the tree to sync
    /// * `requesting_public_key` - The formatted public key string (e.g., "ed25519:base64...")
    /// * `requesting_key_name` - The name/ID of the requesting key for audit trail
    /// * `requested_permission` - The permission level being requested
    ///
    /// # Returns
    /// A Result indicating success or failure.
    ///
    /// # Example
    /// ```rust,ignore
    /// // With User API managed keys:
    /// let public_key = user.get_public_key(user_key_id)?;
    /// sync.sync_with_peer_for_bootstrap_with_key(
    ///     &Address::http("127.0.0.1:8080"),
    ///     &tree_id,
    ///     &public_key,
    ///     user_key_id,
    ///     Permission::Write(5),
    /// ).await?;
    /// ```
    pub async fn sync_with_peer_for_bootstrap_with_key(
        &self,
        address: &Address,
        tree_id: &ID,
        requesting_public_key: &str,
        requesting_key_name: &str,
        requested_permission: Permission,
    ) -> Result<()> {
        // Delegate to internal method
        self.sync_with_peer_for_bootstrap_internal(
            address,
            tree_id,
            requesting_public_key.to_string(),
            requesting_key_name,
            requested_permission,
        )
        .await
    }

    // === Bootstrap Request Management Methods ===

    /// Get all pending bootstrap requests.
    ///
    /// # Returns
    /// A vector of (request_id, bootstrap_request) pairs for pending requests.
    pub async fn pending_bootstrap_requests(&self) -> Result<Vec<(String, BootstrapRequest)>> {
        let txn = self.sync_tree.new_transaction().await?;
        let manager = BootstrapRequestManager::new(&txn);
        manager.pending_requests().await
    }

    /// Get all approved bootstrap requests.
    ///
    /// # Returns
    /// A vector of (request_id, bootstrap_request) pairs for approved requests.
    pub async fn approved_bootstrap_requests(&self) -> Result<Vec<(String, BootstrapRequest)>> {
        let txn = self.sync_tree.new_transaction().await?;
        let manager = BootstrapRequestManager::new(&txn);
        manager.approved_requests().await
    }

    /// Get all rejected bootstrap requests.
    ///
    /// # Returns
    /// A vector of (request_id, bootstrap_request) pairs for rejected requests.
    pub async fn rejected_bootstrap_requests(&self) -> Result<Vec<(String, BootstrapRequest)>> {
        let txn = self.sync_tree.new_transaction().await?;
        let manager = BootstrapRequestManager::new(&txn);
        manager.rejected_requests().await
    }

    /// Get a specific bootstrap request by ID.
    ///
    /// # Arguments
    /// * `request_id` - The unique identifier of the request
    ///
    /// # Returns
    /// A tuple of (request_id, bootstrap_request) if found, None otherwise.
    pub async fn get_bootstrap_request(
        &self,
        request_id: &str,
    ) -> Result<Option<(String, BootstrapRequest)>> {
        let txn = self.sync_tree.new_transaction().await?;
        let manager = BootstrapRequestManager::new(&txn);

        match manager.get_request(request_id).await? {
            Some(request) => Ok(Some((request_id.to_string(), request))),
            None => Ok(None),
        }
    }

    /// Approve a bootstrap request using a `DatabaseKey`.
    ///
    /// This variant allows approval using keys that are not stored in the backend,
    /// such as user keys managed in memory.
    ///
    /// # Arguments
    /// * `request_id` - The unique identifier of the request to approve
    /// * `key` - The `DatabaseKey` to use for the transaction and audit trail
    ///
    /// # Returns
    /// Result indicating success or failure of the approval operation.
    ///
    /// # Errors
    /// Returns `SyncError::InsufficientPermission` if the approving key does not have
    /// Admin permission on the target database.
    pub async fn approve_bootstrap_request_with_key(
        &self,
        request_id: &str,
        key: &DatabaseKey,
    ) -> Result<()> {
        // Load the request from sync database
        let sync_op = self.sync_tree.new_transaction().await?;
        let manager = BootstrapRequestManager::new(&sync_op);

        let request = manager
            .get_request(request_id)
            .await?
            .ok_or_else(|| SyncError::RequestNotFound(request_id.to_string()))?;

        // Validate request is still pending
        if !matches!(request.status, RequestStatus::Pending) {
            return Err(SyncError::InvalidRequestState {
                request_id: request_id.to_string(),
                current_status: format!("{:?}", request.status),
                expected_status: "Pending".to_string(),
            }
            .into());
        }

        // Load the existing database with the user's signing key
        let database = Database::open(self.instance()?, &request.tree_id, key.clone()).await?;

        // Explicitly check that the approving user has Admin permission
        // This provides clear error messages and fails fast before modifying the database
        let permission = database.current_permission().await?;
        if !permission.can_admin() {
            return Err(SyncError::InsufficientPermission {
                request_id: request_id.to_string(),
                required_permission: "Admin".to_string(),
                actual_permission: permission,
            }
            .into());
        }

        // Create transaction - this will use the provided signing key
        let tx = database.new_transaction().await?;

        // Get settings store and update auth configuration
        let settings_store = tx.get_settings()?;

        // Create the auth key for the requesting device
        // Keys are stored by pubkey, with name as optional metadata
        let auth_key = AuthKey::active(
            Some(&request.requesting_key_name), // name metadata
            request.requested_permission.clone(),
        );

        // Add the new key to auth settings using SettingsStore API
        // Store by pubkey (this provides proper upsert behavior and validation)
        settings_store
            .set_auth_key(&request.requesting_pubkey, auth_key)
            .await?;

        // Commit will validate that the user's key has Admin permission
        // If this fails, it means the user lacks the necessary permission
        tx.commit().await?;

        // Update request status to approved
        let approver_id = key.identity().display_id();
        let approval_time = self
            .instance
            .upgrade()
            .ok_or(SyncError::InstanceDropped)?
            .clock()
            .now_rfc3339();
        manager
            .update_status(
                request_id,
                RequestStatus::Approved {
                    approved_by: approver_id.to_string(),
                    approval_time,
                },
            )
            .await?;
        sync_op.commit().await?;

        info!(
            request_id = %request_id,
            tree_id = %request.tree_id,
            approved_by = %approver_id,
            "Bootstrap request approved and key added to database using user-provided key"
        );

        Ok(())
    }

    /// Reject a bootstrap request using a `DatabaseKey` with Admin permission validation.
    ///
    /// This variant allows rejection using keys that are not stored in the backend,
    /// such as user keys managed in memory. It validates that the rejecting user has
    /// Admin permission on the target database before allowing the rejection.
    ///
    /// # Arguments
    /// * `request_id` - The unique identifier of the request to reject
    /// * `key` - The `DatabaseKey` to use for permission validation and audit trail
    ///
    /// # Returns
    /// Result indicating success or failure of the rejection operation.
    ///
    /// # Errors
    /// Returns `SyncError::InsufficientPermission` if the rejecting key does not have
    /// Admin permission on the target database.
    pub async fn reject_bootstrap_request_with_key(
        &self,
        request_id: &str,
        key: &DatabaseKey,
    ) -> Result<()> {
        // Load the request from sync database
        let sync_op = self.sync_tree.new_transaction().await?;
        let manager = BootstrapRequestManager::new(&sync_op);

        let request = manager
            .get_request(request_id)
            .await?
            .ok_or_else(|| SyncError::RequestNotFound(request_id.to_string()))?;

        // Validate request is still pending
        if !matches!(request.status, RequestStatus::Pending) {
            return Err(SyncError::InvalidRequestState {
                request_id: request_id.to_string(),
                current_status: format!("{:?}", request.status),
                expected_status: "Pending".to_string(),
            }
            .into());
        }

        // Load the existing database with the user's signing key to validate permissions
        let database = Database::open(self.instance()?, &request.tree_id, key.clone()).await?;

        // Check that the rejecting user has Admin permission
        let permission = database.current_permission().await?;
        if !permission.can_admin() {
            return Err(SyncError::InsufficientPermission {
                request_id: request_id.to_string(),
                required_permission: "Admin".to_string(),
                actual_permission: permission,
            }
            .into());
        }

        // User has Admin permission, proceed with rejection
        let rejecter_id = key.identity().display_id();
        let rejection_time = self
            .instance
            .upgrade()
            .ok_or(SyncError::InstanceDropped)?
            .clock()
            .now_rfc3339();
        manager
            .update_status(
                request_id,
                RequestStatus::Rejected {
                    rejected_by: rejecter_id.to_string(),
                    rejection_time,
                },
            )
            .await?;
        sync_op.commit().await?;

        info!(
            request_id = %request_id,
            tree_id = %request.tree_id,
            rejected_by = %rejecter_id,
            "Bootstrap request rejected by user with Admin permission"
        );

        Ok(())
    }
}
