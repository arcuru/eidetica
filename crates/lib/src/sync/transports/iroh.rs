//! Iroh transport implementation for sync communication.
//!
//! This module provides peer-to-peer sync communication using
//! Iroh's QUIC-based networking with hole punching and relay servers.

use std::{sync::Arc, time::Duration};
use tokio::{sync::Mutex, time::timeout};

use async_trait::async_trait;
use iroh::{
    Endpoint, RelayMode, SecretKey,
    endpoint::{
        Builder as EndpointBuilder, Connection, PortmapperConfig, RecvStream, SendStream, presets,
    },
};
use iroh_mdns_address_lookup::MdnsAddressLookup;
use iroh_tickets::{Ticket, endpoint::EndpointTicket};
use serde::{Deserialize, Serialize};
#[allow(unused_imports)] // Used by write_all method on streams
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

use super::{SyncTransport, TransportBuilder, TransportConfig, shared::*};
use crate::{
    Result,
    crdt::Doc,
    store::Registered,
    sync::{
        error::{SyncError, TimeoutPhase},
        handler::SyncHandler,
        peer_types::Address,
        protocol::{RequestContext, SyncRequest, SyncResponse},
    },
};

const SYNC_ALPN: &[u8] = b"eidetica/v0";

/// Maximum time spent establishing a connection to a peer.
///
/// Looser than the HTTP transport's equivalent, because it bounds something
/// larger: hole punching and relay fallback, not a TCP handshake. Against a
/// live but awkwardly-NATed peer that work can legitimately run past ten
/// seconds, and cutting it short would report a reachable peer as unreachable
/// rather than merely slow.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum time for the request/response exchange once connected, covering
/// opening the stream, writing the request and reading the response.
///
/// Without it, a peer that completes the QUIC handshake and then stops
/// answering holds the request open forever. Requests are awaited inline by the
/// background sync engine, so an unbounded one stalls every other thing that
/// engine does.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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
            // Generate new secret key
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
///     .n0_dns(false)
///     .build()?;
/// // Local mDNS discovery and direct P2P, no public services
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
    // None inherits N0's relay selection; Some(RelayMode::Disabled) opts out.
    relay_mode: Option<RelayMode>,
    secret_key: Option<SecretKey>,
    // None inherits iroh's port mapping; Some(PortmapperConfig::Disabled) opts out.
    portmapper_config: Option<PortmapperConfig>,
    mdns: bool,
    n0_dns: bool,
}

impl IrohTransportBuilder {
    /// Create a builder with iroh's public services and local mDNS enabled.
    ///
    /// Relay selection and router port mapping inherit `presets::N0` unless
    /// explicitly overridden. Their stored `None` values mean "inherit", not
    /// "disabled"; effective settings are resolved when the endpoint is built.
    /// This preserves iroh's `IROH_FORCE_STAGING_RELAYS` environment switch.
    ///
    /// n0 address publication and DNS/HTTPS resolution remain enabled, with
    /// best-effort mDNS adding local lookup alongside those public services.
    pub fn new() -> Self {
        Self {
            relay_mode: None,
            secret_key: None,
            portmapper_config: None,
            mdns: true,
            n0_dns: true,
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
        self.relay_mode = Some(mode);
        self
    }

    /// Override iroh's router port mapping configuration (UPnP, PCP, NAT-PMP).
    ///
    /// When unset, iroh's own default applies. Use `PortmapperConfig::Disabled`
    /// to skip router discovery and mapping, even if another dependency enables
    /// iroh's `portmapper` Cargo feature.
    ///
    /// This is a runtime-only override, not persisted with the node identity.
    /// Supply it each time the transport is constructed or registered, including
    /// after a restart.
    ///
    /// ```no_run
    /// use eidetica::sync::transports::iroh::IrohTransport;
    /// use iroh::endpoint::PortmapperConfig;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let transport = IrohTransport::builder()
    ///     .portmapper_config(PortmapperConfig::Disabled)
    ///     .build()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn portmapper_config(mut self, config: PortmapperConfig) -> Self {
        self.portmapper_config = Some(config);
        self
    }

    /// Enable or disable local-network mDNS address lookup (enabled by default).
    ///
    /// Advertises this endpoint's addresses to peers on the same host or LAN.
    /// Disable it on networks where multicast is unavailable or unwanted.
    pub fn mdns(mut self, enabled: bool) -> Self {
        self.mdns = enabled;
        self
    }

