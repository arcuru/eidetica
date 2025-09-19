//! Error types for the synchronization module.

use thiserror::Error;

/// Errors that can occur during synchronization operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SyncError {
    /// No transport has been enabled for network operations.
    #[error("No transport enabled. Call enable_http_transport() first")]
    NoTransportEnabled,

    /// Attempted to start a server when one is already running.
    #[error("Server already running on {address}")]
    ServerAlreadyRunning { address: String },

    /// Attempted to stop a server when none is running.
    #[error("Server not running")]
    ServerNotRunning,

    /// Unexpected response type received from peer.
    #[error("Unexpected response type: expected {expected}, got {actual}")]
    UnexpectedResponse {
        expected: &'static str,
        actual: String,
    },

    /// Network communication error.
    #[error("Network error: {0}")]
    Network(String),

    /// Command channel send error.
    #[error("Failed to send command to background sync: {0}")]
    CommandSendError(String),

    /// Transport initialization error.
    #[error("Failed to initialize transport: {0}")]
    TransportInit(String),

    /// Runtime creation error for async operations.
    #[error("Failed to create async runtime: {0}")]
    RuntimeCreation(String),

    /// Server bind error.
    #[error("Failed to bind server to {address}: {reason}")]
    ServerBind { address: String, reason: String },

    /// Client connection error.
    #[error("Failed to connect to {address}: {reason}")]
    ConnectionFailed { address: String, reason: String },

    /// Device key not found in backend storage.
    #[error("Device key '{key_name}' not found in backend storage")]
    DeviceKeyNotFound { key_name: String },

    /// Transport type not supported by this transport implementation.
    #[error("Transport type '{transport_type}' not supported")]
    UnsupportedTransport { transport_type: String },

    /// Peer not found.
    #[error("Peer not found: {0}")]
    PeerNotFound(String),

    /// Peer already exists.
    #[error("Peer already exists: {0}")]
    PeerAlreadyExists(String),

    /// Serialization error.
    #[error("Serialization error: {0}")]
    SerializationError(String),

    /// Protocol version mismatch.
    #[error("Protocol version mismatch: expected {expected}, received {received}")]
    ProtocolMismatch { expected: u32, received: u32 },

    /// Handshake failed.
    #[error("Handshake failed: {0}")]
    HandshakeFailed(String),

    /// Entry not found in backend storage.
    #[error("Entry not found: {0}")]
    EntryNotFound(crate::entry::ID),

    /// Invalid entry received (validation failed).
    #[error("Invalid entry: {0}")]
    InvalidEntry(String),

    /// Sync protocol error.
    #[error("Sync protocol error: {0}")]
    SyncProtocolError(String),

    /// Backend storage error.
    #[error("Backend error: {0}")]
    BackendError(String),

    /// Bootstrap request not found.
    #[error("Bootstrap request not found: {0}")]
    RequestNotFound(String),

    /// Bootstrap request already exists.
    #[error("Bootstrap request already exists: {0}")]
    RequestAlreadyExists(String),

    /// Invalid bootstrap request state.
    #[error(
        "Invalid request state for '{request_id}': expected {expected_status}, found {current_status}"
    )]
    InvalidRequestState {
        request_id: String,
        current_status: String,
        expected_status: String,
    },

    /// Invalid data format in stored bootstrap request.
    #[error("Invalid data: {0}")]
    InvalidData(String),
}

impl SyncError {
    /// Check if this is a configuration error (no transport enabled).
    pub fn is_configuration_error(&self) -> bool {
        matches!(self, SyncError::NoTransportEnabled)
    }

    /// Check if this is a server lifecycle error.
    pub fn is_server_error(&self) -> bool {
        matches!(
            self,
            SyncError::ServerAlreadyRunning { .. }
                | SyncError::ServerNotRunning
                | SyncError::ServerBind { .. }
        )
    }

    /// Check if this is a network/connection error.
    pub fn is_network_error(&self) -> bool {
        matches!(
            self,
            SyncError::Network(_) | SyncError::ConnectionFailed { .. }
        )
    }

    /// Check if this is a protocol error (unexpected response).
    pub fn is_protocol_error(&self) -> bool {
        matches!(self, SyncError::UnexpectedResponse { .. })
    }

    /// Check if this is a not found error.
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            SyncError::PeerNotFound(_) | SyncError::EntryNotFound(_)
        )
    }

    /// Check if this is a validation error.
    pub fn is_validation_error(&self) -> bool {
        matches!(self, SyncError::InvalidEntry(_))
    }

    /// Check if this is a backend error.
    pub fn is_backend_error(&self) -> bool {
        matches!(self, SyncError::BackendError(_))
    }
}
