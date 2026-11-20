//! Content-addressed blob references, verified byte ranges and the wanted
//! sets that Database registrations ask an Instance to keep.
//!
//! Bytes live outside the Entry DAG. Store an optional [`BlobRef`] in any
//! ordinary Store value and read bytes through [`crate::Instance`].

pub(crate) mod bao;
mod blob_ref;
mod fetch;
mod status;

pub use blob_ref::BlobRef;
pub use fetch::BlobFetchAttempt;
pub use status::{WantedBlobState, WantedBlobStatus};

/// Maximum number of CIDs in one peer availability query.
pub const MAX_BLOB_AVAILABILITY_BATCH: usize = 64;

/// Maximum number of distinct CIDs in a registration's wanted set, or total
/// CIDs (including duplicates) in one explicitly named status or wait call.
///
/// The set is replaced as a whole and travels in one service frame, so it is
/// bounded like every other request.
pub const MAX_WANTED_BLOBS: usize = 4096;
