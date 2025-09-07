//! Shared utilities for transport implementations.
//!
//! This module provides common functionality used across different transport
//! implementations to reduce code duplication and ensure consistency.

use tokio::sync::oneshot;

use crate::sync::{
    error::SyncError,
    protocol::{SyncRequest, SyncResponse},
};

/// Manages server state common to all transport implementations.
/// Since transports are owned exclusively by Sync instances and all operations
/// require &mut self, no internal locking is needed - the Rust ownership system
/// provides the necessary synchronization guarantees.
pub struct ServerState {
    /// Whether the server is running.
    running: bool,
    /// Shutdown signal for the server loop.
    shutdown: Option<oneshot::Sender<()>>,
    /// The server's address.
    address: Option<String>,
}

impl Default for ServerState {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerState {
    /// Create a new server state manager.
    pub fn new() -> Self {
        Self {
            running: false,
            shutdown: None,
            address: None,
        }
    }

    /// Check if the server is currently running.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Get the server address if available.
    pub fn get_address(&self) -> Result<String, SyncError> {
        if let Some(addr) = &self.address {
            Ok(addr.clone())
        } else {
            Err(SyncError::ServerNotRunning)
        }
    }

    /// Start the server by setting it as running with the given address and shutdown sender.
    /// This combines the commonly used pair: set_running + set_shutdown_sender.
    pub fn server_started(&mut self, address: String, shutdown_sender: oneshot::Sender<()>) {
        self.running = true;
        self.address = Some(address);
        self.shutdown = Some(shutdown_sender);
    }

    /// Stop the server by triggering shutdown and clearing state.
    /// This combines the commonly used pair: trigger_shutdown + set_stopped.
    pub fn stop_server(&mut self) {
        // First trigger shutdown if we have a sender
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        // Then mark as stopped and clear address
        self.running = false;
        self.address = None;
    }
}

/// Utilities for handling JSON serialization/deserialization in transports.
pub struct JsonHandler;

impl JsonHandler {
    /// Serialize a SyncRequest to JSON bytes.
    pub fn serialize_request(request: &SyncRequest) -> Result<Vec<u8>, SyncError> {
        serde_json::to_vec(request)
            .map_err(|e| SyncError::Network(format!("Failed to serialize request: {e}")))
    }

    /// Serialize a SyncResponse to JSON bytes.
    pub fn serialize_response(response: &SyncResponse) -> Result<Vec<u8>, SyncError> {
        serde_json::to_vec(response)
            .map_err(|e| SyncError::Network(format!("Failed to serialize response: {e}")))
    }

    /// Deserialize JSON bytes to a SyncRequest.
    pub fn deserialize_request(bytes: &[u8]) -> Result<SyncRequest, SyncError> {
        serde_json::from_slice(bytes)
            .map_err(|e| SyncError::Network(format!("Failed to deserialize request: {e}")))
    }

    /// Deserialize JSON bytes to a SyncResponse.
    pub fn deserialize_response(bytes: &[u8]) -> Result<SyncResponse, SyncError> {
        serde_json::from_slice(bytes)
            .map_err(|e| SyncError::Network(format!("Failed to deserialize response: {e}")))
    }
}

/// Waits for server ready signal and maps errors appropriately.
pub async fn wait_for_ready(
    ready_rx: oneshot::Receiver<()>,
    address: &str,
) -> Result<(), SyncError> {
    ready_rx.await.map_err(|_| SyncError::ServerBind {
        address: address.to_string(),
        reason: "Server startup failed".to_string(),
    })
}
