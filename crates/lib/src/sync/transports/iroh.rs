//! Iroh transport implementation for sync communication.
//!
//! This module provides peer-to-peer sync communication using
//! Iroh's QUIC-based networking with hole punching and relay servers.

use super::SyncTransport;
use crate::Result;
use crate::sync::error::SyncError;
use crate::sync::handler::handle_request;
use crate::sync::protocol::{SyncRequest, SyncResponse};
use async_trait::async_trait;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{Endpoint, NodeAddr};
use std::sync::Arc;
#[allow(unused_imports)] // Used by write_all method on streams
use tokio::io::AsyncWriteExt;
use tokio::sync::{RwLock, oneshot};

const SYNC_ALPN: &[u8] = b"eidetica/v0";

/// Iroh transport implementation using QUIC peer-to-peer networking.
pub struct IrohTransport {
    /// The Iroh endpoint for P2P communication.
    endpoint: Option<Endpoint>,
    /// Whether the server is running.
    server_running: Arc<RwLock<bool>>,
    /// Shutdown signal for the server loop.
    server_shutdown: Arc<RwLock<Option<oneshot::Sender<()>>>>,
    /// The endpoint's node address for client connections.
    node_addr: Arc<RwLock<Option<String>>>,
}

impl IrohTransport {
    /// Create a new Iroh transport instance.
    pub fn new() -> Result<Self> {
        Ok(Self {
            endpoint: None,
            server_running: Arc::new(RwLock::new(false)),
            server_shutdown: Arc::new(RwLock::new(None)),
            node_addr: Arc::new(RwLock::new(None)),
        })
    }

    /// Initialize the Iroh endpoint if not already done.
    async fn ensure_endpoint(&mut self) -> Result<&Endpoint> {
        if self.endpoint.is_none() {
            // Create a new Iroh endpoint with ALPN support
            let endpoint = Endpoint::builder()
                .alpns(vec![SYNC_ALPN.to_vec()])
                .bind()
                .await
                .map_err(|e| {
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
    ) -> Result<()> {
        let server_running = Arc::clone(&self.server_running);
        let server_shutdown = Arc::clone(&self.server_shutdown);

        // Create shutdown channel
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

        // Store shutdown sender
        {
            let mut shutdown = server_shutdown.write().await;
            *shutdown = Some(shutdown_tx);
        }

        // Mark server as running
        {
            let mut running = server_running.write().await;
            *running = true;
        }

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
                                tokio::spawn(async move {
                                    if let Ok(conn) = connecting.await {
                                        Self::handle_connection(conn).await;
                                    }
                                });
                            }
                            None => break, // Endpoint closed
                        }
                    }
                }
            }

            // Mark server as no longer running
            let mut running = server_running.write().await;
            *running = false;
        });

        Ok(())
    }

    /// Handle an incoming connection.
    async fn handle_connection(conn: Connection) {
        // Accept incoming streams
        while let Ok((send_stream, recv_stream)) = conn.accept_bi().await {
            tokio::spawn(Self::handle_stream(send_stream, recv_stream));
        }
    }

    /// Handle an incoming bidirectional stream.
    async fn handle_stream(mut send_stream: SendStream, mut recv_stream: RecvStream) {
        // Read the request with size limit (1MB)
        let buffer: Vec<u8> = match recv_stream.read_to_end(1024 * 1024).await {
            Ok(buffer) => buffer,
            Err(e) => {
                eprintln!("Failed to read stream: {e}");
                return;
            }
        };

        // Deserialize the request
        let request: SyncRequest = match serde_json::from_slice(&buffer) {
            Ok(req) => req,
            Err(e) => {
                eprintln!("Failed to deserialize request: {e}");
                return;
            }
        };

        // Handle the request
        let response = handle_request(request).await;

        // Serialize and send response
        match serde_json::to_vec(&response) {
            Ok(response_bytes) => {
                if let Err(e) = send_stream.write_all(&response_bytes).await {
                    eprintln!("Failed to write response: {e}");
                    return;
                }
                if let Err(e) = send_stream.finish() {
                    eprintln!("Failed to finish stream: {e}");
                }
            }
            Err(e) => {
                eprintln!("Failed to serialize response: {e}");
            }
        }
    }
}

