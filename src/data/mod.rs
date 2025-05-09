//!
//! Defines core data handling traits and specific CRDT implementations.
//!
//! This module provides the `Data` trait for serializable types and the `CRDT` trait
//! for types that support conflict-free merging. It also includes `KVOverWrite`, a
//! simple last-write-wins key-value store implementation.

mod types;
pub use types::{Data, KVNested, KVOverWrite, NestedValue, CRDT};
