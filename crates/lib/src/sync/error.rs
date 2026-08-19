//! Error types for the synchronization module.

use std::time::Duration;

use thiserror::Error;

use super::peer_types::Address;
use crate::{auth::Permission, entry::ID};

/// Which part of an outbound request ran out of time.
///
/// Carries what the peer proved about itself before the deadline: a peer that
/// never completed a connection said nothing, while one that connected and then
/// stopped answering is reachable and merely unresponsive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutPhase {
    /// The connection was never established, so the peer may not be there at all.
    Connect,
    /// The connection was established and the exchange then stalled, so the peer
    /// is reachable.
    Request,
}

impl TimeoutPhase {
    /// Whether the peer answered far enough to prove it is reachable.
    pub fn peer_reachable(&self) -> bool {
        matches!(self, TimeoutPhase::Request)
    }
}

impl std::fmt::Display for TimeoutPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TimeoutPhase::Connect => write!(f, "connecting"),
            TimeoutPhase::Request => write!(f, "awaiting a response"),
        }
    }
}

/// Errors that can occur during synchronization operations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SyncError {
    /// No transport has been enabled for network operations.
    #[error("No transport enabled. Call enable_http_transport() first")]
    NoTransportEnabled,

    /// Sync has not been enabled on the Instance.
    #[error("Sync is not enabled on this Instance. Call Instance::enable_sync() first")]
    SyncNotEnabled,

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

    /// A peer did not answer within the transport's deadline.
    ///
    /// Kept apart from [`SyncError::ConnectionFailed`] and
    /// [`SyncError::Network`] because a caller reacts differently to silence
    /// than to a refusal: a peer that never answered is worth asking again,
    /// while one that answered with an error usually is not. `phase` carries
    /// what the peer proved about itself before the deadline.
    #[error("Request to {address} timed out after {elapsed:?} while {phase}")]
    Timeout {
        address: String,
        phase: TimeoutPhase,
        elapsed: Duration,
    },

    /// Device key not found in backend storage.
    #[error("Device key '{key_name}' not found in backend storage")]
    DeviceKeyNotFound { key_name: String },

    /// Transport type not supported by this transport implementation.
    #[error("Transport type '{transport_type}' not supported")]
    UnsupportedTransport { transport_type: String },

    /// Invalid address format.
    #[error("Invalid address: {0}")]
    InvalidAddress(String),

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
    EntryNotFound(ID),

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

    /// Insufficient permission for the requested operation.
    #[error(
        "Insufficient permission for request '{request_id}': required {required_permission}, but key has {actual_permission:?}"
    )]
    InsufficientPermission {
        request_id: String,
        required_permission: String,
        actual_permission: Permission,
    },

    /// A request that would be served data carried no proof of key possession.
    #[error("Authentication required to read database '{0}'")]
    AuthenticationRequired(String),

    /// The proof of key possession did not hold up.
    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    /// The caller proved its key, but that key has no read access.
    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    /// Invalid public key provided.
    #[error("Invalid public key: {reason}")]
    InvalidPublicKey { reason: String },

    /// Invalid key name provided.
    #[error("Invalid key name: {reason}")]
    InvalidKeyName { reason: String },

    /// Instance has been dropped and is no longer available.
    #[error("Instance has been dropped")]
    InstanceDropped,

    /// Bootstrap request is pending manual approval.
    #[error("Bootstrap request pending approval (request_id: {request_id}): {message}")]
    BootstrapPending { request_id: String, message: String },

    /// Transport configuration type mismatch.
    #[error("Transport config type mismatch for '{name}': expected '{expected}', found '{found}'")]
    TransportTypeMismatch {
        name: String,
        expected: String,
        found: String,
    },

    /// Transport not found by name.
    #[error("Transport not found: {name}")]
    TransportNotFound { name: String },

    /// No transport can handle the given address.
    #[error("No transport can handle address: {address:?}")]
    NoTransportForAddress { address: Address },

    /// Multiple transport operations failed.
    #[error("Multiple transport errors: {}", errors.join(", "))]
    MultipleTransportErrors { errors: Vec<String> },
}

