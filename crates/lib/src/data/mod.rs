//!
//! Defines core data handling traits and specific CRDT implementations.
//!
//! This module provides the `Data` trait for serializable types and the `CRDT` trait
//! for types that support conflict-free merging.
//!
//! **Note**: This module is maintained for backward compatibility. New code should use
//! the `crdt` module directly.

// Re-export everything from the crdt module for backward compatibility
pub use crate::crdt::{CRDT, CrdtArray, Data, KVNested, NestedValue};
