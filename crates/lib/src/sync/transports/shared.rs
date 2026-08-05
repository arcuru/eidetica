//! Shared utilities for transport implementations.
//!
//! This module provides common functionality used across different transport
//! implementations to reduce code duplication and ensure consistency.

use std::sync::Mutex;

use tokio::sync::oneshot;

use crate::sync::{
    error::SyncError,
    protocol::{SyncRequest, SyncResponse},
};

/// Manages server state common to all transport implementations.
///
/// Transports are shared rather than owned exclusively, so that an outbound
/// request can be served from a clone while the engine goes on handling other
/// commands. That rules out `&mut self`, so the mutable parts live behind a
/// lock. It is a `std` mutex deliberately: every critical section here is a
/// few field assignments with no await inside, so an async mutex would buy
/// nothing and cost a scheduling point.
pub struct ServerState {
    inner: Mutex<ServerStateInner>,
}

struct ServerStateInner {
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
            inner: Mutex::new(ServerStateInner {
                running: false,
                shutdown: None,
                address: None,
            }),
        }
    }

    /// Check if the server is currently running.
    pub fn is_running(&self) -> bool {
        self.lock().running
    }

    /// Get the server address if available.
    pub fn get_address(&self) -> Result<String, SyncError> {
        self.lock()
            .address
            .clone()
            .ok_or(SyncError::ServerNotRunning)
    }

    /// Start the server by setting it as running with the given address and shutdown sender.
    /// This combines the commonly used pair: set_running + set_shutdown_sender.
    pub fn server_started(&self, address: String, shutdown_sender: oneshot::Sender<()>) {
        let mut inner = self.lock();
        inner.running = true;
        inner.address = Some(address);
        inner.shutdown = Some(shutdown_sender);
    }

    /// Stop the server by triggering shutdown and clearing state.
    /// This combines the commonly used pair: trigger_shutdown + set_stopped.
    pub fn stop_server(&self) {
        let mut inner = self.lock();
        // First trigger shutdown if we have a sender
        if let Some(tx) = inner.shutdown.take() {
            let _ = tx.send(());
        }
        // Then mark as stopped and clear address
        inner.running = false;
        inner.address = None;
    }

    /// Take the lock, recovering from a poisoned one.
    ///
    /// A panic while holding this lock leaves the flags describing a server
    /// that is no longer being driven. Refusing to serve from then on would
    /// turn one panicked task into a permanently dead transport, so the state
    /// is taken as-is and the next start/stop corrects it.
    fn lock(&self) -> std::sync::MutexGuard<'_, ServerStateInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
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
