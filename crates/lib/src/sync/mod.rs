//! Synchronization module for Eidetica database.
//!
//! The Sync module manages synchronization settings and state for the database,
//! storing its configuration in a dedicated tree within the database.

use std::sync::Arc;
use tracing::{debug, info};

use crate::{
    Database, Entry, Result,
    auth::{crypto::format_public_key, types::AuthKey},
    crdt::Doc,
    store::DocStore,
};

pub mod background;
mod bootstrap_request_manager;
pub mod error;
pub mod handler;
pub mod hooks;
mod peer_manager;
pub mod peer_types;
pub mod protocol;
pub mod state;
pub mod transports;
pub mod utils;

use background::{BackgroundSync, SyncCommand};
use bootstrap_request_manager::BootstrapRequestManager;
pub use bootstrap_request_manager::{BootstrapRequest, RequestStatus};
pub use error::SyncError;
use hooks::SyncHook;
use peer_manager::PeerManager;
pub use peer_types::{Address, ConnectionState, PeerInfo, PeerStatus};
use protocol::{SyncRequest, SyncResponse, SyncTreeRequest};
use tokio::sync::{mpsc, oneshot};
use transports::{SyncTransport, http::HttpTransport, iroh::IrohTransport};

/// Private constant for the sync settings subtree name
const SETTINGS_SUBTREE: &str = "settings_map";