#[async_trait]
impl SyncTransport for IrohTransport {
    async fn start_server(&mut self, _addr: &str) -> Result<()> {
        // Check if server is already running
        if self.is_server_running() {
            return Err(SyncError::ServerAlreadyRunning {
                address: "iroh-endpoint".to_string(),
            }
            .into());
        }

        // Ensure we have an endpoint and get node ID before borrowing
        let endpoint = self.ensure_endpoint().await?;
        let node_id = endpoint.node_id().to_string();
        let endpoint_clone = endpoint.clone();

        // Store the endpoint address as node ID string
        {
            let mut addr_lock = self.node_addr.write().await;
            *addr_lock = Some(node_id);
        }

        // Create ready channel
        let (ready_tx, ready_rx) = oneshot::channel::<()>();

        // Start server loop
        self.start_server_loop(endpoint_clone, ready_tx).await?;

        // Wait for server to be ready
        ready_rx.await.map_err(|_| SyncError::ServerBind {
            address: "iroh-endpoint".to_string(),
            reason: "Server startup failed".to_string(),
        })?;

        Ok(())
    }

    async fn stop_server(&mut self) -> Result<()> {
        if !self.is_server_running() {
            return Err(SyncError::ServerNotRunning.into());
        }

        // Send shutdown signal
        let mut shutdown = self.server_shutdown.write().await;
        if let Some(tx) = shutdown.take() {
            let _ = tx.send(());
        }

        // Mark server as stopped immediately
        {
            let mut running = self.server_running.write().await;
            *running = false;
        }

        // Clear node address
        let mut addr_lock = self.node_addr.write().await;
        *addr_lock = None;

        Ok(())
    }

    async fn send_request(&self, addr: &str, request: SyncRequest) -> Result<SyncResponse> {
        // Ensure we have an endpoint
        let endpoint = match &self.endpoint {
            Some(endpoint) => endpoint,
            None => {
                return Err(
                    SyncError::TransportInit("Endpoint not initialized".to_string()).into(),
                );
            }
        };

        // Parse the target node address - for now, just treat as node ID
        let node_addr = NodeAddr::new(addr.parse().map_err(|e| SyncError::ConnectionFailed {
            address: addr.to_string(),
            reason: format!("Invalid NodeId: {e}"),
        })?);

        // Connect to the peer
        let conn = endpoint.connect(node_addr, SYNC_ALPN).await.map_err(|e| {
            SyncError::ConnectionFailed {
                address: addr.to_string(),
                reason: e.to_string(),
            }
        })?;

        // Open a bidirectional stream
        let (mut send_stream, mut recv_stream) = conn
            .open_bi()
            .await
            .map_err(|e| SyncError::Network(format!("Failed to open stream: {e}")))?;

        // Serialize and send the request
        let request_bytes = serde_json::to_vec(&request)
            .map_err(|e| SyncError::Network(format!("Failed to serialize request: {e}")))?;

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

        // Deserialize the response
        let response: SyncResponse = serde_json::from_slice(&response_bytes)
            .map_err(|e| SyncError::Network(format!("Failed to deserialize response: {e}")))?;

        Ok(response)
    }

    fn is_server_running(&self) -> bool {
        // Use try_read to avoid needing async context
        if let Ok(running) = self.server_running.try_read() {
            *running
        } else {
            // If we can't get the lock, assume it's being modified (likely running)
            true
        }
    }

    fn get_server_address(&self) -> Result<String> {
        if let Ok(addr_lock) = self.node_addr.try_read() {
            if let Some(addr) = addr_lock.as_ref() {
                Ok(addr.clone())
            } else {
                Err(SyncError::ServerNotRunning.into())
            }
        } else {
            Err(SyncError::ServerNotRunning.into())
        }
    }
}
