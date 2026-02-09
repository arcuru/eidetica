//! Background sync engine implementation.
//!
//! This module provides the BackgroundSync struct that handles all sync operations
//! in a single background thread, removing circular dependency issues and providing
//! automatic retry, periodic sync, and reconnection handling.

use std::{sync::Arc, time::Duration};

use tokio::{
    sync::{mpsc, oneshot},
    time::interval,
};
use tracing::{Instrument, debug, info, info_span, trace};

use super::{
    ADMIN_KEY_NAME,
    error::SyncError,
    handler::SyncHandlerImpl,
    peer_manager::PeerManager,
    peer_types::{Address, PeerId, PeerStatus},
    protocol::{SyncRequest, SyncResponse, SyncTreeRequest},
    queue::SyncQueue,
    transport_manager::TransportManager,
};
use crate::{
    Database, Error, Instance, Result, WeakInstance,
    entry::{Entry, ID},
    store::DocStore,
};

mod conn;

/// Commands that can be sent to the background sync engine
pub enum SyncCommand {
    /// Send entries to a specific peer
    SendEntries { peer: PeerId, entries: Vec<Entry> },
    /// Trigger immediate sync with a peer
    SyncWithPeer { peer: PeerId },
    /// Shutdown the background engine
    Shutdown,

    // Transport management
    /// Add a named transport to the transport manager
    AddTransport {
        name: String,
        transport: Box<dyn super::transports::SyncTransport>,
        response: oneshot::Sender<Result<()>>,
    },

    // Server management commands
    /// Start the sync server on specified or all transports
    StartServer {
        /// Transport name to start, or None for all transports
        name: Option<String>,
        response: oneshot::Sender<Result<()>>,
    },
    /// Stop the sync server on specified or all transports
    StopServer {
        /// Transport name to stop, or None for all transports
        name: Option<String>,
        response: oneshot::Sender<Result<()>>,
    },
    /// Get the server's listening address for a specific transport
    GetServerAddress {
        name: String,
        response: oneshot::Sender<Result<String>>,
    },
    /// Get all server addresses for running servers
    GetAllServerAddresses {
        response: oneshot::Sender<Result<Vec<(String, String)>>>,
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

    /// Flush: process all queued entries and retry queue, then respond.
    Flush {
        response: oneshot::Sender<Result<()>>,
    },
}

// Manual Debug impl required because:
// - `Box<dyn SyncTransport>` doesn't implement Debug (trait object)
// - `oneshot::Sender` doesn't implement Debug (channel internals)
// - Transports may contain secrets (e.g., Iroh's cryptographic keys)
// This impl provides safe, useful debug output for logging.
impl std::fmt::Debug for SyncCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SendEntries { peer, entries } => f
                .debug_struct("SendEntries")
                .field("peer", peer)
                .field("entries_count", &entries.len())
                .finish(),
            Self::SyncWithPeer { peer } => {
                f.debug_struct("SyncWithPeer").field("peer", peer).finish()
            }
            Self::Shutdown => write!(f, "Shutdown"),
            Self::AddTransport {
                name, transport, ..
            } => f
                .debug_struct("AddTransport")
                .field("name", name)
                .field("transport_type", &transport.transport_type())
                .finish(),
            Self::StartServer { name, .. } => {
                f.debug_struct("StartServer").field("name", name).finish()
            }
            Self::StopServer { name, .. } => {
                f.debug_struct("StopServer").field("name", name).finish()
            }
            Self::GetServerAddress { name, .. } => f
                .debug_struct("GetServerAddress")
                .field("name", name)
                .finish(),
            Self::GetAllServerAddresses { .. } => write!(f, "GetAllServerAddresses"),
            Self::ConnectToPeer { address, .. } => f
                .debug_struct("ConnectToPeer")
                .field("address", address)
                .finish(),
            Self::SendRequest {
                address, request, ..
            } => f
                .debug_struct("SendRequest")
                .field("address", address)
                .field("request", request)
                .finish(),
            Self::Flush { .. } => write!(f, "Flush"),
        }
    }
}

/// Entry in the retry queue for failed sends
#[derive(Debug, Clone)]
struct RetryEntry {
    peer: PeerId,
    entries: Vec<Entry>,
    attempts: u32,
    /// Timestamp of last attempt in milliseconds since Unix epoch
    last_attempt_ms: u64,
}

/// Background sync engine that owns all sync state and handles operations
pub struct BackgroundSync {
    // Core components - owns everything
    pub(super) transport_manager: TransportManager,
    instance: WeakInstance,
    pub(super) sync_tree_id: ID,

