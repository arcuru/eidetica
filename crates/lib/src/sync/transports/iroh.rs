//! Iroh transport implementation for sync communication.
//!
//! This module provides peer-to-peer sync communication using
//! Iroh's QUIC-based networking with hole punching and relay servers.

use std::{collections::BTreeSet, net::SocketAddr, sync::Arc};
use tokio::sync::Mutex;

use async_trait::async_trait;
use iroh::{
    Endpoint, EndpointAddr, RelayMode, SecretKey, TransportAddr,
    endpoint::{Connection, RecvStream, SendStream},
};
use serde::{Deserialize, Serialize};
#[allow(unused_imports)] // Used by write_all method on streams
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

use super::{SyncTransport, TransportConfig, shared::*};
use crate::{
    Result,
    store::Registered,
    sync::{
        error::SyncError,
        handler::SyncHandler,
        peer_types::Address,
        protocol::{RequestContext, SyncRequest, SyncResponse},
    },
};

const SYNC_ALPN: &[u8] = b"eidetica/v0";

/// Serializable representation of EndpointAddr for storage
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EndpointAddrInfo {
    endpoint_id: String,
    direct_addresses: BTreeSet<SocketAddr>,
}

impl From<&EndpointAddr> for EndpointAddrInfo {
    fn from(endpoint_addr: &EndpointAddr) -> Self {
        Self {
            endpoint_id: endpoint_addr.id.to_string(),
            direct_addresses: endpoint_addr.ip_addrs().cloned().collect(),
        }
    }
}

impl TryFrom<EndpointAddrInfo> for EndpointAddr {
    type Error = crate::Error;

    fn try_from(info: EndpointAddrInfo) -> Result<Self> {
        let endpoint_id = info.endpoint_id.parse().map_err(|e| {
            SyncError::SerializationError(format!(
                "Invalid EndpointId '{}': {}",
                info.endpoint_id, e
            ))
        })?;

        // Convert SocketAddrs to TransportAddrs
        let transport_addrs: Vec<TransportAddr> = info
            .direct_addresses
            .into_iter()
            .map(TransportAddr::Ip)
            .collect();

        Ok(EndpointAddr::from_parts(endpoint_id, transport_addrs))
    }
}

/// Serializable relay mode setting for transport configuration.
///
/// This is a simplified version of Iroh's `RelayMode` that can be
/// persisted to storage. Custom relay configurations are not supported
/// in the persisted config (use the builder API for custom relays).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum RelayModeSetting {
    /// Use n0's production relay servers (recommended for most deployments)
    #[default]
    Default,
    /// Use n0's staging relay infrastructure (for testing)
    Staging,
    /// Disable relay servers entirely (local/direct connections only)
    Disabled,
}

impl From<RelayModeSetting> for RelayMode {
    fn from(setting: RelayModeSetting) -> Self {
        match setting {
            RelayModeSetting::Default => RelayMode::Default,
            RelayModeSetting::Staging => RelayMode::Staging,
            RelayModeSetting::Disabled => RelayMode::Disabled,
        }
    }
}

/// Persistable configuration for the Iroh transport.
///
/// This configuration is stored in the `_sync` database's `transport_configs`
/// subtree and is automatically loaded when `enable_iroh_transport()` is called.
///
/// The most important field is `secret_key_hex`, which stores the node's
/// cryptographic identity. When this is persisted, the node will have the
/// same address across restarts.
///
/// # Example
///
/// ```ignore
/// use eidetica::sync::transports::iroh::IrohTransportConfig;
///
/// // Create a default config (secret key will be generated on first use)
/// let config = IrohTransportConfig::default();
///
/// // Or create with specific settings
/// let config = IrohTransportConfig {
///     relay_mode: RelayModeSetting::Disabled,
///     ..Default::default()
/// };
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrohTransportConfig {
    /// Secret key bytes (hex encoded for JSON storage).
    ///
    /// When `None`, a new secret key will be generated on first use
    /// and stored back to the config. Once set, this ensures the node
    /// maintains the same identity (and thus address) across restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_key_hex: Option<String>,

    /// Relay mode setting for NAT traversal.
    #[serde(default)]
    pub relay_mode: RelayModeSetting,
}

impl Default for IrohTransportConfig {
    fn default() -> Self {
        Self {
            secret_key_hex: None,
            relay_mode: RelayModeSetting::Default,
        }
    }
}

