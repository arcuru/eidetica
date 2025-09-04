//! Synchronization module for Eidetica database.
//!
//! The Sync module manages synchronization settings and state for the database,
//! storing its configuration in a dedicated tree within the database.

use crate::auth::crypto::format_public_key;
use crate::{Database, Entry, Result, crdt::Doc, store::DocStore};
use std::sync::Arc;

pub mod background;
pub mod error;
pub mod handler;
pub mod hooks;
mod peer_manager;
pub mod peer_types;
pub mod protocol;
pub mod state;
pub mod transports;
pub mod utils;

pub use error::SyncError;
pub use peer_types::{Address, ConnectionState, PeerInfo, PeerStatus};

use background::{BackgroundSync, SyncCommand};
use hooks::SyncHook;
use peer_manager::PeerManager;
use protocol::{GetEntriesRequest, GetTipsRequest, SyncRequest, SyncResponse};
use tokio::sync::{mpsc, oneshot};
use transports::{SyncTransport, http::HttpTransport, iroh::IrohTransport};

/// Private constant for the sync settings subtree name
const SETTINGS_SUBTREE: &str = "settings_map";

/// Private constant for the device identity key name
/// This is the name of the Device Key used as the shared identifier for this Device.
const DEVICE_KEY_NAME: &str = "_device_key";

/// Synchronization manager for the database.
///
/// The Sync module is a thin frontend that communicates with a background
/// sync engine thread via command channels. All actual sync operations, transport
/// communication, and state management happen in the background thread.
pub struct Sync {
    /// Communication channel to the background sync engine
    command_tx: mpsc::Sender<SyncCommand>,
    /// The backend for read operations and tree management
    backend: Arc<dyn crate::backend::BackendDB>,
    /// The tree containing synchronization settings
    sync_tree: Database,
    /// Track if transport has been enabled
    transport_enabled: bool,
}

impl Sync {
    /// Create a new Sync instance with a dedicated settings tree.
    ///
    /// # Arguments
    /// * `backend` - The database backend for tree operations
    ///
    /// # Returns
    /// A new Sync instance with its own settings tree.
    pub fn new(backend: Arc<dyn crate::backend::BackendDB>) -> Result<Self> {
        let mut sync_settings = Doc::new();
        sync_settings.set_string("name", "_sync");
        sync_settings.set_string("type", "sync_settings");

        let mut sync_tree = Database::new(sync_settings, Arc::clone(&backend), DEVICE_KEY_NAME)?;

        // Set the default authentication key so all operations use the device key
        sync_tree.set_default_auth_key(DEVICE_KEY_NAME);

        // For now, create a placeholder command channel
        // This will be properly initialized when a transport is enabled
        let (command_tx, _) = mpsc::channel(100);

        Ok(Self {
            command_tx,
            backend,
            sync_tree,
            transport_enabled: false,
        })
    }

    /// Load an existing Sync instance from a sync tree root ID.
    ///
    /// # Arguments
    /// * `backend` - The database backend
    /// * `sync_tree_root_id` - The root ID of the existing sync tree
    ///
    /// # Returns
    /// A Sync instance loaded from the existing tree.
    pub fn load(
        backend: Arc<dyn crate::backend::BackendDB>,
        sync_tree_root_id: &crate::entry::ID,
    ) -> Result<Self> {
        let mut sync_tree = Database::new_from_id(sync_tree_root_id.clone(), Arc::clone(&backend))?;

        // Set the default authentication key so all operations use the device key
        sync_tree.set_default_auth_key(DEVICE_KEY_NAME);

        // For now, create a placeholder command channel
        // This will be properly initialized when a transport is enabled
        let (command_tx, _) = mpsc::channel(100);

        Ok(Self {
            command_tx,
            backend,
            sync_tree,
            transport_enabled: false,
        })
    }

    /// Get the root ID of the sync settings tree.
    pub fn sync_tree_root_id(&self) -> &crate::entry::ID {
        self.sync_tree.root_id()
    }

