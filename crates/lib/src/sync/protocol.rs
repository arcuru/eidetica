//! Protocol definitions for sync communication.
//!
//! This module defines transport-agnostic message types that can be
//! used across different network transports (HTTP, Iroh, Bluetooth, etc.).

use serde::{Deserialize, Serialize};

/// Request messages that can be sent to a sync peer.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum SyncRequest {
    /// Request a hello message from the peer.
    Hello,
    /// Request the current sync status from the peer.
    Status,
}

/// Response messages returned from a sync peer.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum SyncResponse {
    /// Hello message response containing a greeting.
    Hello(String),
    /// Status response containing current sync status.
    Status(String),
}