impl Registered for IrohTransportConfig {
    fn type_id() -> &'static str {
        "iroh:v0"
    }
}

impl TransportConfig for IrohTransportConfig {}

impl IrohTransportConfig {
    /// Get the secret key from config, or generate a new one.
    ///
    /// If a secret key is already stored in the config, it will be decoded
    /// and returned. Otherwise, a new random secret key is generated,
    /// stored in the config (as hex), and returned.
    ///
    /// This method mutates the config to store the newly generated key,
    /// so the caller should persist the config after calling this.
    pub fn get_or_create_secret_key(&mut self) -> SecretKey {
        if let Some(hex) = &self.secret_key_hex {
            let bytes = hex::decode(hex).expect("valid hex in stored secret key");
            let bytes: [u8; 32] = bytes.try_into().expect("secret key should be 32 bytes");
            SecretKey::from_bytes(&bytes)
        } else {
            // Generate 32 random bytes for the secret key using OsRng
            use rand::RngCore;
            let mut secret_bytes = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut secret_bytes);
            let key = SecretKey::from_bytes(&secret_bytes);
            self.secret_key_hex = Some(hex::encode(key.to_bytes()));
            key
        }
    }

    /// Check if a secret key has been set in this config.
    pub fn has_secret_key(&self) -> bool {
        self.secret_key_hex.is_some()
    }
}

/// Builder for configuring IrohTransport with different relay modes and options.
///
/// # Examples
///
/// ## Production deployment (default)
/// ```no_run
/// use eidetica::sync::transports::iroh::IrohTransport;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let transport = IrohTransport::builder()
///     .build()?;
/// // Uses n0's production relay servers by default
/// # Ok(())
/// # }
/// ```
///
/// ## Local testing without internet
/// ```no_run
/// use eidetica::sync::transports::iroh::IrohTransport;
/// use iroh::RelayMode;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let transport = IrohTransport::builder()
///     .relay_mode(RelayMode::Disabled)
///     .build()?;
/// // Direct P2P only, no relay servers
/// # Ok(())
/// # }
/// ```
///
/// ## Enterprise deployment with custom relay
/// ```no_run
/// use eidetica::sync::transports::iroh::IrohTransport;
/// use iroh::{RelayConfig, RelayMode, RelayMap, RelayUrl};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let relay_url: RelayUrl = "https://relay.example.com".parse()?;
/// let relay_config: RelayConfig = relay_url.into();
/// let transport = IrohTransport::builder()
///     .relay_mode(RelayMode::Custom(RelayMap::from_iter([relay_config])))
///     .build()?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct IrohTransportBuilder {
    relay_mode: RelayMode,
    secret_key: Option<SecretKey>,
}

impl IrohTransportBuilder {
    /// Create a new builder with production defaults.
    ///
    /// By default, uses `RelayMode::Default` which connects to n0's
    /// production relay infrastructure for NAT traversal.
    pub fn new() -> Self {
        Self {
            relay_mode: RelayMode::Default, // Use n0's production relays by default
            secret_key: None,
        }
    }

    /// Set the relay mode for the transport.
    ///
    /// # Relay Modes
    ///
    /// - `RelayMode::Default` - Use n0's production relay servers (recommended)
    /// - `RelayMode::Staging` - Use n0's staging infrastructure for testing
    /// - `RelayMode::Disabled` - No relay servers, direct P2P only (local testing)
    /// - `RelayMode::Custom(RelayMap)` - Use custom relay servers (enterprise deployments)
    ///
    /// # Example
    ///
    /// ```no_run
    /// use eidetica::sync::transports::iroh::IrohTransport;
    /// use iroh::RelayMode;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = IrohTransport::builder()
    ///     .relay_mode(RelayMode::Disabled)
    ///     .build()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn relay_mode(mut self, mode: RelayMode) -> Self {
        self.relay_mode = mode;
        self
    }

    /// Set the secret key for persistent node identity.
    ///
    /// When a secret key is provided, the node will have the same
    /// cryptographic identity (and thus the same address) across restarts.
    /// This is essential for maintaining stable peer connections.
    ///
    /// If not set, a random secret key will be generated on each startup,
    /// resulting in a different node address each time.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use eidetica::sync::transports::iroh::IrohTransport;
    /// use iroh::SecretKey;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// // Generate or load secret key from storage
    /// use rand::RngCore;
    /// let mut secret_bytes = [0u8; 32];
    /// rand::rngs::OsRng.fill_bytes(&mut secret_bytes);
    /// let secret_key = SecretKey::from_bytes(&secret_bytes);
    ///
    /// let transport = IrohTransport::builder()
    ///     .secret_key(secret_key)
    ///     .build()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn secret_key(mut self, key: SecretKey) -> Self {
        self.secret_key = Some(key);
        self
    }

    /// Build the IrohTransport with the configured options.
    ///
    /// Returns a configured `IrohTransport` ready to be used with
    /// `SyncEngine::enable_iroh_transport_with_config()`.
    pub fn build(self) -> Result<IrohTransport> {
        Ok(IrohTransport {
            endpoint: Arc::new(Mutex::new(None)),
            server_state: ServerState::new(),
            handler: None,
            runtime_config: IrohRuntimeConfig {
                relay_mode: self.relay_mode,
                secret_key: self.secret_key,
            },
        })
    }
}

