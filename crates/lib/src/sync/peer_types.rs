//! Peer management types for the sync module.
//!
//! This module defines the data structures used to track remote peers,
//! their sync relationships, and simple address information for transports.

use std::borrow::Borrow;
use std::fmt;

use serde::{Deserialize, Serialize};

/// A peer's unique identifier, derived from their public key.
///
/// The format is `ed25519:{base64_encoded_key}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PeerId(String);

impl PeerId {
    /// Create a new PeerId from a string.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Get the underlying string representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for PeerId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for PeerId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<String> for PeerId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for PeerId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<&String> for PeerId {
    fn from(s: &String) -> Self {
        Self(s.clone())
    }
}

/// Connection state for a peer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ConnectionState {
    /// Not connected to the peer
    Disconnected,
    /// Currently attempting to connect
    Connecting,
    /// Successfully connected
    Connected,
    /// Connection failed with error message
    Failed(String),
}

/// Simple address type containing transport type and address string
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Address {
    /// Transport type identifier ("http", "iroh", etc.)
    pub transport_type: String,
    /// The actual address string
    pub address: String,
}

impl Address {
    /// Create a new Address
    pub fn new(transport_type: impl Into<String>, address: impl Into<String>) -> Self {
        Self {
            transport_type: transport_type.into(),
            address: address.into(),
        }
    }

    // Helpers for the internally implemented Transports.

    /// Create an HTTP address
    pub fn http(address: impl Into<String>) -> Self {
        Self::new("http", address)
    }

    /// Create an Iroh address from a node ID string
    pub fn iroh(node_id: impl Into<String>) -> Self {
        Self::new("iroh", node_id)
    }

    /// Create an Iroh address from an EndpointAddr (requires iroh dependency)
    pub fn from_endpoint_addr(endpoint_addr: &iroh::EndpointAddr) -> Self {
        // Serialize the EndpointAddr to a string format that can be parsed later
        // For now, we'll just use the EndpointId as the address since that's what our
        // current send_request method expects
        Self::new("iroh", endpoint_addr.id.to_string())
    }
}

/// Information about a remote peer in the sync network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerInfo {
    /// The peer's unique identifier
    pub id: PeerId,
    /// Optional human-readable display name for the peer
    pub display_name: Option<String>,
    /// ISO timestamp when this peer was first seen
    pub first_seen: String,
    /// ISO timestamp when this peer was last seen/active
    pub last_seen: String,
    /// Current status of the peer
    pub status: PeerStatus,
    /// Connection addresses for this peer
    pub addresses: Vec<Address>,
    /// Current connection state
    pub connection_state: ConnectionState,
    /// ISO timestamp of last successful sync
    pub last_successful_sync: Option<String>,
    /// Number of connection attempts
    pub connection_attempts: u32,
    /// Last connection error if any
    pub last_error: Option<String>,
}

/// Status of a remote peer in the sync network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum PeerStatus {
    /// Peer is active and available for sync
    #[default]
    Active,
    /// Peer is inactive (not currently reachable)
    Inactive,
    /// Peer is blocked and should not be synced with
    Blocked,
}

impl PeerInfo {
    /// Create a new PeerInfo with the given timestamp.
    ///
    /// Use `instance.now_rfc3339()` or `transaction.now_rfc3339()` to get the timestamp.
    pub fn new_at(id: impl Into<PeerId>, display_name: Option<&str>, timestamp: String) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.map(|s| s.to_string()),
            first_seen: timestamp.clone(),
            last_seen: timestamp,
            status: PeerStatus::Active,
            addresses: Vec::new(),
            connection_state: ConnectionState::Disconnected,
            last_successful_sync: None,
            connection_attempts: 0,
            last_error: None,
        }
    }

    /// Update the last_seen timestamp.
    ///
    /// Use `instance.now_rfc3339()` or `transaction.now_rfc3339()` to get the timestamp.
    pub fn touch_at(&mut self, timestamp: String) {
        self.last_seen = timestamp;
    }

    /// Add an address if not already present
    pub fn add_address(&mut self, address: Address) {
        if !self.addresses.contains(&address) {
            self.addresses.push(address);
        }
    }

    /// Remove a specific address
    pub fn remove_address(&mut self, address: &Address) -> bool {
        let initial_len = self.addresses.len();
        self.addresses.retain(|a| a != address);
        self.addresses.len() != initial_len
    }

    /// Get addresses for a specific transport type
    pub fn get_addresses(&self, transport_type: impl AsRef<str>) -> Vec<&Address> {
        self.addresses
            .iter()
            .filter(|a| a.transport_type == transport_type.as_ref())
            .collect()
    }

    /// Get all addresses
    pub fn get_all_addresses(&self) -> &Vec<Address> {
        &self.addresses
    }

    /// Check if peer has any addresses for a transport type
    pub fn has_transport(&self, transport_type: impl AsRef<str>) -> bool {
        self.addresses
            .iter()
            .any(|a| a.transport_type == transport_type.as_ref())
    }
}
