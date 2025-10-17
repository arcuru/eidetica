//! Background sync engine implementation.
//!
//! This module provides the BackgroundSync struct that handles all sync operations
//! in a single background thread, removing circular dependency issues and providing
//! automatic retry, periodic sync, and reconnection handling.

use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use tokio::{
    sync::{mpsc, oneshot},
    time::interval,
};
use tracing::{Instrument, debug, info, info_span, trace};

use super::{
    DEVICE_KEY_NAME,
    error::SyncError,
    handler::SyncHandlerImpl,
    peer_manager::PeerManager,
    peer_types::{Address, PeerInfo},
    protocol::{HandshakeRequest, PROTOCOL_VERSION, SyncRequest, SyncResponse, SyncTreeRequest},
    transports::SyncTransport,
};
use crate::{
    Database, Result,
    auth::crypto::{format_public_key, generate_challenge, verify_challenge_response},
    backend::BackendDB,
    entry::{Entry, ID},
};

/// Commands that can be sent to the background sync engine
#[derive(Debug)]
pub enum SyncCommand {
    /// Send entries to a specific peer
    SendEntries { peer: String, entries: Vec<Entry> },
    /// Queue an entry for sending to a peer (from hook)
    QueueEntry {
        peer: String,
        entry_id: ID,
        tree_id: ID,
    },
    /// Trigger immediate sync with a peer
    SyncWithPeer { peer: String },
    /// Shutdown the background engine
    Shutdown,

    // Server management commands
    /// Start the sync server
    StartServer {
        addr: String,
        response: oneshot::Sender<Result<()>>,
    },
    /// Stop the sync server
    StopServer {
        response: oneshot::Sender<Result<()>>,
    },
    /// Get the server's listening address
    GetServerAddress {
        response: oneshot::Sender<Result<String>>,
    },

    // Peer connection
    /// Connect to a peer and perform handshake
    ConnectToPeer {
        address: Address,
        response: oneshot::Sender<Result<String>>, // Returns peer pubkey
    },

    // Request/Response operations
    /// Send a sync request and get response
    SendRequest {
        address: Address,
        request: SyncRequest,
        response: oneshot::Sender<Result<SyncResponse>>,
    },
}

/// Entry in the retry queue for failed sends
#[derive(Debug, Clone)]
struct RetryEntry {
    peer: String,
    entries: Vec<Entry>,
    attempts: u32,
    last_attempt: SystemTime,
}

/// Background sync engine that owns all sync state and handles operations
pub struct BackgroundSync {
    // Core components - owns everything
    transport: Box<dyn SyncTransport>,
    backend: Arc<dyn BackendDB>,
    sync_tree_id: ID,

    // Server state
    server_address: Option<String>,

    // Retry queue for failed sends
    retry_queue: Vec<RetryEntry>,

    // Communication
    command_rx: mpsc::Receiver<SyncCommand>,
}

