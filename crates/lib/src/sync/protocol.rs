//! Protocol definitions for sync communication.
//!
//! This module defines transport-agnostic message types that can be
//! used across different network transports (HTTP, Iroh, Bluetooth, etc.).

use crate::entry::Entry;
use serde::{Deserialize, Serialize};

/// Request messages that can be sent to a sync peer.
/// Just a list of entries to synchronize.
pub type SyncRequest = Vec<Entry>;

/// Response messages returned from a sync peer.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum SyncResponse {
    /// Acknowledgment that entries were received successfully.
    Ack,
    /// Number of entries received (for multiple entries).
    Count(usize),
}