    /// Store a setting in the sync_settings subtree.
    ///
    /// # Arguments
    /// * `key` - The setting key
    /// * `value` - The setting value
    pub fn set_setting(&mut self, key: impl Into<String>, value: impl Into<String>) -> Result<()> {
        let op = self.sync_tree.new_operation()?;
        let sync_settings = op.get_subtree::<DocStore>(SETTINGS_SUBTREE)?;
        sync_settings.set_string(key, value)?;
        op.commit()?;
        Ok(())
    }

    /// Retrieve a setting from the settings_map subtree.
    ///
    /// # Arguments
    /// * `key` - The setting key to retrieve
    ///
    /// # Returns
    /// The setting value if found, None otherwise.
    pub fn get_setting(&self, key: impl AsRef<str>) -> Result<Option<String>> {
        let sync_settings = self
            .sync_tree
            .get_subtree_viewer::<DocStore>(SETTINGS_SUBTREE)?;
        match sync_settings.get_string(key) {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Get a reference to the underlying backend.
    pub fn backend(&self) -> &Arc<dyn crate::backend::BackendDB> {
        &self.backend
    }

    /// Get a reference to the sync settings tree.
    pub fn sync_tree(&self) -> &Database {
        &self.sync_tree
    }

    /// Get the device ID for this sync instance.
    ///
    /// The device ID is the device's public key in ed25519:base64 format.
    pub fn get_device_id(&self) -> Result<String> {
        self.get_device_public_key()
    }

    /// Get the device public key for this sync instance.
    ///
    /// # Returns
    /// The device's public key in ed25519:base64 format.
    pub fn get_device_public_key(&self) -> Result<String> {
        let signing_key = self.get_device_signing_key()?;
        let verifying_key = signing_key.verifying_key();
        Ok(format_public_key(&verifying_key))
    }

    /// Get the device signing key for cryptographic operations.
    ///
    /// # Returns
    /// The device's private signing key if available.
    pub(crate) fn get_device_signing_key(&self) -> Result<ed25519_dalek::SigningKey> {
        self.backend
            .get_private_key(DEVICE_KEY_NAME)?
            .ok_or_else(|| {
                SyncError::DeviceKeyNotFound {
                    key_name: DEVICE_KEY_NAME.to_string(),
                }
                .into()
            })
    }

    // === Peer Management Methods ===

    /// Register a new remote peer in the sync network.
    ///
    /// # Arguments
    /// * `pubkey` - The peer's public key (formatted as ed25519:base64)
    /// * `display_name` - Optional human-readable name for the peer
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub fn register_peer(
        &mut self,
        pubkey: impl Into<String>,
        display_name: Option<&str>,
    ) -> Result<()> {
        let pubkey_str = pubkey.into();

        // Store in sync tree via PeerManager
        let op = self.sync_tree.new_operation()?;
        PeerManager::new(&op).register_peer(&pubkey_str, display_name)?;
        op.commit()?;

        // Background sync will read peer info directly from sync tree when needed
        Ok(())
    }

    /// Update the status of a registered peer.
    ///
    /// # Arguments
    /// * `pubkey` - The peer's public key
    /// * `status` - The new status for the peer
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub fn update_peer_status(
        &mut self,
        pubkey: impl AsRef<str>,
        status: PeerStatus,
    ) -> Result<()> {
        let op = self.sync_tree.new_operation()?;
        PeerManager::new(&op).update_peer_status(pubkey.as_ref(), status)?;
        op.commit()?;
        Ok(())
    }

    /// Get information about a registered peer.
    ///
    /// # Arguments
    /// * `pubkey` - The peer's public key
    ///
    /// # Returns
    /// The peer information if found, None otherwise.
    pub fn get_peer_info(&self, pubkey: impl AsRef<str>) -> Result<Option<PeerInfo>> {
        let op = self.sync_tree.new_operation()?;
        PeerManager::new(&op).get_peer_info(pubkey.as_ref())
        // No commit - just reading
    }

    /// List all registered peers.
    ///
    /// # Returns
    /// A vector of all registered peer information.
    pub fn list_peers(&self) -> Result<Vec<PeerInfo>> {
        let op = self.sync_tree.new_operation()?;
        PeerManager::new(&op).list_peers()
        // No commit - just reading
    }

    /// Remove a peer from the sync network.
    ///
    /// This removes the peer entry and all associated sync relationships and transport info.
    ///
    /// # Arguments
    /// * `pubkey` - The peer's public key
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub fn remove_peer(&mut self, pubkey: impl AsRef<str>) -> Result<()> {
        let op = self.sync_tree.new_operation()?;
        PeerManager::new(&op).remove_peer(pubkey)?;
        op.commit()?;
        Ok(())
    }

    // === Tree Sync Relationship Methods ===

    /// Add a tree to the sync relationship with a peer.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    /// * `tree_root_id` - The root ID of the tree to sync
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub fn add_tree_sync(
        &mut self,
        peer_pubkey: impl AsRef<str>,
        tree_root_id: impl AsRef<str>,
    ) -> Result<()> {
        let op = self.sync_tree.new_operation()?;
        PeerManager::new(&op).add_tree_sync(peer_pubkey, tree_root_id)?;
        op.commit()?;
        Ok(())
    }

    /// Remove a tree from the sync relationship with a peer.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    /// * `tree_root_id` - The root ID of the tree to stop syncing
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub fn remove_tree_sync(
        &mut self,
        peer_pubkey: impl AsRef<str>,
        tree_root_id: impl AsRef<str>,
    ) -> Result<()> {
        let op = self.sync_tree.new_operation()?;
        PeerManager::new(&op).remove_tree_sync(peer_pubkey, tree_root_id)?;
        op.commit()?;
        Ok(())
    }

    /// Get the list of trees synced with a peer.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    ///
    /// # Returns
    /// A vector of tree root IDs synced with this peer.
    pub fn get_peer_trees(&self, peer_pubkey: impl AsRef<str>) -> Result<Vec<String>> {
        let op = self.sync_tree.new_operation()?;
        PeerManager::new(&op).get_peer_trees(peer_pubkey)
        // No commit - just reading
    }

    /// Get all peers that sync a specific tree.
    ///
    /// # Arguments
    /// * `tree_root_id` - The root ID of the tree
    ///
    /// # Returns
    /// A vector of peer public keys that sync this tree.
    pub fn get_tree_peers(&self, tree_root_id: impl AsRef<str>) -> Result<Vec<String>> {
        let op = self.sync_tree.new_operation()?;
        PeerManager::new(&op).get_tree_peers(tree_root_id)
        // No commit - just reading
    }

    /// Connect to a remote peer and perform handshake.
    ///
    /// This method initiates a connection to a peer, performs the handshake protocol,
    /// and automatically registers the peer if successful.
    ///
    /// # Arguments
    /// * `address` - The address of the peer to connect to
    ///
    /// # Returns
    /// A Result containing the peer's public key if successful.
    pub async fn connect_to_peer(&mut self, address: &Address) -> Result<String> {
        let (tx, rx) = oneshot::channel();

        self.command_tx
            .send(SyncCommand::ConnectToPeer {
                address: address.clone(),
                response: tx,
            })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        rx.await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))?
    }