    // Queue for entries pending synchronization (shared with Sync frontend)
    queue: Arc<SyncQueue>,

    // Retry queue for failed sends
    retry_queue: Vec<RetryEntry>,

    // Communication
    command_rx: mpsc::Receiver<SyncCommand>,
}

impl BackgroundSync {
    /// Start the background sync engine and return a command sender.
    ///
    /// The engine starts with no transports registered. Use `AddTransport`
    /// commands to add transports after starting.
    pub fn start(
        instance: Instance,
        sync_tree_id: ID,
        queue: Arc<SyncQueue>,
    ) -> mpsc::Sender<SyncCommand> {
        let (tx, rx) = mpsc::channel(100);

        let background = Self {
            transport_manager: TransportManager::new(),
            instance: instance.downgrade(),
            sync_tree_id,
            queue,
            retry_queue: Vec::new(),
            command_rx: rx,
        };

        // Spawn background sync as a regular tokio task
        // (Transaction is now Send since it uses Arc<Mutex>)
        tokio::spawn(background.run());
        tx
    }

    /// Upgrade the weak instance reference to a strong reference.
    pub(super) fn instance(&self) -> Result<Instance> {
        self.instance
            .upgrade()
            .ok_or_else(|| SyncError::InstanceDropped.into())
    }

    /// Get the sync tree for accessing peer data
    pub(super) async fn get_sync_tree(&self) -> Result<Database> {
        // Load sync tree with the device key
        let instance = self.instance()?;
        let signing_key = instance.device_key().clone();

        Database::open(
            instance,
            &self.sync_tree_id,
            signing_key,
            ADMIN_KEY_NAME.to_string(),
        )
        .await
    }

    /// Get the minimum sync interval from all tracked databases
    /// Returns None if no databases are tracked or no intervals are set
    async fn get_min_sync_interval(&self) -> Option<u64> {
        let sync_tree = match self.get_sync_tree().await {
            Ok(tree) => tree,
            Err(_) => return None,
        };

        let txn = match sync_tree.new_transaction().await {
            Ok(txn) => txn,
            Err(_) => return None,
        };

        let user_mgr = super::user_sync_manager::UserSyncManager::new(&txn);

        // Get all tracked database IDs from the DATABASE_USERS_SUBTREE
        let database_users = match txn
            .get_store::<DocStore>(super::user_sync_manager::DATABASE_USERS_SUBTREE)
            .await
        {
            Ok(store) => store,
            Err(_) => return None,
        };

        let all_dbs = match database_users.get_all().await {
            Ok(doc) => doc,
            Err(_) => return None,
        };

        // Find the minimum interval across all databases
        let mut min_interval: Option<u64> = None;
        for db_id_str in all_dbs.keys() {
            if let Ok(db_id) = ID::parse(db_id_str)
                && let Ok(Some(settings)) = user_mgr.get_combined_settings(&db_id).await
                && let Some(interval) = settings.interval_seconds
            {
                min_interval = Some(match min_interval {
                    Some(current_min) => current_min.min(interval),
                    None => interval,
                });
            }
        }

        min_interval
    }

