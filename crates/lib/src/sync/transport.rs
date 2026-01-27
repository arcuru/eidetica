//! Network transport management for the sync system.

use std::sync::Arc;
use tokio::sync::oneshot;
use tracing::info;

use super::{
    Sync, SyncError, TRANSPORT_STATE_STORE,
    background::BackgroundSync,
    background::SyncCommand,
    transports::{SyncTransport, TransportBuilder},
};
use crate::{
    Result,
    crdt::{Doc, doc::Value},
    store::DocStore,
};

impl Sync {
    // === Network Transport Methods ===

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
                name: None, // Stop all transports
                response: tx,
            })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        rx.await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))?
    }

    /// Start accepting incoming connections on all registered transports.
    ///
    /// Must be called each time the instance is created to accept inbound sync requests.
    /// This starts servers on all transports that have been registered via `register_transport()`.
    /// Each transport uses its pre-configured bind address.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// instance.enable_sync().await?;
    /// let sync = instance.sync().unwrap();
    ///
    /// // Register transports with their configurations
    /// sync.register_transport("http-local", HttpTransport::builder()
    ///     .bind("127.0.0.1:8080")
    /// ).await?;
    /// sync.register_transport("p2p", IrohTransport::builder()).await?;
    ///
    /// // Start all servers
    /// sync.accept_connections().await?;
    /// ```
    pub async fn accept_connections(&self) -> Result<()> {
        // Ensure transports are enabled
        if self.background_tx.get().is_none() {
            return Err(SyncError::NoTransportEnabled.into());
        }

        // Start servers on all registered transports
        // Each transport uses its pre-configured bind address
        let (tx, rx) = oneshot::channel();

        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::StartServer {
                name: None, // Start all
                response: tx,
            })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        rx.await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))??;

        info!("Started servers on all registered transports");
        Ok(())
    }

    /// Start accepting incoming connections on a specific named transport.
    ///
    /// Use this when you want fine-grained control over which transports
    /// accept connections and when.
    ///
    /// # Arguments
    /// * `name` - The name of the transport to start (as used in `register_transport`)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Register transports
    /// sync.register_transport("http-local", HttpTransport::builder()
    ///     .bind("127.0.0.1:8080")
    /// ).await?;
    /// sync.register_transport("p2p", IrohTransport::builder()).await?;
    ///
    /// // Only start HTTP server (P2P stays inactive)
    /// sync.accept_connections_on("http-local").await?;
    /// ```
    pub async fn accept_connections_on(&self, name: impl Into<String>) -> Result<()> {
        let name = name.into();

        if self.background_tx.get().is_none() {
            return Err(SyncError::NoTransportEnabled.into());
        }

        let (tx, rx) = oneshot::channel();

        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::StartServer {
                name: Some(name.clone()),
                response: tx,
            })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        rx.await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))??;

        info!(name = %name, "Started server on transport");
        Ok(())
    }

    /// Add a named transport to the sync system.
    ///
    /// If a transport with the same name already exists, it will be replaced
    /// (the old transport's server will be stopped if running).
    ///
    /// # Arguments
    /// * `name` - Unique name for this transport instance (e.g., "http-local", "p2p")
    /// * `transport` - The transport to add
    pub async fn add_transport(
        &self,
        name: impl Into<String>,
        transport: Box<dyn SyncTransport>,
    ) -> Result<()> {
        // Ensure background sync is running
        self.start_background_sync()?;

        let name = name.into();
        let (tx, rx) = oneshot::channel();

        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::AddTransport {
                name,
                transport,
                response: tx,
            })
            .await
            .map_err(|e| SyncError::CommandSendError(e.to_string()))?;

        rx.await
            .map_err(|e| SyncError::Network(format!("Response channel error: {e}")))?
    }

    /// Register a named transport instance with persisted state.
    ///
    /// This is the recommended way to add transports. The builder's `build()` method
    /// receives persisted state for this named instance (may be empty on first run)
    /// and can update it. The updated state is automatically saved.
    ///
    /// # Arguments
    /// * `name` - Unique name for this transport instance (e.g., "http-local", "p2p")
    /// * `builder` - The transport builder that creates the transport
    ///
    /// # Example
    ///
    /// ```ignore
    /// use eidetica::sync::transports::http::HttpTransport;
    /// use eidetica::sync::transports::iroh::IrohTransport;
    ///
    /// // Register an HTTP transport with bind address
    /// sync.register_transport("http-local", HttpTransport::builder()
    ///     .bind("127.0.0.1:8080")
    /// ).await?;
    ///
    /// // Register an Iroh transport (generates and persists node ID on first run)
    /// sync.register_transport("p2p", IrohTransport::builder()).await?;
    ///
    /// // Register multiple Iroh transports with different identities
    /// sync.register_transport("p2p-work", IrohTransport::builder()).await?;
    /// sync.register_transport("p2p-personal", IrohTransport::builder()).await?;
    /// ```
    pub async fn register_transport<B: TransportBuilder>(
        &self,
        name: impl Into<String>,
        builder: B,
    ) -> Result<()>
    where
        B::Transport: 'static,
    {
        let name = name.into();

        // Load persisted state for this named instance
        let persisted = self.load_transport_state(&name).await?;

        // Build transport - may generate/update persisted state
        let (transport, updated) = builder.build(persisted).await?;

        // Save updated persisted state if changed
        if let Some(state) = updated {
            self.save_transport_state(&name, &state).await?;
        }

        // Add transport by name
        self.add_transport(name, Box::new(transport)).await
    }

    /// Load persisted state for a named transport instance.
    ///
    /// Returns an empty Doc if no state exists for this transport.
    async fn load_transport_state(&self, name: &str) -> Result<Doc> {
        let tx = self.sync_tree.new_transaction().await?;
        let store = tx.get_store::<DocStore>(TRANSPORT_STATE_STORE).await?;

        match store.get(name).await {
            Ok(Value::Doc(doc)) => Ok(doc),
            Ok(_) => Ok(Doc::new()), // Unexpected value type
            Err(e) if e.is_not_found() => Ok(Doc::new()),
            Err(e) => Err(e),
        }
    }

    /// Save persisted state for a named transport instance.
    async fn save_transport_state(&self, name: &str, state: &Doc) -> Result<()> {
        let tx = self.sync_tree.new_transaction().await?;
        let store = tx.get_store::<DocStore>(TRANSPORT_STATE_STORE).await?;
        store.set(name, Value::Doc(state.clone())).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Start the background sync engine.
    ///
    /// The engine starts with no transports registered. Use `add_transport()`
    /// or `register_transport()` to add transports.
    pub(crate) fn start_background_sync(&self) -> Result<()> {
        if self.background_tx.get().is_some() {
            return Ok(()); // Already enabled
        }

        let sync_tree_id = self.sync_tree.root_id().clone();
        let instance = self.instance()?;

        // Create the background sync and get command sender
        let background_tx = BackgroundSync::start(instance, sync_tree_id, Arc::clone(&self.queue));

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
    /// Use `get_server_address_for` for specific transports.
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
    /// * `name` - The name of the transport
    ///
    /// # Returns
    /// The address the server is bound to for that transport.
    pub async fn get_server_address_for(&self, name: &str) -> Result<String> {
        let (tx, rx) = oneshot::channel();

        self.background_tx
            .get()
            .ok_or(SyncError::NoTransportEnabled)?
            .send(SyncCommand::GetServerAddress {
                name: name.to_string(),
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
}