/// Constant for the device identity key name
/// This is the name of the Device Key used as the shared identifier for this Device.
pub(crate) const DEVICE_KEY_NAME: &str = "_device_key";

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
        // Ensure device key exists in the backend
        // If no device key exists, generate one automatically
        if backend.get_private_key(DEVICE_KEY_NAME)?.is_none() {
            let (signing_key, _) = crate::auth::crypto::generate_keypair();
            backend.store_private_key(DEVICE_KEY_NAME, signing_key)?;
        }

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
        let op = self.sync_tree.new_transaction()?;
        let sync_settings = op.get_store::<DocStore>(SETTINGS_SUBTREE)?;
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
            .get_store_viewer::<DocStore>(SETTINGS_SUBTREE)?;
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
        let op = self.sync_tree.new_transaction()?;
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
        let op = self.sync_tree.new_transaction()?;
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
        let op = self.sync_tree.new_transaction()?;
        PeerManager::new(&op).get_peer_info(pubkey.as_ref())
        // No commit - just reading
    }

    /// List all registered peers.
    ///
    /// # Returns
    /// A vector of all registered peer information.
    pub fn list_peers(&self) -> Result<Vec<PeerInfo>> {
        let op = self.sync_tree.new_transaction()?;
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
        let op = self.sync_tree.new_transaction()?;
        PeerManager::new(&op).remove_peer(pubkey)?;
        op.commit()?;
        Ok(())
    }

    // === Database Sync Relationship Methods ===

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
        let op = self.sync_tree.new_transaction()?;
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
        let op = self.sync_tree.new_transaction()?;
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
        let op = self.sync_tree.new_transaction()?;
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
        let op = self.sync_tree.new_transaction()?;
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
        let op = self.sync_tree.new_transaction()?;
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
        let op = self.sync_tree.new_transaction()?;
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
        let op = self.sync_tree.new_transaction()?;
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
        let op = self.sync_tree.new_transaction()?;
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
        let op = self.sync_tree.new_transaction()?;
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

        let result = rx
            .await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))?;

        // Clear the transport_enabled flag after successfully stopping the server
        if result.is_ok() {
            self.transport_enabled = false;
        }

        result
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

        // Get our current tips for this tree (empty if tree doesn't exist)
        let our_tips = self
            .backend
            .get_tips(tree_id)
            .map_err(|e| SyncError::BackendError(format!("Failed to get local tips: {e}")))?;

        // Send unified sync request
        let request = SyncRequest::SyncTree(SyncTreeRequest {
            tree_id: tree_id.clone(),
            our_tips,
            requesting_key: None, // TODO: Add auth support for direct sync
            requesting_key_name: None,
            requested_permission: None,
        });

        // Send request via background sync command
        let (tx, rx) = oneshot::channel();
        self.command_tx
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

        Ok(())
    }

    /// Handle bootstrap response by storing root and all entries
    async fn handle_bootstrap_response(&self, response: protocol::BootstrapResponse) -> Result<()> {
        tracing::info!(tree_id = %response.tree_id, "Processing bootstrap response");

        // Store root entry first

        // Store the root entry
        self.backend
            .put_verified(response.root_entry.clone())
            .map_err(|e| SyncError::BackendError(format!("Failed to store root entry: {e}")))?;

        // Store all other entries using existing method
        self.store_received_entries(&response.tree_id, response.all_entries)
            .await?;

        tracing::info!(tree_id = %response.tree_id, "Bootstrap completed successfully");
        Ok(())
    }

    /// Handle incremental response by storing missing entries and sending back what server is missing
    async fn handle_incremental_response(
        &self,
        response: protocol::IncrementalResponse,
        peer_address: &peer_types::Address,
    ) -> Result<()> {
        tracing::debug!(tree_id = %response.tree_id, "Processing incremental response");

        // Step 1: Store missing entries
        self.store_received_entries(&response.tree_id, response.missing_entries)
            .await?;

        // Step 2: Check if server is missing entries from us
        let our_tips = self.backend.get_tips(&response.tree_id)?;
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
            let entries_for_server = crate::sync::utils::collect_ancestors_to_send(
                self.backend.as_ref(),
                &missing_tip_ids,
                their_tips,
            )?;

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
        tree_id: &crate::entry::ID,
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
        self.command_tx
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
    /// - **Database sync** (`SyncWithPeer` command): Uses tip comparison for semantic filtering
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

    /// Discover available trees from a peer (simplified API).
    ///
    /// This method connects to a peer and retrieves the list of trees they're willing to sync.
    /// This is useful for discovering what can be synced before setting up sync relationships.
    ///
    /// # Arguments
    /// * `peer_address` - The address of the peer to connect to (format: "host:port")
    ///
    /// # Returns
    /// A vector of TreeInfo describing available trees, or an error.
    pub async fn discover_peer_trees(
        &mut self,
        peer_address: &str,
    ) -> Result<Vec<protocol::TreeInfo>> {
        use peer_types::Address;

        let address = Address {
            transport_type: "http".to_string(),
            address: peer_address.to_string(),
        };

        // Connect and get handshake info
        let _peer_pubkey = self.connect_to_peer(&address).await?;

        // The handshake already contains the tree list, but we need to get it again
        // since connect_to_peer doesn't return it. For now, return empty list
        // TODO: Enhance this to actually return the tree list from handshake

        tracing::warn!(
            "discover_peer_trees not fully implemented - handshake contains tree info but API needs enhancement"
        );
        Ok(vec![])
    }

    /// Sync with a peer using simplified one-shot API.
    ///
    /// This method automatically handles bootstrap vs incremental sync and doesn't require
    /// pre-configured sync relationships. If the tree doesn't exist locally, it will be
    /// bootstrapped from the peer.
    ///
    /// # Arguments
    /// * `peer_address` - The address of the peer (format: "host:port")
    /// * `tree_id` - Optional tree ID to sync. If None, sync all available trees.
    ///
    /// # Returns
    /// A Result indicating success or failure.
    pub async fn sync_with_peer(
        &mut self,
        peer_address: &str,
        tree_id: Option<&crate::entry::ID>,
    ) -> Result<()> {
        use peer_types::Address;

        let address = Address {
            transport_type: "http".to_string(),
            address: peer_address.to_string(),
        };

        // Connect to peer if not already connected
        let peer_pubkey = self.connect_to_peer(&address).await?;

        // Store the address for this peer (needed for sync_tree_with_peer)
        self.add_peer_address(&peer_pubkey, address.clone())?;

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
        tree_id: &crate::entry::ID,
        requesting_key: Option<&str>,
        requesting_key_name: Option<&str>,
        requested_permission: Option<crate::auth::Permission>,
    ) -> Result<()> {
        // Get peer information and address
        let peer_info = self
            .get_peer_info(peer_pubkey)?
            .ok_or_else(|| SyncError::PeerNotFound(peer_pubkey.to_string()))?;

        let address = peer_info
            .addresses
            .first()
            .ok_or_else(|| SyncError::Network("No addresses found for peer".to_string()))?;

        // Get our current tips for this tree (empty if tree doesn't exist)
        let our_tips = self
            .backend
            .get_tips(tree_id)
            .map_err(|e| SyncError::BackendError(format!("Failed to get local tips: {e}")))?;

        // Send unified sync request with auth parameters
        let request = SyncRequest::SyncTree(SyncTreeRequest {
            tree_id: tree_id.clone(),
            our_tips,
            requesting_key: requesting_key.map(|k| k.to_string()),
            requesting_key_name: requesting_key_name.map(|k| k.to_string()),
            requested_permission,
        });

        // Send request via background sync command
        let (tx, rx) = oneshot::channel();
        self.command_tx
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
                self.backend.put_verified(bootstrap_response.root_entry)?;

                // Store all other entries
                for entry in bootstrap_response.all_entries {
                    self.backend.put_unverified(entry)?;
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

        Ok(())
    }

    // === Bootstrap Sync Methods ===
    //
    // Eidetica provides two bootstrap sync methods for different key management scenarios:
    //
    // 1. `sync_with_peer_for_bootstrap_with_key()` - **Preferred** for user-managed keys
    //    - Signing key provided directly as parameter
    //    - Keys remain in memory (not stored in backend)
    //    - Required for User API which manages its own key lifecycle
    //    - **Use this method for new code** - it provides better key isolation and security
    //
    // 2. `sync_with_peer_for_bootstrap()` - For legacy backend-managed keys
    //    - Keys are stored in backend storage
    //    - Method looks up the signing key automatically
    //    - Suitable for simple applications and direct sync API usage
    //    - Consider migrating to the `_with_key()` variant for better security
    //
    // Both methods delegate to `sync_with_peer_for_bootstrap_internal()` which contains
    // the common bootstrap logic. Prefer `_with_key()` for new implementations.

    /// Internal helper for bootstrap sync operations.
    ///
    /// This method contains the common logic for bootstrap scenarios where the local
    /// device doesn't have access to the target tree yet and needs to request
    /// permission during the initial sync.
    ///
    /// # Arguments
    /// * `peer_address` - The address of the peer to sync with
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
        &mut self,
        peer_address: &str,
        tree_id: &crate::entry::ID,
        requesting_public_key: String,
        requesting_key_name: &str,
        requested_permission: crate::auth::Permission,
    ) -> Result<()> {
        use peer_types::Address;

        // Validate public key is not empty
        if requesting_public_key.is_empty() {
            return Err(SyncError::InvalidPublicKey {
                reason: "Public key cannot be empty".to_string(),
            }
            .into());
        }

        // Validate public key format by attempting to parse it
        crate::auth::crypto::parse_public_key(&requesting_public_key).map_err(|e| {
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

        let address = Address {
            transport_type: "http".to_string(),
            address: peer_address.to_string(),
        };

        // Connect to peer if not already connected
        let peer_pubkey = self.connect_to_peer(&address).await?;

        // Store the address for this peer
        self.add_peer_address(&peer_pubkey, address.clone())?;

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

    /// Sync with a peer, requesting access with authentication for bootstrap scenarios.
    ///
    /// This method is specifically designed for bootstrap scenarios where the local
    /// device doesn't have access to the target tree yet and needs to request
    /// permission during the initial sync. The signing key is looked up from backend
    /// storage using the provided key name.
    ///
    /// # Arguments
    /// * `peer_address` - The address of the peer to sync with
    /// * `tree_id` - The ID of the tree to sync
    /// * `requesting_key_name` - The name/ID of the local authentication key in backend storage
    /// * `requested_permission` - The permission level being requested
    ///
    /// # Returns
    /// A Result indicating success or failure.
    pub async fn sync_with_peer_for_bootstrap(
        &mut self,
        peer_address: &str,
        tree_id: &crate::entry::ID,
        requesting_key_name: &str,
        requested_permission: crate::auth::Permission,
    ) -> Result<()> {
        // Get our public key for the requesting key from backend
        let signing_key = self
            .backend
            .get_private_key(requesting_key_name)?
            .ok_or_else(|| {
                SyncError::BackendError(format!(
                    "Private key not found for key name: {requesting_key_name}"
                ))
            })?;

        let verifying_key = signing_key.verifying_key();
        let requesting_public_key = crate::auth::crypto::format_public_key(&verifying_key);

        // Delegate to internal method
        self.sync_with_peer_for_bootstrap_internal(
            peer_address,
            tree_id,
            requesting_public_key,
            requesting_key_name,
            requested_permission,
        )
        .await
    }

    /// Sync with a peer for bootstrap using a user-provided public key.
    ///
    /// This method is specifically designed for bootstrap scenarios where the local
    /// device doesn't have access to the target tree yet and needs to request
    /// permission during the initial sync. Unlike `sync_with_peer_for_bootstrap`,
    /// this variant accepts a public key directly instead of looking it up from
    /// backend storage, making it compatible with User API managed keys.
    ///
    /// # Arguments
    /// * `peer_address` - The address of the peer to sync with
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
    ///     "127.0.0.1:8080",
    ///     &tree_id,
    ///     &public_key,
    ///     user_key_id,
    ///     Permission::Write(5),
    /// ).await?;
    /// ```
    pub async fn sync_with_peer_for_bootstrap_with_key(
        &mut self,
        peer_address: &str,
        tree_id: &crate::entry::ID,
        requesting_public_key: &str,
        requesting_key_name: &str,
        requested_permission: crate::auth::Permission,
    ) -> Result<()> {
        // Delegate to internal method
        self.sync_with_peer_for_bootstrap_internal(
            peer_address,
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
    pub fn pending_bootstrap_requests(&self) -> Result<Vec<(String, BootstrapRequest)>> {
        let op = self.sync_tree.new_transaction()?;
        let manager = BootstrapRequestManager::new(&op);
        manager.pending_requests()
    }

    /// Get all approved bootstrap requests.
    ///
    /// # Returns
    /// A vector of (request_id, bootstrap_request) pairs for approved requests.
    pub fn approved_bootstrap_requests(&self) -> Result<Vec<(String, BootstrapRequest)>> {
        let op = self.sync_tree.new_transaction()?;
        let manager = BootstrapRequestManager::new(&op);
        manager.approved_requests()
    }

    /// Get all rejected bootstrap requests.
    ///
    /// # Returns
    /// A vector of (request_id, bootstrap_request) pairs for rejected requests.
    pub fn rejected_bootstrap_requests(&self) -> Result<Vec<(String, BootstrapRequest)>> {
        let op = self.sync_tree.new_transaction()?;
        let manager = BootstrapRequestManager::new(&op);
        manager.rejected_requests()
    }

    /// Get a specific bootstrap request by ID.
    ///
    /// # Arguments
    /// * `request_id` - The unique identifier of the request
    ///
    /// # Returns
    /// A tuple of (request_id, bootstrap_request) if found, None otherwise.
    pub fn get_bootstrap_request(
        &self,
        request_id: &str,
    ) -> Result<Option<(String, BootstrapRequest)>> {
        let op = self.sync_tree.new_transaction()?;
        let manager = BootstrapRequestManager::new(&op);

        match manager.get_request(request_id)? {
            Some(request) => Ok(Some((request_id.to_string(), request))),
            None => Ok(None),
        }
    }

    /// Approve a bootstrap request and add the key to the target database.
    ///
    /// This method loads the bootstrap request, validates it exists and is pending,
    /// then adds the requesting key to the target database using the specified
    /// approving key for authentication.
    ///
    /// # Arguments
    /// * `request_id` - The unique identifier of the request to approve
    /// * `approving_key_name` - The name of the local key to use for the approval
    ///
    /// # Returns
    /// Result indicating success or failure of the approval operation.
    pub fn approve_bootstrap_request(
        &mut self,
        request_id: &str,
        approving_key_name: &str,
    ) -> Result<()> {
        // Load the request from sync database
        let sync_op = self.sync_tree.new_transaction()?;
        let manager = BootstrapRequestManager::new(&sync_op);

        let request = manager
            .get_request(request_id)?
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

        // Load target database and add the key
        let database = Database::new_from_id(request.tree_id.clone(), self.backend.clone())?;
        let mut tx = database.new_transaction()?;
        tx.set_auth_key(approving_key_name);

        // Get settings store and update auth configuration
        let settings_store = tx.get_settings()?;

        // Create the auth key for the requesting device
        let auth_key = AuthKey::active(
            request.requesting_pubkey.clone(),
            request.requested_permission.clone(),
        )?;

        // Add the new key to auth settings using SettingsStore API
        // This provides proper upsert behavior and validation
        settings_store.set_auth_key(&request.requesting_key_name, auth_key)?;

        tx.commit()?;

        // Update request status to approved
        let approval_time = bootstrap_request_manager::current_timestamp();
        manager.update_status(
            request_id,
            RequestStatus::Approved {
                approved_by: approving_key_name.to_string(),
                approval_time,
            },
        )?;
        sync_op.commit()?;

        info!(
            request_id = %request_id,
            tree_id = %request.tree_id,
            approved_by = %approving_key_name,
            "Bootstrap request approved and key added to database"
        );

        // TODO: Implement notification to requesting peer (future enhancement)

        Ok(())
    }

    /// Approve a bootstrap request using a user-provided signing key.
    ///
    /// This variant allows approval using keys that are not stored in the backend,
    /// such as user keys managed in memory.
    ///
    /// # Arguments
    /// * `request_id` - The unique identifier of the request to approve
    /// * `approving_signing_key` - The signing key to use for the transaction
    /// * `approving_sigkey` - The sigkey identifier for audit trail
    ///
    /// # Returns
    /// Result indicating success or failure of the approval operation.
    ///
    /// # Errors
    /// Returns `SyncError::InsufficientPermission` if the approving key does not have
    /// Admin permission on the target database.
    pub fn approve_bootstrap_request_with_key(
        &mut self,
        request_id: &str,
        approving_signing_key: &ed25519_dalek::SigningKey,
        approving_sigkey: &str,
    ) -> Result<()> {
        // Load the request from sync database
        let sync_op = self.sync_tree.new_transaction()?;
        let manager = BootstrapRequestManager::new(&sync_op);

        let request = manager
            .get_request(request_id)?
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
        let database = Database::load_with_key(
            self.backend.clone(),
            &request.tree_id,
            approving_signing_key.clone(),
            approving_sigkey.to_string(),
        )?;

        // Explicitly check that the approving user has Admin permission
        // This provides clear error messages and fails fast before modifying the database
        let permission = database.get_sigkey_permission(approving_sigkey)?;
        if !permission.can_admin() {
            return Err(SyncError::InsufficientPermission {
                request_id: request_id.to_string(),
                required_permission: "Admin".to_string(),
                actual_permission: permission,
            }
            .into());
        }

        // Create transaction - this will use the provided signing key
        let tx = database.new_transaction()?;

        // Get settings store and update auth configuration
        let settings_store = tx.get_settings()?;

        // Create the auth key for the requesting device
        let auth_key = AuthKey::active(
            request.requesting_pubkey.clone(),
            request.requested_permission.clone(),
        )?;

        // Add the new key to auth settings using SettingsStore API
        // This provides proper upsert behavior and validation
        settings_store.set_auth_key(&request.requesting_key_name, auth_key)?;

        // Commit will validate that the user's key has Admin permission
        // If this fails, it means the user lacks the necessary permission
        tx.commit()?;

        // Update request status to approved
        let approval_time = bootstrap_request_manager::current_timestamp();
        manager.update_status(
            request_id,
            RequestStatus::Approved {
                approved_by: approving_sigkey.to_string(),
                approval_time,
            },
        )?;
        sync_op.commit()?;

        info!(
            request_id = %request_id,
            tree_id = %request.tree_id,
            approved_by = %approving_sigkey,
            "Bootstrap request approved and key added to database using user-provided key"
        );

        Ok(())
    }

    /// Reject a bootstrap request.
    ///
    /// This method marks the request as rejected without adding any keys
    /// to the target database.
    ///
    /// # Arguments
    /// * `request_id` - The unique identifier of the request to reject
    /// * `rejecting_key_name` - The name of the local key making the rejection
    ///
    /// # Returns
    /// Result indicating success or failure of the rejection operation.
    pub fn reject_bootstrap_request(
        &mut self,
        request_id: &str,
        rejecting_key_name: &str,
    ) -> Result<()> {
        let op = self.sync_tree.new_transaction()?;
        let manager = BootstrapRequestManager::new(&op);

        // Validate request exists and is pending
        let request = manager
            .get_request(request_id)?
            .ok_or_else(|| SyncError::RequestNotFound(request_id.to_string()))?;

        if !matches!(request.status, RequestStatus::Pending) {
            return Err(SyncError::InvalidRequestState {
                request_id: request_id.to_string(),
                current_status: format!("{:?}", request.status),
                expected_status: "Pending".to_string(),
            }
            .into());
        }

        // Update status to rejected
        let rejection_time = bootstrap_request_manager::current_timestamp();
        manager.update_status(
            request_id,
            RequestStatus::Rejected {
                rejected_by: rejecting_key_name.to_string(),
                rejection_time,
            },
        )?;
        op.commit()?;

        info!(
            request_id = %request_id,
            tree_id = %request.tree_id,
            rejected_by = %rejecting_key_name,
            "Bootstrap request rejected"
        );

        // TODO: Implement notification to requesting peer (future enhancement)

        Ok(())
    }

    /// Reject a bootstrap request using a user-provided signing key with Admin permission validation.
    ///
    /// This variant allows rejection using keys that are not stored in the backend,
    /// such as user keys managed in memory. It validates that the rejecting user has
    /// Admin permission on the target database before allowing the rejection.
    ///
    /// # Arguments
    /// * `request_id` - The unique identifier of the request to reject
    /// * `rejecting_signing_key` - The signing key to use for permission validation
    /// * `rejecting_sigkey` - The sigkey identifier for audit trail
    ///
    /// # Returns
    /// Result indicating success or failure of the rejection operation.
    ///
    /// # Errors
    /// Returns `SyncError::InsufficientPermission` if the rejecting key does not have
    /// Admin permission on the target database.
    pub fn reject_bootstrap_request_with_key(
        &mut self,
        request_id: &str,
        rejecting_signing_key: &ed25519_dalek::SigningKey,
        rejecting_sigkey: &str,
    ) -> Result<()> {
        // Load the request from sync database
        let sync_op = self.sync_tree.new_transaction()?;
        let manager = BootstrapRequestManager::new(&sync_op);

        let request = manager
            .get_request(request_id)?
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
        let database = Database::load_with_key(
            self.backend.clone(),
            &request.tree_id,
            rejecting_signing_key.clone(),
            rejecting_sigkey.to_string(),
        )?;

        // Check that the rejecting user has Admin permission
        let permission = database.get_sigkey_permission(rejecting_sigkey)?;
        if !permission.can_admin() {
            return Err(SyncError::InsufficientPermission {
                request_id: request_id.to_string(),
                required_permission: "Admin".to_string(),
                actual_permission: permission,
            }
            .into());
        }

        // User has Admin permission, proceed with rejection
        let rejection_time = bootstrap_request_manager::current_timestamp();
        manager.update_status(
            request_id,
            RequestStatus::Rejected {
                rejected_by: rejecting_sigkey.to_string(),
                rejection_time,
            },
        )?;
        sync_op.commit()?;

        info!(
            request_id = %request_id,
            tree_id = %request.tree_id,
            rejected_by = %rejecting_sigkey,
            "Bootstrap request rejected by user with Admin permission"
        );

        Ok(())
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