impl Default for IrohTransportBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Runtime configuration for IrohTransport (internal, not persisted).
///
/// This holds the actual runtime values used by the transport,
/// including the decoded secret key and relay mode.
#[derive(Debug, Clone)]
struct IrohRuntimeConfig {
    relay_mode: RelayMode,
    secret_key: Option<SecretKey>,
}

/// Iroh transport implementation using QUIC peer-to-peer networking.
///
/// Provides NAT traversal and direct peer-to-peer connectivity using the Iroh
/// protocol. Supports both relay-assisted and direct connections.
///
/// # How It Works
///
/// 1. **Discovery**: Peers find each other via relay servers or direct addresses
/// 2. **Connection**: Attempts direct connection through NAT hole-punching
/// 3. **Fallback**: Uses relay servers if direct connection fails
/// 4. **Upgrade**: Automatically upgrades to direct connection when possible
///
/// # Server Addresses
///
/// When `get_server_address()` is called, returns a JSON string containing:
/// - `node_id`: The cryptographic identity of the node
/// - `direct_addresses`: Socket addresses where the node can be reached
///
/// This allows peers to connect using the best available path.
///
/// # Example
///
/// ```no_run
/// use eidetica::sync::transports::iroh::IrohTransport;
/// use iroh::RelayMode;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // Create with defaults (production relay servers)
/// let transport = IrohTransport::new()?;
///
/// // Or use the builder for custom configuration
/// let transport = IrohTransport::builder()
///     .relay_mode(RelayMode::Staging)
///     .build()?;
/// # Ok(())
/// # }
/// ```
pub struct IrohTransport {
    /// The Iroh endpoint for P2P communication (lazily initialized).
    endpoint: Arc<Mutex<Option<Endpoint>>>,
    /// Shared server state management.
    server_state: ServerState,
    /// Handler for processing sync requests.
    handler: Option<Arc<dyn SyncHandler>>,
    /// Runtime configuration (relay mode, secret key, etc.)
    runtime_config: IrohRuntimeConfig,
}

impl IrohTransport {
    /// Transport type identifier for Iroh
    pub const TRANSPORT_TYPE: &'static str = "iroh";

