//! Bootstrap sync operations and request management.

use tracing::{Instrument, info};

use super::protocol::{SyncRequestAuth, SyncTreeRequest};
use super::{
    Address, BootstrapRequest, DatabaseTicket, RequestStatus, Sync, SyncError,
    bootstrap_request_manager::BootstrapRequestManager, operation::SyncOperation,
};
use crate::{
    Database, Result, Snapshot,
    auth::{
        Permission,
        crypto::{PrivateKey, PublicKey},
        types::AuthKey,
    },
    constants::SETTINGS,
    crdt::Doc,
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
    // Use `sync_with_peer_for_bootstrap_with_key()` with AccountSession API managed keys.

    /// Internal helper for bootstrap sync operations.
    ///
    /// This method contains the common logic for bootstrap scenarios where the local
    /// device doesn't have access to the target tree yet and needs to request
    /// permission during the initial sync.
    ///
    /// # Arguments
    /// * `address` - The transport address of the peer to sync with
    /// * `tree_id` - The ID of the tree to sync
    /// * `requesting_key` - The private key to sign the request with and request access for
    /// * `requesting_key_name` - The name/ID of the requesting key
    /// * `requested_permission` - The permission level being requested
    ///
    /// # Returns
    /// A Result indicating success or failure.
    ///
    /// # Errors
    /// * `SyncError::InvalidPublicKey` if the public key is empty or malformed
    /// * `SyncError::InvalidKeyName` if the key name is empty
    #[tracing::instrument(skip_all, fields(tree = %tree_id, address = ?address))]
    async fn sync_with_peer_for_bootstrap_internal(
        &self,
        address: &Address,
        tree_id: &ID,
        requesting_key: &PrivateKey,
        requesting_key_name: &str,
        requested_permission: Permission,
        metadata: Option<Doc>,
    ) -> Result<()> {
        let mut operation = SyncOperation::new("bootstrap_address");
        let span = operation.span();
        let result = async {
            // Validate key name is not empty
            if requesting_key_name.is_empty() {
                return Err(SyncError::InvalidKeyName {
                    reason: "Key name cannot be empty".to_string(),
                }
                .into());
            }

            // Connect to peer if not already connected
            let peer_pubkey = self.connect_for_operation(address, &mut operation).await?;

            // Store the address for this peer
            self.add_peer_address(&peer_pubkey, address.clone()).await?;

            // Sync tree with authentication
            self.sync_tree_with_peer_auth_in_operation(
                &peer_pubkey,
                tree_id,
                Some(requesting_key),
                Some(requesting_key_name),
                Some(requested_permission),
                metadata,
                &mut operation,
            )
            .await?;

            Ok(())
        }
        .instrument(span)
        .await;
        operation.finish(&result);
        result
    }

    /// Sync with a peer for bootstrap using a user-provided public key.
    ///
    /// This method is specifically designed for bootstrap scenarios where the local
    /// device doesn't have access to the target tree yet and needs to request
    /// permission during the initial sync. The public key is provided directly
    /// rather than looked up from backend storage, making it compatible with
    /// AccountSession API managed keys.
    ///
    /// # Arguments
    /// * `address` - The transport address of the peer to sync with
    /// * `tree_id` - The ID of the tree to sync
    /// * `requesting_key` - The private key to sign the request with and request access for
    /// * `requesting_key_name` - The name/ID of the requesting key for audit trail
    /// * `requested_permission` - The permission level being requested
    ///
    /// # Returns
    /// A Result indicating success or failure.
    ///
    /// # Example
    /// ```rust,ignore
    /// // With AccountSession API managed keys:
    /// let signing_key = user.get_signing_key(user_key_id)?;
    /// sync.sync_with_peer_for_bootstrap_with_key(
    ///     &Address::http("127.0.0.1:8080"),
    ///     &tree_id,
    ///     &signing_key,
    ///     user_key_id,
    ///     Permission::Write(5),
    /// ).await?;
    /// ```
    pub async fn sync_with_peer_for_bootstrap_with_key(
        &self,
        address: &Address,
        tree_id: &ID,
        requesting_key: &PrivateKey,
        requesting_key_name: &str,
        requested_permission: Permission,
    ) -> Result<()> {
        // Delegate to internal method. This lower-level entry point carries no
        // approver metadata; use `bootstrap_with_ticket` to attach it.
        self.sync_with_peer_for_bootstrap_internal(
            address,
            tree_id,
            requesting_key,
            requesting_key_name,
            requested_permission,
            None,
        )
        .await
    }

    /// Bootstrap with a peer using a [`DatabaseTicket`].
    ///
    /// Races bounded handshakes against every address hint, then performs the
    /// bootstrap exchange once through the first usable route.
    ///
    /// # Arguments
    /// * `ticket` - A ticket containing the database ID and address hints.
    /// * `requesting_key` - The private key to sign the request with and request access for.
    /// * `requesting_key_name` - The name/ID of the requesting key.
    /// * `requested_permission` - The permission level being requested.
    /// * `metadata` - Optional free-form context the requester attaches for the
    ///   approver to inspect, surfaced verbatim on the stored [`BootstrapRequest`].
    ///
    /// # Errors
    /// Returns [`SyncError::InvalidAddress`] if the ticket has no address hints.
    /// Returns the last sync error if no address succeeded.
    #[tracing::instrument(skip_all, fields(tree = %ticket.database_id()))]
    pub async fn bootstrap_with_ticket(
        &self,
        ticket: &DatabaseTicket,
        requesting_key: &PrivateKey,
        requesting_key_name: &str,
        requested_permission: Permission,
        metadata: Option<Doc>,
    ) -> Result<()> {
        self.bootstrap_ticket_request(
            ticket,
            requesting_key,
            requesting_key_name,
            requested_permission,
            metadata,
            false,
        )
        .await
    }

    /// Request an explicit key membership grant through the ordinary bootstrap lifecycle.
    /// Public/global or delegated access does not substitute for that direct grant.
    /// Uses a distinct request identity and a proof bound to membership intent.
    pub(crate) async fn request_membership_with_ticket(
        &self,
        ticket: &DatabaseTicket,
        requesting_key: &PrivateKey,
        requesting_key_name: &str,
        requested_permission: Permission,
        metadata: Option<Doc>,
    ) -> Result<()> {
        self.bootstrap_ticket_request(
            ticket,
            requesting_key,
            requesting_key_name,
            requested_permission,
            metadata,
            true,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn bootstrap_ticket_request(
        &self,
        ticket: &DatabaseTicket,
        requesting_key: &PrivateKey,
        requesting_key_name: &str,
        requested_permission: Permission,
        metadata: Option<Doc>,
        require_direct_key: bool,
    ) -> Result<()> {
        let mut operation = SyncOperation::new("bootstrap_ticket");
        let span = operation.span();
        let result = async {
            let database_id = ticket.database_id().clone();
            let instance = self.instance()?;
            // Held/unreadable acquisition remains fail-closed. Empty tips protect
            // confidentiality; they do not settle native competing grant/revoke rules.
            if !instance.backend().snapshot(&database_id).await?.is_empty()
                && !Database::can_access(
                    &instance,
                    &database_id,
                    &requesting_key.public_key(),
                    &Permission::Read,
                )
                .await?
            {
                return Err(SyncError::AuthenticationFailed(
                    "held database is not readable by the requesting key".to_string(),
                )
                .into());
            }
            if requesting_key_name.is_empty() {
                return Err(SyncError::InvalidKeyName {
                    reason: "Key name cannot be empty".to_string(),
                }
                .into());
            }
            let (address, peer_pubkey) = self
                .select_address(ticket.addresses(), None, &mut operation)
                .await?;
            let tips = Snapshot::EMPTY;
            let auth = SyncRequestAuth::sign_access(
                requesting_key,
                &peer_pubkey,
                &database_id,
                &tips,
                instance.clock().now_millis(),
                require_direct_key.then_some(requested_permission),
            );
            self.add_peer_address(&peer_pubkey, address.clone()).await?;
            let result = self
                .exchange_tree_request_at(
                    &address,
                    &peer_pubkey,
                    SyncTreeRequest {
                        tree_id: database_id,
                        our_tips: tips,
                        peer_pubkey: self.get_device_pubkey().ok(),
                        requesting_key: Some(requesting_key.public_key()),
                        requesting_key_name: Some(requesting_key_name.to_string()),
                        requested_permission: Some(requested_permission),
                        metadata,
                        auth: Some(auth),
                    },
                )
                .await;
            operation.tree_result(&result);
            result
        }
        .instrument(span)
        .await;
        operation.finish(&result);
        result
    }

    /// Select a ticket route and report what a remote signer must bind.
    ///
    /// A request proof is bound to the answering peer and to the tips the
    /// request advertises, so a signer outside this process (a service client
    /// holding its own key) needs both before it can sign. Returns the chosen
    /// address, the identity that answered on it, and an empty tip set.
    /// Preparation never reads or discloses local database state.
    #[tracing::instrument(skip_all, fields(tree = %ticket.database_id()))]
    pub(crate) async fn prepare_ticket_bootstrap(
        &self,
        ticket: &DatabaseTicket,
    ) -> Result<(Address, PublicKey, Snapshot)> {
        let mut operation = SyncOperation::new("prepare_ticket");
        let span = operation.span();
        let result = async {
            let (address, peer_pubkey) = self
                .select_address(ticket.addresses(), None, &mut operation)
                .await?;
            Ok((address, peer_pubkey, Snapshot::EMPTY))
        }
        .instrument(span)
        .await;
        operation.finish(&result);
        result
    }

    /// Bootstrap a database with a request whose proof was signed elsewhere.
    ///
    /// The counterpart of [`Self::bootstrap_with_ticket`] for a requester
    /// whose private key never enters this process: the caller supplies the
    /// complete named-key request, including its [`SyncRequestAuth`] proof.
    /// The proof is checked here before anything is sent, so this instance
    /// never relays a request the peer would reject for a mismatched key,
    /// peer, database, or tip set. Pending and rejected outcomes surface as
    /// [`SyncError::BootstrapPending`] and [`SyncError::BootstrapRejected`].
    #[tracing::instrument(skip_all, fields(peer = %peer_pubkey, tree = %request.tree_id))]
    pub(crate) async fn bootstrap_with_signed_request(
        &self,
        address: &Address,
        peer_pubkey: &PublicKey,
        mut request: SyncTreeRequest,
    ) -> Result<()> {
        let mut operation = SyncOperation::new("bootstrap_signed");
        let span = operation.span();
        let result = async {
            if !request.our_tips.is_empty() {
                return Err(SyncError::SyncProtocolError(
                    "ticket bootstrap must advertise empty tips".to_string(),
                )
                .into());
            }
            let auth = request
                .auth
                .as_ref()
                .ok_or_else(|| SyncError::AuthenticationRequired(request.tree_id.to_string()))?;
            if request.requesting_key.as_ref() != Some(&auth.key) {
                return Err(SyncError::InvalidPublicKey {
                    reason: "requesting key does not match the request proof".to_string(),
                }
                .into());
            }
            if request
                .requesting_key_name
                .as_deref()
                .is_none_or(str::is_empty)
            {
                return Err(SyncError::InvalidKeyName {
                    reason: "Key name cannot be empty".to_string(),
                }
                .into());
            }
            let permission = request.requested_permission.ok_or_else(|| SyncError::SyncProtocolError(
                    "bootstrap request must name a permission".to_string(),
                ))?;
            auth.verify_access(peer_pubkey, &request.tree_id, &request.our_tips, Some(permission))
                .map_err(|_| {
                    SyncError::AuthenticationFailed(
                        "request proof does not cover this peer, database, tip set or membership intent".to_string(),
                    )
                })?;

            // The route arrives from outside this process, so prove the address
            // answers as `peer_pubkey` before recording it in the shared peer
            // store; otherwise a caller could attach any address to a real peer.
            self.select_address(
                std::slice::from_ref(address),
                Some(peer_pubkey),
                &mut operation,
            )
            .await?;
            request.peer_pubkey = self.get_device_pubkey().ok();
            self.add_peer_address(peer_pubkey, address.clone()).await?;
            let result = self
                .exchange_tree_request_at(address, peer_pubkey, request)
                .await;
            operation.tree_result(&result);
            result
        }
        .instrument(span)
        .await;
        operation.finish(&result);
        result
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
    /// Returns `SyncError::InsufficientPermission` if the approving key lacks current
    /// Admin permission or a new grant would exceed its permission/priority ceiling.
    pub async fn approve_bootstrap_request_with_key(
        &self,
        request_id: &str,
        key: &DatabaseKey,
    ) -> Result<()> {
        let instance = self.instance()?;
        let lock = instance.tree_lock(self.sync_tree.root_id());
        self.sync_tree.snapshot().await?;
        let guard = lock.lock_owned().await;

        // Load the request from sync database
        let sync_snapshot = Snapshot::from(self.sync_tree.verified_frontier().await?);
        let sync_op = self.sync_tree.new_transaction_at(&sync_snapshot).await?;
        let manager = BootstrapRequestManager::new(&sync_op);

        let request = manager
            .get_request(request_id)
            .await?
            .ok_or_else(|| SyncError::RequestNotFound(request_id.to_string()))?;

        if matches!(request.status, RequestStatus::Rejected { .. }) {
            return Err(SyncError::InvalidRequestState {
                request_id: request_id.to_string(),
                current_status: format!("{:?}", request.status),
                expected_status: "Pending".to_string(),
            }
            .into());
        }

        // Load the existing database with the user's signing key
        let database = Database::open(&self.instance()?, &request.tree_id)
            .await?
            .with_key(key.clone());

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

        // A completed decision acknowledges its history, not a new grant.
        if matches!(request.status, RequestStatus::Approved { .. }) {
            return Ok(());
        }
        if !request_satisfied(
            &instance,
            &request.tree_id,
            &request.requesting_pubkey,
            &request.requested_permission,
            request.require_direct_key,
        )
        .await?
        {
            let target_guard = instance.tree_lock(database.root_id()).lock_owned().await;
            let snapshot = Snapshot::from(database.verified_frontier().await?);
            let tx = database.new_transaction_at(&snapshot).await?;
            require_new_grant(&instance, &database, &snapshot, &request).await?;
            // Key creation must obey the same permission/priority ceiling as
            // ordinary Auth management. Recheck under the target tree lock so
            // a concurrent downgrade cannot turn an old Admin check into a grant.
            let permission = database.current_permission().await?;
            if !permission.can_admin() || permission < request.requested_permission {
                return Err(SyncError::InsufficientPermission {
                    request_id: request_id.to_string(),
                    required_permission: format!("{:?}", request.requested_permission),
                    actual_permission: permission,
                }
                .into());
            }
            tx.get_settings()?
                .set_auth_key(
                    &request.requesting_pubkey,
                    AuthKey::active(
                        Some(&request.requesting_key_name),
                        request.requested_permission,
                    ),
                )
                .await?;
            tx.commit_under_tree_lock(target_guard).await?;
        }

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
        sync_op.commit_under_tree_lock(guard).await?;

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
        let instance = self.instance()?;
        let lock = instance.tree_lock(self.sync_tree.root_id());
        self.sync_tree.snapshot().await?;
        let guard = lock.lock_owned().await;

        // Load the request from sync database
        let sync_snapshot = Snapshot::from(self.sync_tree.verified_frontier().await?);
        let sync_op = self.sync_tree.new_transaction_at(&sync_snapshot).await?;
        let manager = BootstrapRequestManager::new(&sync_op);

        let request = manager
            .get_request(request_id)
            .await?
            .ok_or_else(|| SyncError::RequestNotFound(request_id.to_string()))?;

        // Validate request is still pending
        if matches!(request.status, RequestStatus::Approved { .. }) {
            return Err(SyncError::InvalidRequestState {
                request_id: request_id.to_string(),
                current_status: format!("{:?}", request.status),
                expected_status: "Pending".to_string(),
            }
            .into());
        }

        // Load the existing database with the user's signing key to validate permissions
        let database = Database::open(&self.instance()?, &request.tree_id)
            .await?
            .with_key(key.clone());

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

        if matches!(request.status, RequestStatus::Rejected { .. }) {
            return Ok(());
        }
        // AccountSession has Admin permission, proceed with rejection
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
        sync_op.commit_under_tree_lock(guard).await?;

        info!(
            request_id = %request_id,
            tree_id = %request.tree_id,
            rejected_by = %rejecter_id,
            "Bootstrap request rejected by user with Admin permission"
        );

        Ok(())
    }
}

async fn require_new_grant(
    instance: &crate::Instance,
    database: &Database,
    snapshot: &Snapshot,
    request: &BootstrapRequest,
) -> Result<()> {
    let auth = database.auth_settings_at(snapshot).await?;
    let key_path = format!("auth.keys.{}", request.requesting_pubkey);
    // Missing current authority is not proof it was never granted.
    // Reuse signed history; uncertain/reduced/removed authority needs a
    // fresh explicit settings decision, not replay of this Pending row.
    let mut seen_authority = auth.get_key_by_pubkey(&request.requesting_pubkey).is_ok();
    for entry in instance
        .backend()
        .store_at(database.root_id(), SETTINGS, snapshot)
        .await?
    {
        let delta: Doc = serde_json::from_slice(entry.data(SETTINGS)?)?;
        if delta.get(&key_path).is_some()
            || delta.is_tombstone(&key_path)
            || delta.get("auth.global").is_some()
            || delta.is_tombstone("auth.global")
            || delta.get("auth.delegations").is_some()
            || delta.is_tombstone("auth.delegations")
        {
            seen_authority = true;
            break;
        }
    }
    if seen_authority || *snapshot != instance.backend().snapshot(database.root_id()).await? {
        return Err(SyncError::PermissionDenied("approval retry cannot restore reduced, revoked, removed or uncertain authority; explicitly grant access before finalizing".to_string()).into());
    }
    Ok(())
}

/// Shared explicit-key versus ordinary access decision. Global Read never counts
/// as direct membership; ordinary joins retain their existing authority discovery.
pub(crate) async fn request_satisfied(
    instance: &crate::Instance,
    tree: &ID,
    key: &PublicKey,
    permission: &Permission,
    require_direct_key: bool,
) -> Result<bool> {
    if !require_direct_key {
        return Database::can_access(instance, tree, key, permission).await;
    }
    let auth = Database::open(instance, tree)
        .await?
        .live_auth_settings()
        .await?;
    Ok(auth
        .get_key_by_pubkey(key)
        .is_ok_and(|grant| grant.is_active() && grant.permissions() >= permission))
}

#[cfg(all(unix, feature = "service"))]
pub(crate) struct PreparedAccessGrant {
    root: ID,
    request_id: String,
    observed_request: BootstrapRequest,
    target: Snapshot,
    key: PublicKey,
    identity: crate::auth::SigKey,
    pub(crate) draft: crate::transaction::PreparedEntry,
}

#[cfg(all(unix, feature = "service"))]
impl Sync {
    /// Stage exactly the native grant for a Pending row (or return None if
    /// no grant is needed). Never hold a tree lock across the IPC round trip.
    pub(crate) async fn prepare_access_grant(
        &self,
        root: &ID,
        request_id: &str,
        key: &PublicKey,
        identity: &crate::auth::SigKey,
    ) -> Result<Option<PreparedAccessGrant>> {
        let instance = self.instance()?;
        self.require_request_admin(root, key, identity).await?;
        let sync_tx = self.sync_tree.new_transaction().await?;
        let observed_request = BootstrapRequestManager::new(&sync_tx)
            .get_request(request_id)
            .await?
            .filter(|row| &row.tree_id == root && matches!(row.status, RequestStatus::Pending))
            .ok_or_else(|| SyncError::RequestNotFound(request_id.to_owned()))?;
        if request_satisfied(
            &instance,
            root,
            &observed_request.requesting_pubkey,
            &observed_request.requested_permission,
            observed_request.require_direct_key,
        )
        .await?
        {
            return Ok(None);
        }
        let db = Database::open(&instance, root).await?;
        let target = db.snapshot().await?;
        grant_permission(
            &instance,
            &db,
            &target,
            key,
            identity,
            observed_request.requested_permission,
        )
        .await?;
        require_new_grant(&instance, &db, &target, &observed_request).await?;
        let txn = db.new_transaction_at(&target).await?;
        txn.get_settings()?
            .set_auth_key(
                &observed_request.requesting_pubkey,
                AuthKey::active(
                    Some(&observed_request.requesting_key_name),
                    observed_request.requested_permission,
                ),
            )
            .await?;
        let draft = txn.prepare_entry(identity).await?;
        Ok(Some(PreparedAccessGrant {
            root: root.clone(),
            request_id: request_id.to_owned(),
            observed_request,
            target,
            key: key.clone(),
            identity: identity.clone(),
            draft,
        }))
    }

    /// Write only the prepared grant at the exact observed frontier and for
    /// the exact still-pending row. Signature and Auth validation are native.
    pub(crate) async fn commit_access_grant(
        &self,
        prepared: PreparedAccessGrant,
        signed: crate::Entry,
        key: &PublicKey,
        identity: &crate::auth::SigKey,
    ) -> Result<()> {
        let instance = self.instance()?;
        if &prepared.key != key || &prepared.identity != identity {
            return Err(grant_changed());
        }
        let root = &prepared.root;
        let db = Database::open(&instance, root).await?;
        self.sync_tree.snapshot().await?;
        let sync_guard = instance
            .tree_lock(self.sync_tree.root_id())
            .lock_owned()
            .await;
        let tx = self
            .sync_tree
            .new_transaction_at(&Snapshot::from(self.sync_tree.verified_frontier().await?))
            .await?;
        let queue = BootstrapRequestManager::new(&tx);
        if queue.get_request(&prepared.request_id).await?.as_ref()
            != Some(&prepared.observed_request)
        {
            return Err(grant_changed());
        }
        let target_guard = instance.tree_lock(root).lock_owned().await;
        let snapshot = Snapshot::from(db.verified_frontier().await?);
        if snapshot != prepared.target {
            return Err(grant_changed());
        }
        grant_permission(
            &instance,
            &db,
            &snapshot,
            key,
            identity,
            prepared.observed_request.requested_permission,
        )
        .await?;
        require_new_grant(&instance, &db, &snapshot, &prepared.observed_request).await?;
        prepared.draft.validate_signed(&instance, &signed).await?;
        // Do not use generic historical-entry ingestion for a live action.
        instance
            .put_entry_under_tree_lock(
                target_guard,
                root,
                crate::backend::VerificationStatus::Verified,
                signed,
                crate::instance::WriteSource::Local,
            )
            .await?;
        if !request_satisfied(
            &instance,
            root,
            &prepared.observed_request.requesting_pubkey,
            &prepared.observed_request.requested_permission,
            prepared.observed_request.require_direct_key,
        )
        .await?
        {
            return Err(grant_changed());
        }
        queue
            .update_status(
                &prepared.request_id,
                RequestStatus::Approved {
                    approved_by: identity.display_id(),
                    approval_time: instance.clock().now_rfc3339(),
                },
            )
            .await?;
        tx.commit_under_tree_lock(sync_guard).await?;
        Ok(())
    }
}

#[cfg(all(unix, feature = "service"))]
fn grant_changed() -> crate::Error {
    SyncError::PermissionDenied(
        "access request or current authority changed; refresh before retrying".to_owned(),
    )
    .into()
}

#[cfg(all(unix, feature = "service"))]
async fn grant_permission(
    instance: &crate::Instance,
    db: &Database,
    snapshot: &Snapshot,
    key: &PublicKey,
    identity: &crate::auth::SigKey,
    requested: Permission,
) -> Result<()> {
    let permission = crate::auth::validation::permissions::resolve_identity_permission(
        key,
        identity,
        &db.auth_settings_at(snapshot).await?,
        Some(instance),
    )
    .await?;
    if !permission.can_admin() || permission < requested {
        return Err(grant_changed());
    }
    Ok(())
}

impl Sync {
    /// Read only one authorized target's pending requests, never the raw sync store.
    pub(crate) async fn pending_access_requests_for(
        &self,
        root_id: &ID,
        key: &PublicKey,
        identity: &crate::auth::SigKey,
    ) -> Result<Vec<(String, BootstrapRequest)>> {
        self.require_request_admin(root_id, key, identity).await?;
        let tx = self.sync_tree.new_transaction().await?;
        BootstrapRequestManager::new(&tx)
            .pending_requests_for(root_id)
            .await
    }

    async fn require_request_admin(
        &self,
        root_id: &ID,
        key: &PublicKey,
        identity: &crate::auth::SigKey,
    ) -> Result<()> {
        let instance = self.instance()?;
        let denied = || crate::auth::AuthError::PermissionDenied {
            reason: format!("access request management requires Admin on {root_id}"),
        };
        // The lifecycle store is not an enrollment target (and this check can
        // run under its lock while committing a terminal decision).
        if root_id == self.sync_tree.root_id() || !instance.has_database(root_id).await {
            return Err(denied().into());
        }
        let auth = Database::open(&instance, root_id)
            .await?
            .live_auth_settings()
            .await?;
        let permission = crate::auth::validation::permissions::resolve_identity_permission(
            key,
            identity,
            &auth,
            Some(&instance),
        )
        .await
        .map_err(|_| denied())?;
        if !permission.can_admin() {
            return Err(denied().into());
        }
        Ok(())
    }

    /// Finalize an explicit admin decision after any ordinary target Auth write.
    /// Approval never creates/restores a grant: sufficient current authority must
    /// already exist. This is the shared connected-client bookkeeping seam.
    pub(crate) async fn finish_access_request(
        &self,
        root_id: &ID,
        request_id: &str,
        key: &PublicKey,
        identity: &crate::auth::SigKey,
        reject: bool,
    ) -> Result<()> {
        let instance = self.instance()?;
        self.sync_tree.snapshot().await?;
        let guard = instance
            .tree_lock(self.sync_tree.root_id())
            .lock_owned()
            .await;
        self.require_request_admin(root_id, key, identity).await?;
        let snapshot = Snapshot::from(self.sync_tree.verified_frontier().await?);
        let tx = self.sync_tree.new_transaction_at(&snapshot).await?;
        let manager = BootstrapRequestManager::new(&tx);
        let request = manager
            .get_request(request_id)
            .await?
            .filter(|request| &request.tree_id == root_id)
            .ok_or_else(|| SyncError::RequestNotFound(request_id.to_owned()))?;
        let status = match (&request.status, reject) {
            (RequestStatus::Pending, true) => RequestStatus::Rejected {
                rejected_by: identity.display_id(),
                rejection_time: instance.clock().now_rfc3339(),
            },
            (RequestStatus::Pending, false) => {
                if !request_satisfied(
                    &instance,
                    &request.tree_id,
                    &request.requesting_pubkey,
                    &request.requested_permission,
                    request.require_direct_key,
                )
                .await?
                {
                    return Err(SyncError::PermissionDenied(
                        "explicitly grant the requested authority before finalizing approval"
                            .to_owned(),
                    )
                    .into());
                }
                RequestStatus::Approved {
                    approved_by: identity.display_id(),
                    approval_time: instance.clock().now_rfc3339(),
                }
            }
            (RequestStatus::Approved { .. }, false) | (RequestStatus::Rejected { .. }, true) => {
                return Ok(());
            }
            _ => {
                return Err(SyncError::InvalidRequestState {
                    request_id: request_id.to_owned(),
                    current_status: format!("{:?}", request.status),
                    expected_status: "Pending or matching terminal decision".to_owned(),
                }
                .into());
            }
        };
        manager.update_status(request_id, status).await?;
        tx.commit_under_tree_lock(guard).await?;
        Ok(())
    }
}
