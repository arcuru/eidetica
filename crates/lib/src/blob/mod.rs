//! Content-addressed blob helpers that sit above the storage backend.
//!
//! Blob *storage* (the CID-keyed `0x55` arm) lives in [`crate::backend`]; this
//! module holds the verified-streaming machinery layered over it. The blob
//! public API is on [`Instance`](crate::Instance)
//! (`put_blob`/`get_blob`/`get_blob_local`/`get_blob_range`).

pub(crate) mod bao;
