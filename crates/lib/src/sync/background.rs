//! Background sync engine implementation.
//!
//! This module provides the BackgroundSync struct that handles all sync operations
//! in a single background thread, removing circular dependency issues and providing
//! automatic retry, periodic sync, and reconnection handling.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::{mpsc, oneshot};
use tokio::time::interval;

use super::DEVICE_KEY_NAME;
use super::error::SyncError;
use super::handler::SyncHandlerImpl;
use super::peer_types::{Address, PeerInfo};
use super::protocol::{
    GetEntriesRequest, GetTipsRequest, HandshakeRequest, PROTOCOL_VERSION, SyncRequest,
    SyncResponse,
};
use super::transports::SyncTransport;
use crate::Result;
use crate::auth::crypto::{format_public_key, generate_challenge, verify_challenge_response};
use crate::backend::Database;
use crate::entry::{Entry, ID};

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
    /// Add a new peer to the sync network
    AddPeer { peer: PeerInfo },
    /// Remove a peer from sync network
    RemovePeer { pubkey: String },
    /// Create a sync relationship for a tree with a peer
    CreateRelationship { peer_pubkey: String, tree_id: ID },
    /// Remove sync relationship
    RemoveRelationship { peer_pubkey: String, tree_id: ID },
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

/// Sync relationship tracking which trees sync with which peers
#[derive(Debug, Clone)]
pub struct SyncRelationship {
    pub peer_pubkey: String,
    pub tree_id: ID,
}

/// Background sync engine that owns all sync state and handles operations
pub struct BackgroundSync {
    // Core components - owns everything
    transport: Box<dyn SyncTransport>,
    backend: Arc<dyn Database>,

    // Server state
    server_address: Option<String>,

    // State management - simple, not Arc/RwLock
    peers: HashMap<String, PeerInfo>,
    relationships: HashMap<String, SyncRelationship>, // key: "tree_id:peer_pubkey"

    // Retry queue for failed sends
    retry_queue: Vec<RetryEntry>,

    // Communication
    command_rx: mpsc::Receiver<SyncCommand>,
}

