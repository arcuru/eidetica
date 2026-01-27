//! Protocol definitions for sync communication.
//!
//! This module defines transport-agnostic message types that can be
//! used across different network transports (HTTP, Iroh, Bluetooth, etc.).

use serde::{Deserialize, Serialize};

use super::peer_types::Address;
use crate::{
    auth::Permission,
    entry::{Entry, ID},
};

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
    /// Addresses where this peer can be reached for sync
    pub listen_addresses: Vec<Address>,
}

/// Information about a tree available for sync
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TreeInfo {
    /// The root ID of the tree
    pub tree_id: ID,
    /// Optional human-readable name for the tree
    pub name: Option<String>,
    /// Number of entries in the tree
    pub entry_count: usize,
    /// Unix timestamp of last modification
    pub last_modified: u64,
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
    /// Trees available for synchronization
    pub available_trees: Vec<TreeInfo>,
}

/// Unified sync request for both bootstrap and incremental sync
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SyncTreeRequest {
    /// Database ID to sync
    pub tree_id: ID,
    /// Our current tips (empty vec signals bootstrap needed)
    pub our_tips: Vec<ID>,
    /// Device public key of the requesting peer (used for automatic tree/peer relationship tracking)
    pub peer_pubkey: Option<String>,
    // Note: requesting_key is unverified but this is safe - see handler.rs
    // handle_bootstrap_request() for detailed explanation.
    /// Authentication key requesting access (for bootstrap)
    pub requesting_key: Option<String>,
    /// Key name/identifier for the requesting key
    pub requesting_key_name: Option<String>,
    /// Desired permission level for bootstrap
    pub requested_permission: Option<Permission>,
}

/// Bootstrap response containing complete tree state
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct BootstrapResponse {
    /// Database ID being bootstrapped
    pub tree_id: ID,
    /// The root entry of the tree
    pub root_entry: Entry,
    /// All entries in the tree (excluding root)
    pub all_entries: Vec<Entry>,
    /// Whether the requesting key was approved and added
    pub key_approved: bool,
    /// The permission level granted (if approved)
    pub granted_permission: Option<Permission>,
}

/// Incremental sync response for existing trees
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct IncrementalResponse {
    /// Database ID being synced
    pub tree_id: ID,
    /// Peer's current tips
    pub their_tips: Vec<ID>,
    /// Entries missing from our tree
    pub missing_entries: Vec<Entry>,
}

/// Request messages that can be sent to a sync peer.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum SyncRequest {
    /// Initial handshake request
    Handshake(HandshakeRequest),
    /// Unified tree sync request (handles both bootstrap and incremental)
    SyncTree(SyncTreeRequest),
    /// Send entries for synchronization (backward compatibility)
    SendEntries(Vec<Entry>),
}

/// Response messages returned from a sync peer.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum SyncResponse {
    /// Handshake response
    Handshake(HandshakeResponse),
    /// Full database bootstrap for new peers
    Bootstrap(BootstrapResponse),
    /// Incremental sync for existing peers
    Incremental(IncrementalResponse),
    /// Bootstrap request pending manual approval
    BootstrapPending {
        /// Unique identifier for the pending request
        request_id: String,
        /// Human-readable message about the pending status
        message: String,
    },
    /// Acknowledgment that entries were received successfully
    Ack,
    /// Number of entries received (for multiple entries)
    Count(usize),
    /// Error response
    Error(String),
}

/// Current protocol version - 0 indicates unstable
pub const PROTOCOL_VERSION: u32 = 0;

/// Context information about the incoming request.
///
/// This struct captures metadata about the connection that initiated
/// the request, allowing the handler to know where the request came from.
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    /// The remote address from which this request originated.
    /// Extracted from the transport layer's connection metadata.
    pub remote_address: Option<Address>,
    /// The public key of the peer making this request.
    /// Set after successful handshake to identify the authenticated peer.
    pub peer_pubkey: Option<String>,
}
