//! Synchronization module for Eidetica database.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! // Enable sync
//! instance.enable_sync().await?;
//! let sync = instance.sync().unwrap();
//!
//! // Register transports with their configurations
//! sync.register_transport("http", HttpTransport::builder()
//!     .bind("127.0.0.1:8080")
//! ).await?;
//! sync.register_transport("p2p", IrohTransport::builder()).await?;
//!
//! // Start accepting incoming connections
//! sync.accept_connections().await?;
//!
//! // Outbound sync works via the registered transports
//! sync.sync_with_peer(&Address::http("peer:8080"), Some(&tree_id)).await?;
//! ```
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
//! # Connection Model
//!
//! The sync system separates **outbound** and **inbound** connection handling:
//!
//! - **Outbound** ([`Sync::sync_with_peer`]): Works after registering transports via
//!   [`Sync::register_transport`]. Each transport can be configured via its builder.
//! - **Inbound** ([`Sync::accept_connections`]): Must be explicitly called each time
//!   the instance starts. Starts servers on all registered transports.
//!
//! This design provides security by default. Nodes don't accept incoming connections
//! unless explicitly opted in.
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
//! Transports are registered via [`Sync::register_transport`] with their builders.
//! State (like Iroh node identity) is automatically persisted per named instance.
//! Both implement the [`transports::SyncTransport`] trait.
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

use std::sync::{Arc, OnceLock};
use std::time::SystemTime;
use tokio::sync::mpsc;

use handle_trait::Handle;

use crate::{
    Database, Instance, Result, WeakInstance,
    auth::Permission,
    crdt::{Doc, doc::Value},
    database::DatabaseKey,
    entry::ID,
    instance::backend::Backend,
    store::{DocStore, Registry},
};

// Public submodules
pub mod background;
pub mod error;
pub mod handler;
mod handler_tree_ops;
pub mod peer_manager;
pub mod peer_types;
pub mod protocol;
pub mod state;
pub mod ticket;
pub mod transports;
pub mod utils;

// Private submodules
mod bootstrap;
mod bootstrap_request_manager;
mod ops;
mod peer;
mod queue;
mod transport;
mod transport_manager;
mod user;
mod user_sync_manager;

// Re-exports
use background::SyncCommand;
pub use bootstrap_request_manager::{BootstrapRequest, RequestStatus};
pub use error::SyncError;
pub use peer_types::{Address, ConnectionState, PeerId, PeerInfo, PeerStatus};
use queue::SyncQueue;
pub use ticket::DatabaseTicket;
use transports::TransportConfig;

/// Private constant for the sync settings subtree name
const SETTINGS_SUBTREE: &str = "settings_map";

/// Private constant for the transports registry subtree name
const TRANSPORTS_SUBTREE: &str = "transports";

/// Private constant for the transport state store name (persisted identity/state per transport instance)
const TRANSPORT_STATE_STORE: &str = "transport_state";

/// Authentication parameters for sync operations.
#[derive(Debug, Clone)]
pub struct AuthParams {
    /// The public key making the request
    pub requesting_key: String,
    /// The name/ID of the requesting key
    pub requesting_key_name: String,
    /// The permission level being requested
    pub requested_permission: Permission,
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
        self.sync
            .get_sync_status(&self.tree_id, &self.peer_pubkey)
            .await
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
    /// Queue for entries pending synchronization
    queue: Arc<SyncQueue>,
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
            queue: Arc::clone(&self.queue),
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
        // Get device key from instance
        let signing_key = instance.device_key().clone();

        let mut sync_settings = Doc::new();
        sync_settings.set("name", "_sync");
        sync_settings.set("type", "sync_settings");

        let sync_tree = Database::create(&instance, signing_key, sync_settings).await?;

        let sync = Self {
            background_tx: OnceLock::new(),
            instance: instance.downgrade(),
            sync_tree,
            queue: Arc::new(SyncQueue::new()),
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
        let device_key = instance.device_key().clone();

        let sync_tree = Database::open(
            instance.handle(),
            sync_tree_root_id,
            DatabaseKey::new(device_key),
        )
        .await?;

        let sync = Self {
            background_tx: OnceLock::new(),
            instance: instance.downgrade(),
            sync_tree,
            queue: Arc::new(SyncQueue::new()),
        };

        // Initialize combined settings for all tracked users
        sync.initialize_user_settings().await?;

        Ok(sync)
    }

    /// Get the root ID of the sync settings tree.
    pub fn sync_tree_root_id(&self) -> &ID {
        self.sync_tree.root_id()
    }

    /// Store a setting in the sync_settings subtree.
    ///
    /// # Arguments
    /// * `key` - The setting key
    /// * `value` - The setting value
    pub async fn set_setting(
        &self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<()> {
        let txn = self.sync_tree.new_transaction().await?;
        let sync_settings = txn.get_store::<DocStore>(SETTINGS_SUBTREE).await?;
        sync_settings.set(key, Value::Text(value.into())).await?;
        txn.commit().await?;
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
                entry.config.get_json("data").map_err(|e| {
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
    pub async fn save_transport_config<T: TransportConfig>(
        &self,
        name: &str,
        config: &T,
    ) -> Result<()> {
        let mut config_doc = crate::crdt::Doc::new();
        config_doc.set_json("data", config).map_err(|e| {
            SyncError::SerializationError(format!(
                "Failed to serialize transport config '{name}': {e}"
            ))
        })?;
        let tx = self.sync_tree.new_transaction().await?;
        let registry = Registry::new(&tx, TRANSPORTS_SUBTREE).await?;
        registry.set_entry(name, T::type_id(), config_doc).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Get a reference to the Instance.
    pub fn instance(&self) -> Result<Instance> {
        self.instance
            .upgrade()
            .ok_or_else(|| SyncError::InstanceDropped.into())
    }

    /// Get a reference to the backend.
    pub fn backend(&self) -> Result<Backend> {
        Ok(self.instance()?.backend().clone())
    }

    /// Get the sync tree database.
    pub fn sync_tree(&self) -> &Database {
        &self.sync_tree
    }

    /// Get the device public key for this sync instance.
    ///
    /// # Returns
    /// The device's public key in ed25519:base64 format.
    pub fn get_device_id(&self) -> Result<String> {
        Ok(self.instance()?.device_id_string())
    }
}

impl Sync {
    // === Test Helpers ===
}
