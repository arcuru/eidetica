//! Content-addressed blob helpers that sit above the storage backend.
//!
//! Blob *storage* (the CID-keyed `0x55` arm) lives in [`crate::backend`]; this
//! module holds the verified-streaming machinery layered over it and the typed
//! [`BlobRef`] reference. The blob public API is on [`Instance`](crate::Instance)
//! (`put_blob`/`put_blob_ref`/`get_blob`/`get_blob_ref`/`get_blob_local`/`get_blob_range`).

pub(crate) mod bao;
mod blob_ref;

pub use blob_ref::BlobRef;
