//! Peer management types for the sync module.
//!
//! This module defines the data structures used to track remote peers,
//! their sync relationships, and simple address information for transports.

use serde::{Deserialize, Serialize};

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

    /// Create an Iroh address
    pub fn iroh(node_id: impl Into<String>) -> Self {
        Self::new("iroh", node_id)
    }
}

/// Information about a remote peer in the sync network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerInfo {
    /// The peer's public key (formatted as ed25519:base64)
    pub pubkey: String,
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
}

/// Status of a remote peer in the sync network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PeerStatus {
    /// Peer is active and available for sync
    Active,
    /// Peer is inactive (not currently reachable)
    Inactive,
    /// Peer is blocked and should not be synced with
    Blocked,
}

impl PeerInfo {
    /// Create a new PeerInfo with current timestamp.
    pub fn new(pubkey: String, display_name: Option<&str>) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            pubkey,
            display_name: display_name.map(|s| s.to_string()),
            first_seen: now.clone(),
            last_seen: now,
            status: PeerStatus::Active,
            addresses: Vec::new(),
        }
    }

    /// Update the last_seen timestamp to now.
    pub fn touch(&mut self) {
        self.last_seen = chrono::Utc::now().to_rfc3339();
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
    pub fn get_addresses(&self, transport_type: &str) -> Vec<&Address> {
        self.addresses
            .iter()
            .filter(|a| a.transport_type == transport_type)
            .collect()
    }

    /// Get all addresses
    pub fn get_all_addresses(&self) -> &Vec<Address> {
        &self.addresses
    }

    /// Check if peer has any addresses for a transport type
    pub fn has_transport(&self, transport_type: &str) -> bool {
        self.addresses
            .iter()
            .any(|a| a.transport_type == transport_type)
    }
}

impl Default for PeerStatus {
    fn default() -> Self {
        Self::Active
    }
}
