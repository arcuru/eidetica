//! Content-addressed blob references and verified local byte ranges.
//!
//! Bytes live outside the Entry DAG. Store an optional [`BlobRef`] in any
//! ordinary Store value and read bytes through [`crate::Instance`].

pub(crate) mod bao;
mod blob_ref;
mod fetch;

pub use blob_ref::BlobRef;
pub use fetch::BlobFetchAttempt;

/// Maximum number of CIDs in one peer availability query.
pub const MAX_BLOB_AVAILABILITY_BATCH: usize = 64;
