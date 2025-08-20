//! Protocol definitions for sync communication.
//!
//! This module defines transport-agnostic message types that can be
//! used across different network transports (HTTP, Iroh, Bluetooth, etc.).

use crate::entry::{Entry, ID};
use serde::{Deserialize, Serialize};

/// Handshake request sent when establishing a peer connection.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct HandshakeRequest {
    /// Unique device identifier
    pub device_id: String,
    /// Ed25519 public key of the sender
    pub public_key: String,
    /// Optional human-readable display name
    pub display_name: Option<String>,
    /// Protocol version number
    pub protocol_version: u32,
    /// Random challenge bytes for signature verification
    pub challenge: Vec<u8>,
}

/// Handshake response sent in reply to a handshake request.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct HandshakeResponse {
    /// Unique device identifier
    pub device_id: String,
    /// Ed25519 public key of the responder
    pub public_key: String,
    /// Optional human-readable display name
    pub display_name: Option<String>,
    /// Protocol version number
    pub protocol_version: u32,
    /// Signed challenge from the request
    pub challenge_response: Vec<u8>,
    /// New challenge for mutual authentication
    pub new_challenge: Vec<u8>,
}

/// Request for tree tips to determine sync state.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct GetTipsRequest {
    /// Tree ID to get tips for
    pub tree_id: ID,
}

/// Response containing tree tips.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct GetTipsResponse {
    /// Tree ID these tips belong to
    pub tree_id: ID,
    /// Current tip entry IDs for the tree
    pub tips: Vec<ID>,
}

/// Request for specific entries.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct GetEntriesRequest {
    /// Entry IDs to retrieve
    pub entry_ids: Vec<ID>,
}

/// Response containing requested entries.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct GetEntriesResponse {
    /// The requested entries
    pub entries: Vec<Entry>,
}

/// Request messages that can be sent to a sync peer.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum SyncRequest {
    /// Initial handshake request
    Handshake(HandshakeRequest),
    /// Send entries for synchronization
    SendEntries(Vec<Entry>),
    /// Request tree tips
    GetTips(GetTipsRequest),
    /// Request specific entries
    GetEntries(GetEntriesRequest),
}

/// Response messages returned from a sync peer.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum SyncResponse {
    /// Handshake response
    Handshake(HandshakeResponse),
    /// Acknowledgment that entries were received successfully
    Ack,
    /// Number of entries received (for multiple entries)
    Count(usize),
    /// Tree tips response
    Tips(GetTipsResponse),
    /// Entries response
    Entries(GetEntriesResponse),
    /// Error response
    Error(String),
}

/// Current protocol version
pub const PROTOCOL_VERSION: u32 = 1;
