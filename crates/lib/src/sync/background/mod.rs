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
use tracing::{Instrument, debug, info, info_span, trace, warn};

use super::{
    error::SyncError,
    handler::SyncHandlerImpl,
    peer_manager::PeerManager,
    peer_state::PeerStates,
    peer_types::{Address, PeerId, PeerStatus},
    protocol::{SyncRequest, SyncRequestAuth, SyncResponse, SyncTreeRequest},
    queue::SyncQueue,
    transport_manager::TransportManager,
    transports::SyncTransport,
};
use crate::{
    Database, Error, Instance, Result, WeakInstance,
    auth::crypto::PublicKey,
    entry::{Entry, ID},
    store::DocStore,
};

mod conn;

/// Commands that can be sent to the background sync engine
#[allow(clippy::large_enum_variant)]
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
        response: oneshot::Sender<Result<PublicKey>>, // Returns peer pubkey
    },

    // Request/Response operations
    /// Send a sync request and get response
    SendRequest {
        address: Address,
        request: Box<SyncRequest>,
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

    // Per-peer liveness state (shared with Sync frontend, written only here)
    peer_state: Arc<PeerStates>,

    // Retry queue for failed sends
    retry_queue: Vec<RetryEntry>,

    // Communication
    command_rx: mpsc::Receiver<SyncCommand>,
}

/// How many peers may be synced at once, and so how many outbound connections a
/// periodic sync can hold open. Bounds a large peer set; well above the point
/// where the work stops being dominated by any single unreachable peer.
const MAX_CONCURRENT_PEER_SYNCS: usize = 16;

/// Bound on a single registration handshake while a route is being selected.
///
/// It covers the handshake only. The transfer that follows runs outside it, so
/// a legitimately large exchange is never cut short by a connect-shaped deadline
/// — the transports impose their own read-silence timeouts for that.
const ADDRESS_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(30);