    /// Enable or disable n0 DNS address lookup (enabled by default).
    ///
    /// Controls publication to n0's DNS service and resolution via DNS and HTTPS.
    /// Set this to false, along with [`Self::relay_mode`] set to
    /// [`RelayMode::Disabled`], for local-only connections without public services.
    /// To also skip router discovery and mapping, set [`Self::portmapper_config`]
    /// to [`PortmapperConfig::Disabled`]. Like [`Self::mdns`], this is not persisted.
    pub fn n0_dns(mut self, enabled: bool) -> Self {
        self.n0_dns = enabled;
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
    /// use rand::RngCore;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// // Generate or load secret key from storage
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
            runtime_config: IrohRuntimeConfig {
                relay_mode: self.relay_mode,
                secret_key: self.secret_key,
                portmapper_config: self.portmapper_config,
                mdns: self.mdns,
                n0_dns: self.n0_dns,
            },
        })
    }
}

impl Default for IrohTransportBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Key for storing secret key in persisted Doc
const SECRET_KEY_FIELD: &str = "secret_key";

#[async_trait]
impl TransportBuilder for IrohTransportBuilder {
    type Transport = IrohTransport;

    /// Build the transport using persisted state for identity.
    ///
    /// The secret key (node identity) is loaded from the persisted Doc.
    /// If no secret key exists, a new one is generated and the Doc is returned
    /// for persistence. Any secret_key set via the builder is ignored -
    /// persisted state takes precedence.
    async fn build(self, mut persisted: Doc) -> Result<(Self::Transport, Option<Doc>)> {
        use crate::crdt::doc::Value;

        // Load or generate secret key from persisted state
        let (secret_key, updated) = match persisted.get(SECRET_KEY_FIELD) {
            Some(Value::Text(hex_str)) => {
                // Decode existing secret key
                let bytes = hex::decode(hex_str).map_err(|e| {
                    SyncError::TransportInit(format!("Invalid secret key hex: {}", e))
                })?;
                let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
                    SyncError::TransportInit("Secret key must be 32 bytes".to_string())
                })?;
                (SecretKey::from_bytes(&bytes), None)
            }
            Some(_) => {
                return Err(SyncError::TransportInit(
                    "Secret key must be a text value".to_string(),
                )
                .into());
            }
            None => {
                // Generate new secret key and store it
                // FIXME: Use SecretKey::generate() here (and elsewhere)
                // Waiting on a rand_core version mismatch to be resolved.
                use rand::RngCore;
                let mut secret_bytes = [0u8; 32];
                rand::rngs::OsRng.fill_bytes(&mut secret_bytes);
                let key = SecretKey::from_bytes(&secret_bytes);
                persisted.set(SECRET_KEY_FIELD, hex::encode(key.to_bytes()));
                (key, Some(persisted))
            }
        };

        let transport = IrohTransport {
            endpoint: Arc::new(Mutex::new(None)),
            server_state: ServerState::new(),
            runtime_config: IrohRuntimeConfig {
                relay_mode: self.relay_mode,
                secret_key: Some(secret_key),
                portmapper_config: self.portmapper_config,
                mdns: self.mdns,
                n0_dns: self.n0_dns,
            },
        };

        Ok((transport, updated))
    }
}

/// Runtime configuration for IrohTransport (internal, not persisted).
///
/// This holds the decoded secret key and optional networking overrides.
/// Unset overrides are resolved by iroh when the endpoint is built.
#[derive(Debug, Clone)]
struct IrohRuntimeConfig {
    // None inherits N0's relay selection; Some(RelayMode::Disabled) opts out.
    relay_mode: Option<RelayMode>,
    secret_key: Option<SecretKey>,
    // None inherits iroh's port mapping; Some(PortmapperConfig::Disabled) opts out.
    portmapper_config: Option<PortmapperConfig>,
    mdns: bool,
    n0_dns: bool,
}

