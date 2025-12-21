//! Synchronization module for Eidetica database.
//!
//! # Architecture
//!
//! The sync system uses a Background Sync architecture with command-pattern communication:
//!
//! - **[`Sync`]**: Thread-safe frontend using `Arc<Sync>` with interior mutability
//!   (`OnceLock`). Provides the public API and sends commands to the background.
//! - **[`background::BackgroundSync`]**: Single background thread handling all sync operations
//!   via a command loop. Owns the transport and retry queue.
//! - **Write Callbacks**: Automatically trigger sync when entries are committed via
//!   database write callbacks.
//!
//! # Bootstrap Protocol
//!
//! The sync protocol detects whether a client needs bootstrap or incremental sync:
//!
//! - **Empty tips** → Full bootstrap (complete database transfer)
//! - **Has tips** → Incremental sync (only missing entries)
//!
//! Use [`Sync::sync_with_peer`] which handles both cases automatically.
//!
//! # Duplicate Prevention
//!
//! Uses Merkle-DAG tip comparison instead of tracking individual sent entries:
//!
//! 1. Exchange tips with peer
//! 2. Compare DAGs to find missing entries
//! 3. Send only what peer doesn't have
//! 4. Receive only what we're missing
//!
//! This approach requires no extra storage apart from tracking relevant Merkle-DAG tips.
//!
//! # Transport Layer
//!
//! Two transport implementations are available:
//!
//! - **HTTP** ([`transports::http::HttpTransport`]): REST API at `/api/v0`, JSON serialization
//! - **Iroh P2P** ([`transports::iroh::IrohTransport`]): QUIC-based with NAT traversal
//!
//! Both implement the [`transports::SyncTransport`] trait. Other transport layers can be supported by implementing the trait.
//!
//! # Peer and State Management
//!
//! Peers and sync relationships are stored in a dedicated sync database (`_sync`):
//!
//! - `peer_manager::PeerManager`: Handles peer registration and relationships
//! - [`state::SyncStateManager`]: Tracks sync cursors, metadata, and history
//!
//! # Connection Behavior
//!
//! - **Lazy connections**: Established on-demand, not at peer registration
//! - **Periodic sync**: Configurable interval (default 5 minutes)
//! - **Retry queue**: Failed sends retried with exponential backoff

use handle_trait::Handle;
use std::sync::OnceLock;
use tracing::{debug, info};

use crate::{
    Database, Entry, Instance, Result, WeakInstance,
    auth::{crypto::format_public_key, types::AuthKey},
    crdt::{Doc, doc::Value},
    entry::ID,
    instance::backend::Backend,
    store::{DocStore, Registry, SettingsStore},
};

pub mod background;
mod bootstrap_request_manager;
pub mod error;
pub mod handler;
pub mod peer_manager;
pub mod peer_types;
pub mod protocol;
pub mod state;
mod transport_manager;
pub mod transports;
mod user_sync_manager;
pub mod utils;

use background::{BackgroundSync, SyncCommand};
use bootstrap_request_manager::BootstrapRequestManager;
pub use bootstrap_request_manager::{BootstrapRequest, RequestStatus};
pub use error::SyncError;
use peer_manager::PeerManager;
pub use peer_types::{Address, ConnectionState, PeerInfo, PeerStatus};
use protocol::{SyncRequest, SyncResponse, SyncTreeRequest};
use std::time::SystemTime;
use tokio::sync::{mpsc, oneshot};
use transports::{
    SyncTransport, TransportConfig,
    http::HttpTransport,
    iroh::{IrohTransport, IrohTransportConfig},
};
use user_sync_manager::UserSyncManager;

/// Private constant for the sync settings subtree name
const SETTINGS_SUBTREE: &str = "settings_map";

/// Private constant for the transports registry subtree name
const TRANSPORTS_SUBTREE: &str = "transports";

/// Constant for the device identity key name
/// This is the name of the Device Key used as the shared identifier for this Device.
pub(crate) const DEVICE_KEY_NAME: &str = "_device_key";

/// Authentication parameters for sync operations.
#[derive(Debug, Clone)]
pub struct AuthParams {
    /// The public key making the request
    pub requesting_key: String,
    /// The name/ID of the requesting key
    pub requesting_key_name: String,
    /// The permission level being requested
    pub requested_permission: crate::auth::Permission,
}

/// Information needed to register a peer for syncing.
///
/// This is used with [`Sync::register_sync_peer()`] to declare sync intent.
#[derive(Debug, Clone)]
pub struct SyncPeerInfo {
    /// The peer's public key
    pub peer_pubkey: String,
    /// The tree/database to sync
    pub tree_id: ID,
    /// Initial address hints where the peer might be found
    pub addresses: Vec<Address>,
    /// Optional authentication parameters for bootstrap
    pub auth: Option<AuthParams>,
    /// Optional display name for the peer
    pub display_name: Option<String>,
}

/// Handle for tracking sync status with a specific peer.
///
/// Returned by [`Sync::register_sync_peer()`].
#[derive(Debug, Clone)]
pub struct SyncHandle {
    tree_id: ID,
    peer_pubkey: String,
    sync: Sync,
}

impl SyncHandle {
    /// Get the current sync status.
    pub async fn status(&self) -> Result<SyncStatus> {
        self.sync.get_sync_status(&self.tree_id, &self.peer_pubkey).await
    }

    /// Add another address hint for this peer.
    pub async fn add_address(&self, address: Address) -> Result<()> {
        self.sync.add_peer_address(&self.peer_pubkey, address).await
    }