    /// Create a new Iroh transport instance with production defaults.
    ///
    /// Uses `RelayMode::Default` which connects to n0's production relay
    /// infrastructure. For custom configuration, use `IrohTransport::builder()`.
    ///
    /// The endpoint will be lazily initialized on first use.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use eidetica::sync::transports::iroh::IrohTransport;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = IrohTransport::new()?;
    /// // Use with: sync.enable_iroh_transport_with_config(transport)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new() -> Result<Self> {
        IrohTransportBuilder::new().build()
    }

    /// Create a builder for configuring the transport.
    ///
    /// Allows customization of relay modes and other transport options.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use eidetica::sync::transports::iroh::IrohTransport;
    /// use iroh::RelayMode;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = IrohTransport::builder()
    ///     .relay_mode(RelayMode::Disabled)
    ///     .build()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn builder() -> IrohTransportBuilder {
        IrohTransportBuilder::new()
    }

    /// Initialize the Iroh endpoint if not already done.
    async fn ensure_endpoint(&self) -> Result<Endpoint> {
        let mut endpoint_lock = self.endpoint.lock().await;

        if endpoint_lock.is_none() {
            // Create a new Iroh endpoint with configured relay mode
            let mut builder = Endpoint::builder()
                .alpns(vec![SYNC_ALPN.to_vec()])
                .relay_mode(self.runtime_config.relay_mode.clone());

            // Use the provided secret key for persistent identity
            if let Some(secret_key) = &self.runtime_config.secret_key {
                builder = builder.secret_key(secret_key.clone());
            }

            let endpoint = builder.bind().await.map_err(|e| {
                SyncError::TransportInit(format!("Failed to create Iroh endpoint: {e}"))
            })?;

            *endpoint_lock = Some(endpoint);
        }

        Ok(endpoint_lock.as_ref().unwrap().clone())
    }

    /// Start the server request handling loop.
    async fn start_server_loop(
        &self,
        endpoint: Endpoint,
        ready_tx: oneshot::Sender<()>,
        shutdown_rx: oneshot::Receiver<()>,
        handler: Arc<dyn SyncHandler>,
    ) -> Result<()> {
        let mut shutdown_rx = shutdown_rx;

        // Signal that we're ready
        let _ = ready_tx.send(());

        // Accept incoming connections
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    // Check for shutdown signal
                    _ = &mut shutdown_rx => {
                        break;
                    }
                    // Accept incoming connections
                    connection_result = endpoint.accept() => {
                        match connection_result {
                            Some(connecting) => {
                                let handler_clone = handler.clone();
                                tokio::spawn(async move {
                                    if let Ok(conn) = connecting.await {
                                        Self::handle_connection(conn, handler_clone).await;
                                    }
                                });
                            }
                            None => break, // Endpoint closed
                        }
                    }
                }
            }
            // Server loop has exited - the shutdown was triggered by stop_server()
            // which already marked the server as stopped, so no additional cleanup needed here
        });

        Ok(())
    }

    /// Handle an incoming connection.
    async fn handle_connection(conn: Connection, handler: Arc<dyn SyncHandler>) {
        // Get the remote peer node ID for context
        let remote_endpoint_id = conn.remote_id();
        let remote_address = Address {
            transport_type: Self::TRANSPORT_TYPE.to_string(),
            address: remote_endpoint_id.to_string(),
        };

        // Accept incoming streams and process sequentially
        // Note: We process streams sequentially because SyncHandler::handle_request
        // returns non-Send futures (internal types use Rc/RefCell).
        while let Ok((send_stream, recv_stream)) = conn.accept_bi().await {
            Self::handle_stream(
                send_stream,
                recv_stream,
                handler.clone(),
                remote_address.clone(),
            )
            .await;
        }
    }

    /// Handle an incoming bidirectional stream.
    async fn handle_stream(
        mut send_stream: SendStream,
        mut recv_stream: RecvStream,
        handler: Arc<dyn SyncHandler>,
        remote_address: Address,
    ) {
        // Read the request with size limit (1MB)
        let buffer: Vec<u8> = match recv_stream.read_to_end(1024 * 1024).await {
            Ok(buffer) => buffer,
            Err(e) => {
                tracing::error!("Failed to read stream: {e}");
                return;
            }
        };

        // Deserialize the request using JsonHandler
        let request: SyncRequest = match JsonHandler::deserialize_request(&buffer) {
            Ok(req) => req,
            Err(e) => {
                tracing::error!("Failed to deserialize request: {e}");
                return;
            }
        };

        // Extract peer_pubkey from SyncTreeRequest if present
        let peer_pubkey = match &request {
            SyncRequest::SyncTree(sync_tree_request) => sync_tree_request.peer_pubkey.clone(),
            _ => None,
        };

        // Create request context with remote address and peer pubkey
        let context = RequestContext {
            remote_address: Some(remote_address),
            peer_pubkey,
        };

        // Handle the request using the SyncHandler
        let response = handler.handle_request(&request, &context).await;

        // Serialize and send response using JsonHandler
        match JsonHandler::serialize_response(&response) {
            Ok(response_bytes) => {
                if let Err(e) = send_stream.write_all(&response_bytes).await {
                    tracing::error!("Failed to write response: {e}");
                    return;
                }
                if let Err(e) = send_stream.finish() {
                    tracing::error!("Failed to finish stream: {e}");
                }
            }
            Err(e) => {
                tracing::error!("Failed to serialize response: {e}");
            }
        }
    }
}

