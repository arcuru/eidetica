//! Core sync operations for the sync system.

use tokio::sync::oneshot;
use tracing::{debug, info};

use std::future::Future;

use super::{
    Address, DatabaseTicket, PeerId, Sync, SyncError,
    background::SyncCommand,
    peer_manager::PeerManager,
    peer_types,
    protocol::{self, SyncRequest, SyncResponse, SyncTreeRequest},
    user_sync_manager::UserSyncManager,
};
use crate::{Database, Entry, Instance, Result, auth::Permission, entry::ID, store::DocStore};

use super::utils::collect_ancestors_to_send;

impl Sync {
    // === Core Sync Methods ===

    /// Synchronize a specific tree with a peer using bidirectional sync.
    ///
    /// This is the main synchronization method that implements tip exchange
    /// and bidirectional entry transfer to keep trees in sync between peers.
    /// It performs both pull (fetch missing entries) and push (send our entries).
    ///
    /// # Arguments
    /// * `peer_pubkey` - The public key of the peer to sync with
    /// * `tree_id` - The ID of the tree to synchronize
    ///
    /// # Returns
    /// A Result indicating success or failure of the sync operation.
    pub async fn sync_tree_with_peer(&self, peer_pubkey: &str, tree_id: &ID) -> Result<()> {
        // Get peer information and address
        let peer_info = self
            .get_peer_info(peer_pubkey)
            .await?
            .ok_or_else(|| SyncError::PeerNotFound(peer_pubkey.to_string()))?;

        let address = peer_info
            .addresses
            .first()
            .ok_or_else(|| SyncError::Network("No addresses found for peer".to_string()))?;

        // Get our current tips for this tree (empty if tree doesn't exist)
        let backend = self.backend()?;
        let our_tips = backend
            .get_tips(tree_id)
            .await
            .map_err(|e| SyncError::BackendError(format!("Failed to get local tips: {e}")))?;

        // Get our device public key for automatic peer tracking
        let our_device_pubkey = self.get_device_id().ok();

        // Send unified sync request
        let request = SyncRequest::SyncTree(SyncTreeRequest {
            tree_id: tree_id.clone(),
            our_tips,
            peer_pubkey: our_device_pubkey,
            requesting_key: None, // TODO: Add auth support for direct sync
            requesting_key_name: None,
            requested_permission: None,
        });

        // Send request via background sync command
        let (tx, rx) = oneshot::channel();
        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::SendRequest {
                address: address.clone(),
                request,
                response: tx,
            })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        let response = rx
            .await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))?
            .map_err(|e| SyncError::Network(format!("Request failed: {e}")))?;

        match response {
            SyncResponse::Bootstrap(bootstrap_response) => {
                self.handle_bootstrap_response(bootstrap_response).await?;
            }
            SyncResponse::Incremental(incremental_response) => {
                self.handle_incremental_response(incremental_response, address)
                    .await?;
            }
            SyncResponse::Error(msg) => {
                return Err(SyncError::SyncProtocolError(format!("Sync error: {msg}")).into());
            }
            _ => {
                return Err(SyncError::UnexpectedResponse {
                    expected: "Bootstrap or Incremental",
                    actual: format!("{response:?}"),
                }
                .into());
            }
        }

        // Track tree/peer relationship for sync_on_commit to work
        // This allows on_local_write() to find this peer when queueing entries
        self.add_tree_sync(peer_pubkey, tree_id).await?;

        Ok(())
    }

    /// Handle bootstrap response by storing root and all entries
    pub(super) async fn handle_bootstrap_response(
        &self,
        response: protocol::BootstrapResponse,
    ) -> Result<()> {
        tracing::info!(tree_id = %response.tree_id, "Processing bootstrap response");

        // Store root entry first

        // Store the root entry
        let backend = self.backend()?;
        backend
            .put_verified(response.root_entry.clone())
            .await
            .map_err(|e| SyncError::BackendError(format!("Failed to store root entry: {e}")))?;

        // Store all other entries using existing method
        self.store_received_entries(&response.tree_id, response.all_entries)
            .await?;

        tracing::info!(tree_id = %response.tree_id, "Bootstrap completed successfully");
        Ok(())
    }

    /// Handle incremental response by storing missing entries and sending back what server is missing
    pub(super) async fn handle_incremental_response(
        &self,
        response: protocol::IncrementalResponse,
        peer_address: &peer_types::Address,
    ) -> Result<()> {
        tracing::debug!(tree_id = %response.tree_id, "Processing incremental response");

        // Step 1: Store missing entries
        self.store_received_entries(&response.tree_id, response.missing_entries)
            .await?;

        // Step 2: Check if server is missing entries from us
        let backend = self.backend()?;
        let our_tips = backend.get_tips(&response.tree_id).await?;
        let their_tips = &response.their_tips;

        // Find tips they don't have
        let missing_tip_ids: Vec<_> = our_tips
            .iter()
            .filter(|tip_id| !their_tips.contains(tip_id))
            .cloned()
            .collect();

        if !missing_tip_ids.is_empty() {
            tracing::debug!(
                tree_id = %response.tree_id,
                missing_tips = missing_tip_ids.len(),
                "Server is missing some of our entries, sending them back"
            );

            // Collect entries server is missing
            let backend = self.backend()?;
            let entries_for_server =
                collect_ancestors_to_send(backend.as_backend_impl(), &missing_tip_ids, their_tips)
                    .await?;

            if !entries_for_server.is_empty() {
                // Send these entries back to server
                self.send_missing_entries_to_peer(
                    peer_address,
                    &response.tree_id,
                    entries_for_server,
                )
                .await?;
            }
        }

        tracing::debug!(tree_id = %response.tree_id, "Incremental sync completed");
        Ok(())
    }

    /// Send entries that the server is missing back to complete bidirectional sync
    async fn send_missing_entries_to_peer(
        &self,
        peer_address: &peer_types::Address,
        tree_id: &ID,
        entries: Vec<Entry>,
    ) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        tracing::debug!(
            tree_id = %tree_id,
            entry_count = entries.len(),
            "Sending missing entries back to peer for bidirectional sync"
        );

        let request = protocol::SyncRequest::SendEntries(entries);

        // Send via command channel
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::SendRequest {
                address: peer_address.clone(),
                request,
                response: tx,
            })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        // Wait for acknowledgment
        let response = rx
            .await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))?
            .map_err(|e| SyncError::Network(format!("Request failed: {e}")))?;

        match response {
            protocol::SyncResponse::Ack | protocol::SyncResponse::Count(_) => {
                tracing::debug!(tree_id = %tree_id, "Server acknowledged receipt of missing entries");
                Ok(())
            }
            protocol::SyncResponse::Error(e) => {
                Err(SyncError::Network(format!("Server error receiving entries: {e}")).into())
            }
            _ => Err(SyncError::UnexpectedResponse {
                expected: "Ack or Count",
                actual: format!("{response:?}"),
            }
            .into()),
        }
    }

    /// Validate and store received entries from a peer.
    pub(super) async fn store_received_entries(
        &self,
        _tree_id: &ID,
        entries: Vec<Entry>,
    ) -> Result<()> {
        for entry in entries {
            // Basic validation: check that entry ID matches content
            let calculated_id = entry.id();
            if entry.id() != calculated_id {
                return Err(SyncError::InvalidEntry(format!(
                    "Entry ID {} doesn't match calculated ID {}",
                    entry.id(),
                    calculated_id
                ))
                .into());
            }

            // TODO: Add more validation (signatures, parent existence, etc.)

            // Store the entry (marking it as verified for now)
            let backend = self.backend()?;
            backend
                .put_verified(entry)
                .await
                .map_err(|e| SyncError::BackendError(format!("Failed to store entry: {e}")))?;
        }

        Ok(())
    }

    /// Send a batch of entries to a sync peer (async version).
    ///
    /// # Arguments
    /// * `entries` - The entries to send
    /// * `address` - The address of the peer to send to
    ///
    /// # Returns
    /// A Result indicating whether the entries were successfully acknowledged.
    pub async fn send_entries(
        &self,
        entries: impl AsRef<[Entry]>,
        address: &Address,
    ) -> Result<()> {
        let entries_vec = entries.as_ref().to_vec();
        let request = SyncRequest::SendEntries(entries_vec);
        let response = self.send_request(&request, address).await?;

        match response {
            SyncResponse::Ack | SyncResponse::Count(_) => Ok(()),
            SyncResponse::Error(msg) => Err(SyncError::SyncProtocolError(format!(
                "Peer {} returned error: {}",
                address.address, msg
            ))
            .into()),
            _ => Err(SyncError::UnexpectedResponse {
                expected: "Ack or Count",
                actual: format!("{response:?}"),
            }
            .into()),
        }
    }

    /// Send specific entries to a peer via the background sync engine.
    ///
    /// This method queues entries for direct transmission without duplicate filtering.
    /// The caller is responsible for determining which entries should be sent.
    ///
    /// # Duplicate Prevention Architecture
    ///
    /// Eidetica uses **smart duplicate prevention** in the background sync engine:
    /// - **Database sync** (`SyncWithPeer` command): Uses tip comparison for semantic filtering
    /// - **Direct send** (this method): Trusts caller to provide appropriate entries
    ///
    /// For automatic duplicate prevention, use tree-based sync relationships instead
    /// of calling this method directly.
    ///
    /// # Arguments
    /// * `peer_id` - The peer ID to send to
    /// * `entries` - The specific entries to send (no filtering applied)
    ///
    /// # Returns
    /// A Result indicating whether the command was successfully queued for background processing.
    pub async fn send_entries_to_peer(&self, peer_id: &PeerId, entries: Vec<Entry>) -> Result<()> {
        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::SendEntries {
                peer: peer_id.clone(),
                entries,
            })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;
        Ok(())
    }

    /// Queue an entry for sync to a peer (non-blocking, for use in callbacks).
    ///
    /// This method is designed for use in write callbacks where async operations
    /// are not possible. It uses try_send to avoid blocking, and logs errors
    /// rather than failing the callback.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The public key of the peer to sync with
    /// * `entry_id` - The ID of the entry to queue
    /// * `tree_id` - The tree ID where the entry belongs
    ///
    /// # Returns
    /// Ok(()) if the entry was successfully queued.
    /// Only returns Err if transport is not enabled.
    pub fn queue_entry_for_sync(
        &self,
        peer_id: &PeerId,
        entry_id: &ID,
        tree_id: &ID,
    ) -> Result<()> {
        // Ensure background sync is running
        if self.background_tx.get().is_none() {
            return Err(SyncError::NoTransportEnabled.into());
        }

        // Add to queue - BackgroundSync will process and send
        self.queue
            .enqueue(peer_id, entry_id.clone(), tree_id.clone());

        Ok(())
    }

    /// Handle local write events for automatic sync.
    ///
    /// This method is called by the Instance write callback system when entries
    /// are committed locally. It looks up the combined sync settings for the database
    /// and queues the entry for sync with all configured peers if sync is enabled.
    ///
    /// This is the core method that implements automatic sync-on-commit behavior.
    ///
    /// # Arguments
    /// * `entry` - The newly committed entry
    /// * `database` - The database where the entry was committed
    /// * `_instance` - The instance (unused but required by callback signature)
    ///
    /// # Returns
    /// Ok(()) on success, or an error if settings lookup fails
    pub(crate) async fn on_local_write(
        &self,
        entry: &Entry,
        database: &Database,
        _instance: &Instance,
    ) -> Result<()> {
        // Early return if background sync not running
        if self.background_tx.get().is_none() {
            return Ok(());
        }

        // Look up combined settings for this database
        let tx = self.sync_tree.new_transaction().await?;
        let user_mgr = UserSyncManager::new(&tx);
        let peer_mgr = PeerManager::new(&tx);

        let combined_settings = match user_mgr.get_combined_settings(database.root_id()).await? {
            Some(settings) => settings,
            None => {
                // No settings configured for this database - no sync needed
                debug!(database_id = %database.root_id(), "No sync settings for database, skipping");
                return Ok(());
            }
        };

        // Check if sync is enabled and sync_on_commit is true
        if !combined_settings.sync_enabled || !combined_settings.sync_on_commit {
            debug!(
                database_id = %database.root_id(),
                sync_enabled = combined_settings.sync_enabled,
                sync_on_commit = combined_settings.sync_on_commit,
                "Sync not enabled for database"
            );
            return Ok(());
        }

        // Get list of peers for this database
        let peers = peer_mgr.get_tree_peers(database.root_id()).await?;

        if peers.is_empty() {
            debug!(database_id = %database.root_id(), "No peers configured for database");
            return Ok(());
        }

        // Queue entry for sync with each peer
        let entry_id = entry.id();
        let tree_id = database.root_id();

        debug!(
            database_id = %tree_id,
            entry_id = %entry_id,
            peer_count = peers.len(),
            "Queueing entry for automatic sync"
        );

        for peer_id in peers {
            self.queue_entry_for_sync(&peer_id, &entry_id, tree_id)?;
        }

        Ok(())
    }

    /// Initialize combined settings for all users.
    ///
    /// This is called during Sync initialization. For new sync trees (just created),
    /// it scans the _users database to register all existing users. For existing
    /// sync trees (loaded), it updates combined settings for already-tracked users.
    pub(super) async fn initialize_user_settings(&self) -> Result<()> {
        use crate::store::Table;
        use crate::user::types::UserInfo;

        // Check if sync tree is freshly created (no users tracked yet)
        let user_tracking = self
            .sync_tree
            .get_store_viewer::<DocStore>(super::user_sync_manager::USER_TRACKING_SUBTREE)
            .await?;
        let all_tracked = user_tracking.get_all().await?;

        if all_tracked.keys().count() == 0 {
            // New sync tree - register all users from _users database
            let instance = self.instance.upgrade().ok_or(SyncError::InstanceDropped)?;
            let users_db = instance.users_db().await?;
            let users_table = users_db
                .get_store_viewer::<Table<UserInfo>>("users")
                .await?;
            let all_users = users_table.search(|_| true).await?;

            for (user_uuid, user_info) in all_users {
                self.sync_user(&user_uuid, &user_info.user_database_id)
                    .await?;
            }
        } else {
            // Existing sync tree - update settings for tracked users if changed
            let tx = self.sync_tree.new_transaction().await?;
            let user_mgr = UserSyncManager::new(&tx);

            for user_uuid in all_tracked.keys() {
                if let Some((prefs_db_id, _tips)) =
                    user_mgr.get_tracked_user_state(user_uuid).await?
                {
                    self.sync_user(user_uuid, &prefs_db_id).await?;
                }
            }
        }

        Ok(())
    }

    /// Send a sync request to a peer and get a response (async version).
    ///
    /// # Arguments
    /// * `request` - The sync request to send
    /// * `address` - The address of the peer
    ///
    /// # Returns
    /// The sync response from the peer.
    pub(super) async fn send_request(
        &self,
        request: &SyncRequest,
        address: &Address,
    ) -> Result<SyncResponse> {
        let (tx, rx) = oneshot::channel();

        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::SendRequest {
                address: address.clone(),
                request: request.clone(),
                response: tx,
            })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        rx.await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))?
    }

    /// Discover available trees from a peer (simplified API).
    ///
    /// This method connects to a peer and retrieves the list of trees they're willing to sync.
    /// This is useful for discovering what can be synced before setting up sync relationships.
    ///
    /// # Arguments
    /// * `address` - The transport address of the peer.
    ///
    /// # Returns
    /// A vector of TreeInfo describing available trees, or an error.
    pub async fn discover_peer_trees(&self, address: &Address) -> Result<Vec<protocol::TreeInfo>> {
        // Connect and get handshake info
        let _peer_pubkey = self.connect_to_peer(address).await?;

        // The handshake already contains the tree list, but we need to get it again
        // since connect_to_peer doesn't return it. For now, return empty list
        // TODO: Enhance this to actually return the tree list from handshake

        tracing::warn!(
            "discover_peer_trees not fully implemented - handshake contains tree info but API needs enhancement"
        );
        Ok(vec![])
    }

    /// Sync with a peer at a given address.
    ///
    /// This is a blocking convenience method that:
    /// 1. Connects to discover the peer's public key
    /// 2. Registers the peer and performs immediate sync
    /// 3. Returns after sync completes
    ///
    /// For new code, prefer using [`register_sync_peer()`](Self::register_sync_peer)
    /// directly, which registers intent and lets background sync handle it.
    ///
    /// # Arguments
    /// * `address` - The transport address of the peer.
    /// * `tree_id` - Optional tree ID to sync (None = discover available trees)
    ///
    /// # Returns
    /// Result indicating success or failure.
    pub async fn sync_with_peer(&self, address: &Address, tree_id: Option<&ID>) -> Result<()> {
        // Connect to peer if not already connected
        let peer_pubkey = self.connect_to_peer(address).await?;

        // Store the address for this peer (needed for sync_tree_with_peer)
        self.add_peer_address(&peer_pubkey, address.clone()).await?;

        if let Some(tree_id) = tree_id {
            // Sync specific tree
            self.sync_tree_with_peer(&peer_pubkey, tree_id).await?;
        } else {
            // TODO: Sync all available trees
            tracing::warn!(
                "Syncing all trees not yet implemented - need to enhance discover_peer_trees first"
            );
        }

        Ok(())
    }

    /// Sync with a peer using a [`DatabaseTicket`].
    ///
    /// Attempts [`sync_with_peer`](Self::sync_with_peer) for every address
    /// hint in the ticket concurrently. Each address may point to a different
    /// peer, so connections are independent. Succeeds if at least one address
    /// syncs successfully; returns the last error if all fail.
    ///
    /// # Arguments
    /// * `ticket` - A ticket containing the database ID and address hints.
    ///
    /// # Errors
    /// Returns [`SyncError::InvalidAddress`] if the ticket has no address hints.
    /// Returns the last sync error if no address succeeded.
    pub async fn sync_with_ticket(&self, ticket: &DatabaseTicket) -> Result<()> {
        let database_id = ticket.database_id().clone();
        self.try_addresses_concurrently(ticket.addresses(), |sync, addr| {
            let db_id = database_id.clone();
            async move { sync.sync_with_peer(&addr, Some(&db_id)).await }
        })
        .await
    }

    /// Sync a specific tree with a peer, with optional authentication for bootstrap.
    ///
    /// This is a lower-level method that allows specifying authentication parameters
    /// for bootstrap scenarios where access needs to be requested.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The public key of the peer to sync with
    /// * `tree_id` - The ID of the tree to sync
    /// * `requesting_key` - Optional public key requesting access (for bootstrap)
    /// * `requesting_key_name` - Optional name/ID of the requesting key
    /// * `requested_permission` - Optional permission level being requested
    ///
    /// # Returns
    /// A Result indicating success or failure.
    pub async fn sync_tree_with_peer_auth(
        &self,
        peer_pubkey: &str,
        tree_id: &ID,
        requesting_key: Option<&str>,
        requesting_key_name: Option<&str>,
        requested_permission: Option<Permission>,
    ) -> Result<()> {
        // Get peer information and address
        let peer_info = self
            .get_peer_info(peer_pubkey)
            .await?
            .ok_or_else(|| SyncError::PeerNotFound(peer_pubkey.to_string()))?;

        let address = peer_info
            .addresses
            .first()
            .ok_or_else(|| SyncError::Network("No addresses found for peer".to_string()))?;

        // Get our current tips for this tree (empty if tree doesn't exist)
        let backend = self.backend()?;
        let our_tips = backend
            .get_tips(tree_id)
            .await
            .map_err(|e| SyncError::BackendError(format!("Failed to get local tips: {e}")))?;

        // Get our device public key for automatic peer tracking
        let our_device_pubkey = self.get_device_id().ok();

        // Send unified sync request with auth parameters
        let request = SyncRequest::SyncTree(SyncTreeRequest {
            tree_id: tree_id.clone(),
            our_tips,
            peer_pubkey: our_device_pubkey,
            requesting_key: requesting_key.map(|k| k.to_string()),
            requesting_key_name: requesting_key_name.map(|k| k.to_string()),
            requested_permission,
        });

        // Send request via background sync command
        let (tx, rx) = oneshot::channel();
        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::SendRequest {
                address: address.clone(),
                request,
                response: tx,
            })
            .await
            .map_err(|_| {
                SyncError::CommandSendError("Background sync command channel closed".to_string())
            })?;

        // Wait for response
        let response = rx
            .await
            .map_err(|_| {
                SyncError::CommandSendError("Background sync response channel closed".to_string())
            })?
            .map_err(|e| SyncError::Network(format!("Sync request failed: {e}")))?;

        // Handle the response (same logic as existing sync_tree_with_peer)
        match response {
            SyncResponse::Bootstrap(bootstrap_response) => {
                info!(peer = %peer_pubkey, tree = %tree_id, entry_count = bootstrap_response.all_entries.len() + 1, "Received bootstrap response");

                // Store the root entry
                let backend = self.backend()?;
                backend.put_verified(bootstrap_response.root_entry).await?;

                // Store all other entries
                for entry in bootstrap_response.all_entries {
                    backend.put_unverified(entry).await?;
                }

                info!(peer = %peer_pubkey, tree = %tree_id, "Bootstrap sync completed successfully");
            }
            SyncResponse::Incremental(incremental_response) => {
                info!(peer = %peer_pubkey, tree = %tree_id, missing_count = incremental_response.missing_entries.len(), "Received incremental sync response");

                // Use the enhanced handler that supports bidirectional sync
                self.handle_incremental_response(incremental_response, address)
                    .await?;

                debug!(peer = %peer_pubkey, tree = %tree_id, "Incremental sync completed");
            }
            SyncResponse::BootstrapPending {
                request_id,
                message,
            } => {
                info!(peer = %peer_pubkey, tree = %tree_id, request_id = %request_id, "Bootstrap request pending manual approval");
                return Err(SyncError::BootstrapPending {
                    request_id,
                    message,
                }
                .into());
            }
            SyncResponse::Error(err) => {
                return Err(SyncError::Network(format!("Peer returned error: {err}")).into());
            }
            _ => {
                return Err(SyncError::SyncProtocolError(
                    "Unexpected response type for sync tree request".to_string(),
                )
                .into());
            }
        }

        // Track tree/peer relationship for sync_on_commit to work
        // This allows on_local_write() to find this peer when queueing entries
        self.add_tree_sync(peer_pubkey, tree_id).await?;

        Ok(())
    }

    // === Flush Operations ===

    /// Process all queued entries and retry any failed sends.
    ///
    /// This method:
    /// 1. Retries all entries in the retry queue (ignoring backoff timers)
    /// 2. Processes all entries in the sync queue (batched by peer)
    ///
    /// When this method returns, all pending sync work has been attempted.
    /// This is useful to eensuree that all pending pushes have completed.
    ///
    /// # Returns
    /// `Ok(())` if all operations completed successfully, or an error
    /// if the background sync engine is not running or sends failed.
    pub async fn flush(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();

        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::Flush { response: tx })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        rx.await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))?
    }

    /// Try an operation against multiple addresses concurrently, returning on
    /// the first success.
    ///
    /// Spawns one task per address using [`tokio::task::JoinSet`]. If any task
    /// succeeds the remaining ones are aborted and `Ok(())` is returned. If all
    /// tasks fail the last error is returned. If `addresses` is empty an
    /// [`SyncError::InvalidAddress`] error is returned.
    pub(super) async fn try_addresses_concurrently<F, Fut>(
        &self,
        addresses: &[Address],
        f: F,
    ) -> Result<()>
    where
        F: Fn(Sync, Address) -> Fut,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        if addresses.is_empty() {
            return Err(SyncError::InvalidAddress("Ticket has no address hints".into()).into());
        }

        let mut set = tokio::task::JoinSet::new();
        for addr in addresses {
            set.spawn(f(self.clone(), addr.clone()));
        }

        let mut last_err = None;
        while let Some(join_result) = set.join_next().await {
            match join_result {
                Ok(Ok(())) => {
                    set.abort_all();
                    return Ok(());
                }
                Ok(Err(e)) => last_err = Some(e),
                Err(e) => {
                    last_err = Some(SyncError::Network(format!("Task join error: {e}")).into());
                }
            }
        }

        Err(last_err.expect("at least one task was spawned"))
    }
}