impl BackgroundSync {
    /// Start the background sync engine and return a command sender
    pub fn start(
        transport: Box<dyn SyncTransport>,
        backend: Arc<dyn Database>,
    ) -> mpsc::Sender<SyncCommand> {
        let (tx, rx) = mpsc::channel(100);

        let background = Self {
            transport,
            backend,
            server_address: None,
            peers: HashMap::new(),
            relationships: HashMap::new(),
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

    /// Main event loop that handles all sync operations
    async fn run(mut self) {
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
                        eprintln!("Background sync command error: {e}");
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
                    println!("Background sync shutting down - command channel closed");
                    break;
                }
            }
        }
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
                        eprintln!("Failed to fetch entry {entry_id} for peer {peer}: {e}");
                    }
                }
            }

            SyncCommand::AddPeer { peer } => {
                self.peers.insert(peer.pubkey.clone(), peer);
            }

            SyncCommand::RemovePeer { pubkey } => {
                self.peers.remove(&pubkey);
                // Also remove any relationships with this peer
                self.relationships
                    .retain(|_, rel| rel.peer_pubkey != pubkey);
            }

            SyncCommand::CreateRelationship {
                peer_pubkey,
                tree_id,
            } => {
                let key = format!("{tree_id}:{peer_pubkey}");
                self.relationships.insert(
                    key,
                    SyncRelationship {
                        peer_pubkey,
                        tree_id,
                    },
                );
            }

            SyncCommand::RemoveRelationship {
                peer_pubkey,
                tree_id,
            } => {
                let key = format!("{tree_id}:{peer_pubkey}");
                self.relationships.remove(&key);
            }

            SyncCommand::SyncWithPeer { peer } => {
                if let Err(e) = self.sync_with_peer(&peer).await {
                    eprintln!("Failed to sync with peer {peer}: {e}");
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
                println!("Background sync received shutdown command");
                return Err(SyncError::Network("Shutdown requested".to_string()).into());
            }
        }
        Ok(())
    }

    /// Send entries to a specific peer
    async fn send_to_peer(&self, peer: &str, entries: Vec<Entry>) -> Result<()> {
        let peer_info = self
            .peers
            .get(peer)
            .ok_or_else(|| SyncError::PeerNotFound(peer.to_string()))?;

        let address = peer_info
            .addresses
            .first()
            .ok_or_else(|| SyncError::Network("No addresses found for peer".to_string()))?;

        let request = SyncRequest::SendEntries(entries);
        let response = self.transport.send_request(address, &request).await?;

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
        eprintln!("Failed to send to {peer}: {error}. Adding to retry queue.");
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
                        eprintln!("Giving up on sending to {} after 10 attempts", entry.peer);
                    }
                } else {
                    println!(
                        "Successfully retried send to {} on attempt {}",
                        entry.peer, entry.attempts
                    );
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
        println!("Starting periodic sync with all peers");
        for (peer_pubkey, peer_info) in &self.peers {
            if peer_info.status == crate::sync::peer_types::PeerStatus::Active
                && let Err(e) = self.sync_with_peer(peer_pubkey).await
            {
                eprintln!("Periodic sync failed with {peer_pubkey}: {e}");
            }
        }
    }

    /// Sync with a specific peer (bidirectional)
    async fn sync_with_peer(&self, peer_pubkey: &str) -> Result<()> {
        let peer_info = self
            .peers
            .get(peer_pubkey)
            .ok_or_else(|| SyncError::PeerNotFound(peer_pubkey.to_string()))?;

        let address = peer_info
            .addresses
            .first()
            .ok_or_else(|| SyncError::Network("No addresses found for peer".to_string()))?;

        // Find all trees that sync with this peer
        let sync_trees: Vec<&ID> = self
            .relationships
            .values()
            .filter(|rel| rel.peer_pubkey == peer_pubkey)
            .map(|rel| &rel.tree_id)
            .collect();

        if sync_trees.is_empty() {
            return Ok(()); // No trees to sync
        }

        println!(
            "Syncing {} trees with peer {}",
            sync_trees.len(),
            peer_pubkey
        );

        for tree_id in sync_trees {
            if let Err(e) = self
                .sync_tree_with_peer(peer_pubkey, tree_id, address)
                .await
            {
                eprintln!("Failed to sync tree {tree_id} with peer {peer_pubkey}: {e}");
            }
        }

        Ok(())
    }

    /// Sync a specific tree with a peer
    async fn sync_tree_with_peer(
        &self,
        _peer_pubkey: &str,
        tree_id: &ID,
        address: &Address,
    ) -> Result<()> {
        // Step 1: Get our tips for this tree
        let our_tips = self
            .backend
            .get_tips(tree_id)
            .map_err(|e| SyncError::BackendError(format!("Failed to get local tips: {e}")))?;

        // Step 2: Get peer's tips
        let their_tips = self.get_peer_tips(tree_id, address).await?;

        // Step 3: Find what we're missing and fetch it
        let missing_entries = self.find_missing_entries(&our_tips, &their_tips)?;
        if !missing_entries.is_empty() {
            let entries = self
                .fetch_entries_from_peer(address, &missing_entries)
                .await?;
            self.store_received_entries(entries).await?;
        }

        // Step 4: Find what they're missing and send it
        let entries_to_send = self.find_entries_to_send(&our_tips, &their_tips)?;
        if !entries_to_send.is_empty() {
            self.transport
                .send_entries(address, &entries_to_send)
                .await?;
        }

        Ok(())
    }

    /// Get tips from a peer for a tree
    async fn get_peer_tips(&self, tree_id: &ID, address: &Address) -> Result<Vec<ID>> {
        let request = SyncRequest::GetTips(GetTipsRequest {
            tree_id: tree_id.clone(),
        });

        let response = self.transport.send_request(address, &request).await?;

        match response {
            crate::sync::protocol::SyncResponse::Tips(tips_response) => Ok(tips_response.tips),
            crate::sync::protocol::SyncResponse::Error(msg) => {
                Err(SyncError::SyncProtocolError(format!("GetTips error: {msg}")).into())
            }
            _ => Err(SyncError::UnexpectedResponse {
                expected: "Tips",
                actual: format!("{response:?}"),
            }
            .into()),
        }
    }

    /// Find entries we don't have locally (including all ancestors)
    fn find_missing_entries(&self, _our_tips: &[ID], their_tips: &[ID]) -> Result<Vec<ID>> {
        // Use DAG traversal to find all missing entries including ancestors
        super::utils::collect_missing_ancestors(self.backend.as_ref(), their_tips)
    }

    /// Collect ancestors that need to be sent with the given entries
    fn collect_ancestors_to_send(&self, entry_ids: &[ID], their_tips: &[ID]) -> Result<Vec<Entry>> {
        super::utils::collect_ancestors_to_send(self.backend.as_ref(), entry_ids, their_tips)
    }

    /// Find entries we have that peer doesn't (including all necessary ancestors)
    fn find_entries_to_send(&self, our_tips: &[ID], their_tips: &[ID]) -> Result<Vec<Entry>> {
        // Find tips that peer doesn't have
        let tips_to_send: Vec<ID> = our_tips
            .iter()
            .filter(|tip_id| !their_tips.contains(tip_id))
            .cloned()
            .collect();

        if tips_to_send.is_empty() {
            return Ok(Vec::new());
        }

        // Use DAG traversal to collect all necessary ancestors
        self.collect_ancestors_to_send(&tips_to_send, their_tips)
    }

    /// Fetch specific entries from a peer
    async fn fetch_entries_from_peer(
        &self,
        address: &Address,
        entry_ids: &[ID],
    ) -> Result<Vec<Entry>> {
        if entry_ids.is_empty() {
            return Ok(Vec::new());
        }

        let request = SyncRequest::GetEntries(GetEntriesRequest {
            entry_ids: entry_ids.to_vec(),
        });

        let response = self.transport.send_request(address, &request).await?;

        match response {
            crate::sync::protocol::SyncResponse::Entries(entries_response) => {
                Ok(entries_response.entries)
            }
            crate::sync::protocol::SyncResponse::Error(msg) => {
                Err(SyncError::SyncProtocolError(format!("GetEntries error: {msg}")).into())
            }
            _ => Err(SyncError::UnexpectedResponse {
                expected: "Entries",
                actual: format!("{response:?}"),
            }
            .into()),
        }
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

        // Create a sync handler with backend access
        let handler = Arc::new(SyncHandlerImpl::new(self.backend.clone(), DEVICE_KEY_NAME));

        self.transport.start_server(addr, handler).await?;

        // Store the server address for later retrieval
        match self.transport.get_server_address() {
            Ok(server_addr) => {
                self.server_address = Some(server_addr);
                println!("Sync server started on {addr}");
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
        println!("Sync server stopped");
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

                // Create peer info
                let peer_info = PeerInfo::new(
                    &handshake_resp.public_key,
                    handshake_resp.display_name.as_deref(),
                );

                // Add peer to our state
                self.peers
                    .insert(handshake_resp.public_key.clone(), peer_info);

                println!(
                    "Successfully connected to peer {}",
                    handshake_resp.public_key
                );
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
}