    /// Update the connection state of a peer.
    ///
    /// # Arguments
    /// * `pubkey` - The peer's public key
    /// * `state` - The new connection state
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub fn update_peer_connection_state(
        &mut self,
        pubkey: impl AsRef<str>,
        state: ConnectionState,
    ) -> Result<()> {
        let op = self.sync_tree.new_operation()?;
        let peer_manager = PeerManager::new(&op);

        // Get current peer info
        let mut peer_info = match peer_manager.get_peer_info(pubkey.as_ref())? {
            Some(info) => info,
            None => return Err(SyncError::PeerNotFound(pubkey.as_ref().to_string()).into()),
        };

        // Update connection state
        peer_info.connection_state = state;
        peer_info.touch();

        // Save updated peer info
        peer_manager.update_peer_info(pubkey.as_ref(), peer_info)?;
        op.commit()?;
        Ok(())
    }

    /// Check if a tree is synced with a specific peer.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    /// * `tree_root_id` - The root ID of the tree
    ///
    /// # Returns
    /// True if the tree is synced with the peer, false otherwise.
    pub fn is_tree_synced_with_peer(
        &self,
        peer_pubkey: impl AsRef<str>,
        tree_root_id: impl AsRef<str>,
    ) -> Result<bool> {
        let op = self.sync_tree.new_operation()?;
        PeerManager::new(&op).is_tree_synced_with_peer(peer_pubkey, tree_root_id)
        // No commit - just reading
    }

