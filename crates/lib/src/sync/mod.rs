//! Synchronization module for Eidetica database.
//!
//! The Sync module manages synchronization settings and state for the database,
//! storing its configuration in a dedicated tree within the database.

use crate::{Result, crdt::Doc, entry::Entry, subtree::DocStore, tree::Tree};
use std::sync::Arc;

pub mod error;
pub mod handler;
mod peer_manager;
pub mod peer_types;
pub mod protocol;
pub mod transports;

pub use error::SyncError;
pub use peer_types::{Address, ConnectionState, PeerInfo, PeerStatus};

use peer_manager::PeerManager;
use protocol::{HandshakeRequest, PROTOCOL_VERSION, SyncRequest, SyncResponse};
use transports::{SyncTransport, http::HttpTransport, iroh::IrohTransport};

/// Private constant for the sync settings subtree name
const SETTINGS_SUBTREE: &str = "settings_map";

/// Private constant for the device identity key name
const DEVICE_KEY_NAME: &str = "_device_key";

/// Synchronization manager for the database.
///
/// The Sync module maintains its own tree for storing synchronization settings
/// and managing the synchronization state of the database.
pub struct Sync {
    /// The backend for tree operations
    backend: Arc<dyn crate::backend::Database>,
    /// The tree containing synchronization settings
    sync_tree: Tree,
    /// Optional network transport for sync communication
    // Uses simple Box ownership rather than Arc<RwLock<>> because:
    // 1. Each Sync instance exclusively owns its transport (1:1 relationship)
    // 2. All transport operations require &mut self, ensuring exclusive access
    // 3. No sharing between Sync instances - each BaseDB has exactly one Sync
    // 4. Simpler ownership model without unnecessary concurrency overhead
    transport: Option<Box<dyn SyncTransport>>,
}

