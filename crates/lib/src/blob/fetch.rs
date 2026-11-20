//! Outcomes of one bounded Database-peer blob read.

use serde::{Deserialize, Serialize};

/// Per-provider outcomes of an unsuccessful bounded read for one blob.
///
/// Each queried candidate provider contributes exactly one outcome. Only a round in
/// which every queried provider answered "absent" is an authoritative "no
/// provider we asked holds it"; any other failure leaves the answer unknown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobFetchAttempt {
    /// Candidate providers queried in the round.
    pub providers: u32,
    /// Providers that answered that they do not hold the blob.
    pub absent: u32,
    /// Providers whose transport could not establish a connection.
    pub unreachable: u32,
    /// Providers that connected and then stopped answering within the deadline.
    pub stalled: u32,
    /// Providers that returned bytes or a stream that failed verification.
    pub invalid: u32,
    /// Providers with no address this Instance has a transport for.
    pub unsupported: u32,
    /// Attempts that failed for another reason, including local publication
    /// failure when reconciling wanted blobs.
    pub failed: u32,
}

impl BlobFetchAttempt {
    /// Whether the round had no candidate provider to ask.
    pub fn had_no_providers(&self) -> bool {
        self.providers == 0
    }

    /// Whether every provider queried answered that it lacks the blob.
    pub fn absent_from_all_queried(&self) -> bool {
        self.providers > 0 && self.absent == self.providers
    }
}