    /// Block until initial sync completes (has local data).
    ///
    /// This is a convenience method for backwards compatibility.
    /// The sync happens in the background, this just polls until data arrives.
    pub async fn wait_for_initial_sync(&self) -> Result<()> {
        loop {
            let status = self.status().await?;
            if status.has_local_data {
                return Ok(());
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
    }

    /// Get the tree ID being synced.
    pub fn tree_id(&self) -> &ID {
        &self.tree_id
    }

    /// Get the peer public key.
    pub fn peer_pubkey(&self) -> &str {
        &self.peer_pubkey
    }
}

/// Current sync status for a tree/peer pair.
#[derive(Debug, Clone)]
pub struct SyncStatus {
    /// Whether we have local data for this tree
    pub has_local_data: bool,
    /// Last time sync succeeded (if ever)
    pub last_sync: Option<SystemTime>,
    /// Last error encountered (if any)
    pub last_error: Option<String>,
}

/// Synchronization manager for the database.
///
/// The Sync module is a thin frontend that communicates with a background
/// sync engine thread via command channels. All actual sync operations, transport
/// communication, and state management happen in the background thread.
///
/// ## Multi-Transport Support
///
/// Multiple transports can be enabled simultaneously (e.g., HTTP + Iroh P2P),
/// allowing peers to be reachable via different networks. Requests are automatically
/// routed to the appropriate transport based on address type.
///
/// ```rust,ignore
/// // Enable both HTTP and Iroh transports
/// sync.enable_http_transport().await?;
/// sync.enable_iroh_transport().await?;
///
/// // Start servers on all transports
/// sync.start_server("127.0.0.1:0").await?;
///
/// // Get all server addresses
/// let addresses = sync.get_all_server_addresses().await?;
/// ```
#[derive(Debug)]
pub struct Sync {
    /// Communication channel to the background sync engine.
    /// Initialized when the first transport is enabled via `enable_*_transport()` or `add_transport()`.
    background_tx: OnceLock<mpsc::Sender<SyncCommand>>,
    /// The instance for read operations and tree management
    instance: WeakInstance,
    /// The tree containing synchronization settings
    sync_tree: Database,
}

impl Clone for Sync {
    fn clone(&self) -> Self {
        let background_tx = OnceLock::new();
        if let Some(tx) = self.background_tx.get() {
            let _ = background_tx.set(tx.clone());
        }
        Self {
            background_tx,
            instance: self.instance.clone(),
            sync_tree: self.sync_tree.clone(),
        }
    }
}

impl Sync {
    /// Create a new Sync instance with a dedicated settings tree.
    ///
    /// # Arguments
    /// * `instance` - The database instance for tree operations
    ///
    /// # Returns
    /// A new Sync instance with its own settings tree.
    pub async fn new(instance: Instance) -> Result<Self> {
        // Ensure device key exists in the backend
        // If no device key exists, generate one automatically
        let signing_key = match instance.backend().get_private_key(DEVICE_KEY_NAME).await? {
            Some(key) => key,
            None => {
                let (signing_key, _) = crate::auth::crypto::generate_keypair();
                instance
                    .backend()
                    .store_private_key(DEVICE_KEY_NAME, signing_key.clone())
                    .await?;
                signing_key
            }
        };

        let mut sync_settings = Doc::new();
        sync_settings.set("name", "_sync");
        sync_settings.set("type", "sync_settings");

        let sync_tree = Database::create(
            sync_settings,
            &instance,
            signing_key,
            DEVICE_KEY_NAME.to_string(),
        )
        .await?;

        let sync = Self {
            background_tx: OnceLock::new(),
            instance: instance.downgrade(),
            sync_tree,
        };

        // Initialize combined settings for all tracked users
        sync.initialize_user_settings().await?;

        Ok(sync)
    }

    /// Load an existing Sync instance from a sync tree root ID.
    ///
    /// # Arguments
    /// * `instance` - The database instance
    /// * `sync_tree_root_id` - The root ID of the existing sync tree
    ///
    /// # Returns
    /// A Sync instance loaded from the existing tree.
    pub async fn load(instance: Instance, sync_tree_root_id: &ID) -> Result<Self> {
        let device_key = instance
            .backend()
            .get_private_key(DEVICE_KEY_NAME)
            .await?
            .ok_or_else(|| SyncError::DeviceKeyNotFound {
                key_name: DEVICE_KEY_NAME.to_string(),
            })?;

        let sync_tree = Database::open(
            instance.handle(),
            sync_tree_root_id,
            device_key,
            DEVICE_KEY_NAME.to_string(),
        )?;

        let sync = Self {
            background_tx: OnceLock::new(),
            instance: instance.downgrade(),
            sync_tree,
        };

        // Initialize combined settings for all tracked users
        sync.initialize_user_settings().await?;

        Ok(sync)
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
    pub async fn set_setting(&self, key: impl Into<String>, value: impl Into<String>) -> Result<()> {
        let op = self.sync_tree.new_transaction().await?;
        let sync_settings = op.get_store::<DocStore>(SETTINGS_SUBTREE).await?;
        sync_settings.set(key, Value::Text(value.into())).await?;
        op.commit().await?;
        Ok(())
    }

    /// Retrieve a setting from the settings_map subtree.
    ///
    /// # Arguments
    /// * `key` - The setting key to retrieve
    ///
    /// # Returns
    /// The setting value if found, None otherwise.
    pub async fn get_setting(&self, key: impl AsRef<str>) -> Result<Option<String>> {
        let sync_settings = self
            .sync_tree
            .get_store_viewer::<DocStore>(SETTINGS_SUBTREE)
            .await?;
        match sync_settings.get_string(key).await {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Load a transport configuration from the `_sync` database.
    ///
    /// Transport configurations are stored in the `transports` subtree,
    /// keyed by their name. If no configuration exists for the transport,
    /// returns the default configuration.
    ///
    /// # Type Parameters
    /// * `T` - The transport configuration type implementing [`TransportConfig`]
    ///
    /// # Arguments
    /// * `name` - The name of the transport instance (e.g., "iroh", "http")
    ///
    /// # Returns
    /// The loaded configuration, or the default if not found.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use eidetica::sync::transports::iroh::IrohTransportConfig;
    ///
    /// let config: IrohTransportConfig = sync.load_transport_config("iroh")?;
    /// ```
    pub async fn load_transport_config<T: TransportConfig>(&self, name: &str) -> Result<T> {
        let tx = self.sync_tree.new_transaction().await?;
        let registry = Registry::new(&tx, TRANSPORTS_SUBTREE).await?;

        match registry.get_entry(name).await {
            Ok(entry) => {
                // Verify the type matches
                if entry.type_id != T::type_id() {
                    return Err(SyncError::TransportTypeMismatch {
                        name: name.to_string(),
                        expected: T::type_id().to_string(),
                        found: entry.type_id,
                    }
                    .into());
                }
                serde_json::from_str(&entry.config).map_err(|e| {
                    SyncError::SerializationError(format!(
                        "Failed to deserialize transport config '{name}': {e}"
                    ))
                    .into()
                })
            }
            Err(e) if e.is_not_found() => Ok(T::default()),
            Err(e) => Err(e),
        }
    }

    /// Save a transport configuration to the `_sync` database.
    ///
    /// Transport configurations are stored in the `transports` subtree,
    /// keyed by their name. This persists the configuration so it can
    /// be loaded on subsequent startups.
    ///
    /// # Type Parameters
    /// * `T` - The transport configuration type implementing [`TransportConfig`]
    ///
    /// # Arguments
    /// * `name` - The name of the transport instance (e.g., "iroh", "http")
    /// * `config` - The configuration to save
    ///
    /// # Example
    ///
    /// ```ignore
    /// use eidetica::sync::transports::iroh::IrohTransportConfig;
    ///
    /// let mut config = IrohTransportConfig::default();
    /// config.get_or_create_secret_key(); // Generate key
    /// sync.save_transport_config("iroh", &config)?;
    /// ```
    pub async fn save_transport_config<T: TransportConfig>(&self, name: &str, config: &T) -> Result<()> {
        let json = serde_json::to_string(config).map_err(|e| {
            SyncError::SerializationError(format!(
                "Failed to serialize transport config '{name}': {e}"
            ))
        })?;
        let tx = self.sync_tree.new_transaction().await?;
        let registry = Registry::new(&tx, TRANSPORTS_SUBTREE).await?;
        registry.set_entry(name, T::type_id(), json).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Upgrade the weak instance reference to a strong reference.
    ///
    /// # Returns
    /// A `Result` containing the Instance or an error if the Instance has been dropped.
    pub fn instance(&self) -> Result<Instance> {
        self.instance
            .upgrade()
            .ok_or_else(|| SyncError::InstanceDropped.into())
    }

    /// Get a reference to the underlying backend.
    pub fn backend(&self) -> Result<Backend> {
        Ok(self.instance()?.backend().handle())
    }

    /// Get a reference to the sync settings tree.
    pub fn sync_tree(&self) -> &Database {
        &self.sync_tree
    }

    /// Get the device ID for this sync instance.
    ///
    /// The device ID is the device's public key in ed25519:base64 format.
    pub async fn get_device_id(&self) -> Result<String> {
        self.get_device_public_key().await
    }

    /// Get the device public key for this sync instance.
    ///
    /// # Returns
    /// The device's public key in ed25519:base64 format.
    pub async fn get_device_public_key(&self) -> Result<String> {
        let signing_key = self.get_device_signing_key().await?;
        let verifying_key = signing_key.verifying_key();
        Ok(format_public_key(&verifying_key))
    }

    /// Get the device signing key for cryptographic operations.
    ///
    /// # Returns
    /// The device's private signing key if available.
    pub(crate) async fn get_device_signing_key(&self) -> Result<ed25519_dalek::SigningKey> {
        let backend = self.backend()?;
        backend.get_private_key(DEVICE_KEY_NAME).await?.ok_or_else(|| {
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
    pub async fn register_peer(
        &self,
        pubkey: impl Into<String>,
        display_name: Option<&str>,
    ) -> Result<()> {
        let pubkey_str = pubkey.into();

        // Store in sync tree via PeerManager
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op).register_peer(&pubkey_str, display_name).await?;
        op.commit().await?;

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
    pub async fn update_peer_status(&self, pubkey: impl AsRef<str>, status: PeerStatus) -> Result<()> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op).update_peer_status(pubkey.as_ref(), status).await?;
        op.commit().await?;
        Ok(())
    }

    /// Get information about a registered peer.
    ///
    /// # Arguments
    /// * `pubkey` - The peer's public key
    ///
    /// # Returns
    /// The peer information if found, None otherwise.
    pub async fn get_peer_info(&self, pubkey: impl AsRef<str>) -> Result<Option<PeerInfo>> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op).get_peer_info(pubkey.as_ref()).await
        // No commit - just reading
    }

    /// List all registered peers.
    ///
    /// # Returns
    /// A vector of all registered peer information.
    pub async fn list_peers(&self) -> Result<Vec<PeerInfo>> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op).list_peers().await
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
    pub async fn remove_peer(&self, pubkey: impl AsRef<str>) -> Result<()> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op).remove_peer(pubkey.as_ref()).await?;
        op.commit().await?;
        Ok(())
    }

    // === Declarative Sync API ===

    /// Register a peer for syncing (declarative API).
    ///
    /// This is the recommended way to set up syncing. It immediately registers
    /// the peer and tree/peer relationship, then the background sync engine
    /// handles the actual data synchronization.
    ///
    /// # Arguments
    /// * `info` - Information about the peer and sync configuration
    ///
    /// # Returns
    /// A handle for tracking sync status and adding more address hints.
    ///
    /// # Example
    /// ```no_run
    /// # use eidetica::*;
    /// # use eidetica::sync::{SyncPeerInfo, Address, AuthParams};
    /// # async fn example(sync: sync::Sync, peer_pubkey: String, tree_id: entry::ID) -> Result<()> {
    /// // Register peer for syncing
    /// let handle = sync.register_sync_peer(SyncPeerInfo {
    ///     peer_pubkey,
    ///     tree_id,
    ///     addresses: vec![Address {
    ///         transport_type: "http".to_string(),
    ///         address: "http://localhost:8080".to_string(),
    ///     }],
    ///     auth: None,
    ///     display_name: Some("My Peer".to_string()),
    /// })?;
    ///
    /// // Optionally wait for initial sync
    /// handle.wait_for_initial_sync().await?;
    ///
    /// // Check status anytime
    /// let status = handle.status()?;
    /// println!("Has local data: {}", status.has_local_data);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn register_sync_peer(&self, info: SyncPeerInfo) -> Result<SyncHandle> {
        let op = self.sync_tree.new_transaction().await?;
        let peer_mgr = PeerManager::new(&op);

        // Register peer if it doesn't exist
        if peer_mgr.get_peer_info(&info.peer_pubkey).await?.is_none() {
            peer_mgr.register_peer(&info.peer_pubkey, info.display_name.as_deref()).await?;
        }

        // Add all address hints
        for addr in &info.addresses {
            peer_mgr.add_address(&info.peer_pubkey, addr.clone()).await?;
        }

        // Register the tree/peer relationship
        peer_mgr.add_tree_sync(&info.peer_pubkey, &info.tree_id).await?;

        // TODO: Store auth params if provided for bootstrap
        // For now, auth is passed during the actual sync handshake via on_local_write callback

        op.commit().await?;

        info!(
            peer = %info.peer_pubkey,
            tree = %info.tree_id,
            address_count = info.addresses.len(),
            "Registered peer for syncing"
        );

        Ok(SyncHandle {
            tree_id: info.tree_id,
            peer_pubkey: info.peer_pubkey,
            sync: self.clone(),
        })
    }

    /// Get the current sync status for a tree/peer pair.
    ///
    /// # Arguments
    /// * `tree_id` - The tree to check
    /// * `peer_pubkey` - The peer public key
    ///
    /// # Returns
    /// Current sync status including whether we have local data.
    pub async fn get_sync_status(&self, tree_id: &ID, _peer_pubkey: &str) -> Result<SyncStatus> {
        // Check if we have local data for this tree
        let backend = self.backend()?;
        let our_tips = backend.get_tips(tree_id).await.unwrap_or_default();

        // TODO: Track last_sync time and last_error in sync tree
        // For now, just report if we have data
        Ok(SyncStatus {
            has_local_data: !our_tips.is_empty(),
            last_sync: None,
            last_error: None,
        })
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
    pub async fn add_tree_sync(
        &self,
        peer_pubkey: impl AsRef<str>,
        tree_root_id: impl AsRef<str>,
    ) -> Result<()> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op).add_tree_sync(peer_pubkey.as_ref(), tree_root_id.as_ref()).await?;
        op.commit().await?;
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
    pub async fn remove_tree_sync(
        &self,
        peer_pubkey: impl AsRef<str>,
        tree_root_id: impl AsRef<str>,
    ) -> Result<()> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op).remove_tree_sync(peer_pubkey.as_ref(), tree_root_id.as_ref()).await?;
        op.commit().await?;
        Ok(())
    }

    /// Get the list of trees synced with a peer.
    ///
    /// # Arguments
    /// * `peer_pubkey` - The peer's public key
    ///
    /// # Returns
    /// A vector of tree root IDs synced with this peer.
    pub async fn get_peer_trees(&self, peer_pubkey: impl AsRef<str>) -> Result<Vec<String>> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op).get_peer_trees(peer_pubkey.as_ref()).await
        // No commit - just reading
    }

    /// Get all peers that sync a specific tree.
    ///
    /// # Arguments
    /// * `tree_root_id` - The root ID of the tree
    ///
    /// # Returns
    /// A vector of peer public keys that sync this tree.
    pub async fn get_tree_peers(&self, tree_root_id: impl AsRef<str>) -> Result<Vec<String>> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op).get_tree_peers(tree_root_id.as_ref()).await
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
    pub async fn connect_to_peer(&self, address: &Address) -> Result<String> {
        let (tx, rx) = oneshot::channel();

        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
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
    pub async fn update_peer_connection_state(
        &self,
        pubkey: impl AsRef<str>,
        state: ConnectionState,
    ) -> Result<()> {
        let op = self.sync_tree.new_transaction().await?;
        let peer_manager = PeerManager::new(&op);

        // Get current peer info
        let mut peer_info = match peer_manager.get_peer_info(pubkey.as_ref()).await? {
            Some(info) => info,
            None => return Err(SyncError::PeerNotFound(pubkey.as_ref().to_string()).into()),
        };

        // Update connection state
        peer_info.connection_state = state;
        peer_info.touch();

        // Save updated peer info
        peer_manager.update_peer_info(pubkey.as_ref(), peer_info).await?;
        op.commit().await?;
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
    pub async fn is_tree_synced_with_peer(
        &self,
        peer_pubkey: impl AsRef<str>,
        tree_root_id: impl AsRef<str>,
    ) -> Result<bool> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op).is_tree_synced_with_peer(peer_pubkey.as_ref(), tree_root_id.as_ref()).await
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
    pub async fn add_peer_address(&self, peer_pubkey: impl AsRef<str>, address: Address) -> Result<()> {
        // Update sync tree via PeerManager
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op).add_address(peer_pubkey.as_ref(), address).await?;
        op.commit().await?;

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
    pub async fn remove_peer_address(
        &self,
        peer_pubkey: impl AsRef<str>,
        address: &Address,
    ) -> Result<bool> {
        let op = self.sync_tree.new_transaction().await?;
        let result = PeerManager::new(&op).remove_address(peer_pubkey.as_ref(), address).await?;
        op.commit().await?;
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
    pub async fn get_peer_addresses(
        &self,
        peer_pubkey: impl AsRef<str>,
        transport_type: Option<&str>,
    ) -> Result<Vec<Address>> {
        let op = self.sync_tree.new_transaction().await?;
        PeerManager::new(&op).get_addresses(peer_pubkey.as_ref(), transport_type).await
        // No commit - just reading
    }

    // === User Synchronization Methods ===

    /// Synchronize a user's preferences with the sync system.
    ///
    /// This establishes tracking for a user's preferences database and synchronizes
    /// their current preferences to the sync tree. The sync system will monitor the
    /// user's preferences and automatically sync databases according to their settings.
    ///
    /// This method ensures the user is tracked and reads their preferences database
    /// to update sync configuration. It detects changes via tip comparison and only
    /// processes updates when preferences have changed.
    ///
    /// This operation is idempotent and can be called multiple times safely.
    ///
    /// **CRITICAL**: All updates to the sync tree happen in a single transaction
    /// to ensure atomicity.
    ///
    /// # Arguments
    /// * `user_uuid` - The user's unique identifier
    /// * `preferences_db_id` - The ID of the user's private database
    ///
    /// # Returns
    /// A Result indicating success or an error.
    ///
    /// # Example
    /// ```rust,ignore
    /// // After creating or logging in a user
    /// let user = instance.login_user("alice", Some("password"))?;
    /// sync.sync_user(user.user_uuid(), user.user_database().root_id())?;
    /// ```
    pub async fn sync_user(
        &self,
        user_uuid: impl AsRef<str>,
        preferences_db_id: &crate::entry::ID,
    ) -> Result<()> {
        use crate::store::Table;
        use crate::user::types::TrackedDatabase;

        let user_uuid_str = user_uuid.as_ref();

        // CRITICAL: Single transaction for all sync tree updates
        let tx = self.sync_tree.new_transaction().await?;
        let user_mgr = UserSyncManager::new(&tx);

        // Ensure user is tracked, get their current preferences state
        let old_tips = match user_mgr.get_tracked_user_state(user_uuid_str).await? {
            Some((_stored_prefs_db_id, tips)) => tips,
            None => {
                // User not yet tracked - register them
                user_mgr
                    .track_user_preferences(user_uuid_str, preferences_db_id)
                    .await?;
                Vec::new() // Empty tips means this is first sync
            }
        };

        // Open user's preferences database (read-only)
        let instance = self.instance.upgrade().ok_or(SyncError::InstanceDropped)?;
        let prefs_db = crate::Database::open_readonly(preferences_db_id.clone(), &instance)?;
        let current_tips = prefs_db.get_tips().await?;

        // Check if preferences have changed via tip comparison
        if current_tips == old_tips {
            debug!(user_uuid = %user_uuid_str, "No changes to user preferences, skipping update");
            return Ok(());
        }

        debug!(user_uuid = %user_uuid_str, "User preferences changed, updating sync configuration");

        // Read all tracked databases
        let databases_table = prefs_db
            .get_store_viewer::<Table<TrackedDatabase>>("databases")
            .await?;
        let all_tracked = databases_table.search(|_| true).await?; // Get all entries

        // Get databases user previously tracked
        let old_databases = user_mgr.get_linked_databases(user_uuid_str).await?;

        // Build set of current database IDs
        let current_databases: std::collections::HashSet<_> = all_tracked
            .iter()
            .map(|(_uuid, tracked)| tracked)
            .filter(|t| t.sync_settings.sync_enabled)
            .map(|t| t.database_id.clone())
            .collect();

        // Track which databases need settings recomputation
        let mut affected_databases = std::collections::HashSet::new();

        // Remove user from databases they no longer track
        for old_db in &old_databases {
            if !current_databases.contains(old_db) {
                user_mgr
                    .unlink_user_from_database(old_db, user_uuid_str)
                    .await?;
                affected_databases.insert(old_db.clone());
                debug!(user_uuid = %user_uuid_str, database_id = %old_db, "Removed user from database");
            }
        }

        // Add/update user for current databases
        for (_uuid, tracked) in &all_tracked {
            if tracked.sync_settings.sync_enabled {
                user_mgr
                    .link_user_to_database(&tracked.database_id, user_uuid_str)
                    .await?;
                affected_databases.insert(tracked.database_id.clone());
            }
        }

        // Recompute combined settings for all affected databases
        let affected_count = affected_databases.len();
        for db_id in affected_databases {
            let users = user_mgr.get_linked_users(&db_id).await?;

            if users.is_empty() {
                // No users tracking this database, remove settings
                continue;
            }

            // Collect settings from all users tracking this database
            let instance = self.instance.upgrade().ok_or(SyncError::InstanceDropped)?;
            let mut settings_list = Vec::new();
            for uuid in &users {
                // Read preferences from each user's database
                if let Some((user_prefs_db_id, _)) =
                    user_mgr.get_tracked_user_state(uuid).await?
                {
                    let user_db = crate::Database::open_readonly(user_prefs_db_id, &instance)?;
                    let user_table = user_db
                        .get_store_viewer::<Table<TrackedDatabase>>("databases")
                        .await?;

                    // Find this database's settings
                    for (_key, tracked) in user_table.search(|_| true).await? {
                        if tracked.database_id == db_id && tracked.sync_settings.sync_enabled {
                            settings_list.push(tracked.sync_settings.clone());
                            break;
                        }
                    }
                }
            }

            // Merge settings using most aggressive strategy
            if !settings_list.is_empty() {
                let combined = crate::instance::settings_merge::merge_sync_settings(settings_list);
                user_mgr.set_combined_settings(&db_id, &combined).await?;
                debug!(database_id = %db_id, "Updated combined settings for database");
            }
        }

        // Update stored tips to reflect processed state
        user_mgr.update_tracked_tips(user_uuid_str, &current_tips).await?;

        // Commit all changes atomically
        tx.commit().await?;

        info!(user_uuid = %user_uuid_str, affected_count = affected_count, "Updated user database sync configuration");
        Ok(())
    }

    /// Remove a user from the sync system.
    ///
    /// Removes all tracking for this user and updates affected databases'
    /// combined settings. This should be called when a user is deleted.
    ///
    /// # Arguments
    /// * `user_uuid` - The user's unique identifier
    ///
    /// # Returns
    /// A Result indicating success or an error.
    pub async fn remove_user(&self, user_uuid: impl AsRef<str>) -> Result<()> {
        let user_uuid_str = user_uuid.as_ref();
        let tx = self.sync_tree.new_transaction().await?;
        let user_mgr = UserSyncManager::new(&tx);

        // Get all databases this user was tracking
        let databases = user_mgr.get_linked_databases(user_uuid_str).await?;

        // Remove user from each database
        for db_id in &databases {
            user_mgr.unlink_user_from_database(db_id, user_uuid_str).await?;

            // Recompute combined settings for this database
            let remaining_users = user_mgr.get_linked_users(db_id).await?;
            if remaining_users.is_empty() {
                // No more users, settings will be cleared automatically
                continue;
            }

            // Recompute settings from remaining users
            // (simplified - in practice would read each user's preferences)
            // For now, just note that settings need updating
            debug!(database_id = %db_id, "Database needs settings recomputation after user removal");
        }

        tx.commit().await?;

        info!(user_uuid = %user_uuid_str, database_count = databases.len(), "Removed user from sync system");
        Ok(())
    }

    // === Network Transport Methods ===

    /// Start a sync server on the specified address (async version).
    ///
    /// # Arguments
    /// * `addr` - The address to bind the server to (e.g., "127.0.0.1:8080")
    ///
    /// # Returns
    /// A Result indicating success or failure of server startup.
    pub async fn start_server(&self, addr: &str) -> Result<()> {
        let (tx, rx) = oneshot::channel();

        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::StartServer {
                transport_type: None, // Start on all transports
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
    /// Stops servers on all transports.
    ///
    /// # Returns
    /// A Result indicating success or failure of server shutdown.
    pub async fn stop_server(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();

        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::StopServer {
                transport_type: None, // Stop all transports
                response: tx,
            })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        rx.await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))?
    }

    /// Enable HTTP transport for network communication.
    ///
    /// Can be called multiple times to add HTTP transport alongside other transports.
    pub async fn enable_http_transport(&self) -> Result<()> {
        let transport = HttpTransport::new()?;
        self.add_transport(Box::new(transport)).await
    }

    /// Enable Iroh transport for peer-to-peer network communication.
    ///
    /// This initializes the Iroh transport layer with production defaults (n0's relay servers).
    /// Can be called alongside other transports for multi-transport support.
    ///
    /// The transport's secret key is automatically persisted to the `_sync` database,
    /// ensuring the node maintains a stable identity (and thus address) across restarts.
    /// On first call, a new secret key is generated and saved. On subsequent calls,
    /// the existing key is loaded from storage.
    pub async fn enable_iroh_transport(&self) -> Result<()> {
        // Load existing config or create default
        let mut config: IrohTransportConfig =
            self.load_transport_config(IrohTransport::TRANSPORT_TYPE).await?;

        // Track if config will change (no key yet means one will be generated)
        let config_changed = !config.has_secret_key();

        // Get the secret key (generates if not present)
        let secret_key = config.get_or_create_secret_key();

        // Only save config if it changed (new key was generated)
        if config_changed {
            self.save_transport_config(IrohTransport::TRANSPORT_TYPE, &config).await?;
        }

        // Build transport with the persistent secret key
        let transport = IrohTransport::builder()
            .secret_key(secret_key)
            .relay_mode(config.relay_mode.into())
            .build()?;

        self.add_transport(Box::new(transport)).await
    }

    /// Enable Iroh transport with custom configuration.
    ///
    /// This allows specifying custom relay modes, discovery options, etc.
    /// Use IrohTransport::builder() to create a configured transport.
    /// Can be called alongside other transports for multi-transport support.
    pub async fn enable_iroh_transport_with_config(&self, transport: IrohTransport) -> Result<()> {
        self.add_transport(Box::new(transport)).await
    }

    /// Add a transport to the sync system.
    ///
    /// Multiple transports can be added. The first transport starts the background
    /// sync engine, subsequent transports are added to the existing engine.
    /// This is useful for testing and advanced configuration scenarios.
    pub async fn add_transport(&self, transport: Box<dyn SyncTransport>) -> Result<()> {
        if self.background_tx.get().is_none() {
            // First transport - start background sync
            return self.start_background_sync(transport);
        }

        // Background sync already running, send command to add transport
        let (tx, rx) = oneshot::channel();

        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::AddTransport {
                transport,
                response: tx,
            })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        rx.await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))?
    }

    /// Start the background sync engine with the given transport.
    ///
    /// This is called for the first transport only.
    fn start_background_sync(&self, transport: Box<dyn SyncTransport>) -> Result<()> {
        let sync_tree_id = self.sync_tree.root_id().clone();
        let instance = self.instance()?;

        // Create the background sync and get command sender
        let background_tx = BackgroundSync::start(transport, instance, sync_tree_id);

        // Initialize the command channel (can only be done once)
        self.background_tx
            .set(background_tx)
            .map_err(|_| SyncError::ServerAlreadyRunning {
                address: "command channel already initialized".to_string(),
            })?;

        Ok(())
    }

    /// Get a server address if any transport is running a server.
    ///
    /// For backward compatibility, returns the first available server address.
    /// Use `get_server_address_for_transport` for specific transports.
    ///
    /// # Returns
    /// The address of the first running server, or an error if no server is running.
    pub async fn get_server_address(&self) -> Result<String> {
        let addresses = self.get_all_server_addresses().await?;
        addresses
            .into_iter()
            .next()
            .map(|(_, addr)| addr)
            .ok_or_else(|| SyncError::ServerNotRunning.into())
    }

    /// Get the server address for a specific transport.
    ///
    /// # Arguments
    /// * `transport_type` - The transport type (e.g., "http", "iroh")
    ///
    /// # Returns
    /// The address the server is bound to for that transport.
    pub async fn get_server_address_for_transport(&self, transport_type: &str) -> Result<String> {
        let (tx, rx) = oneshot::channel();

        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::GetServerAddress {
                transport_type: transport_type.to_string(),
                response: tx,
            })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        rx.await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))?
    }

    /// Get all server addresses for running transports.
    ///
    /// # Returns
    /// A vector of (transport_type, address) pairs for all running servers.
    pub async fn get_all_server_addresses(&self) -> Result<Vec<(String, String)>> {
        let (tx, rx) = oneshot::channel();

        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::GetAllServerAddresses { response: tx })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        rx.await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))?
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
            .get_peer_info(peer_pubkey).await?
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
        let our_device_pubkey = self.get_device_public_key().await.ok();

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
    async fn handle_bootstrap_response(&self, response: protocol::BootstrapResponse) -> Result<()> {
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
            let entries_for_server = crate::sync::utils::collect_ancestors_to_send(
                backend.as_backend_impl(),
                &missing_tip_ids,
                their_tips,
            ).await?;

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
    /// * `peer_pubkey` - The public key of the peer to send to
    /// * `entries` - The specific entries to send (no filtering applied)
    ///
    /// # Returns
    /// A Result indicating whether the command was successfully queued for background processing.
    pub async fn send_entries_to_peer(&self, peer_pubkey: &str, entries: Vec<Entry>) -> Result<()> {
        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::SendEntries {
                peer: peer_pubkey.to_string(),
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
    /// Ok(()) if the entry was successfully queued or the queue was full.
    /// Only returns Err if transport is not enabled.
    pub fn queue_entry_for_sync(
        &self,
        peer_pubkey: &str,
        entry_id: &ID,
        tree_id: &ID,
    ) -> Result<()> {
        let background_tx = self
            .background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?;

        let command = SyncCommand::QueueEntry {
            peer: peer_pubkey.to_string(),
            entry_id: entry_id.clone(),
            tree_id: tree_id.clone(),
        };

        // Use try_send for non-blocking operation
        // Log errors but don't fail since this is called during commit
        if let Err(e) = background_tx.try_send(command) {
            tracing::error!(
                "Failed to queue entry {:?} for sync with peer {}: {}",
                entry_id,
                peer_pubkey,
                e
            );
        }

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

        for peer_pubkey in peers {
            self.queue_entry_for_sync(&peer_pubkey, &entry_id, tree_id)?;
        }

        Ok(())
    }

    /// Initialize combined settings for all users.
    ///
    /// This is called during Sync initialization. For new sync trees (just created),
    /// it scans the _users database to register all existing users. For existing
    /// sync trees (loaded), it updates combined settings for already-tracked users.
    async fn initialize_user_settings(&self) -> Result<()> {
        use crate::store::{DocStore, Table};
        use crate::user::types::UserInfo;

        // Check if sync tree is freshly created (no users tracked yet)
        let user_tracking = self
            .sync_tree
            .get_store_viewer::<DocStore>(user_sync_manager::USER_TRACKING_SUBTREE)
            .await?;
        let all_tracked = user_tracking.get_all().await?;

        if all_tracked.keys().count() == 0 {
            // New sync tree - register all users from _users database
            let instance = self.instance.upgrade().ok_or(SyncError::InstanceDropped)?;
            let users_db = instance.users_db().await?;
            let users_table = users_db.get_store_viewer::<Table<UserInfo>>("users").await?;
            let all_users = users_table.search(|_| true).await?;

            for (user_uuid, user_info) in all_users {
                self.sync_user(&user_uuid, &user_info.user_database_id).await?;
            }
        } else {
            // Existing sync tree - update settings for tracked users if changed
            let tx = self.sync_tree.new_transaction().await?;
            let user_mgr = UserSyncManager::new(&tx);

            for user_uuid in all_tracked.keys() {
                if let Some((prefs_db_id, _tips)) = user_mgr.get_tracked_user_state(user_uuid).await?
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
    async fn send_request(&self, request: &SyncRequest, address: &Address) -> Result<SyncResponse> {
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
    /// * `peer_address` - The address of the peer to connect to (format: "host:port")
    ///
    /// # Returns
    /// A vector of TreeInfo describing available trees, or an error.
    pub async fn discover_peer_trees(&self, peer_address: &str) -> Result<Vec<protocol::TreeInfo>> {
        use peer_types::Address;

        let address = Address {
            transport_type: HttpTransport::TRANSPORT_TYPE.to_string(),
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
    /// * `peer_address` - The address of the peer (format: "host:port")
    /// * `tree_id` - Optional tree ID to sync (None = discover available trees)
    ///
    /// # Returns
    /// Result indicating success or failure.
    pub async fn sync_with_peer(
        &self,
        peer_address: &str,
        tree_id: Option<&crate::entry::ID>,
    ) -> Result<()> {
        use peer_types::Address;

        // Auto-detect transport type from address format
        let address = if peer_address.starts_with('{') || peer_address.contains("\"node_id\"") {
            // JSON format indicates Iroh NodeAddr
            Address {
                transport_type: IrohTransport::TRANSPORT_TYPE.to_string(),
                address: peer_address.to_string(),
            }
        } else {
            // Default to HTTP for traditional host:port format
            Address {
                transport_type: HttpTransport::TRANSPORT_TYPE.to_string(),
                address: peer_address.to_string(),
            }
        };

        // Connect to peer if not already connected
        let peer_pubkey = self.connect_to_peer(&address).await?;

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
            .get_peer_info(peer_pubkey).await?
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
        let our_device_pubkey = self.get_device_public_key().await.ok();

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
        &self,
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

        // Auto-detect transport type from address format
        let address = if peer_address.starts_with('{') || peer_address.contains("\"node_id\"") {
            // JSON format indicates Iroh NodeAddr
            Address {
                transport_type: IrohTransport::TRANSPORT_TYPE.to_string(),
                address: peer_address.to_string(),
            }
        } else {
            // Default to HTTP for traditional host:port format
            Address {
                transport_type: HttpTransport::TRANSPORT_TYPE.to_string(),
                address: peer_address.to_string(),
            }
        };

        // Connect to peer if not already connected
        let peer_pubkey = self.connect_to_peer(&address).await?;

        // Store the address for this peer
        self.add_peer_address(&peer_pubkey, address.clone()).await?;

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
        &self,
        peer_address: &str,
        tree_id: &crate::entry::ID,
        requesting_key_name: &str,
        requested_permission: crate::auth::Permission,
    ) -> Result<()> {
        // Get our public key for the requesting key from backend
        let backend = self.backend()?;
        let signing_key = backend
            .get_private_key(requesting_key_name)
            .await?
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
        &self,
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
    pub async fn pending_bootstrap_requests(&self) -> Result<Vec<(String, BootstrapRequest)>> {
        let op = self.sync_tree.new_transaction().await?;
        let manager = BootstrapRequestManager::new(&op);
        manager.pending_requests().await
    }

    /// Get all approved bootstrap requests.
    ///
    /// # Returns
    /// A vector of (request_id, bootstrap_request) pairs for approved requests.
    pub async fn approved_bootstrap_requests(&self) -> Result<Vec<(String, BootstrapRequest)>> {
        let op = self.sync_tree.new_transaction().await?;
        let manager = BootstrapRequestManager::new(&op);
        manager.approved_requests().await
    }

    /// Get all rejected bootstrap requests.
    ///
    /// # Returns
    /// A vector of (request_id, bootstrap_request) pairs for rejected requests.
    pub async fn rejected_bootstrap_requests(&self) -> Result<Vec<(String, BootstrapRequest)>> {
        let op = self.sync_tree.new_transaction().await?;
        let manager = BootstrapRequestManager::new(&op);
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
        let op = self.sync_tree.new_transaction().await?;
        let manager = BootstrapRequestManager::new(&op);

        match manager.get_request(request_id).await? {
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
    pub async fn approve_bootstrap_request(
        &self,
        request_id: &str,
        approving_key_name: &str,
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

        // Load target database with the approving key
        let backend = self.backend()?;
        let approving_signing_key = backend
            .get_private_key(approving_key_name)
            .await?
            .ok_or_else(|| {
                SyncError::BackendError(format!(
                    "Approving key not found: {approving_key_name}"
                ))
            })?;

        let database = Database::open(
            self.instance()?,
            &request.tree_id,
            approving_signing_key,
            approving_key_name.to_string(),
        )?;
        let tx = database.new_transaction().await?;

        // Get settings store and update auth configuration using SettingsStore API
        let settings_store = SettingsStore::new(&tx)?;

        // Create the auth key for the requesting device
        let auth_key = AuthKey::active(
            request.requesting_pubkey.clone(),
            request.requested_permission.clone(),
        )?;

        // Add the new key to auth settings using SettingsStore API
        // This provides proper upsert behavior and validation
        settings_store.set_auth_key(&request.requesting_key_name, auth_key).await?;

        tx.commit().await?;

        // Update request status to approved
        let approval_time = bootstrap_request_manager::current_timestamp();
        manager
            .update_status(
                request_id,
                RequestStatus::Approved {
                    approved_by: approving_key_name.to_string(),
                    approval_time,
                },
            )
            .await?;
        sync_op.commit().await?;

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
    pub async fn approve_bootstrap_request_with_key(
        &self,
        request_id: &str,
        approving_signing_key: &ed25519_dalek::SigningKey,
        approving_sigkey: &str,
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
        let database = Database::open(
            self.instance()?,
            &request.tree_id,
            approving_signing_key.clone(),
            approving_sigkey.to_string(),
        )?;

        // Explicitly check that the approving user has Admin permission
        // This provides clear error messages and fails fast before modifying the database
        let permission = database.get_sigkey_permission(approving_sigkey).await?;
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
        let auth_key = AuthKey::active(
            request.requesting_pubkey.clone(),
            request.requested_permission.clone(),
        )?;

        // Add the new key to auth settings using SettingsStore API
        // This provides proper upsert behavior and validation
        settings_store.set_auth_key(&request.requesting_key_name, auth_key).await?;

        // Commit will validate that the user's key has Admin permission
        // If this fails, it means the user lacks the necessary permission
        tx.commit().await?;

        // Update request status to approved
        let approval_time = bootstrap_request_manager::current_timestamp();
        manager
            .update_status(
                request_id,
                RequestStatus::Approved {
                    approved_by: approving_sigkey.to_string(),
                    approval_time,
                },
            )
            .await?;
        sync_op.commit().await?;

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
    pub async fn reject_bootstrap_request(
        &self,
        request_id: &str,
        rejecting_key_name: &str,
    ) -> Result<()> {
        let op = self.sync_tree.new_transaction().await?;
        let manager = BootstrapRequestManager::new(&op);

        // Validate request exists and is pending
        let request = manager
            .get_request(request_id)
            .await?
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
        manager
            .update_status(
                request_id,
                RequestStatus::Rejected {
                    rejected_by: rejecting_key_name.to_string(),
                    rejection_time,
                },
            )
            .await?;
        op.commit().await?;

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
    pub async fn reject_bootstrap_request_with_key(
        &self,
        request_id: &str,
        rejecting_signing_key: &ed25519_dalek::SigningKey,
        rejecting_sigkey: &str,
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
        let database = Database::open(
            self.instance()?,
            &request.tree_id,
            rejecting_signing_key.clone(),
            rejecting_sigkey.to_string(),
        )?;

        // Check that the rejecting user has Admin permission
        let permission = database.get_sigkey_permission(rejecting_sigkey).await?;
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
        manager
            .update_status(
                request_id,
                RequestStatus::Rejected {
                    rejected_by: rejecting_sigkey.to_string(),
                    rejection_time,
                },
            )
            .await?;
        sync_op.commit().await?;

        info!(
            request_id = %request_id,
            tree_id = %request.tree_id,
            rejected_by = %rejecting_sigkey,
            "Bootstrap request rejected by user with Admin permission"
        );

        Ok(())
    }
}

impl Sync {
    // === Test Helpers ===
}
