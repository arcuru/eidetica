//! Content-addressed blob references and verified local byte ranges.
//!
//! Bytes live outside the Entry DAG. Store an optional [`BlobRef`] in any
//! ordinary Store value and read bytes through [`crate::Instance`].

pub(crate) mod bao;
mod blob_ref;

pub use blob_ref::BlobRef;
