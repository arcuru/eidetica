//! Bootstrap sync operations and request management.

use tracing::{debug, info, warn};

use super::{
    Address, BootstrapRequest, DatabaseTicket, RequestStatus, Sync, SyncError,
    bootstrap_request_manager::BootstrapRequestManager,
    outgoing_bootstrap_request_manager::{
        OutgoingBootstrapRequest, OutgoingBootstrapRequestManager, OutgoingRequestStatus,
    },
    peer_manager::PeerManager,
};
use crate::{
    Database, Result,
    auth::{Permission, crypto::PublicKey, types::AuthKey},
    crdt::Doc,
    database::DatabaseKey,
    entry::ID,
    user::types::SyncSettings,
};

/// RAII guard marking an outgoing-bootstrap completion in flight for a tree.
///
/// Claims the tree id in [`Sync::completing_bootstraps`](super::Sync) on
/// construction and releases it on drop, so a re-entrant completion (triggered
/// by the pull's own remote-write callback) becomes a no-op rather than an
/// overlapping pull.
struct InFlightGuard<'a> {
    sync: &'a Sync,
    tree_id: ID,
}

impl<'a> InFlightGuard<'a> {
    /// Claim `tree_id`, returning a guard, or `None` if a completion for it is
    /// already in flight.
    fn try_claim(sync: &'a Sync, tree_id: ID) -> Option<Self> {
        let mut set = sync.completing_bootstraps.lock().unwrap();
        if set.insert(tree_id.clone()) {
            drop(set);
            Some(Self { sync, tree_id })
        } else {
            None
        }
    }
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.sync
            .completing_bootstraps
            .lock()
            .unwrap()
            .remove(&self.tree_id);
    }
}

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
        requesting_public_key: &PublicKey,
        requesting_key_name: &str,
        requested_permission: Permission,
        metadata: Option<Doc>,
    ) -> Result<()> {
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

        // Sync tree with authentication (ordinary tip-driven bootstrap-vs-incremental)
        self.sync_tree_with_peer_auth(
            &peer_pubkey,
            tree_id,
            Some(requesting_public_key),
            Some(requesting_key_name),
            Some(requested_permission),
            metadata,
            false,
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
        requesting_public_key: &PublicKey,
        requesting_key_name: &str,
        requested_permission: Permission,
    ) -> Result<()> {
        // Delegate to internal method. This lower-level entry point carries no
        // approver metadata; use `bootstrap_with_ticket` to attach it.
        self.sync_with_peer_for_bootstrap_internal(
            address,
            tree_id,
            requesting_public_key,
            requesting_key_name,
            requested_permission,
            None,
        )
        .await
    }

    /// Bootstrap with a peer using a [`DatabaseTicket`].
    ///
    /// Tries every address hint in the ticket concurrently. Succeeds if at
    /// least one address connects and syncs; returns the last error if all
    /// fail.
    ///
    /// # Arguments
    /// * `ticket` - A ticket containing the database ID and address hints.
    /// * `requesting_public_key` - The formatted public key string for authentication.
    /// * `requesting_key_name` - The name/ID of the requesting key.
    /// * `requested_permission` - The permission level being requested.
    /// * `metadata` - Optional free-form context the requester attaches for the
    ///   approver to inspect, surfaced verbatim on the stored [`BootstrapRequest`].
    /// * `sync_settings` - The caller's desired sync settings for the database,
    ///   applied locally once access is granted. Passed in as plain data so the
    ///   sync layer never reads User state (dependency direction stays User ->
    ///   Sync).
    ///
    /// This is the raw sync-layer primitive: it grants sync-layer access (auth
    /// entries + data sync) but does **not** establish the User-layer SigKey
    /// mapping that [`User::open_database`](crate::user::User::open_database) and
    /// `find_key` require. Calling it alone leaves the database unopenable ("No
    /// key found for database"). It is therefore `pub(crate)`; external callers
    /// must go through [`User::request_database_access`](crate::user::User::request_database_access),
    /// which performs both halves. Callers that must run the network round-trip
    /// without holding a `&mut User` lock use
    /// [`User::request_database_access_network`](crate::user::User::request_database_access_network)
    /// followed by
    /// [`User::record_database_access`](crate::user::User::record_database_access).
    ///
    /// When the peer requires manual approval the request comes back
    /// [`SyncError::BootstrapPending`]; before re-raising it, an
    /// [`OutgoingBootstrapRequest`] capturing the ticket target, addresses, key,
    /// permission, and `sync_settings` is persisted so Sync can finish the join
    /// locally once the approval lands (see
    /// [`complete_outgoing_bootstrap`](Self::complete_outgoing_bootstrap)).
    ///
    /// # Errors
    /// Returns [`SyncError::InvalidAddress`] if the ticket has no address hints.
    /// Returns the last sync error if no address succeeded.
    pub(crate) async fn bootstrap_with_ticket(
        &self,
        ticket: &DatabaseTicket,
        requesting_public_key: &PublicKey,
        requesting_key_name: &str,
        requested_permission: Permission,
        metadata: Option<Doc>,
        sync_settings: SyncSettings,
    ) -> Result<()> {
        let database_id = ticket.database_id().clone();
        let pubkey = requesting_public_key.clone();
        let key_name = requesting_key_name.to_string();
        let result = self
            .try_addresses_concurrently(ticket.addresses(), |sync, addr| {
                let db_id = database_id.clone();
                let pubkey = pubkey.clone();
                let key_name = key_name.clone();
                let metadata = metadata.clone();
                async move {
                    sync.sync_with_peer_for_bootstrap_internal(
                        &addr,
                        &db_id,
                        &pubkey,
                        &key_name,
                        requested_permission,
                        metadata,
                    )
                    .await
                }
            })
            .await;

        // On manual-approval peers the request is queued rather than granted.
        // Record everything needed to finish the join once access is granted,
        // then re-raise the pending error unchanged. Recording is best-effort:
        // failing to persist it must not mask the pending status the caller
        // reacts to; the periodic sweep only completes what was recorded.
        if let Err(e) = &result
            && let crate::Error::Sync(sync_err) = e
            && matches!(sync_err.as_ref(), SyncError::BootstrapPending { .. })
        {
            let record = OutgoingBootstrapRequest {
                tree_id: database_id.clone(),
                addresses: ticket.addresses().to_vec(),
                requesting_pubkey: requesting_public_key.clone(),
                requesting_key_name: requesting_key_name.to_string(),
                requested_permission,
                sync_settings,
                timestamp: self.instance()?.clock().now_rfc3339(),
                status: OutgoingRequestStatus::Pending,
            };
            if let Err(record_err) = self.record_outgoing_bootstrap_request(record).await {
                warn!(
                    tree_id = %database_id,
                    error = %record_err,
                    "Bootstrap pending but failed to record outgoing request for later completion"
                );
            }
        }

        result
    }

    /// Mark an outgoing bootstrap request rejected so the sweep stops retrying it.
    async fn retire_outgoing_bootstrap(&self, request_id: &str) -> Result<()> {
        let txn = self.sync_tree.new_transaction().await?;
        let manager = OutgoingBootstrapRequestManager::new(&txn);
        manager.mark_rejected(request_id).await?;
        txn.commit().await?;
        Ok(())
    }

    /// Persist an [`OutgoingBootstrapRequest`] in the `_sync` tree.
    async fn record_outgoing_bootstrap_request(
        &self,
        request: OutgoingBootstrapRequest,
    ) -> Result<()> {
        let txn = self.sync_tree.new_transaction().await?;
        let manager = OutgoingBootstrapRequestManager::new(&txn);
        manager.store_request(request).await?;
        txn.commit().await?;
        Ok(())
    }

    /// Get all pending outgoing bootstrap requests recorded on this node.
    ///
    /// The client-side symmetric counterpart to
    /// [`pending_bootstrap_requests`](Self::pending_bootstrap_requests): those
    /// are requests this node must approve, these are requests this node sent
    /// and is still waiting on.
    pub async fn pending_outgoing_bootstrap_requests(
        &self,
    ) -> Result<Vec<(String, OutgoingBootstrapRequest)>> {
        let txn = self.sync_tree.new_transaction().await?;
        let manager = OutgoingBootstrapRequestManager::new(&txn);
        manager.pending_requests().await
    }

    // === Outgoing Bootstrap Completion (client-side, Sync-owned) ===

    /// Complete a single outgoing bootstrap request whose access has (or may
    /// have) been granted.
    ///
    /// This is the shared completion path for both wake sources — the periodic
    /// [`sweep_outgoing_bootstrap_requests`](Self::sweep_outgoing_bootstrap_requests)
    /// and the broadcast-woken reaction in the remote-write callback. It:
    ///
    /// 1. Pulls the full tree from the stored addresses. This reuses the
    ///    existing [`sync_with_peer`](Self::sync_with_peer) path, which already
    ///    picks bootstrap-vs-incremental from our local tips — now that access
    ///    is authorized, the peer returns full state rather than a pending
    ///    stub. If the peer still returns [`SyncError::BootstrapPending`] the
    ///    request is not yet approved; leave the record Pending and return.
    ///    (A future targeted want-list fetch for just this tree's frontier could
    ///    slot in here in place of the full pull; the current full bootstrap
    ///    pull is intentional and sufficient — see the sync module's fetch TODOs.)
    /// 2. Registers the peer as a tree peer and applies the stored
    ///    [`SyncSettings`] via [`UserSyncManager`](super::user_sync_manager::UserSyncManager),
    ///    entirely Sync-side.
    /// 3. Marks the request `Hydrated`.
    ///
    /// The database is then openable because the provisional SigKey mapping was
    /// front-loaded on the pending path.
    pub(super) async fn complete_outgoing_bootstrap(
        &self,
        request_id: &str,
        request: &OutgoingBootstrapRequest,
    ) -> Result<()> {
        // Re-entrancy guard: the pull below fires the remote-write callback for
        // this same tree, which would otherwise re-enter here on the still-Pending
        // record and start an overlapping pull. Claim the tree; skip if already
        // in flight. The guard is released when `_guard` drops (including on
        // early return or error).
        let _guard = match InFlightGuard::try_claim(self, request.tree_id.clone()) {
            Some(g) => g,
            None => {
                debug!(tree_id = %request.tree_id, "Outgoing bootstrap completion already in flight; skipping re-entrant call");
                return Ok(());
            }
        };

        // Pull the now-authorized tree. Try every recorded address; succeed on
        // the first that syncs the tree to full state.
        //
        // This reuses the same authenticated bootstrap path the original request
        // took (connect, then a `SyncTree` request carrying the requesting key).
        // The server already dispatches bootstrap-vs-incremental from our tips;
        // the only difference from the first attempt is that our key is now
        // authorized, so the peer returns full state instead of another
        // `BootstrapPending`. A future targeted want-list fetch for just this
        // tree's frontier could slot in here in place of the full pull; the full
        // bootstrap pull is intentional and sufficient (see the sync module's
        // fetch TODOs).
        // If we don't yet hold the tree's root entry, force a full bootstrap
        // rather than a tip diff: the approval broadcast may have deposited an
        // orphan entry (a tip with no root), which would otherwise steer the
        // peer onto the incremental path and never backfill the missing root.
        let has_root = self.backend()?.get(&request.tree_id).await.is_ok();
        let force_bootstrap = !has_root;

        let mut pulled_peer: Option<PublicKey> = None;
        let mut last_err: Option<crate::Error> = None;
        for address in &request.addresses {
            // Connect first so we know the peer pubkey and have the address on
            // record for the tree/peer relationship registered below.
            let peer_pubkey = match self.connect_to_peer(address).await {
                Ok(pk) => pk,
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            };
            if let Err(e) = self.add_peer_address(&peer_pubkey, address.clone()).await {
                last_err = Some(e);
                continue;
            }

            match self
                .sync_tree_with_peer_auth(
                    &peer_pubkey,
                    &request.tree_id,
                    Some(&request.requesting_pubkey),
                    Some(&request.requesting_key_name),
                    Some(request.requested_permission),
                    None,
                    force_bootstrap,
                )
                .await
            {
                Ok(()) => {
                    pulled_peer = Some(peer_pubkey);
                    break;
                }
                Err(e) => {
                    // Still pending means not yet approved: leave the record
                    // Pending for the next wake. A rejection is terminal — retrying
                    // cannot change it, so retire the record instead of sweeping it
                    // forever. Any other error is a transient pull failure we also
                    // retry on the next sweep.
                    if let crate::Error::Sync(sync_err) = &e {
                        match sync_err.as_ref() {
                            SyncError::BootstrapPending { .. } => {
                                debug!(tree_id = %request.tree_id, "Outgoing bootstrap still pending approval");
                                return Ok(());
                            }
                            SyncError::BootstrapRejected { .. } => {
                                info!(
                                    request_id = %request_id,
                                    tree_id = %request.tree_id,
                                    "Outgoing bootstrap was rejected by the peer; retiring request"
                                );
                                return self.retire_outgoing_bootstrap(request_id).await;
                            }
                            _ => {}
                        }
                    }
                    last_err = Some(e);
                }
            }
        }

        if pulled_peer.is_none() {
            if let Some(e) = last_err {
                return Err(e);
            }
            return Err(SyncError::InvalidAddress(
                "outgoing bootstrap request has no usable addresses".to_string(),
            )
            .into());
        }

        // Apply the stored settings and register the tree/peer relationship,
        // all Sync-side, in one transaction over the `_sync` tree.
        let txn = self.sync_tree.new_transaction().await?;
        {
            use super::user_sync_manager::UserSyncManager;
            let user_mgr = UserSyncManager::new(&txn);
            user_mgr
                .set_combined_settings(&request.tree_id, &request.sync_settings)
                .await?;

            if let Some(peer_pubkey) = &pulled_peer {
                let peer_mgr = PeerManager::new(&txn);
                peer_mgr
                    .add_tree_sync(peer_pubkey, &request.tree_id)
                    .await?;
            }

            let outgoing_mgr = OutgoingBootstrapRequestManager::new(&txn);
            outgoing_mgr.mark_hydrated(request_id).await?;
        }
        txn.commit().await?;

        info!(
            request_id = %request_id,
            tree_id = %request.tree_id,
            "Completed outgoing bootstrap: tree pulled, settings applied, request hydrated"
        );
        Ok(())
    }

    /// Sweep every pending outgoing bootstrap request and attempt completion.
    ///
    /// This is the correctness / restart-safety trigger: it re-checks every
    /// recorded pending request regardless of whether an approval broadcast was
    /// ever received (a client that was offline when the approval was sent, or
    /// restarted, still converges). The broadcast-woken reaction shares the same
    /// internal completion path for lower latency; this sweep is the backstop.
    ///
    /// Each request is completed independently; a failure on one is logged and
    /// does not abort the sweep.
    ///
    /// This is invoked automatically by the background engine on a timer; it is
    /// also public so a caller can force an immediate completion check (e.g.
    /// right after learning a request was approved) without waiting for the next
    /// tick.
    pub async fn sweep_outgoing_bootstrap_requests(&self) -> Result<()> {
        let pending = self.pending_outgoing_bootstrap_requests().await?;
        if pending.is_empty() {
            return Ok(());
        }
        debug!(
            count = pending.len(),
            "Sweeping outgoing bootstrap requests"
        );
        for (request_id, request) in pending {
            if let Err(e) = self
                .complete_outgoing_bootstrap(&request_id, &request)
                .await
            {
                warn!(
                    request_id = %request_id,
                    tree_id = %request.tree_id,
                    error = %e,
                    "Failed to complete outgoing bootstrap request during sweep"
                );
            }
        }
        Ok(())
    }

    /// React to remote entries landing for a tree that matches a pending
    /// outgoing bootstrap request.
    ///
    /// This is the low-latency wake source: the approval broadcast (Half 1)
    /// arrives via [`Instance::put_remote_entries`](crate::instance::Instance),
    /// which calls this directly with the written tree id (it does so even when
    /// the joining client cannot yet open the tree, which the write-callback
    /// path could not). When the tree matches a pending outgoing request, kick
    /// completion immediately rather than waiting for the next periodic sweep.
    /// Same completion code path as the sweep, different wake source. The
    /// in-flight guard in [`complete_outgoing_bootstrap`](Self::complete_outgoing_bootstrap)
    /// makes the re-entrant call from completion's own pull a no-op.
    pub(crate) async fn on_remote_write_for_outgoing_bootstrap(&self, tree_id: &ID) -> Result<()> {
        let matching = {
            let txn = self.sync_tree.new_transaction().await?;
            let manager = OutgoingBootstrapRequestManager::new(&txn);
            manager.pending_requests_for_tree(tree_id).await?
        };
        debug!(tree_id = %tree_id, matches = matching.len(), "Remote write woke outgoing bootstrap completion");
        for (request_id, request) in matching {
            if let Err(e) = self
                .complete_outgoing_bootstrap(&request_id, &request)
                .await
            {
                warn!(
                    request_id = %request_id,
                    tree_id = %tree_id,
                    error = %e,
                    "Failed to complete outgoing bootstrap request on broadcast"
                );
            }
        }
        Ok(())
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

        // Create transaction - this will use the provided signing key
        let tx = database.new_transaction().await?;

        // Get settings store and update auth configuration
        let settings_store = tx.get_settings()?;

        // Create the auth key for the requesting device
        // Keys are stored by pubkey, with name as optional metadata
        let auth_key = AuthKey::active(
            Some(&request.requesting_key_name), // name metadata
            request.requested_permission,
        );

        // Add the new key to auth settings using SettingsStore API
        // Store by pubkey (this provides proper upsert behavior and validation)
        settings_store
            .set_auth_key(&request.requesting_pubkey, auth_key)
            .await?;

        // Commit will validate that the user's key has Admin permission
        // If this fails, it means the user lacks the necessary permission
        let approval_entry_id = tx.commit().await?;

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

        // Broadcast the approval entry to all of the database's peers,
        // regardless of its sync_on_commit setting. Broadcasting to every peer
        // (not just the requester) is intentional: the auth change is relevant
        // to every replica, and reusing the general send path keeps the
        // mechanism uniform. The requesting peer is registered as a tree peer
        // (see `Handler::track_tree_sync_relationship`), so it is included and
        // learns access was granted as soon as any network path to it succeeds
        // — improving time-to-visibility under fluctuating network conditions
        // versus waiting on a fixed poll interval. Delivery reuses the normal
        // send queue, so an unreachable peer falls through to the existing
        // retry/backoff. A failure here must not undo the already-committed
        // approval, so it is best-effort: log and move on.
        if self.background_tx.get().is_some() {
            let enqueue = async {
                // Add the requester to the database's tree-peer set now that access
                // is granted. It is deliberately absent until this point: that set
                // is the push list, so registering at request time would have fed
                // database contents and auth metadata to a peer still awaiting a
                // decision — or already refused one.
                if let Some(device_pubkey) = &request.peer_device_pubkey {
                    let reg_tx = self.sync_tree.new_transaction().await?;
                    PeerManager::new(&reg_tx)
                        .add_tree_sync(device_pubkey, &request.tree_id)
                        .await?;
                    reg_tx.commit().await?;
                } else {
                    debug!(
                        request_id = %request_id,
                        tree_id = %request.tree_id,
                        "Approved bootstrap request has no recorded device key; \
                         skipping broadcast (requester converges via its own sweep)"
                    );
                }

                let peer_tx = self.sync_tree.new_transaction().await?;
                let peers = PeerManager::new(&peer_tx)
                    .get_tree_peers(&request.tree_id)
                    .await?;
                drop(peer_tx);
                for peer_id in &peers {
                    self.queue_entry_for_sync(peer_id, &approval_entry_id, &request.tree_id)?;
                }
                Ok::<_, crate::Error>(())
            };
            if let Err(e) = enqueue.await {
                warn!(
                    request_id = %request_id,
                    tree_id = %request.tree_id,
                    error = %e,
                    "Bootstrap approved but failed to enqueue approval entry to peers"
                );
            }
        }

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