impl BackgroundSync {
    /// Start the background sync engine and return a command sender.
    ///
    /// The engine starts with no transports registered. Use `AddTransport`
    /// commands to add transports after starting.
    pub fn start(
        instance: Instance,
        sync_tree_id: ID,
        queue: Arc<SyncQueue>,
        peer_state: Arc<PeerStates>,
    ) -> mpsc::Sender<SyncCommand> {
        let (tx, rx) = mpsc::channel(100);

        let background = Self {
            transport_manager: TransportManager::new(),
            instance: instance.downgrade(),
            sync_tree_id,
            queue,
            peer_state,
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

    /// Collect what a handshake needs so it can run off the command loop.
    ///
    /// Everything gathered here is either a cheap handle or local engine state
    /// (the listen addresses), so the borrow ends before the handshake starts.
    fn handshake_ctx(&self, address: &Address) -> Result<conn::HandshakeCtx> {
        let transport = self
            .transport_manager
            .handle_for_address(address)
            .ok_or_else(|| SyncError::NoTransportForAddress {
                address: address.clone(),
            })?;
        Ok(conn::HandshakeCtx {
            transport,
            instance: self.instance.clone(),
            sync_tree_id: self.sync_tree_id.clone(),
            listen_addresses: self
                .transport_manager
                .get_all_server_addresses()
                .into_iter()
                .map(|(transport_type, addr)| Address {
                    transport_type,
                    address: addr,
                })
                .collect(),
        })
    }

    /// Get the sync tree for accessing peer data
    pub(super) async fn get_sync_tree(&self) -> Result<Database> {
        // Load sync tree with the device key
        let instance = self.instance()?;
        let signing_key = instance.signing_key()?.clone();
        Ok(Database::open(&instance, &self.sync_tree_id)
            .await?
            .with_key(signing_key))
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
                self.transport_manager.add(&name, Arc::from(transport));
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
                // Served off the loop, for the same reason as SendRequest
                // below: a handshake is aimed at a peer this engine has never
                // reached, which is precisely the peer most likely not to be
                // there. Awaiting it inline hands the whole engine to a
                // stranger for a full connect deadline.
                match self.handshake_ctx(&address) {
                    Ok(ctx) => {
                        tokio::spawn(async move {
                            let _ = response.send(conn::run_handshake(ctx, address).await);
                        });
                    }
                    Err(e) => {
                        let _ = response.send(Err(e));
                    }
                }
            }

            SyncCommand::SendRequest {
                address,
                request,
                response,
            } => {
                // Serve this from a task of its own rather than inline.
                //
                // Commands are handled one at a time, so awaiting a request
                // here holds the engine for as long as the peer takes to
                // answer — and a peer that accepts a connection and then goes
                // quiet takes the full transport deadline. Every other
                // request, the queue drain and the retry queue all wait behind
                // it, so a deployment carrying a few retired peers starves the
                // live ones. That presents as "everything times out", which
                // reads as a local fault rather than as one absent peer.
                //
                // The transport handle is owned, so the task borrows nothing
                // from the engine and the loop is free immediately.
                match self.transport_manager.handle_for_address(&address) {
                    Some(transport) => {
                        tokio::spawn(async move {
                            let result = transport.send_request(&address, &request).await;
                            let _ = response.send(result);
                        });
                    }
                    None => {
                        let _ =
                            response.send(Err(SyncError::NoTransportForAddress { address }.into()));
                    }
                }
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

    /// Race bounded registration handshakes and return the first usable route
    /// **to `expected`**.
    ///
    /// For each address a transport handle is obtained via
    /// [`TransportManager::handle_for_address`]. One task is spawned per
    /// address, each subject to [`ADDRESS_ATTEMPT_TIMEOUT`]. Remaining
    /// handshakes are **not** cancelled — they continue running so that
    /// additional addresses can be registered by the remote peer. The caller
    /// performs the real operation once on the selected route, outside this
    /// timeout.
    ///
    /// A handshake identifies who actually answered, and only a route that
    /// answers as `expected` is selected. A peer's address list only ever grows,
    /// so a stale entry can be reoccupied by an unrelated node; that node
    /// completes a handshake perfectly well. Taking it as the route would send
    /// this peer's traffic to a stranger and, worse, stop the working address
    /// behind it from ever being tried — the exact failure this whole path
    /// exists to remove.
    ///
    /// If no address has a matching transport an error is returned. If all
    /// tasks fail the last error is returned.
    async fn select_route(
        &self,
        addresses: &[Address],
        expected: &PublicKey,
    ) -> Result<(std::sync::Arc<dyn SyncTransport>, Address)> {
        // Collect transport handles upfront (before any spawn).
        let mut tasks: Vec<(std::sync::Arc<dyn SyncTransport>, Address)> = Vec::new();
        for addr in addresses {
            if let Some(transport) = self.transport_manager.handle_for_address(addr) {
                tasks.push((transport, addr.clone()));
            }
        }

        if tasks.is_empty() {
            return Err(
                SyncError::InvalidAddress("No matching transport for any address".into()).into(),
            );
        }

        let (tx, mut rx) = mpsc::channel(tasks.len());

        for (transport, addr) in tasks {
            let tx = tx.clone();
            let mut ctx = self.handshake_ctx(&addr)?;
            ctx.transport = Arc::clone(&transport);
            let addr_info = addr.clone();
            tokio::spawn(async move {
                let result = tokio::time::timeout(
                    ADDRESS_ATTEMPT_TIMEOUT,
                    conn::run_handshake(ctx, addr.clone()),
                )
                .await;
                let result = match result {
                    Ok(Ok(answered)) => Ok((transport, addr, answered)),
                    Ok(Err(error)) => Err(error),
                    Err(_) => {
                        warn!(
                            address = ?addr_info,
                            timeout = ?ADDRESS_ATTEMPT_TIMEOUT,
                            "Address attempt timed out",
                        );
                        Err(SyncError::Network(format!(
                            "Address attempt timed out after {ADDRESS_ATTEMPT_TIMEOUT:?}"
                        ))
                        .into())
                    }
                };
                let _ = tx.send(result).await;
            });
        }
        drop(tx);

        let mut last_err = None;
        while let Some(result) = rx.recv().await {
            match result {
                Ok((transport, addr, answered)) if &answered == expected => {
                    return Ok((transport, addr));
                }
                Ok((_, addr, answered)) => {
                    warn!(
                        address = ?addr,
                        expected = %expected,
                        answered = %answered,
                        "Address answered as a different peer; not selecting it as the route"
                    );
                    last_err = Some(
                        SyncError::HandshakeFailed(format!(
                            "{addr:?} answered as {answered}, not {expected}"
                        ))
                        .into(),
                    );
                }
                Err(e) => last_err = Some(e),
            }
        }

        Err(last_err.expect("at least one task was spawned"))
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
        // Get peer addresses from sync tree (extract and drop transaction before await)
        let (addresses, request) = {
            let sync_tree = self.get_sync_tree().await?;
            let txn = sync_tree.new_transaction().await?;
            let peer_info = PeerManager::new(&txn)
                .get_peer_info(peer.public_key())
                .await?
                .ok_or_else(|| SyncError::PeerNotFound(peer.to_string()))?;

            let addresses = peer_info.addresses.clone();
            let request = SyncRequest::SendEntries(entries);
            (addresses, request)
        }; // Transaction is dropped here

        let (transport, address) = self.select_route(&addresses, peer.public_key()).await?;
        let response = transport.send_request(&address, &request).await?;

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

        // Sync peers concurrently (the transaction is dropped, so no Send
        // issues). A round is I/O-bound and dominated by peers that are down:
        // done serially, one unreachable peer delays every peer behind it by a
        // full connect timeout, so the round degrades with the number of dead
        // peers rather than with the work there is to do. Concurrently, a round
        // costs about as long as its slowest peer.
        //
        // Bounded so a large peer set can't open an unbounded number of
        // connections at once. These futures are polled on this task rather
        // than spawned, so they need no `Send`/`'static` bounds.
        use futures_util::stream::StreamExt;
        futures_util::stream::iter(
            peers
                .into_iter()
                .filter(|p| p.status == PeerStatus::Active)
                .map(|peer_info| async move {
                    if let Err(e) = self.sync_with_peer(&peer_info.id).await {
                        // Log individual peer sync failure but continue with others
                        tracing::error!("Periodic sync failed with {}: {e}", peer_info.id);
                    }
                }),
        )
        .buffer_unordered(MAX_CONCURRENT_PEER_SYNCS)
        .collect::<()>()
        .await;
    }

    /// Sync with a specific peer (bidirectional)
    async fn sync_with_peer(&self, peer_id: &PeerId) -> Result<()> {
        async move {
            info!(peer = %peer_id, "Starting peer synchronization");

            // Get peer addresses and tree list from sync tree (extract and
            // drop transaction before any network I/O).
            let (addresses, sync_trees) = {
                let sync_tree = self.get_sync_tree().await?;
                let txn = sync_tree.new_transaction().await?;
                let peer_manager = PeerManager::new(&txn);

                let peer_info = peer_manager
                    .get_peer_info(peer_id.public_key())
                    .await?
                    .ok_or_else(|| SyncError::PeerNotFound(peer_id.to_string()))?;

                let addresses = peer_info.addresses.clone();

                // Find all trees that sync with this peer from sync tree
                let sync_trees = peer_manager.get_peer_trees(peer_id.public_key()).await?;

                (addresses, sync_trees)
            }; // Transaction is dropped here

            if sync_trees.is_empty() {
                debug!(peer = %peer_id, "No trees configured for sync with peer");
                return Ok(()); // No trees to sync
            }

            // Race bounded handshakes to select one route. Other successful
            // handshakes may finish registration, but only the selected route
            // continues into the tree exchange.
            let (_transport, address) = self.select_route(&addresses, peer_id.public_key()).await?;

            info!(peer = %peer_id, tree_count = sync_trees.len(), "Synchronizing trees with peer");

            let tree_count = sync_trees.len();
            let mut synced_any = false;
            for (index, tree_id) in sync_trees.iter().enumerate() {
                let Err(e) = self.sync_tree_with_peer(peer_id, tree_id, &address).await else {
                    synced_any = true;
                    continue;
                };

                tracing::error!("Failed to sync tree {tree_id} with peer {peer_id}: {e}");

                // A failure that is about the peer rather than about this tree
                // ends the walk. Every remaining tree would fail the same way,
                // each paying a full deadline to establish what this one just
                // established, so a peer that is down costs one round trip
                // rather than one per tree registered against it.
                //
                // Any other failure is about this tree alone: the peer answered,
                // so the trees behind it are still worth attempting. Keeping
                // that distinction is what lets the walk stop early without a
                // single broken tree stalling every tree behind it.
                if e.is_network_error() {
                    debug!(
                        peer = %peer_id,
                        skipped = tree_count - index - 1,
                        "Peer stopped answering; ending the tree walk"
                    );
                    break;
                }
            }

            // A round that moved at least one tree is the peer answering, which
            // is the fact `SyncStatus.last_sync` reports. A round in which every
            // tree failed is not, even though the walk itself returns `Ok`.
            if synced_any && let Ok(instance) = self.instance() {
                self.peer_state
                    .record_success(peer_id, instance.clock().now_millis());
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
                .snapshot(tree_id)
                .await
                .map_err(|e| SyncError::BackendError(format!("Failed to get local tips: {e}")))?;

            // Get our device public key for automatic peer tracking
            let our_device_pubkey = Some(instance.id());

            debug!(peer = %peer_id, tree = %tree_id, our_tips = our_tips.len(), "Sending sync tree request");

            // Send unified sync request, signed so the peer can authorize the pull
            let auth = SyncRequestAuth::sign(
                instance.signing_key()?,
                peer_id.public_key(),
                tree_id,
                &our_tips,
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::{
        Error,
        sync::error::{SyncError, TimeoutPhase},
    };

    /// The rule the tree walk applies to a failed tree: end the peer's round, or
    /// carry on to the trees behind it.
    fn ends_the_walk(err: SyncError) -> bool {
        Error::from(err).is_network_error()
    }

    /// The peer itself stopped answering, so every remaining tree would pay a
    /// full deadline to establish what this one just established.
    #[test]
    fn a_peer_that_stopped_answering_ends_the_walk() {
        assert!(ends_the_walk(SyncError::ConnectionFailed {
            address: "peer:8080".to_string(),
            reason: "connection refused".to_string(),
        }));
        assert!(ends_the_walk(SyncError::Timeout {
            address: "peer:8080".to_string(),
            phase: TimeoutPhase::Connect,
            elapsed: Duration::from_secs(10),
        }));
        assert!(ends_the_walk(SyncError::Network(
            "connection reset by peer".to_string()
        )));
    }

    /// A peer that connected and then went quiet is still a peer that stopped
    /// answering. The phase says whether it proved itself reachable, which is
    /// worth reporting, but it does not make the remaining trees worth
    /// attempting this round.
    #[test]
    fn a_peer_that_went_quiet_after_connecting_also_ends_the_walk() {
        assert!(ends_the_walk(SyncError::Timeout {
            address: "peer:8080".to_string(),
            phase: TimeoutPhase::Request,
            elapsed: Duration::from_secs(30),
        }));
    }

    /// The peer answered in every one of these: the failure is about the one
    /// tree, so the trees behind it are still worth attempting. Ending the walk
    /// on these would let a single misconfigured tree stall every tree
    /// registered behind it against a peer that is perfectly healthy.
    #[test]
    fn a_failure_about_one_tree_does_not_end_the_walk() {
        assert!(!ends_the_walk(SyncError::PermissionDenied(
            "key has no write access".to_string()
        )));
        assert!(!ends_the_walk(SyncError::AuthenticationFailed(
            "signature did not verify".to_string()
        )));
        assert!(!ends_the_walk(SyncError::UnexpectedResponse {
            expected: "SyncResponse",
            actual: "Error".to_string(),
        }));
        assert!(!ends_the_walk(SyncError::SyncProtocolError(
            "malformed tip set".to_string()
        )));
    }
}