/// Iroh transport implementation using QUIC peer-to-peer networking.
///
/// Provides NAT traversal and direct peer-to-peer connectivity using the Iroh
/// protocol. Supports both relay-assisted and direct connections.
///
/// # How It Works
///
/// 1. **Discovery**: Resolve peer addresses via local mDNS or n0 DNS lookup
/// 2. **Connection**: Attempts direct connection through NAT hole-punching
/// 3. **Fallback**: Uses relay servers if direct connection fails
/// 4. **Upgrade**: Automatically upgrades to direct connection when possible
///
/// # Networking Defaults
///
/// Eidetica enables iroh's default Cargo features and uses its `presets::N0`
/// endpoint defaults, including router port mapping (UPnP, PCP, NAT-PMP),
/// with best-effort local mDNS address lookup added. Both local mDNS and n0
/// publication/DNS/HTTPS resolution can be disabled via the builder.
/// Applications can opt out at construction with
/// [`IrohTransportBuilder::portmapper_config`]. This setting is not persisted.
///
/// # Server Addresses
///
/// Addresses use iroh's standard `EndpointTicket` format (postcard + base32-lower
/// with `endpoint` prefix) for both `get_server_address()` and `DatabaseTicket` URLs.
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
    /// Runtime configuration (relay mode, secret key, etc.)
    runtime_config: IrohRuntimeConfig,
}

impl IrohTransport {
    /// Transport type identifier for Iroh
    pub const TRANSPORT_TYPE: &'static str = "iroh";

    /// Create a new Iroh transport instance with production defaults.
    ///
    /// Uses iroh's `presets::N0` defaults (normally n0's production relays).
    /// For explicit overrides, use `IrohTransport::builder()`.
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

    /// Configure the endpoint without binding sockets or starting discovery.
    fn endpoint_builder(&self) -> EndpointBuilder {
        let mut builder = Endpoint::builder(presets::N0).alpns(vec![SYNC_ALPN.to_vec()]);

        if let Some(relay_mode) = &self.runtime_config.relay_mode {
            builder = builder.relay_mode(relay_mode.clone());
        }
        if let Some(secret_key) = &self.runtime_config.secret_key {
            builder = builder.secret_key(secret_key.clone());
        }
        if let Some(config) = &self.runtime_config.portmapper_config {
            builder = builder.portmapper_config(config.clone());
        }

        if !self.runtime_config.n0_dns {
            builder = builder.clear_address_lookup();
        }

        builder
    }