impl SyncError {
    /// Check if this is a configuration error: the sync stack isn't ready
    /// (sync not attached or no transport registered).
    pub fn is_configuration_error(&self) -> bool {
        matches!(
            self,
            SyncError::NoTransportEnabled | SyncError::SyncNotEnabled
        )
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
    ///
    /// Includes timeouts: a deadline that expires is one way a network call
    /// fails, and callers classifying by this predicate should not have to
    /// learn about a new variant to keep treating it as one.
    pub fn is_network_error(&self) -> bool {
        matches!(
            self,
            SyncError::Network(_) | SyncError::ConnectionFailed { .. } | SyncError::Timeout { .. }
        )
    }

    /// Check if this is a timeout, and if so in which phase.
    ///
    /// `Some(TimeoutPhase::Request)` means the peer was reachable and stopped
    /// answering; `Some(TimeoutPhase::Connect)` means it never answered at all.
    pub fn timeout_phase(&self) -> Option<TimeoutPhase> {
        match self {
            SyncError::Timeout { phase, .. } => Some(*phase),
            _ => None,
        }
    }

    /// Check if this is a timeout rather than a refusal or a protocol failure.
    pub fn is_timeout(&self) -> bool {
        self.timeout_phase().is_some()
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
        matches!(
            self,
            SyncError::InvalidEntry(_)
                | SyncError::InvalidPublicKey { .. }
                | SyncError::InvalidKeyName { .. }
        )
    }

    /// Check if this is a backend error.
    pub fn is_backend_error(&self) -> bool {
        matches!(self, SyncError::BackendError(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timeout(phase: TimeoutPhase) -> SyncError {
        SyncError::Timeout {
            address: "127.0.0.1:8080".to_string(),
            phase,
            elapsed: Duration::from_secs(30),
        }
    }

    /// The point of the variant: a caller can tell silence from a refusal
    /// without matching on the message text.
    #[test]
    fn a_timeout_is_distinguishable_from_a_refusal() {
        let refused = SyncError::ConnectionFailed {
            address: "127.0.0.1:8080".to_string(),
            reason: "connection refused".to_string(),
        };

        assert!(timeout(TimeoutPhase::Request).is_timeout());
        assert!(!refused.is_timeout());
        assert!(!SyncError::Network("read failed".to_string()).is_timeout());
    }

    /// Callers that already classify by `is_network_error` keep working: a
    /// deadline expiring is still a way a network call failed.
    #[test]
    fn a_timeout_is_still_a_network_error() {
        assert!(timeout(TimeoutPhase::Connect).is_network_error());
        assert!(timeout(TimeoutPhase::Request).is_network_error());
    }

    /// The reachability distinction the two transports draw survives into the
    /// error, rather than being encoded by which variant was chosen.
    #[test]
    fn the_phase_records_whether_the_peer_answered_at_all() {
        assert_eq!(
            timeout(TimeoutPhase::Connect).timeout_phase(),
            Some(TimeoutPhase::Connect)
        );
        assert!(!TimeoutPhase::Connect.peer_reachable());
        assert!(TimeoutPhase::Request.peer_reachable());

        assert_eq!(SyncError::ServerNotRunning.timeout_phase(), None);
    }

    /// The message has to name the peer and the deadline, since that is what a
    /// log reader has to act on.
    #[test]
    fn the_message_names_the_peer_and_the_deadline() {
        let rendered = timeout(TimeoutPhase::Request).to_string();
        assert!(rendered.contains("127.0.0.1:8080"), "{rendered}");
        assert!(rendered.contains("30s"), "{rendered}");
        assert!(rendered.contains("awaiting a response"), "{rendered}");
    }
}
