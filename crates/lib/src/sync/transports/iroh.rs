//! Iroh transport implementation for sync communication.
//!
//! This module provides peer-to-peer sync communication using
//! Iroh's QUIC-based networking with hole punching and relay servers.

use std::{collections::BTreeSet, net::SocketAddr, sync::Arc};

use async_trait::async_trait;
use iroh::{
    Endpoint, NodeAddr, RelayMode, Watcher,
    endpoint::{Connection, RecvStream, SendStream},
};
use serde::{Deserialize, Serialize};
#[allow(unused_imports)] // Used by write_all method on streams
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

use super::{SyncTransport, shared::*};
use crate::{
    Result,
    sync::{
        error::SyncError,
        handler::SyncHandler,
        peer_types::Address,
        protocol::{SyncRequest, SyncResponse},
    },
};

const SYNC_ALPN: &[u8] = b"eidetica/v0";

/// Serializable representation of NodeAddr for storage
#[derive(Debug, Clone, Serialize, Deserialize)]
struct NodeAddrInfo {
    node_id: String,
    direct_addresses: BTreeSet<SocketAddr>,
}

impl From<&NodeAddr> for NodeAddrInfo {
    fn from(node_addr: &NodeAddr) -> Self {
        Self {
            node_id: node_addr.node_id.to_string(),
            direct_addresses: node_addr.direct_addresses.iter().cloned().collect(),
        }
    }
}

impl TryFrom<NodeAddrInfo> for NodeAddr {
    type Error = crate::Error;

    fn try_from(info: NodeAddrInfo) -> Result<Self> {
        let node_id = info.node_id.parse().map_err(|e| {
            SyncError::SerializationError(format!("Invalid NodeId '{}': {}", info.node_id, e))
        })?;

        Ok(NodeAddr::from_parts(
            node_id,
            None, // Direct addresses are provided, relay will be used if needed
            info.direct_addresses,
        ))
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
/// use iroh::{RelayMode, RelayMap, RelayNode, RelayUrl};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let relay_url: RelayUrl = "https://relay.example.com".parse()?;
/// let relay_node = RelayNode {
///     url: relay_url,
///     quic: Some(Default::default()),
/// };
/// let transport = IrohTransport::builder()
///     .relay_mode(RelayMode::Custom(RelayMap::from_iter([relay_node])))
///     .build()?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct IrohTransportBuilder {
    relay_mode: RelayMode,
}

impl IrohTransportBuilder {
    /// Create a new builder with production defaults.
    ///
    /// By default, uses `RelayMode::Default` which connects to n0's
    /// production relay infrastructure for NAT traversal.
    pub fn new() -> Self {
        Self {
            relay_mode: RelayMode::Default, // Use n0's production relays by default
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

    /// Build the IrohTransport with the configured options.
    ///
    /// Returns a configured `IrohTransport` ready to be used with
    /// `SyncEngine::enable_iroh_transport_with_config()`.
    pub fn build(self) -> Result<IrohTransport> {
        Ok(IrohTransport {
            endpoint: None,
            server_state: ServerState::new(),
            handler: None,
            config: IrohTransportConfig {
                relay_mode: self.relay_mode,
            },
        })
    }
}

impl Default for IrohTransportBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration for IrohTransport
#[derive(Debug, Clone)]
struct IrohTransportConfig {
    relay_mode: RelayMode,
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
    /// The Iroh endpoint for P2P communication.
    endpoint: Option<Endpoint>,
    /// Shared server state management.
    server_state: ServerState,
    /// Handler for processing sync requests.
    handler: Option<Arc<dyn SyncHandler>>,
    /// Transport configuration
    config: IrohTransportConfig,
}

impl IrohTransport {
    /// Transport type identifier for Iroh
    pub const TRANSPORT_TYPE: &'static str = "iroh";

    /// Create a new Iroh transport instance with production defaults.
    ///
    /// Uses `RelayMode::Default` which connects to n0's production relay
    /// infrastructure. For custom configuration, use `IrohTransport::builder()`.
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
    async fn ensure_endpoint(&mut self) -> Result<&Endpoint> {
        if self.endpoint.is_none() {
            // Create a new Iroh endpoint with configured relay mode
            let builder = Endpoint::builder()
                .alpns(vec![SYNC_ALPN.to_vec()])
                .relay_mode(self.config.relay_mode.clone());

            let endpoint = builder.bind().await.map_err(|e| {
                SyncError::TransportInit(format!("Failed to create Iroh endpoint: {e}"))
            })?;

            self.endpoint = Some(endpoint);
        }

        Ok(self.endpoint.as_ref().unwrap())
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
        // Accept incoming streams
        while let Ok((send_stream, recv_stream)) = conn.accept_bi().await {
            let handler_clone = handler.clone();
            tokio::spawn(Self::handle_stream(send_stream, recv_stream, handler_clone));
        }
    }

    /// Handle an incoming bidirectional stream.
    async fn handle_stream(
        mut send_stream: SendStream,
        mut recv_stream: RecvStream,
        handler: Arc<dyn SyncHandler>,
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

        // Handle the request using the SyncHandler
        let response = handler.handle_request(&request).await;

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

        // Ensure we have an endpoint and get NodeAddr with direct addresses
        let endpoint = self.ensure_endpoint().await?;
        let endpoint_clone = endpoint.clone();

        // Get the full NodeAddr with direct addresses initialized
        let node_addr = endpoint.node_addr().initialized().await;

        // Serialize NodeAddr to string for storage
        let node_addr_info = NodeAddrInfo::from(&node_addr);
        let node_addr_str = serde_json::to_string(&node_addr_info)
            .map_err(|e| SyncError::TransportInit(format!("Failed to serialize NodeAddr: {e}")))?;

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

        // Start server state with NodeAddr string and shutdown sender
        self.server_state.server_started(node_addr_str, shutdown_tx);

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

        // Ensure we have an endpoint
        let endpoint = match &self.endpoint {
            Some(endpoint) => endpoint,
            None => {
                return Err(
                    SyncError::TransportInit("Endpoint not initialized".to_string()).into(),
                );
            }
        };

        // Parse the target node address from serialized NodeAddrInfo
        let node_addr_info: NodeAddrInfo = serde_json::from_str(&address.address).map_err(|e| {
            SyncError::SerializationError(format!(
                "Failed to parse NodeAddrInfo from '{}': {}",
                address.address, e
            ))
        })?;
        let node_addr = NodeAddr::try_from(node_addr_info)?;

        // Connect to the peer
        let conn = endpoint.connect(node_addr, SYNC_ALPN).await.map_err(|e| {
            SyncError::ConnectionFailed {
                address: address.address.clone(),
                reason: e.to_string(),
            }
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