    /// Initialize the Iroh endpoint if not already done.
    async fn ensure_endpoint(&self) -> Result<Endpoint> {
        let mut endpoint_lock = self.endpoint.lock().await;

        if endpoint_lock.is_none() {
            let endpoint = self.endpoint_builder().bind().await.map_err(|e| {
                SyncError::TransportInit(format!("Failed to create Iroh endpoint: {e}"))
            })?;

            if self.runtime_config.mdns {
                // Multicast can be unavailable in sandboxes and restricted networks.
                // Register after binding to use the actual identity without changing N0 defaults.
                match MdnsAddressLookup::builder().build(endpoint.id()) {
                    Ok(mdns) => endpoint
                        .address_lookup()
                        .expect("just created the endpoint")
                        .add(mdns),
                    Err(error) => tracing::warn!(?error, "Local mDNS address lookup unavailable"),
                }
            }

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

    async fn start_server(&self, handler: Arc<dyn SyncHandler>) -> Result<()> {
        let start = self.server_state.begin_start("iroh-endpoint")?;

        // Ensure we have an endpoint and get EndpointAddr with direct addresses
        let endpoint = self.ensure_endpoint().await?;
        let endpoint_clone = endpoint.clone();

        // Get the EndpointAddr with direct addresses
        // Note: We don't wait for online() - direct addresses are available immediately
        // after bind(), and relay connections happen asynchronously in the background.
        let endpoint_addr = endpoint.addr();
        let endpoint_addr_str = EndpointTicket::new(endpoint_addr).encode_string();

        // Create server coordination channels
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        // Start server loop
        self.start_server_loop(endpoint_clone, ready_tx, shutdown_rx, handler)
            .await?;

        // Wait for server to be ready using shared utility
        wait_for_ready(ready_rx, "iroh-endpoint").await?;

        // Start server state with EndpointAddr string and shutdown sender
        start.complete(endpoint_addr_str, shutdown_tx);

        Ok(())
    }

    async fn stop_server(&self) -> Result<()> {
        if !self.server_state.is_running() {
            return Err(SyncError::ServerNotRunning.into());
        }

        self.server_state.stop_server();
        if let Some(endpoint) = self.endpoint.lock().await.take() {
            endpoint.close().await;
        }

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

        // Deserialize the EndpointTicket address
        let endpoint_ticket =
            <EndpointTicket as Ticket>::decode_string(&address.address).map_err(|e| {
                SyncError::SerializationError(format!(
                    "Failed to parse EndpointTicket '{}': {e}",
                    address.address
                ))
            })?;
        let endpoint_addr = endpoint_ticket.endpoint_addr().clone();

        // Connect to the peer, bounding how long an unreachable one can cost.
        let conn = timeout(CONNECT_TIMEOUT, endpoint.connect(endpoint_addr, SYNC_ALPN))
            .await
            .map_err(|_| SyncError::Timeout {
                address: address.address.clone(),
                phase: TimeoutPhase::Connect,
                elapsed: CONNECT_TIMEOUT,
            })?
            .map_err(|e| SyncError::ConnectionFailed {
                address: address.address.clone(),
                reason: e.to_string(),
            })?;

        // Serialize the request before starting the exchange clock.
        let request_bytes = JsonHandler::serialize_request(request)?;

        // One deadline covers the whole exchange: a peer can otherwise stall
        // any single step of it indefinitely.
        let response_bytes: Vec<u8> = timeout(REQUEST_TIMEOUT, async {
            // Open a bidirectional stream
            let (mut send_stream, mut recv_stream) = conn
                .open_bi()
                .await
                .map_err(|e| SyncError::Network(format!("Failed to open stream: {e}")))?;

            send_stream
                .write_all(&request_bytes)
                .await
                .map_err(|e| SyncError::Network(format!("Failed to write request: {e}")))?;

            send_stream
                .finish()
                .map_err(|e| SyncError::Network(format!("Failed to finish send stream: {e}")))?;

            // Read the response with size limit (1MB)
            recv_stream
                .read_to_end(1024 * 1024)
                .await
                .map_err(|e| SyncError::Network(format!("Failed to read response: {e}")))
        })
        .await
        .map_err(|_| {
            // The peer answered the connection attempt, so it is reachable:
            // the phase records that, rather than the peer being reported as
            // unreachable.
            SyncError::Timeout {
                address: address.address.clone(),
                phase: TimeoutPhase::Request,
                elapsed: REQUEST_TIMEOUT,
            }
        })??;

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

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::{EndpointAddr, TransportAddr};

    // Iroh exposes these settings through Builder's Debug, but has no getters.
    // Inspect only configuration markers, without binding or printing key material.
    fn assert_endpoint_portmapper(transport: &IrohTransport, expected: PortmapperConfig) {
        let config = format!("{:?}", transport.endpoint_builder());
        assert!(config.contains(&format!("portmapper_config: {expected:?}")));
        assert!(config.contains(&format!("alpn_protocols: {:?}", vec![SYNC_ALPN.to_vec()])));
    }

    #[tokio::test]
    async fn test_default_networking_defers_to_iroh() {
        let (persisted_transport, _) =
            TransportBuilder::build(IrohTransport::builder(), Doc::new())
                .await
                .unwrap();
        for transport in [
            IrohTransport::new().unwrap(),
            IrohTransportBuilder::new().build().unwrap(),
            IrohTransportBuilder::default().build().unwrap(),
            IrohTransport::builder().build().unwrap(),
            persisted_transport,
        ] {
            assert!(transport.runtime_config.portmapper_config.is_none());
            assert!(transport.runtime_config.relay_mode.is_none());
            assert!(transport.runtime_config.mdns);
            assert!(transport.runtime_config.n0_dns);
            assert_endpoint_portmapper(&transport, PortmapperConfig::default());
            let config = format!("{:?}", transport.endpoint_builder());
            let default_relays = iroh::endpoint::default_relay_mode().relay_map();
            assert!(config.contains(&format!("relay_map: {default_relays:?}")));
        }
    }

    #[tokio::test]
    async fn test_portmapper_disabled_survives_all_build_paths() {
        let key = SecretKey::from_bytes(&[42; 32]);
        let builder = IrohTransport::builder()
            .relay_mode(RelayMode::Disabled)
            .secret_key(key.clone())
            .portmapper_config(PortmapperConfig::Disabled)
            .mdns(false)
            .n0_dns(false);
        let direct = builder.clone().build().unwrap();
        let (generated, updated) = TransportBuilder::build(builder.clone(), Doc::new())
            .await
            .unwrap();
        let persisted = updated.unwrap();
        let mut expected = Doc::new();
        expected.set(
            SECRET_KEY_FIELD,
            hex::encode(
                generated
                    .runtime_config
                    .secret_key
                    .as_ref()
                    .unwrap()
                    .to_bytes(),
            ),
        );
        assert_eq!(persisted, expected, "only node identity is persisted");
        let (restored, updated) = TransportBuilder::build(builder, persisted).await.unwrap();
        assert!(updated.is_none());
        assert_eq!(
            restored
                .runtime_config
                .secret_key
                .as_ref()
                .unwrap()
                .public(),
            generated
                .runtime_config
                .secret_key
                .as_ref()
                .unwrap()
                .public()
        );
        assert_eq!(
            direct.runtime_config.secret_key.as_ref().unwrap().public(),
            key.public()
        );

        for transport in [direct, generated, restored] {
            assert!(matches!(
                transport.runtime_config.portmapper_config,
                Some(PortmapperConfig::Disabled)
            ));
            assert_endpoint_portmapper(&transport, PortmapperConfig::Disabled);
            assert!(matches!(
                transport.runtime_config.relay_mode,
                Some(RelayMode::Disabled)
            ));
            let config = format!("{:?}", transport.endpoint_builder());
            assert!(
                !config.contains("Relay {"),
                "explicit disabled relay reaches endpoint setup"
            );

            // Bind only with both mappings and relays disabled.
            let endpoint = transport.ensure_endpoint().await.unwrap();
            assert!(endpoint.address_lookup().unwrap().is_empty());
            assert_eq!(
                endpoint.id(),
                transport
                    .runtime_config
                    .secret_key
                    .as_ref()
                    .unwrap()
                    .public()
            );
            endpoint.close().await;
        }
    }

    #[tokio::test]
    async fn lookup_selection_reaches_endpoint_in_both_build_paths() {
        // Inspect the actual registered services: the public transport API hides them.
        for n0_dns in [false, true] {
            let builder = IrohTransport::builder()
                .relay_mode(RelayMode::Disabled)
                .portmapper_config(PortmapperConfig::Disabled)
                .mdns(false)
                .n0_dns(n0_dns);
            let ephemeral = builder.clone().build().unwrap();
            let (persistent, updated) = TransportBuilder::build(builder.clone(), Doc::new())
                .await
                .unwrap();
            let (restored, updated) = TransportBuilder::build(builder, updated.unwrap())
                .await
                .unwrap();
            assert!(updated.is_none());

            for transport in [ephemeral, persistent, restored] {
                let endpoint = transport.ensure_endpoint().await.unwrap();
                assert_eq!(
                    endpoint.address_lookup().unwrap().len(),
                    if n0_dns { 3 } else { 0 },
                    "mDNS must be disabled and n0 selection must reach the endpoint",
                );
                endpoint.close().await;
            }
        }
    }

    /// Round-trip: EndpointAddr → EndpointTicket string → EndpointAddr
    #[test]
    fn endpoint_ticket_round_trip() {
        let secret_key = SecretKey::from_bytes(&[1u8; 32]);
        let endpoint_addr = EndpointAddr::from_parts(
            secret_key.public(),
            vec![
                TransportAddr::Ip("127.0.0.1:1234".parse().unwrap()),
                TransportAddr::Ip("192.168.1.1:5678".parse().unwrap()),
            ],
        );

        // Serialize to EndpointTicket string
        let ticket_str = EndpointTicket::new(endpoint_addr.clone()).encode_string();
        assert!(
            ticket_str.starts_with("endpoint"),
            "EndpointTicket should start with 'endpoint' prefix: {ticket_str}"
        );

        // Deserialize back
        let ticket = <EndpointTicket as Ticket>::decode_string(&ticket_str).unwrap();
        let round_tripped = ticket.endpoint_addr();

        assert_eq!(endpoint_addr.id, round_tripped.id);
        assert_eq!(
            endpoint_addr.ip_addrs().collect::<Vec<_>>(),
            round_tripped.ip_addrs().collect::<Vec<_>>()
        );
    }
}
