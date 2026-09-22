//! Core sync operations for the sync system.

use std::collections::HashSet;
use std::future::Future;
use std::time::Duration;

use tokio::sync::oneshot;
use tracing::{debug, info, warn};

use super::{
    Address, DatabaseTicket, PeerId, Sync, SyncError,
    background::SyncCommand,
    peer_manager::PeerManager,
    peer_types,
    protocol::{self, SyncRequest, SyncRequestAuth, SyncResponse, SyncTreeRequest},
    user_sync_manager::UserSyncManager,
};
use crate::{
    Database, Entry, Result,
    auth::Permission,
    auth::crypto::{PrivateKey, PublicKey},
    crdt::Doc,
    entry::ID,
    store::Table,
    user::types::UserInfo,
};

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
    pub async fn sync_tree_with_peer(&self, peer_pubkey: &PublicKey, tree_id: &ID) -> Result<()> {
        self.sync_tree_with_peer_as(peer_pubkey, tree_id, None)
            .await
    }

    /// Synchronize a tree with a peer, signing the request with `signing_key`.
    ///
    /// The peer authorizes the pull against the key that signed it, so this must
    /// be a key with read access on `tree_id`. `None` uses this instance's device
    /// key, which is right when the device itself holds the access (directly or
    /// through a delegated tree). A database that instead granted a *user* key —
    /// the usual outcome of bootstrapping with one — needs that key here.
    pub async fn sync_tree_with_peer_as(
        &self,
        peer_pubkey: &PublicKey,
        tree_id: &ID,
        signing_key: Option<&PrivateKey>,
    ) -> Result<()> {
        let addresses = self.peer_addresses(peer_pubkey).await?;
        let peer = peer_pubkey.clone();
        let tree = tree_id.clone();
        let key = signing_key.cloned();
        self.with_selected_address(&addresses, peer_pubkey, move |sync, addr| {
            let peer = peer.clone();
            let tree = tree.clone();
            let key = key.clone();
            async move {
                sync.sync_tree_with_peer_at(&addr, &peer, &tree, key.as_ref())
                    .await
            }
        })
        .await
    }

    /// One address's attempt at [`Self::sync_tree_with_peer_as`].
    async fn sync_tree_with_peer_at(
        &self,
        address: &Address,
        peer_pubkey: &PublicKey,
        tree_id: &ID,
        signing_key: Option<&PrivateKey>,
    ) -> Result<()> {
        // Persist the parent's same-peer relationship before discovery so each
        // dependency edge can inherit it atomically as it is recorded.
        self.add_tree_sync(peer_pubkey, tree_id).await?;
        let mut seen = HashSet::new();
        let mut pending = vec![(tree_id.clone(), 0usize, Vec::new())];

        while let Some((next_tree, depth, dependency_path)) = pending.pop() {
            if !seen.insert(next_tree.clone()) {
                continue;
            }
            if depth > super::MAX_DEPENDENCY_DEPTH {
                return Err(SyncError::SyncProtocolError(format!(
                    "delegated database dependency depth exceeds {}",
                    super::MAX_DEPENDENCY_DEPTH
                ))
                .into());
            }
            let dependencies = self
                .sync_one_tree_with_peer_at(
                    address,
                    peer_pubkey,
                    &next_tree,
                    signing_key,
                    &dependency_path,
                )
                .await?;
            for dependency in dependencies.into_iter().rev() {
                self.persist_dependency(&next_tree, &dependency).await?;
                let mut child_path = dependency_path.clone();
                child_path.push(next_tree.clone());
                pending.push((dependency, depth + 1, child_path));
            }
        }

        Database::open(&self.instance()?, tree_id)
            .await?
            .verify()
            .await?;

        Ok(())
    }

    async fn persist_dependency(&self, parent: &ID, dependency: &ID) -> Result<()> {
        let txn = self.sync_tree.new_transaction().await?;
        PeerManager::new(&txn)
            .add_dependency(parent, dependency)
            .await?;
        txn.commit().await?;
        Ok(())
    }

    async fn sync_one_tree_with_peer_at(
        &self,
        address: &Address,
        peer_pubkey: &PublicKey,
        tree_id: &ID,
        signing_key: Option<&PrivateKey>,
        dependency_path: &[ID],
    ) -> Result<Vec<ID>> {
        // Get our current tips for this tree (empty if tree doesn't exist)
        let backend = self.backend()?;
        let our_tips = backend
            .snapshot(tree_id)
            .await
            .map_err(|e| SyncError::BackendError(format!("Failed to get local tips: {e}")))?;

        // Get our device public key for automatic peer tracking
        let our_device_pubkey = self.get_device_pubkey().ok();

        // Send unified sync request, signed so the peer can authorize the pull
        let instance = self.instance()?;
        let auth = SyncRequestAuth::sign(
            signing_key.unwrap_or(instance.signing_key()?),
            peer_pubkey,
            tree_id,
            &our_tips,
            dependency_path,
            instance.clock().now_millis(),
        );
        let request = SyncRequest::SyncTree(SyncTreeRequest {
            tree_id: tree_id.clone(),
            our_tips,
            peer_pubkey: our_device_pubkey,
            requesting_key: None,
            requesting_key_name: None,
            requested_permission: None,
            metadata: None,
            auth: Some(auth),
            dependency_path: dependency_path.to_vec(),
        });

        // Send request via background sync command
        let (tx, rx) = oneshot::channel();
        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::SendRequest {
                address: address.clone(),
                request: Box::new(request),
                response: tx,
            })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        let response = rx
            .await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))?
            .map_err(|e| SyncError::Network(format!("Request failed: {e}")))?;

        let report = match response {
            SyncResponse::Bootstrap(bootstrap_response) => {
                self.handle_bootstrap_response(bootstrap_response).await?
            }
            SyncResponse::Incremental(incremental_response) => {
                self.handle_incremental_response(incremental_response, address)
                    .await?
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
        };

        Ok(report
            .dependencies
            .into_iter()
            .map(|dependency| dependency.database_id)
            .collect())
    }

    /// Handle bootstrap response by storing root and all entries
    pub(super) async fn handle_bootstrap_response(
        &self,
        response: protocol::BootstrapResponse,
    ) -> Result<crate::database::VerifyReport> {
        tracing::info!(tree_id = %response.tree_id, "Processing bootstrap response");

        // Integrity check: the root entry's content must hash to the declared
        // tree_id. Rejects peers serving substituted content. A mismatch here
        // also covers the cross-algorithm bootstrap case (e.g. a SHA-256 tree
        // advertised to a BLAKE3-default node) — those are unsupported until
        // the backend gains multi-CID-per-entry storage, so failing loudly is
        // better than silently re-keying the DAG under the wrong algorithm.
        let derived = response.root_entry.id();
        if derived != response.tree_id {
            return Err(SyncError::InvalidEntry(format!(
                "root entry content hashes to {} but bootstrap response declares tree_id {}",
                derived, response.tree_id
            ))
            .into());
        }

        // Combine root entry with all other entries into a single batch
        let mut all_entries = Vec::with_capacity(1 + response.all_entries.len());
        all_entries.push(response.root_entry);
        all_entries.extend(response.all_entries);

        // Store all entries and fire callbacks once
        let report = self
            .store_received_entries(&response.tree_id, all_entries)
            .await?;

        tracing::info!(tree_id = %response.tree_id, "Bootstrap completed successfully");
        Ok(report)
    }

    /// Handle incremental response by storing missing entries and sending back what server is missing
    pub(super) async fn handle_incremental_response(
        &self,
        response: protocol::IncrementalResponse,
        peer_address: &peer_types::Address,
    ) -> Result<crate::database::VerifyReport> {
        tracing::debug!(tree_id = %response.tree_id, "Processing incremental response");

        // Step 1: Store missing entries
        let report = self
            .store_received_entries(&response.tree_id, response.missing_entries)
            .await?;

        // Step 2: Check if server is missing entries from us
        let backend = self.backend()?;
        let our_snapshot = backend.snapshot(&response.tree_id).await?;
        let their_tips = &response.their_tips;

        // Find tips they don't have
        let missing_tip_ids: Vec<_> = our_snapshot
            .tips()
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
            let engine = self
                .backend()?
                .local_engine()
                .expect("sync requires local backend");
            let entries_for_server =
                collect_ancestors_to_send(engine.as_ref(), &missing_tip_ids, their_tips).await?;

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
        Ok(report)
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
                request: Box::new(request),
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

    /// Validate and store received entries from a peer, firing remote write callbacks.
    pub(super) async fn store_received_entries(
        &self,
        tree_id: &ID,
        entries: Vec<Entry>,
    ) -> Result<crate::database::VerifyReport> {
        // These entries arrive without per-entry declared IDs — they were batched
        // under a single tree_id by the sender. Content is stored under whatever
        // ID our local `entry.id()` derives, so substitution attacks on individual
        // entries would fail DAG connectivity checks via parent pointers rather
        // than a per-entry hash check here. Root-level integrity is verified by
        // the bootstrap handler against the declared tree_id.
        //
        // TODO: Add signature verification and parent-existence / DAG-connectivity
        // checks before marking entries as verified.

        // Store entries and fire callbacks via Instance::put_remote_entries.
        // Stored Unverified: these arrived from a peer and have not been
        // verified by this node.
        let instance = self.instance()?;
        instance
            .put_remote_entries(tree_id, entries)
            .await
            .map_err(|e| SyncError::BackendError(format!("Failed to store entries: {e}")).into())
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
    /// * `event` - The write event containing the newly committed entries
    /// * `database` - The database where the entries were committed
    ///
    /// # Returns
    /// Ok(()) on success, or an error if settings lookup fails
    pub(crate) async fn on_local_write(
        &self,
        event: &crate::instance::WriteEvent,
        database: &Database,
    ) -> Result<()> {
        // Early return if background sync not running
        if self.background_tx.get().is_none() {
            return Ok(());
        }

        // Look up combined settings for this database
        let tx = self.sync_tree.new_transaction().await?;
        let user_mgr = UserSyncManager::new(&tx);
        let peer_mgr = PeerManager::new(&tx);

        let combined_settings = match peer_mgr
            .inherited_settings(database.root_id(), &user_mgr)
            .await?
        {
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

        // Queue each entry for sync with each peer. The event no longer
        // carries entry payloads — expand the cursor advance back into a
        // concrete set of IDs by walking the DAG diff. Cost is bounded
        // by the cursor delta, not the full tree.
        let tree_id = database.root_id();

        let new_ids = database
            .ids_added(event.previous_tips(), event.post_tips())
            .await?;

        for entry_id in &new_ids {
            debug!(
                database_id = %tree_id,
                entry_id = %entry_id,
                peer_count = peers.len(),
                "Queueing entry for automatic sync"
            );

            for peer_id in &peers {
                self.queue_entry_for_sync(peer_id, entry_id, tree_id)?;
            }
        }

        Ok(())
    }

    /// Initialize combined settings for all users.
    ///
    /// This is called during Sync initialization. For new sync trees (just created),
    /// it scans the _users database to register all existing users. For existing
    /// sync trees (loaded), it updates combined settings for already-tracked users.
    pub(super) async fn initialize_user_settings(&self) -> Result<()> {
        self.reconcile_user_settings().await
    }

    /// Reconcile the persisted user directory and every tracked preferences tree.
    ///
    /// The daemon calls this at startup and whenever a service client changes
    /// either source tree. It is intentionally idempotent: `sync_user` compares
    /// the persisted preferences cursor before updating combined settings.
    pub(crate) async fn reconcile_user_settings(&self) -> Result<()> {
        let _guard = self.reconciliation.lock().await;

        let instance = self.instance.upgrade().ok_or(SyncError::InstanceDropped)?;
        let users_db = instance.users_db().await?;
        let users_table = users_db
            .get_store_viewer::<Table<UserInfo>>("users")
            .await?;

        // Always scan `_users`, not only when the sync tree is empty. A daemon
        // may already track some users when another account is created through
        // the service socket.
        for (user_uuid, user_info) in users_table.search(|_| true).await? {
            self.sync_user(&user_uuid, &user_info.user_database_id)
                .await?;
        }

        Ok(())
    }

    /// Whether a local write can change the persisted user sync intent.
    pub(crate) async fn is_reconciliation_source(&self, database_id: &ID) -> Result<bool> {
        let instance = self.instance.upgrade().ok_or(SyncError::InstanceDropped)?;
        if instance.users_db_id() == database_id {
            return Ok(true);
        }
        let users_db = instance.users_db().await?;
        let users_table = users_db
            .get_store_viewer::<Table<UserInfo>>("users")
            .await?;

        Ok(users_table
            .search(|user| &user.user_database_id == database_id)
            .await?
            .into_iter()
            .next()
            .is_some())
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
                request: Box::new(request.clone()),
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
        self.sync_with_peer_as(address, tree_id, None).await
    }

    /// Sync with a peer, signing requests with `signing_key`.
    ///
    /// See [`sync_tree_with_peer_as`](Self::sync_tree_with_peer_as) for which
    /// key to pass.
    pub async fn sync_with_peer_as(
        &self,
        address: &Address,
        tree_id: Option<&ID>,
        signing_key: Option<&PrivateKey>,
    ) -> Result<()> {
        // Connect to peer if not already connected
        let peer_pubkey = self.connect_to_peer(address).await?;

        // Store the address for this peer (needed for sync_tree_with_peer)
        self.add_peer_address(&peer_pubkey, address.clone()).await?;

        if let Some(tree_id) = tree_id {
            // Sync specific tree
            self.sync_tree_with_peer_as(&peer_pubkey, tree_id, signing_key)
                .await?;
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
    /// Races bounded handshakes against every address hint, then performs the
    /// tree exchange once through the first usable route. Other successful
    /// handshakes may finish peer registration in the background.
    ///
    /// # Arguments
    /// * `ticket` - A ticket containing the database ID and address hints.
    ///
    /// # Errors
    /// Returns [`SyncError::InvalidAddress`] if the ticket has no address hints.
    /// Returns the last sync error if no address succeeded.
    pub async fn sync_with_ticket(&self, ticket: &DatabaseTicket) -> Result<()> {
        let database_id = ticket.database_id().clone();
        let (address, peer_pubkey) = self.select_address(ticket.addresses(), None).await?;
        self.add_peer_address(&peer_pubkey, address.clone()).await?;
        self.sync_tree_with_peer_at(&address, &peer_pubkey, &database_id, None)
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
    /// * `requesting_key` - Optional private key to sign with and request access for
    /// * `requesting_key_name` - Optional name/ID of the requesting key
    /// * `requested_permission` - Optional permission level being requested
    ///
    /// # Returns
    /// A Result indicating success or failure.
    pub async fn sync_tree_with_peer_auth(
        &self,
        peer_pubkey: &PublicKey,
        tree_id: &ID,
        requesting_key: Option<&PrivateKey>,
        requesting_key_name: Option<&str>,
        requested_permission: Option<Permission>,
        metadata: Option<Doc>,
    ) -> Result<()> {
        let addresses = self.peer_addresses(peer_pubkey).await?;
        let peer = peer_pubkey.clone();
        let tree = tree_id.clone();
        let key = requesting_key.cloned();
        let key_name = requesting_key_name.map(str::to_string);
        self.with_selected_address(&addresses, peer_pubkey, move |sync, addr| {
            let peer = peer.clone();
            let tree = tree.clone();
            let key = key.clone();
            let key_name = key_name.clone();
            let metadata = metadata.clone();
            async move {
                sync.sync_tree_with_peer_auth_at(
                    &addr,
                    &peer,
                    &tree,
                    key.as_ref(),
                    key_name.as_deref(),
                    requested_permission,
                    metadata,
                )
                .await
            }
        })
        .await
    }

    /// One address's attempt at [`Self::sync_tree_with_peer_auth`].
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn sync_tree_with_peer_auth_at(
        &self,
        address: &Address,
        peer_pubkey: &PublicKey,
        tree_id: &ID,
        requesting_key: Option<&PrivateKey>,
        requesting_key_name: Option<&str>,
        requested_permission: Option<Permission>,
        metadata: Option<Doc>,
    ) -> Result<()> {
        // Get our current tips for this tree (empty if tree doesn't exist)
        let backend = self.backend()?;
        let our_tips = backend
            .snapshot(tree_id)
            .await
            .map_err(|e| SyncError::BackendError(format!("Failed to get local tips: {e}")))?;

        // Get our device public key for automatic peer tracking
        let our_device_pubkey = self.get_device_pubkey().ok();

        // Send the request with proof from the named requesting key. Calls
        // without a named-key request still sign with the device key for
        // authenticated data access where required.
        let instance = self.instance()?;
        let signing_key = requesting_key.unwrap_or(instance.signing_key()?);
        let auth = SyncRequestAuth::sign(
            signing_key,
            peer_pubkey,
            tree_id,
            &our_tips,
            &[],
            instance.clock().now_millis(),
        );
        let request = SyncRequest::SyncTree(SyncTreeRequest {
            tree_id: tree_id.clone(),
            our_tips,
            peer_pubkey: our_device_pubkey,
            requesting_key: requesting_key.map(|k| k.public_key()),
            requesting_key_name: requesting_key_name.map(|k| k.to_string()),
            requested_permission,
            metadata,
            auth: Some(auth),
            dependency_path: vec![],
        });

        // Send request via background sync command
        let (tx, rx) = oneshot::channel();
        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::SendRequest {
                address: address.clone(),
                request: Box::new(request),
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

                // Store root + all entries as a single batch with callback dispatch
                let mut all_entries = Vec::with_capacity(1 + bootstrap_response.all_entries.len());
                all_entries.push(bootstrap_response.root_entry);
                all_entries.extend(bootstrap_response.all_entries);

                // Bootstrap entries come from a peer; stored Unverified.
                let instance = self.instance()?;
                instance.put_remote_entries(tree_id, all_entries).await?;

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
            SyncResponse::BootstrapRejected {
                request_id,
                message,
            } => {
                info!(peer = %peer_pubkey, tree = %tree_id, request_id = %request_id, "Bootstrap request was rejected");
                return Err(SyncError::BootstrapRejected {
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

    /// Every address recorded for `peer_pubkey`.
    async fn peer_addresses(&self, peer_pubkey: &PublicKey) -> Result<Vec<Address>> {
        let peer_info = self
            .get_peer_info(peer_pubkey)
            .await?
            .ok_or_else(|| SyncError::PeerNotFound(peer_pubkey.to_string()))?;
        if peer_info.addresses.is_empty() {
            return Err(
                SyncError::Network(format!("No addresses found for peer {peer_pubkey}")).into(),
            );
        }
        Ok(peer_info.addresses)
    }

    /// Select a route with bounded handshakes, then run the real operation once.
    ///
    /// A peer's address list only ever grows: anything that changes address on
    /// restart appends a new entry and leaves the old one in place. Dialing just
    /// one of them makes a peer that is up and reachable permanently unreachable
    /// as soon as the entry that happens to be dialed goes stale — and the
    /// failure presents as a timeout, which reads as "the peer is down", the one
    /// diagnosis that leads away from the cause.
    ///
    /// Ticket bootstrap has always raced its address hints. This puts every
    /// subsequent sync on the same footing, which makes an accumulated list
    /// harmless rather than fatal. Pruning dead addresses is a separate concern
    /// and is not needed for reachability once all of them are tried.
    ///
    /// On total failure the last error is returned unchanged — callers match on
    /// specific variants (`BootstrapPending`, for one), so it must not be
    /// flattened into a connectivity error. The attempted addresses are logged
    /// instead, since a bare timeout naming no address is not actionable.
    async fn with_selected_address<F, Fut>(
        &self,
        addresses: &[Address],
        peer_pubkey: &PublicKey,
        f: F,
    ) -> Result<()>
    where
        F: Fn(Sync, Address) -> Fut,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let result = match self.select_address(addresses, Some(peer_pubkey)).await {
            Ok((address, _)) => f(self.clone(), address).await,
            Err(error) => Err(error),
        };
        if let Err(e) = &result {
            warn!(
                peer = %peer_pubkey,
                attempted = ?addresses,
                error = %e,
                "No address answered for this peer"
            );
        }
        result
    }

    /// Timeout applied to each address attempt in
    /// [`select_address`](Self::select_address).
    const ADDRESS_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(30);

    /// Race registration handshakes and return the first usable address, along
    /// with the identity that answered on it.
    ///
    /// Spawns one detached task per address via [`tokio::spawn`]. Remaining
    /// tasks are **not** cancelled — they continue running in the background so
    /// that additional addresses can be registered for future syncs. The real
    /// operation runs separately and is therefore not bounded by this timeout.
    ///
    /// `expected` is the peer the caller means to reach, when it knows. A
    /// handshake says who actually answered, and a route that answers as
    /// somebody else is rejected rather than selected: a peer's address list
    /// only ever grows, so a stale entry can be reoccupied by an unrelated node
    /// that handshakes perfectly well, and selecting it would both misdirect the
    /// exchange and stop the working address behind it from ever being tried.
    /// Ticket paths pass `None` — a ticket's whole purpose is to learn an
    /// identity the caller does not yet have.
    ///
    /// If all tasks fail the last error is returned. If `addresses` is empty
    /// an [`SyncError::InvalidAddress`] error is returned.
    pub(super) async fn select_address(
        &self,
        addresses: &[Address],
        expected: Option<&PublicKey>,
    ) -> Result<(Address, PublicKey)> {
        if addresses.is_empty() {
            return Err(SyncError::InvalidAddress("Ticket has no address hints".into()).into());
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(addresses.len());

        for addr in addresses {
            let tx = tx.clone();
            let sync = self.clone();
            let addr = addr.clone();
            let addr_info = addr.clone();
            // Detached spawn: the task keeps running even after we return.
            tokio::spawn(async move {
                let result = tokio::time::timeout(
                    Self::ADDRESS_ATTEMPT_TIMEOUT,
                    sync.connect_to_peer(&addr),
                )
                .await;
                let result = match result {
                    Ok(Ok(peer_pubkey)) => Ok((addr, peer_pubkey)),
                    Ok(Err(error)) => Err(error),
                    Err(_) => {
                        warn!(
                            address = ?addr_info,
                            "Address attempt timed out after {:?}",
                            Self::ADDRESS_ATTEMPT_TIMEOUT,
                        );
                        Err(SyncError::Network(format!(
                            "Address attempt timed out after {:?}",
                            Self::ADDRESS_ATTEMPT_TIMEOUT,
                        ))
                        .into())
                    }
                };
                match &result {
                    Ok(_) => debug!(address = ?addr_info, "Address attempt succeeded"),
                    Err(e) => debug!(address = ?addr_info, error = %e, "Address attempt failed"),
                }
                // Ignore send errors — the receiver is dropped on early success,
                // but the task still completes its work (peer registration, etc.).
                let _ = tx.send(result).await;
            });
        }
        // Drop our sender so the channel closes when all tasks finish.
        drop(tx);

        let mut last_err = None;
        while let Some(result) = rx.recv().await {
            match result {
                Ok((addr, answered)) => match expected {
                    Some(want) if want != &answered => {
                        warn!(
                            address = ?addr,
                            expected = %want,
                            answered = %answered,
                            "Address answered as a different peer; not selecting it as the route"
                        );
                        last_err = Some(
                            SyncError::HandshakeFailed(format!(
                                "{addr:?} answered as {answered}, not {want}"
                            ))
                            .into(),
                        );
                    }
                    _ => return Ok((addr, answered)),
                },
                Err(e) => last_err = Some(e),
            }
        }

        Err(last_err.expect("at least one task was spawned"))
    }
}