impl Sync {
    /// Create a new Sync instance with a dedicated settings tree.
    ///
    /// # Arguments
    /// * `backend` - The database backend for tree operations
    ///
    /// # Returns
    /// A new Sync instance with its own settings tree.
    pub fn new(backend: Arc<dyn crate::backend::Database>) -> Result<Self> {
        let mut sync_settings = Doc::new();
        sync_settings.set_string("name", "_sync");
        sync_settings.set_string("type", "sync_settings");

        let mut sync_tree = Tree::new(sync_settings, Arc::clone(&backend), DEVICE_KEY_NAME)?;

        // Set the default authentication key so all operations use the device key
        sync_tree.set_default_auth_key(DEVICE_KEY_NAME);

        Ok(Self {
            backend,
            sync_tree,
            transport: None,
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
        backend: Arc<dyn crate::backend::Database>,
        sync_tree_root_id: &crate::entry::ID,
    ) -> Result<Self> {
        let mut sync_tree = Tree::new_from_id(sync_tree_root_id.clone(), Arc::clone(&backend))?;

        // Set the default authentication key so all operations use the device key
        sync_tree.set_default_auth_key(DEVICE_KEY_NAME);

        Ok(Self {
            backend,
            sync_tree,
            transport: None,
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
    pub fn set_setting(&mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> Result<()> {
        let op = self.sync_tree.new_operation()?;
        let sync_settings = op.get_subtree::<DocStore>(SETTINGS_SUBTREE)?;
        sync_settings.set_string(key.as_ref(), value.as_ref())?;
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
    pub fn backend(&self) -> &Arc<dyn crate::backend::Database> {
        &self.backend
    }

    /// Get a reference to the sync settings tree.
    pub fn sync_tree(&self) -> &Tree {
        &self.sync_tree
    }

    /// Get the device ID for this sync instance.
    ///
    /// The device ID is generated lazily and stored in settings.
    pub fn get_device_id(&self) -> Result<String> {
        match self.get_setting("device_id")? {
            Some(id) => Ok(id),
            None => {
                // Generate a new device ID if not present
                let id = format!("device_{}", uuid::Uuid::new_v4());
                Ok(id)
            }
        }
    }

    /// Get the device public key for this sync instance.
    ///
    /// # Returns
    /// The device's public key in ed25519:base64 format.
    pub fn get_device_public_key(&self) -> Result<String> {
        // TODO: Get actual public key from auth system
        // For now, return a placeholder
        use base64ct::{Base64, Encoding};
        Ok(format!(
            "ed25519:{}",
            Base64::encode_string(b"placeholder_key")
        ))
    }

    /// Set the device ID (primarily for testing).
    pub fn set_device_id(&mut self, device_id: impl Into<String>) -> Result<()> {
        let device_id = device_id.into();
        self.set_setting("device_id", device_id)
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
        let op = self.sync_tree.new_operation()?;
        let pubkey = pubkey.into();
        PeerManager::new(&op).register_peer(&pubkey, display_name)?;
        op.commit()?;
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
        PeerManager::new(&op).remove_peer(pubkey.as_ref())?;
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
        PeerManager::new(&op).add_tree_sync(peer_pubkey.as_ref(), tree_root_id.as_ref())?;
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
        PeerManager::new(&op).remove_tree_sync(peer_pubkey.as_ref(), tree_root_id.as_ref())?;
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
        PeerManager::new(&op).get_peer_trees(peer_pubkey.as_ref())
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
        PeerManager::new(&op).get_tree_peers(tree_root_id.as_ref())
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
        if self.transport.is_none() {
            return Err(SyncError::NoTransportEnabled.into());
        }

        // Generate challenge for authentication
        let challenge = generate_challenge();

        // Get our device info
        let device_id = self.get_device_id()?;
        let public_key = self.get_device_public_key()?;

        // Create handshake request
        let handshake_request = HandshakeRequest {
            device_id,
            public_key: public_key.clone(),
            display_name: self.get_setting("display_name")?,
            protocol_version: PROTOCOL_VERSION,
            challenge,
        };

        // Send handshake request
        let request = SyncRequest::Handshake(handshake_request);
        let response = self.send_request_async(&request, address).await?;

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

                // TODO: Verify signature on challenge_response
                // For now, we trust the response

                // Auto-register the peer
                self.register_peer(
                    &handshake_resp.public_key,
                    handshake_resp.display_name.as_deref(),
                )?;

                // Add the address to the peer
                self.add_peer_address(&handshake_resp.public_key, address.clone())?;

                // Update connection state
                self.update_peer_connection_state(
                    &handshake_resp.public_key,
                    ConnectionState::Connected,
                )?;

                Ok(handshake_resp.public_key)
            }
            SyncResponse::Error(msg) => Err(SyncError::HandshakeFailed(msg).into()),
            _ => Err(SyncError::HandshakeFailed("Unexpected response type".to_string()).into()),
        }
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
        pubkey: &str,
        state: ConnectionState,
    ) -> Result<()> {
        let op = self.sync_tree.new_operation()?;
        let peer_manager = PeerManager::new(&op);

        // Get current peer info
        let mut peer_info = match peer_manager.get_peer_info(pubkey)? {
            Some(info) => info,
            None => return Err(SyncError::PeerNotFound(pubkey.to_string()).into()),
        };

        // Update connection state
        peer_info.connection_state = state;
        peer_info.touch();

        // Save updated peer info
        peer_manager.update_peer_info(pubkey, peer_info)?;
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
        PeerManager::new(&op).is_tree_synced_with_peer(peer_pubkey.as_ref(), tree_root_id.as_ref())
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
        let op = self.sync_tree.new_operation()?;
        PeerManager::new(&op).add_address(peer_pubkey.as_ref(), address)?;
        op.commit()?;
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
        peer_pubkey: &str,
        transport_type: Option<&str>,
    ) -> Result<Vec<Address>> {
        let op = self.sync_tree.new_operation()?;
        PeerManager::new(&op).get_addresses(peer_pubkey, transport_type)
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
        if let Some(transport) = &mut self.transport {
            transport.start_server(addr).await
        } else {
            Err(SyncError::NoTransportEnabled.into())
        }
    }

    /// Stop the running sync server (async version).
    ///
    /// # Returns
    /// A Result indicating success or failure of server shutdown.
    pub async fn stop_server_async(&mut self) -> Result<()> {
        if let Some(transport) = &mut self.transport {
            transport.stop_server().await
        } else {
            Err(SyncError::NoTransportEnabled.into())
        }
    }

    /// Enable HTTP transport for network communication.
    ///
    /// This initializes the HTTP transport layer, allowing the sync module
    /// to communicate over HTTP/REST APIs.
    pub fn enable_http_transport(&mut self) -> Result<()> {
        let transport = HttpTransport::new()?;
        self.transport = Some(Box::new(transport));
        Ok(())
    }

    /// Enable Iroh transport for peer-to-peer network communication.
    ///
    /// This initializes the Iroh transport layer, allowing the sync module
    /// to communicate over QUIC-based peer-to-peer networking with hole punching.
    pub fn enable_iroh_transport(&mut self) -> Result<()> {
        let transport = IrohTransport::new()?;
        self.transport = Some(Box::new(transport));
        Ok(())
    }

    /// Get the server address if the transport is running a server.
    ///
    /// # Returns
    /// The address the server is bound to, or an error if no server is running.
    pub fn get_server_address(&self) -> Result<String> {
        if let Some(transport) = &self.transport {
            transport.get_server_address()
        } else {
            Err(SyncError::NoTransportEnabled.into())
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
        if let Some(transport) = &self.transport {
            if !transport.can_handle_address(address) {
                return Err(SyncError::UnsupportedTransport {
                    transport_type: address.transport_type.clone(),
                }
                .into());
            }
            transport.send_entries(address, entries.as_ref()).await
        } else {
            Err(SyncError::NoTransportEnabled.into())
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
        if let Some(transport) = &self.transport {
            if !transport.can_handle_address(address) {
                return Err(SyncError::UnsupportedTransport {
                    transport_type: address.transport_type.clone(),
                }
                .into());
            }
            transport.send_request(address, request).await
        } else {
            Err(SyncError::NoTransportEnabled.into())
        }
    }
}

/// Generate random challenge bytes for authentication.
fn generate_challenge() -> Vec<u8> {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut challenge = vec![0u8; 32];
    rng.fill(&mut challenge[..]);
    challenge
}