    /// Main event loop that handles all sync operations
    async fn run(mut self) {
        async move {
            info!("Starting background sync engine");

            // Get initial sync interval from settings (default to 300 seconds if none set)
            let mut current_interval_secs = self.get_min_sync_interval().await.unwrap_or(300);
            info!("Initial periodic sync interval: {} seconds", current_interval_secs);

            // Set up timers
            let mut periodic_sync = interval(Duration::from_secs(current_interval_secs));
            let mut queue_check = interval(Duration::from_secs(5)); // 5 seconds - batches local writes
            let mut retry_check = interval(Duration::from_secs(30)); // 30 seconds
            let mut connection_check = interval(Duration::from_secs(60)); // 1 minute
            let mut settings_check = interval(Duration::from_secs(60)); // Check for settings changes every minute

            // Skip initial tick to avoid immediate execution
            periodic_sync.tick().await;
            queue_check.tick().await;
            retry_check.tick().await;
            connection_check.tick().await;
            settings_check.tick().await;

            loop {
                tokio::select! {
                    // Handle commands from frontend
                    Some(cmd) = self.command_rx.recv() => {
                        if let Err(e) = self.handle_command(cmd).await {
                            // Log errors but continue running - background sync should be resilient
                            tracing::error!("Background sync command error: {e}");
                        }
                    }

                    // Drain sync queue (batched entries)
                    _ = queue_check.tick() => {
                        self.process_queue().await;
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

                    // Check if sync interval settings have changed
                    _ = settings_check.tick() => {
                        if let Some(new_interval) = self.get_min_sync_interval().await
                            && new_interval != current_interval_secs {
                                info!("Sync interval changed from {} to {} seconds", current_interval_secs, new_interval);
                                current_interval_secs = new_interval;
                                // Recreate the periodic sync timer with new interval
                                periodic_sync = interval(Duration::from_secs(new_interval));
                                periodic_sync.tick().await; // Skip initial tick
                            }
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
                    let now_ms = self.instance().map(|i| i.clock().now_millis()).unwrap_or(0);
                    self.add_to_retry_queue(peer, entries, e, now_ms);
                }
            }

            SyncCommand::SyncWithPeer { peer } => {
                if let Err(e) = self.sync_with_peer(&peer).await {
                    // Log sync failure but don't crash the background engine
                    tracing::error!("Failed to sync with peer {peer}: {e}");
                }
            }

            SyncCommand::AddTransport {
                name,
                transport,
                response,
            } => {
                // Stop server on existing transport if running
                if let Some(old) = self.transport_manager.get_mut(&name)
                    && old.is_server_running()
                {
                    let _ = old.stop_server().await;
                }
                self.transport_manager.add(&name, transport);
                tracing::debug!("Added transport: {}", name);
                let _ = response.send(Ok(()));
            }

            SyncCommand::StartServer { name, response } => {
                let result = self.start_server(name.as_deref()).await;
                let _ = response.send(result);
            }

            SyncCommand::StopServer { name, response } => {
                let result = self.stop_server(name.as_deref()).await;
                let _ = response.send(result);
            }

            SyncCommand::GetServerAddress { name, response } => {
                let result = self.transport_manager.get_server_address(&name);
                let _ = response.send(result);
            }

            SyncCommand::GetAllServerAddresses { response } => {
                let addresses = self.transport_manager.get_all_server_addresses();
                let _ = response.send(Ok(addresses));
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

            SyncCommand::Flush { response } => {
                // Process retry queue first (old failures), then main queue (new entries)
                // This avoids double-trying entries that fail in process_queue
                let retry_failures = self.flush_retry_queue().await;
                let queue_failures = self.process_queue().await;

                // Report error if any failures occurred
                let result = match (retry_failures, queue_failures) {
                    (0, 0) => Ok(()),
                    (r, q) => Err(SyncError::Network(format!(
                        "Flush had failures: {r} from retry queue, {q} from new entries"
                    ))
                    .into()),
                };
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
    async fn send_to_peer(&self, peer: &PeerId, entries: Vec<Entry>) -> Result<()> {
        // Get peer address from sync tree (extract and drop transaction before await)
        let address = {
            let sync_tree = self.get_sync_tree().await?;
            let txn = sync_tree.new_transaction().await?;
            let peer_info = PeerManager::new(&txn)
                .get_peer_info(&peer)
                .await?
                .ok_or_else(|| SyncError::PeerNotFound(peer.to_string()))?;

            peer_info
                .addresses
                .first()
                .ok_or_else(|| SyncError::Network("No addresses found for peer".to_string()))?
                .clone()
        }; // Transaction is dropped here

        let request = SyncRequest::SendEntries(entries);
        let response = self
            .transport_manager
            .send_request(&address, &request)
            .await?;

        match response {
            SyncResponse::Ack | SyncResponse::Count(_) => Ok(()),
            SyncResponse::Error(msg) => Err(SyncError::SyncProtocolError(format!(
                "Peer {peer} returned error: {msg}"
            ))
            .into()),
            _ => Err(SyncError::UnexpectedResponse {
                expected: "Ack or Count",
                actual: format!("{response:?}"),
            }
            .into()),
        }
    }

    /// Add failed send to retry queue
    fn add_to_retry_queue(&mut self, peer: PeerId, entries: Vec<Entry>, error: Error, now_ms: u64) {
        // Log send failure and add to retry queue
        tracing::warn!("Failed to send to {peer}: {error}. Adding to retry queue.");
        self.retry_queue.push(RetryEntry {
            peer,
            entries,
            attempts: 1,
            last_attempt_ms: now_ms,
        });
    }

    /// Process entries from the sync queue, batching by peer.
    ///
    /// Drains the queue and sends entries to each peer. Failed sends
    /// are added to the retry queue with exponential backoff.
    /// Returns the number of peers that failed to receive entries.
    async fn process_queue(&mut self) -> usize {
        let batches = self.queue.drain();
        if batches.is_empty() {
            return 0;
        }

        let instance = match self.instance() {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!("Failed to get instance for queue processing: {e}");
                return batches.len(); // All batches failed
            }
        };

        let mut failures = 0;
        for (peer, entry_ids) in batches {
            // Fetch entries from backend
            let mut entries = Vec::with_capacity(entry_ids.len());
            for (entry_id, _tree_id) in &entry_ids {
                match instance.backend().get(entry_id).await {
                    Ok(entry) => entries.push(entry),
                    Err(e) => {
                        tracing::warn!("Failed to fetch entry {entry_id} for peer {peer}: {e}");
                    }
                }
            }

            if entries.is_empty() {
                continue;
            }

            // Send batched entries to peer
            if let Err(e) = self.send_to_peer(&peer, entries.clone()).await {
                let now_ms = instance.clock().now_millis();
                self.add_to_retry_queue(peer, entries, e, now_ms);
                failures += 1;
            }
        }
        failures
    }

    /// Process retry queue with exponential backoff
    async fn process_retry_queue(&mut self) {
        let now_ms = self.instance().map(|i| i.clock().now_millis()).unwrap_or(0);
        let mut still_failed = Vec::new();

        // Take the retry queue to avoid borrowing issues
        let retry_queue = std::mem::take(&mut self.retry_queue);

        // Process entries that are ready for retry
        for mut entry in retry_queue {
            // Backoff in milliseconds: 2^attempts * 1000ms, max 64 seconds
            let backoff_ms = 2u64.pow(entry.attempts.min(6)) * 1000;
            let elapsed_ms = now_ms.saturating_sub(entry.last_attempt_ms);

            if elapsed_ms >= backoff_ms {
                // Try sending again
                if let Err(_e) = self.send_to_peer(&entry.peer, entry.entries.clone()).await {
                    entry.attempts += 1;
                    entry.last_attempt_ms = now_ms;

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

    /// Flush retry queue immediately, ignoring backoff timers.
    /// Returns the number of entries that still failed after retry.
    async fn flush_retry_queue(&mut self) -> usize {
        let mut still_failed = Vec::new();
        let now_ms = self.instance().map(|i| i.clock().now_millis()).unwrap_or(0);

        // Take the retry queue to process
        let retry_queue = std::mem::take(&mut self.retry_queue);

        // Try sending each entry immediately (ignore backoff)
        for mut retry_entry in retry_queue {
            if let Err(_e) = self
                .send_to_peer(&retry_entry.peer, retry_entry.entries.clone())
                .await
            {
                retry_entry.attempts += 1;
                retry_entry.last_attempt_ms = now_ms;

                if retry_entry.attempts < 10 {
                    still_failed.push(retry_entry);
                } else {
                    tracing::error!(
                        "Giving up on sending to {} after 10 attempts",
                        retry_entry.peer
                    );
                }
            }
        }

        let failed_count = still_failed.len();
        self.retry_queue = still_failed;
        failed_count
    }

    /// Perform periodic sync with all active peers
    async fn periodic_sync_all_peers(&self) {
        // Periodic sync triggered

        // Get all peers from sync tree
        let peers = match self.get_sync_tree().await {
            Ok(sync_tree) => match sync_tree.new_transaction().await {
                Ok(txn) => match PeerManager::new(&txn).list_peers().await {
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
                    // Skip sync if we can't create transaction
                    return;
                }
            },
            Err(_) => {
                // Skip sync if we can't get sync tree
                return;
            }
        };

        // Now sync with peers (transaction is dropped, so no Send issues)
        for peer_info in peers {
            if peer_info.status == PeerStatus::Active
                && let Err(e) = self.sync_with_peer(&peer_info.id).await
            {
                // Log individual peer sync failure but continue with others
                tracing::error!("Periodic sync failed with {}: {e}", peer_info.id);
            }
        }
    }

    /// Sync with a specific peer (bidirectional)
    async fn sync_with_peer(&self, peer_id: &PeerId) -> Result<()> {
        async move {
            info!(peer = %peer_id, "Starting peer synchronization");

            // Get peer info and tree list from sync tree (extract and drop transaction before await)
            let (address, sync_trees) = {
                let sync_tree = self.get_sync_tree().await?;
                let txn = sync_tree.new_transaction().await?;
                let peer_manager = PeerManager::new(&txn);

                let peer_info = peer_manager
                    .get_peer_info(peer_id)
                    .await?
                    .ok_or_else(|| SyncError::PeerNotFound(peer_id.to_string()))?;

                let address = peer_info
                    .addresses
                    .first()
                    .ok_or_else(|| SyncError::Network("No addresses found for peer".to_string()))?
                    .clone();

                // Find all trees that sync with this peer from sync tree
                let sync_trees = peer_manager.get_peer_trees(peer_id).await?;

                (address, sync_trees)
            }; // Transaction is dropped here

            if sync_trees.is_empty() {
                debug!(peer = %peer_id, "No trees configured for sync with peer");
                return Ok(()); // No trees to sync
            }

            info!(peer = %peer_id, tree_count = sync_trees.len(), "Synchronizing trees with peer");

            for tree_id_str in sync_trees {
                // Convert string ID to entry ID
                let tree_id = ID::from(tree_id_str.as_str());
                if let Err(e) = self.sync_tree_with_peer(peer_id, &tree_id, &address).await {
                    // Log tree sync failure but continue with other trees
                    tracing::error!("Failed to sync tree {tree_id} with peer {peer_id}: {e}");
                }
            }

            info!(peer = %peer_id, "Completed peer synchronization");
            Ok(())
        }
        .instrument(info_span!("sync_with_peer", peer = %peer_id))
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
        peer_id: &PeerId,
        tree_id: &ID,
        address: &Address,
    ) -> Result<()> {
        async move {
            trace!(peer = %peer_id, tree = %tree_id, "Starting unified tree synchronization");

            // Get our tips for this tree (empty if tree doesn't exist)
            let instance = self.instance()?;
            let our_tips = instance
                .backend()
                .get_tips(tree_id)
                .await
                .map_err(|e| SyncError::BackendError(format!("Failed to get local tips: {e}")))?;

            // Get our device public key for automatic peer tracking
            let our_device_pubkey = Some(instance.device_id_string());

            debug!(peer = %peer_id, tree = %tree_id, our_tips = our_tips.len(), "Sending sync tree request");

            // Send unified sync request
            let request = SyncRequest::SyncTree(SyncTreeRequest {
                tree_id: tree_id.clone(),
                our_tips,
                peer_pubkey: our_device_pubkey,
                requesting_key: None, // TODO: Add auth support for background sync
                requesting_key_name: None,
                requested_permission: None,
            });

            let response = self.transport_manager.send_request(address, &request).await?;

            match response {
                SyncResponse::Bootstrap(bootstrap_response) => {
                    info!(peer = %peer_id, tree = %tree_id, entry_count = bootstrap_response.all_entries.len() + 1, "Received bootstrap response");
                    self.handle_bootstrap_response(bootstrap_response).await?;
                }
                SyncResponse::Incremental(incremental_response) => {
                    debug!(peer = %peer_id, tree = %tree_id,
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

            trace!(peer = %peer_id, tree = %tree_id, "Completed unified tree synchronization");
            Ok(())
        }
        .instrument(info_span!("sync_tree", peer = %peer_id, tree = %tree_id))
        .await
    }

    /// Check peer connections and attempt reconnection
    async fn check_peer_connections(&mut self) {
        // For now, this is a placeholder
        // In the future, we could implement connection health checks
        // and automatic reconnection logic here
    }

    /// Start the sync server on specified or all transports
    async fn start_server(&mut self, name: Option<&str>) -> Result<()> {
        // Create a sync handler with instance access and sync tree ID
        let handler = Arc::new(SyncHandlerImpl::new(
            self.instance()?,
            self.sync_tree_id.clone(),
        ));

        match name {
            Some(name) => {
                // Start server on specific transport
                self.transport_manager.start_server(name, handler).await?;
                tracing::info!("Sync server started for transport {name}");
            }
            None => {
                // Start servers on all transports
                self.transport_manager.start_all_servers(handler).await?;
                tracing::info!("Sync servers started for all transports");
            }
        }

        Ok(())
    }

    /// Stop the sync server on specified or all transports
    async fn stop_server(&mut self, name: Option<&str>) -> Result<()> {
        match name {
            Some(name) => {
                self.transport_manager.stop_server(name).await?;
                tracing::info!("Sync server stopped for transport {name}");
            }
            None => {
                self.transport_manager.stop_all_servers().await?;
                tracing::info!("All sync servers stopped");
            }
        }
        Ok(())
    }
}