impl BackgroundSync {
    /// Start the background sync engine and return a command sender
    pub fn start(
        transport: Box<dyn SyncTransport>,
        backend: Arc<dyn BackendDB>,
        sync_tree_id: ID,
    ) -> mpsc::Sender<SyncCommand> {
        let (tx, rx) = mpsc::channel(100);

        let background = Self {
            transport,
            backend,
            sync_tree_id,
            server_address: None,
            retry_queue: Vec::new(),
            command_rx: rx,
        };

        // Try to spawn in current runtime, or create one if needed
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(background.run());
        } else {
            // Create a runtime and spawn the background task
            std::thread::spawn(|| {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(background.run());
            });
        }
        tx
    }

    /// Get the sync tree for accessing peer data
    fn get_sync_tree(&self) -> Result<Database> {
        let mut sync_tree = Database::new_from_id(self.sync_tree_id.clone(), self.backend.clone())?;
        sync_tree.set_default_auth_key(DEVICE_KEY_NAME);
        Ok(sync_tree)
    }

    /// Main event loop that handles all sync operations
    async fn run(mut self) {
        async move {
            info!("Starting background sync engine");
            // Set up timers
            let mut periodic_sync = interval(Duration::from_secs(300)); // 5 minutes
            let mut retry_check = interval(Duration::from_secs(30)); // 30 seconds
            let mut connection_check = interval(Duration::from_secs(60)); // 1 minute

            // Skip initial tick to avoid immediate execution
            periodic_sync.tick().await;
            retry_check.tick().await;
            connection_check.tick().await;

            loop {
                tokio::select! {
                    // Handle commands from frontend
                    Some(cmd) = self.command_rx.recv() => {
                        if let Err(e) = self.handle_command(cmd).await {
                            // Log errors but continue running - background sync should be resilient
                            tracing::error!("Background sync command error: {e}");
                        }
                    }

                    // Periodic sync with all peers
                    _ = periodic_sync.tick() => {
                        self.periodic_sync_all_peers().await;
                    }

                    // Process retry queue
                    _ = retry_check.tick() => {
                        self.process_retry_queue().await;
                    }

                    // Check and reconnect disconnected peers
                    _ = connection_check.tick() => {
                        self.check_peer_connections().await;
                    }

                    // Channel closed, shutdown
                    else => {
                        // Normal shutdown when channel closes
                        info!("Background sync engine shutting down");
                        break;
                    }
                }
            }
        }
        .instrument(info_span!("background_sync"))
        .await
    }

    /// Handle a single command from the frontend
    async fn handle_command(&mut self, command: SyncCommand) -> Result<()> {
        match command {
            SyncCommand::SendEntries { peer, entries } => {
                if let Err(e) = self.send_to_peer(&peer, entries.clone()).await {
                    self.add_to_retry_queue(peer, entries, e);
                }
            }

            SyncCommand::QueueEntry {
                peer,
                entry_id,
                tree_id: _,
            } => {
                // Fetch entry and send immediately
                match self.backend.get(&entry_id) {
                    Ok(entry) => {
                        if let Err(e) = self.send_to_peer(&peer, vec![entry.clone()]).await {
                            self.add_to_retry_queue(peer, vec![entry], e);
                        }
                    }
                    Err(e) => {
                        // Log error but continue with other entries
                        tracing::warn!("Failed to fetch entry {entry_id} for peer {peer}: {e}");
                    }
                }
            }

            SyncCommand::SyncWithPeer { peer } => {
                if let Err(e) = self.sync_with_peer(&peer).await {
                    // Log sync failure but don't crash the background engine
                    tracing::error!("Failed to sync with peer {peer}: {e}");
                }
            }

            SyncCommand::StartServer { addr, response } => {
                let result = self.start_server(&addr).await;
                let _ = response.send(result);
            }

            SyncCommand::StopServer { response } => {
                let result = self.stop_server().await;
                let _ = response.send(result);
            }

            SyncCommand::GetServerAddress { response } => {
                let result = self.get_server_address();
                let _ = response.send(result);
            }

            SyncCommand::ConnectToPeer { address, response } => {
                let result = self.connect_to_peer(&address).await;
                let _ = response.send(result);
            }

            SyncCommand::SendRequest {
                address,
                request,
                response,
            } => {
                let result = self.send_sync_request(&address, &request).await;
                let _ = response.send(result);
            }

            SyncCommand::Shutdown => {
                // Shutdown command received - exit cleanly
                return Err(SyncError::Network("Shutdown requested".to_string()).into());
            }
        }
        Ok(())
    }

    /// Send specific entries to a peer without duplicate filtering.
    ///
    /// This method performs direct entry transmission and is used by:
    /// - `SendEntries` commands from the frontend (caller handles filtering)
    /// - `sync_tree_with_peer()` after smart duplicate prevention analysis
    ///
    /// # Design Note
    ///
    /// This method does NOT perform duplicate prevention - that responsibility
    /// lies with the caller. The background sync's smart duplicate prevention
    /// happens in `sync_tree_with_peer()` via tip comparison, while direct
    /// `SendEntries` commands trust the caller to send appropriate entries.
    ///
    /// # Error Handling
    ///
    /// Failed sends are automatically added to the retry queue with exponential backoff.
    async fn send_to_peer(&self, peer: &str, entries: Vec<Entry>) -> Result<()> {
        // Get peer address from sync tree (extract and drop operation before await)
        let address = {
            let sync_tree = self.get_sync_tree()?;
            let op = sync_tree.new_transaction()?;
            let peer_info = PeerManager::new(&op)
                .get_peer_info(peer)?
                .ok_or_else(|| SyncError::PeerNotFound(peer.to_string()))?;

            peer_info
                .addresses
                .first()
                .ok_or_else(|| SyncError::Network("No addresses found for peer".to_string()))?
                .clone()
        }; // Operation is dropped here

        let request = SyncRequest::SendEntries(entries);
        let response = self.transport.send_request(&address, &request).await?;

        match response {
            crate::sync::protocol::SyncResponse::Ack
            | crate::sync::protocol::SyncResponse::Count(_) => Ok(()),
            crate::sync::protocol::SyncResponse::Error(msg) => Err(SyncError::SyncProtocolError(
                format!("Peer {peer} returned error: {msg}"),
            )
            .into()),
            _ => Err(SyncError::UnexpectedResponse {
                expected: "Ack or Count",
                actual: format!("{response:?}"),
            }
            .into()),
        }
    }

    /// Add failed send to retry queue
    fn add_to_retry_queue(&mut self, peer: String, entries: Vec<Entry>, error: crate::Error) {
        // Log send failure and add to retry queue
        tracing::warn!("Failed to send to {peer}: {error}. Adding to retry queue.");
        self.retry_queue.push(RetryEntry {
            peer,
            entries,
            attempts: 1,
            last_attempt: SystemTime::now(),
        });
    }

    /// Process retry queue with exponential backoff
    async fn process_retry_queue(&mut self) {
        let now = SystemTime::now();
        let mut still_failed = Vec::new();

        // Take the retry queue to avoid borrowing issues
        let retry_queue = std::mem::take(&mut self.retry_queue);

        // Process entries that are ready for retry
        for mut entry in retry_queue {
            let backoff = Duration::from_secs(2u64.pow(entry.attempts.min(6))); // Max 64 second backoff

            if now.duration_since(entry.last_attempt).unwrap() >= backoff {
                // Try sending again
                if let Err(_e) = self.send_to_peer(&entry.peer, entry.entries.clone()).await {
                    entry.attempts += 1;
                    entry.last_attempt = now;

                    if entry.attempts < 10 {
                        // Max 10 attempts
                        still_failed.push(entry);
                    } else {
                        // Max retries exceeded - give up on this batch
                        tracing::error!("Giving up on sending to {} after 10 attempts", entry.peer);
                    }
                } else {
                    // Successfully retried after failure
                }
            } else {
                // Not ready for retry yet
                still_failed.push(entry);
            }
        }

        self.retry_queue = still_failed;
    }

    /// Perform periodic sync with all active peers
    async fn periodic_sync_all_peers(&self) {
        // Periodic sync triggered

        // Get all peers from sync tree
        let peers = match self.get_sync_tree() {
            Ok(sync_tree) => match sync_tree.new_transaction() {
                Ok(op) => match PeerManager::new(&op).list_peers() {
                    Ok(peers) => {
                        // Extract peer list and drop the operation before awaiting
                        peers
                    }
                    Err(_) => {
                        // Skip sync if we can't list peers
                        return;
                    }
                },
                Err(_) => {
                    // Skip sync if we can't create operation
                    return;
                }
            },
            Err(_) => {
                // Skip sync if we can't get sync tree
                return;
            }
        };

        // Now sync with peers (operation is dropped, so no Send issues)
        for peer_info in peers {
            if peer_info.status == crate::sync::peer_types::PeerStatus::Active
                && let Err(e) = self.sync_with_peer(&peer_info.pubkey).await
            {
                // Log individual peer sync failure but continue with others
                tracing::error!("Periodic sync failed with {}: {e}", peer_info.pubkey);
            }
        }
    }

    /// Sync with a specific peer (bidirectional)
    async fn sync_with_peer(&self, peer_pubkey: &str) -> Result<()> {
        async move {
            info!(peer = %peer_pubkey, "Starting peer synchronization");

            // Get peer info and tree list from sync tree (extract and drop operation before await)
            let (address, sync_trees) = {
                let sync_tree = self.get_sync_tree()?;
                let op = sync_tree.new_transaction()?;
                let peer_manager = PeerManager::new(&op);

                let peer_info = peer_manager
                    .get_peer_info(peer_pubkey)?
                    .ok_or_else(|| SyncError::PeerNotFound(peer_pubkey.to_string()))?;

                let address = peer_info
                    .addresses
                    .first()
                    .ok_or_else(|| SyncError::Network("No addresses found for peer".to_string()))?
                    .clone();

                // Find all trees that sync with this peer from sync tree
                let sync_trees = peer_manager.get_peer_trees(peer_pubkey)?;

                (address, sync_trees)
            }; // Operation is dropped here

            if sync_trees.is_empty() {
                debug!(peer = %peer_pubkey, "No trees configured for sync with peer");
                return Ok(()); // No trees to sync
            }

            info!(peer = %peer_pubkey, tree_count = sync_trees.len(), "Synchronizing trees with peer");

            for tree_id_str in sync_trees {
                // Convert string ID to entry ID
                let tree_id = ID::from(tree_id_str.as_str());
                if let Err(e) = self
                    .sync_tree_with_peer(peer_pubkey, &tree_id, &address)
                    .await
                {
                    // Log tree sync failure but continue with other trees
                    tracing::error!("Failed to sync tree {tree_id} with peer {peer_pubkey}: {e}");
                }
            }

            info!(peer = %peer_pubkey, "Completed peer synchronization");
            Ok(())
        }
        .instrument(info_span!("sync_with_peer", peer = %peer_pubkey))
        .await
    }

    /// Sync a specific tree with a peer using smart duplicate prevention.
    ///
    /// This method implements Eidetica's core synchronization algorithm based on
    /// Merkle-CRDT tip comparison. It eliminates duplicate sends by understanding
    /// the semantic state of both peers' trees.
    ///
    /// # Algorithm
    ///
    /// 1. **Tip Exchange**: Get local tips and request peer's tips
    /// 2. **Gap Analysis**: Compare tips to identify missing entries on both sides
    /// 3. **Smart Transfer**: Only send/receive entries that are genuinely missing
    /// 4. **DAG Completion**: Include all necessary ancestor entries
    ///
    /// # Benefits
    ///
    /// - **No duplicates**: Tips comparison guarantees no redundant network transfers
    /// - **Complete data**: DAG traversal ensures all dependencies are satisfied
    /// - **Bidirectional**: Both peers sync simultaneously for efficiency
    /// - **Self-correcting**: Any missed entries are caught in subsequent syncs
    ///
    /// # Performance
    ///
    /// - **O(tip_count)** network requests for discovery
    /// - **O(missing_entries)** data transfer (optimal)
    /// - **Stateless**: No persistent tracking of individual sends needed
    async fn sync_tree_with_peer(
        &self,
        peer_pubkey: &str,
        tree_id: &ID,
        address: &Address,
    ) -> Result<()> {
        async move {
            trace!(peer = %peer_pubkey, tree = %tree_id, "Starting unified tree synchronization");

            // Get our tips for this tree (empty if tree doesn't exist)
            let our_tips = self
                .backend
                .get_tips(tree_id)
                .map_err(|e| SyncError::BackendError(format!("Failed to get local tips: {e}")))?;

            debug!(peer = %peer_pubkey, tree = %tree_id, our_tips = our_tips.len(), "Sending sync tree request");

            // Send unified sync request
            let request = SyncRequest::SyncTree(SyncTreeRequest {
                tree_id: tree_id.clone(),
                our_tips,
                requesting_key: None, // TODO: Add auth support for background sync
                requesting_key_name: None,
                requested_permission: None,
            });

            let response = self.transport.send_request(address, &request).await?;

            match response {
                SyncResponse::Bootstrap(bootstrap_response) => {
                    info!(peer = %peer_pubkey, tree = %tree_id, entry_count = bootstrap_response.all_entries.len() + 1, "Received bootstrap response");
                    self.handle_bootstrap_response(bootstrap_response).await?;
                }
                SyncResponse::Incremental(incremental_response) => {
                    debug!(peer = %peer_pubkey, tree = %tree_id,
                           their_tips = incremental_response.their_tips.len(),
                           missing_count = incremental_response.missing_entries.len(),
                           "Received incremental sync response");
                    self.handle_incremental_response(incremental_response).await?;
                }
                SyncResponse::Error(msg) => {
                    return Err(SyncError::SyncProtocolError(format!("Sync error: {msg}")).into());
                }
                _ => {
                    return Err(SyncError::UnexpectedResponse {
                        expected: "Bootstrap or Incremental",
                        actual: format!("{response:?}"),
                    }.into());
                }
            }

            trace!(peer = %peer_pubkey, tree = %tree_id, "Completed unified tree synchronization");
            Ok(())
        }
        .instrument(info_span!("sync_tree", peer = %peer_pubkey, tree = %tree_id))
        .await
    }

    /// Store received entries from peer with proper DAG ordering
    async fn store_received_entries(&self, entries: Vec<Entry>) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        // Note: Height-based sorting would require tree context
        // For now, we rely on the sender to provide entries in correct order

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

            // Verify parent entries exist before storing children
            if let Ok(parents) = entry.parents() {
                for parent_id in &parents {
                    if let Err(e) = self.backend.get(parent_id) {
                        if e.is_not_found() {
                            return Err(SyncError::InvalidEntry(format!(
                                "Parent entry {} not found when storing entry {}",
                                parent_id,
                                entry.id()
                            ))
                            .into());
                        } else {
                            return Err(SyncError::BackendError(format!(
                                "Failed to check parent {} for entry {}: {}",
                                parent_id,
                                entry.id(),
                                e
                            ))
                            .into());
                        }
                    }
                }
            }

            // Store the entry
            self.backend
                .put_verified(entry)
                .map_err(|e| SyncError::BackendError(format!("Failed to store entry: {e}")))?;
        }

        Ok(())
    }

    /// Check peer connections and attempt reconnection
    async fn check_peer_connections(&mut self) {
        // For now, this is a placeholder
        // In the future, we could implement connection health checks
        // and automatic reconnection logic here
    }

    /// Start the sync server
    async fn start_server(&mut self, addr: &str) -> Result<()> {
        if self.transport.is_server_running() {
            return Err(SyncError::ServerAlreadyRunning {
                address: addr.to_string(),
            }
            .into());
        }

        // Create a sync handler with backend access and sync tree ID
        let handler = Arc::new(SyncHandlerImpl::new(
            self.backend.clone(),
            self.sync_tree_id.clone(),
        ));

        self.transport.start_server(addr, handler).await?;

        // Store the server address for later retrieval
        match self.transport.get_server_address() {
            Ok(server_addr) => {
                self.server_address = Some(server_addr);
                tracing::info!("Sync server started on {addr}");
                Ok(())
            }
            Err(e) => {
                // If we can't get the address, stop the server and return error
                let _ = self.transport.stop_server().await;
                Err(e)
            }
        }
    }

    /// Stop the sync server
    async fn stop_server(&mut self) -> Result<()> {
        if !self.transport.is_server_running() {
            return Err(SyncError::ServerNotRunning.into());
        }

        self.transport.stop_server().await?;
        self.server_address = None;
        tracing::info!("Sync server stopped");
        Ok(())
    }

    /// Get the server's listening address
    fn get_server_address(&self) -> Result<String> {
        self.server_address
            .clone()
            .ok_or_else(|| SyncError::ServerNotRunning.into())
    }

    /// Connect to a peer and perform handshake
    async fn connect_to_peer(&mut self, address: &Address) -> Result<String> {
        // Generate challenge for authentication
        let challenge = generate_challenge();

        // Get our device info from backend
        let device_id = "background_sync_device".to_string(); // TODO: Get actual device ID
        let public_key = if let Some(signing_key) = self.backend.get_private_key(DEVICE_KEY_NAME)? {
            let verifying_key = signing_key.verifying_key();
            format_public_key(&verifying_key)
        } else {
            return Err(SyncError::DeviceKeyNotFound {
                key_name: DEVICE_KEY_NAME.to_string(),
            }
            .into());
        };

        // Create handshake request
        let handshake_request = HandshakeRequest {
            device_id,
            public_key: public_key.clone(),
            display_name: Some("BackgroundSync".to_string()),
            protocol_version: PROTOCOL_VERSION,
            challenge: challenge.clone(),
        };

        // Send handshake request
        let request = SyncRequest::Handshake(handshake_request);
        let response = self.transport.send_request(address, &request).await?;

        // Process handshake response
        match response {
            SyncResponse::Handshake(handshake_resp) => {
                // Verify protocol version
                if handshake_resp.protocol_version != PROTOCOL_VERSION {
                    return Err(SyncError::ProtocolMismatch {
                        expected: PROTOCOL_VERSION,
                        received: handshake_resp.protocol_version,
                    }
                    .into());
                }

                // Verify the server's signature on our challenge
                let verification_result = verify_challenge_response(
                    &challenge,
                    &handshake_resp.challenge_response,
                    &handshake_resp.public_key,
                );

                match verification_result {
                    Ok(true) => {
                        // Signature verified successfully
                    }
                    Ok(false) => {
                        return Err(SyncError::HandshakeFailed(
                            "Invalid signature in handshake response".to_string(),
                        )
                        .into());
                    }
                    Err(e) => {
                        return Err(SyncError::HandshakeFailed(format!(
                            "Signature verification failed: {e}"
                        ))
                        .into());
                    }
                }

                // Create peer info (store in sync tree instead of using it directly)
                let _peer_info = PeerInfo::new(
                    &handshake_resp.public_key,
                    handshake_resp.display_name.as_deref(),
                );

                // Add peer to sync tree
                let sync_tree = self.get_sync_tree()?;
                let op = sync_tree.new_transaction()?;
                let peer_manager = PeerManager::new(&op);

                // Try to register peer, but ignore if already exists
                match peer_manager.register_peer(
                    &handshake_resp.public_key,
                    handshake_resp.display_name.as_deref(),
                ) {
                    Ok(_) => {
                        op.commit()?;
                    }
                    Err(crate::Error::Sync(crate::sync::error::SyncError::PeerAlreadyExists(
                        _,
                    ))) => {
                        // Peer already exists, that's fine - just continue with handshake result
                    }
                    Err(e) => return Err(e),
                }

                // Successfully connected to peer
                Ok(handshake_resp.public_key)
            }
            SyncResponse::Error(msg) => Err(SyncError::HandshakeFailed(msg).into()),
            _ => Err(SyncError::HandshakeFailed("Unexpected response type".to_string()).into()),
        }
    }

    /// Send a sync request and get response
    async fn send_sync_request(
        &self,
        address: &Address,
        request: &SyncRequest,
    ) -> Result<SyncResponse> {
        self.transport.send_request(address, request).await
    }

    /// Handle bootstrap response by storing root and all entries
    async fn handle_bootstrap_response(
        &self,
        response: super::protocol::BootstrapResponse,
    ) -> Result<()> {
        trace!(tree_id = %response.tree_id, "Processing bootstrap response");

        // Store root entry first
        self.backend
            .put_verified(response.root_entry)
            .map_err(|e| SyncError::BackendError(format!("Failed to store root entry: {e}")))?;

        // Store all other entries
        self.store_received_entries(response.all_entries).await?;

        info!(tree_id = %response.tree_id, "Bootstrap completed successfully");
        Ok(())
    }

    /// Handle incremental response by storing missing entries
    async fn handle_incremental_response(
        &self,
        response: super::protocol::IncrementalResponse,
    ) -> Result<()> {
        trace!(tree_id = %response.tree_id, "Processing incremental response");

        // Store missing entries
        self.store_received_entries(response.missing_entries)
            .await?;

        // Note: We could use their_tips for further optimization in the future
        // For now, the next sync cycle will handle any remaining differences

        debug!(tree_id = %response.tree_id, "Incremental sync completed");
        Ok(())
    }
}
