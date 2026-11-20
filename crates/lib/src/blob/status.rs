//! Observable state of blobs an application asked an Instance to keep.
//!
//! These types report requested-blob availability separately from Database
//! synchronization. A recorded request is not proof that bytes are local, and
//! an unsuccessful fetch is not proof that no provider holds them.

use serde::{Deserialize, Serialize};

use super::BlobFetchAttempt;
use crate::entry::ID;

/// Local state of one requested blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WantedBlobState {
    /// A whole blob verified at publication is recorded locally. This does
    /// not rehash the stored payload; byte reads verify their results.
    Available,
    /// The blob's whole bytes are not held locally.
    Pending {
        /// Outcome of the most recent unsuccessful fetch round for this blob on
        /// this Instance, if still recorded. Outcomes are not durable and
        /// can be forgotten when nothing wants the CID.
        last_fetch: Option<BlobFetchAttempt>,
    },
}

/// Status of one blob in an explicitly named set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WantedBlobStatus {
    /// The blob's content address.
    pub cid: ID,
    /// Whether it is held locally, or what happened when fetching it.
    pub state: WantedBlobState,
}

impl WantedBlobStatus {
    /// Whether local storage reports the whole blob is available.
    pub fn is_available(&self) -> bool {
        matches!(self.state, WantedBlobState::Available)
    }
}
