//! Iroh transport implementation for sync communication.
//!
//! This module provides peer-to-peer sync communication using
//! Iroh's QUIC-based networking with hole punching and relay servers.

use super::{SyncTransport, shared::*};
use crate::Result;
use crate::entry::Entry;
use crate::sync::error::SyncError;
use crate::sync::handler::handle_request;
use crate::sync::protocol::SyncResponse;
use async_trait::async_trait;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{Endpoint, NodeAddr};
#[allow(unused_imports)] // Used by write_all method on streams
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

const SYNC_ALPN: &[u8] = b"eidetica/v0";

/// Iroh transport implementation using QUIC peer-to-peer networking.
pub struct IrohTransport {
    /// The Iroh endpoint for P2P communication.
    endpoint: Option<Endpoint>,
    /// Shared server state management.
    server_state: ServerState,
}

impl IrohTransport {
    /// Create a new Iroh transport instance.
    pub fn new() -> Result<Self> {
        Ok(Self {
            endpoint: None,
            server_state: ServerState::new(),
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
        shutdown_rx: oneshot::Receiver<()>,
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
            // Server loop has exited - the shutdown was triggered by stop_server()
            // which already marked the server as stopped, so no additional cleanup needed here
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

        // Deserialize the request using JsonHandler
        let request: Vec<Entry> = match JsonHandler::deserialize_request(&buffer) {
            Ok(req) => req,
            Err(e) => {
                eprintln!("Failed to deserialize request: {e}");
                return;
            }
        };

        // Handle the request
        let response = handle_request(&request).await;

        // Serialize and send response using JsonHandler
        match JsonHandler::serialize_response(&response) {
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
        if self.server_state.is_running() {
            return Err(SyncError::ServerAlreadyRunning {
                address: "iroh-endpoint".to_string(),
            }
            .into());
        }

        // Ensure we have an endpoint and get node ID before borrowing
        let endpoint = self.ensure_endpoint().await?;
        let node_id = endpoint.node_id().to_string();
        let endpoint_clone = endpoint.clone();

        // Create server coordination channels
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        // Start server loop
        self.start_server_loop(endpoint_clone, ready_tx, shutdown_rx)
            .await?;

        // Wait for server to be ready using shared utility
        wait_for_ready(ready_rx, "iroh-endpoint").await?;

        // Start server state with node ID and shutdown sender
        self.server_state.server_started(node_id, shutdown_tx);

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

    async fn send_request(&self, addr: &str, request: &[Entry]) -> Result<SyncResponse> {
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