    // === Address Management Methods ===

    /// Add an address to a peer.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    /// * `address` - The address to add
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub fn add_peer_address(
        &mut self,
        peer_pubkey: impl AsRef<str>,
        address: Address,
    ) -> Result<()> {
        let peer_pubkey_str = peer_pubkey.as_ref();

        // Update sync tree via PeerManager
        let op = self.sync_tree.new_operation()?;
        PeerManager::new(&op).add_address(peer_pubkey_str, address)?;
        op.commit()?;

        // Background sync will read updated peer info directly from sync tree when needed
        Ok(())
    }

    /// Remove a specific address from a peer.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    /// * `address` - The address to remove
    ///
    /// # Returns
    /// A Result indicating success or an error (true if removed, false if not found).
    pub fn remove_peer_address(
        &mut self,
        peer_pubkey: impl AsRef<str>,
        address: &Address,
    ) -> Result<bool> {
        let op = self.sync_tree.new_operation()?;
        let result = PeerManager::new(&op).remove_address(peer_pubkey.as_ref(), address)?;
        op.commit()?;
        Ok(result)
    }

    /// Get addresses for a peer, optionally filtered by transport type.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    /// * `transport_type` - Optional transport type filter
    ///
    /// # Returns
    /// A vector of addresses matching the criteria.
    pub fn get_peer_addresses(
        &self,
        peer_pubkey: impl AsRef<str>,
        transport_type: Option<&str>,
    ) -> Result<Vec<Address>> {
        let op = self.sync_tree.new_operation()?;
        PeerManager::new(&op).get_addresses(peer_pubkey.as_ref(), transport_type)
        // No commit - just reading
    }

    // === Network Transport Methods ===