#[async_trait]
impl SyncTransport for IrohTransport {
    fn transport_type(&self) -> &'static str {
        Self::TRANSPORT_TYPE
    }

    fn can_handle_address(&self, address: &Address) -> bool {
        address.transport_type == Self::TRANSPORT_TYPE
    }

    async fn start_server(&mut self, _addr: &str, handler: Arc<dyn SyncHandler>) -> Result<()> {
        // Check if server is already running
        if self.server_state.is_running() {
            return Err(SyncError::ServerAlreadyRunning {
                address: "iroh-endpoint".to_string(),
            }
            .into());
        }

        // Store the handler
        self.handler = Some(handler);

        // Ensure we have an endpoint and get EndpointAddr with direct addresses
        let endpoint = self.ensure_endpoint().await?;
        let endpoint_clone = endpoint.clone();

        // Get the EndpointAddr with direct addresses
        // Note: We don't wait for online() - direct addresses are available immediately
        // after bind(), and relay connections happen asynchronously in the background.
        let endpoint_addr = endpoint.addr();

        // Serialize EndpointAddr to string for storage
        let endpoint_addr_info = EndpointAddrInfo::from(&endpoint_addr);
        let endpoint_addr_str = serde_json::to_string(&endpoint_addr_info).map_err(|e| {
            SyncError::TransportInit(format!("Failed to serialize EndpointAddr: {e}"))
        })?;

        // Create server coordination channels
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        // Start server loop
        self.start_server_loop(
            endpoint_clone,
            ready_tx,
            shutdown_rx,
            self.handler.clone().unwrap(),
        )
        .await?;

        // Wait for server to be ready using shared utility
        wait_for_ready(ready_rx, "iroh-endpoint").await?;

        // Start server state with EndpointAddr string and shutdown sender
        self.server_state
            .server_started(endpoint_addr_str, shutdown_tx);

        Ok(())
    }

    async fn stop_server(&mut self) -> Result<()> {
        if !self.server_state.is_running() {
            return Err(SyncError::ServerNotRunning.into());
        }

        // Stop server using combined method
        self.server_state.stop_server();

        Ok(())
    }

    async fn send_request(&self, address: &Address, request: &SyncRequest) -> Result<SyncResponse> {
        if !self.can_handle_address(address) {
            return Err(SyncError::UnsupportedTransport {
                transport_type: address.transport_type.clone(),
            }
            .into());
        }

        // Ensure we have an endpoint (lazy initialization)
        let endpoint = self.ensure_endpoint().await?;

        // Parse the target endpoint address from serialized EndpointAddrInfo
        let endpoint_addr_info: EndpointAddrInfo =
            serde_json::from_str(&address.address).map_err(|e| {
                SyncError::SerializationError(format!(
                    "Failed to parse EndpointAddrInfo from '{}': {}",
                    address.address, e
                ))
            })?;
        let endpoint_addr = EndpointAddr::try_from(endpoint_addr_info)?;

        // Connect to the peer
        let conn = endpoint
            .connect(endpoint_addr, SYNC_ALPN)
            .await
            .map_err(|e| SyncError::ConnectionFailed {
                address: address.address.clone(),
                reason: e.to_string(),
            })?;

        // Open a bidirectional stream
        let (mut send_stream, mut recv_stream) = conn
            .open_bi()
            .await
            .map_err(|e| SyncError::Network(format!("Failed to open stream: {e}")))?;

        // Serialize and send the request using JsonHandler
        let request_bytes = JsonHandler::serialize_request(request)?;

        send_stream
            .write_all(&request_bytes)
            .await
            .map_err(|e| SyncError::Network(format!("Failed to write request: {e}")))?;

        send_stream
            .finish()
            .map_err(|e| SyncError::Network(format!("Failed to finish send stream: {e}")))?;

        // Read the response with size limit (1MB)
        let response_bytes: Vec<u8> = recv_stream
            .read_to_end(1024 * 1024)
            .await
            .map_err(|e| SyncError::Network(format!("Failed to read response: {e}")))?;

        // Deserialize the response using JsonHandler
        let response: SyncResponse = JsonHandler::deserialize_response(&response_bytes)?;

        Ok(response)
    }

    fn is_server_running(&self) -> bool {
        self.server_state.is_running()
    }

    fn get_server_address(&self) -> Result<String> {
        self.server_state.get_address().map_err(|e| e.into())
    }
}