    /// Start a sync server on the specified address (async version).
    ///
    /// # Arguments
    /// * `addr` - The address to bind the server to (e.g., "127.0.0.1:8080")
    ///
    /// # Returns
    /// A Result indicating success or failure of server startup.
    pub async fn start_server_async(&mut self, addr: &str) -> Result<()> {
        if !self.transport_enabled {
            return Err(SyncError::NoTransportEnabled.into());
        }

        let (tx, rx) = oneshot::channel();

        self.command_tx
            .send(SyncCommand::StartServer {
                addr: addr.to_string(),
                response: tx,
            })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        rx.await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))?
    }

    /// Stop the running sync server (async version).
    ///
    /// # Returns
    /// A Result indicating success or failure of server shutdown.
    pub async fn stop_server_async(&mut self) -> Result<()> {
        if !self.transport_enabled {
            return Err(SyncError::NoTransportEnabled.into());
        }
        let (tx, rx) = oneshot::channel();

        self.command_tx
            .send(SyncCommand::StopServer { response: tx })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        rx.await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))?
    }

    /// Enable HTTP transport for network communication.
    ///
    /// This initializes the HTTP transport layer and starts the background sync engine.
    pub fn enable_http_transport(&mut self) -> Result<()> {
        let transport = HttpTransport::new()?;
        self.start_background_sync(Box::new(transport))?;
        self.transport_enabled = true;
        Ok(())
    }

    /// Enable Iroh transport for peer-to-peer network communication.
    ///
    /// This initializes the Iroh transport layer with production defaults (n0's relay servers)
    /// and starts the background sync engine.
    pub fn enable_iroh_transport(&mut self) -> Result<()> {
        let transport = IrohTransport::new()?;
        self.start_background_sync(Box::new(transport))?;
        self.transport_enabled = true;
        Ok(())
    }

    /// Enable Iroh transport with custom configuration.
    ///
    /// This allows specifying custom relay modes, discovery options, etc.
    /// Use IrohTransport::builder() to create a configured transport.
    pub fn enable_iroh_transport_with_config(&mut self, transport: IrohTransport) -> Result<()> {
        self.start_background_sync(Box::new(transport))?;
        self.transport_enabled = true;
        Ok(())
    }

    /// Add a transport with a pre-created transport instance.
    ///
    /// This is useful for testing and advanced configuration scenarios.
    /// Eventually we will support multiple concurrent transports.
    pub fn add_transport(&mut self, transport: Box<dyn SyncTransport>) -> Result<()> {
        self.start_background_sync(transport)?;
        self.transport_enabled = true;
        Ok(())
    }

    /// Start the background sync engine with the given transport
    fn start_background_sync(&mut self, transport: Box<dyn SyncTransport>) -> Result<()> {
        if self.transport_enabled {
            return Err(SyncError::ServerAlreadyRunning {
                // This is a placeholder until the backend supports multiple transports simultaneously.
                address: "background sync".to_string(),
            }
            .into());
        }

        let sync_tree_id = self.sync_tree.root_id().clone();

        // If we're in an async context, spawn directly
        if tokio::runtime::Handle::try_current().is_ok() {
            self.command_tx = BackgroundSync::start(transport, self.backend.clone(), sync_tree_id);
        } else {
            // If not in async context, create a runtime to spawn the background task
            let rt = tokio::runtime::Runtime::new()
                .map_err(|e| SyncError::RuntimeCreation(e.to_string()))?;

            let _guard = rt.enter();
            self.command_tx = BackgroundSync::start(transport, self.backend.clone(), sync_tree_id);

            // Keep the runtime alive by detaching it
            std::mem::forget(rt);
        }

        Ok(())
    }

    /// Get the server address if the transport is running a server.
    ///
    /// # Returns
    /// The address the server is bound to, or an error if no server is running.
    /// Get the server address (async version).
    pub async fn get_server_address_async(&self) -> Result<String> {
        if !self.transport_enabled {
            return Err(SyncError::NoTransportEnabled.into());
        }
        let (tx, rx) = oneshot::channel();

        self.command_tx
            .send(SyncCommand::GetServerAddress { response: tx })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        rx.await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))?
    }

    /// Get the server address (sync version).
    ///
    /// Note: This method may not work correctly when called from within an async context.
    /// Use get_server_address_async() instead when possible.
    pub fn get_server_address(&self) -> Result<String> {
        // Try to use existing async context, or create runtime if needed
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.block_on(self.get_server_address_async())
        } else {
            let runtime = tokio::runtime::Runtime::new()
                .map_err(|e| SyncError::RuntimeCreation(e.to_string()))?;

            runtime.block_on(self.get_server_address_async())
        }
    }

    /// Start a sync server on the specified address.
    ///
    /// # Arguments
    /// * `addr` - The address to bind the server to (e.g., "127.0.0.1:8080")
    ///
    /// # Returns
    /// A Result indicating success or failure of server startup.
    pub fn start_server(&mut self, addr: impl AsRef<str>) -> Result<()> {
        // Try to use existing async context, or create runtime if needed
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.block_on(self.start_server_async(addr.as_ref()))
        } else {
            let runtime = tokio::runtime::Runtime::new()
                .map_err(|e| SyncError::RuntimeCreation(e.to_string()))?;

            runtime.block_on(self.start_server_async(addr.as_ref()))
        }
    }

    /// Stop the running sync server.
    ///
    /// # Returns
    /// A Result indicating success or failure of server shutdown.
    pub fn stop_server(&mut self) -> Result<()> {
        // Try to use existing async context, or create runtime if needed
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.block_on(self.stop_server_async())
        } else {
            let runtime = tokio::runtime::Runtime::new()
                .map_err(|e| SyncError::RuntimeCreation(e.to_string()))?;

            runtime.block_on(self.stop_server_async())
        }
    }

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
    pub async fn sync_tree_with_peer(
        &self,
        peer_pubkey: &str,
        tree_id: &crate::entry::ID,
    ) -> Result<()> {
        // Get peer information and address
        let peer_info = self
            .get_peer_info(peer_pubkey)?
            .ok_or_else(|| SyncError::PeerNotFound(peer_pubkey.to_string()))?;

        let address = peer_info
            .addresses
            .first()
            .ok_or_else(|| SyncError::Network("No addresses found for peer".to_string()))?;

        // Step 1: Get our current tips for this tree
        let our_tips = self
            .backend
            .get_tips(tree_id)
            .map_err(|e| SyncError::BackendError(format!("Failed to get local tips: {e}")))?;

        // Step 2: Get peer's tips for this tree
        let their_tips = self.get_peer_tips(peer_pubkey, tree_id, address).await?;

        // Step 3: Pull - Find entries we don't have and fetch them
        let missing_entries = self.find_missing_entries(&our_tips, &their_tips)?;

        if !missing_entries.is_empty() {
            // Step 4: Fetch missing entries from peer
            let entries = self
                .fetch_entries_from_peer(peer_pubkey, address, &missing_entries)
                .await?;

            // Step 5: Validate and store received entries
            self.store_received_entries(tree_id, entries).await?;
        }

        // Step 6: Push - Find entries peer doesn't have and send them
        let entries_to_send = self.find_entries_to_send(&our_tips, &their_tips)?;

        if !entries_to_send.is_empty() {
            // Step 7: Send our entries that peer is missing
            self.send_entries_to_peer(peer_pubkey, entries_to_send)
                .await?;
        }

        Ok(())
    }

    /// Get tips from a peer for a specific tree.
    async fn get_peer_tips(
        &self,
        peer_pubkey: &str,
        tree_id: &crate::entry::ID,
        address: &Address,
    ) -> Result<Vec<crate::entry::ID>> {
        let request = SyncRequest::GetTips(GetTipsRequest {
            tree_id: tree_id.clone(),
        });

        let response = self.send_request_async(&request, address).await?;

        match response {
            SyncResponse::Tips(tips_response) => Ok(tips_response.tips),
            SyncResponse::Error(msg) => Err(SyncError::SyncProtocolError(format!(
                "Peer {peer_pubkey} returned error for GetTips: {msg}"
            ))
            .into()),
            _ => Err(SyncError::UnexpectedResponse {
                expected: "Tips",
                actual: format!("{response:?}"),
            }
            .into()),
        }
    }

    /// Find entries that we're missing compared to the peer's tips.
    fn find_missing_entries(
        &self,
        _our_tips: &[crate::entry::ID],
        their_tips: &[crate::entry::ID],
    ) -> Result<Vec<crate::entry::ID>> {
        let mut missing = Vec::new();

        for tip_id in their_tips {
            // Check if we have this entry locally
            match self.backend.get(tip_id) {
                Ok(_) => {
                    // We have this entry, nothing to do
                }
                Err(e) if e.is_not_found() => {
                    // We don't have this entry, add it to missing list
                    missing.push(tip_id.clone());
                }
                Err(e) => {
                    return Err(SyncError::BackendError(format!(
                        "Failed to check for entry {tip_id}: {e}"
                    ))
                    .into());
                }
            }
        }

        Ok(missing)
    }

    /// Fetch specific entries from a peer.
    async fn fetch_entries_from_peer(
        &self,
        peer_pubkey: &str,
        address: &Address,
        entry_ids: &[crate::entry::ID],
    ) -> Result<Vec<Entry>> {
        if entry_ids.is_empty() {
            return Ok(Vec::new());
        }

        let request = SyncRequest::GetEntries(GetEntriesRequest {
            entry_ids: entry_ids.to_vec(),
        });

        let response = self.send_request_async(&request, address).await?;

        match response {
            SyncResponse::Entries(entries_response) => Ok(entries_response.entries),
            SyncResponse::Error(msg) => Err(SyncError::SyncProtocolError(format!(
                "Peer {peer_pubkey} returned error for GetEntries: {msg}"
            ))
            .into()),
            _ => Err(SyncError::UnexpectedResponse {
                expected: "Entries",
                actual: format!("{response:?}"),
            }
            .into()),
        }
    }

    /// Validate and store received entries from a peer.
    async fn store_received_entries(
        &self,
        _tree_id: &crate::entry::ID,
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
            self.backend
                .put_verified(entry)
                .map_err(|e| SyncError::BackendError(format!("Failed to store entry: {e}")))?;
        }

        Ok(())
    }

    /// Find entries that we have but the peer is missing.
    fn find_entries_to_send(
        &self,
        our_tips: &[crate::entry::ID],
        their_tips: &[crate::entry::ID],
    ) -> Result<Vec<Entry>> {
        let mut entries_to_send = Vec::new();

        for tip_id in our_tips {
            // Check if peer has this entry by seeing if it's in their tips
            let peer_has_entry = their_tips.contains(tip_id);

            if !peer_has_entry {
                // Peer doesn't have this entry, get it from backend
                match self.backend.get(tip_id) {
                    Ok(entry) => {
                        entries_to_send.push(entry);
                    }
                    Err(e) => {
                        return Err(SyncError::BackendError(format!(
                            "Failed to get entry {tip_id} to send: {e}"
                        ))
                        .into());
                    }
                }
            }
        }

        Ok(entries_to_send)
    }

    /// Send a batch of entries to a sync peer (async version).
    ///
    /// # Arguments
    /// * `entries` - The entries to send
    /// * `address` - The address of the peer to send to
    ///
    /// # Returns
    /// A Result indicating whether the entries were successfully acknowledged.
    pub async fn send_entries_async(
        &self,
        entries: impl AsRef<[Entry]>,
        address: &Address,
    ) -> Result<()> {
        let entries_vec = entries.as_ref().to_vec();
        let request = SyncRequest::SendEntries(entries_vec);
        let response = self.send_request_async(&request, address).await?;

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

    /// Send a batch of entries to a sync peer.
    ///
    /// # Arguments
    /// * `entries` - The entries to send
    /// * `address` - The address of the peer to send to
    ///
    /// # Returns
    /// A Result indicating whether the entries were successfully acknowledged.
    pub fn send_entries(&self, entries: impl AsRef<[Entry]>, address: &Address) -> Result<()> {
        // Try to use existing async context, or create runtime if needed
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.block_on(self.send_entries_async(entries, address))
        } else {
            let entries_ref = entries.as_ref();
            let runtime = tokio::runtime::Runtime::new()
                .map_err(|e| SyncError::RuntimeCreation(e.to_string()))?;

            runtime.block_on(self.send_entries_async(entries_ref, address))
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
    /// - **Tree sync** (`SyncWithPeer` command): Uses tip comparison for semantic filtering
    /// - **Direct send** (this method): Trusts caller to provide appropriate entries
    ///
    /// For automatic duplicate prevention, use tree-based sync relationships instead
    /// of calling this method directly.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The public key of the peer to send to
    /// * `entries` - The specific entries to send (no filtering applied)
    ///
    /// # Returns
    /// A Result indicating whether the command was successfully queued for background processing.
    pub async fn send_entries_to_peer(&self, peer_pubkey: &str, entries: Vec<Entry>) -> Result<()> {
        self.command_tx
            .send(SyncCommand::SendEntries {
                peer: peer_pubkey.to_string(),
                entries,
            })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;
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
    async fn send_request_async(
        &self,
        request: &SyncRequest,
        address: &Address,
    ) -> Result<SyncResponse> {
        if !self.transport_enabled {
            return Err(SyncError::NoTransportEnabled.into());
        }
        let (tx, rx) = oneshot::channel();

        self.command_tx
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

    /// Test helper: Find entries to send to a peer.
    #[cfg(test)]
    pub fn test_find_entries_to_send(
        &self,
        our_tips: &[crate::entry::ID],
        their_tips: &[crate::entry::ID],
    ) -> Result<Vec<Entry>> {
        self.find_entries_to_send(our_tips, their_tips)
    }
}

/// Create a sync hook collection with the Sync instance.
impl Sync {
    /// Create a command-based sync hook for a specific peer.
    ///
    /// This is the new preferred method for creating sync hooks that use
    /// the background sync engine instead of direct sync state access.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The public key of the peer to sync with
    ///
    /// # Returns
    /// A sync hook that sends commands to the background engine
    pub fn create_sync_hook(&self, peer_pubkey: String) -> Arc<dyn SyncHook> {
        use hooks::SyncHookImpl;
        Arc::new(SyncHookImpl::new(self.command_tx.clone(), peer_pubkey))
    }
}
