//! Conflict-free Replicated Data Types (CRDTs) for distributed data structures.
//!
//! This module provides CRDT implementations that enable automatic conflict resolution
//! in distributed systems. CRDTs guarantee that concurrent updates can be merged
//! deterministically, ensuring eventual consistency without coordination.
//!
//! # Core Types
//!
//! - [`doc::Doc`] - The main CRDT document type for user interactions
//! - [`doc::Value`] - The value type for nested structures  
//! - [`doc::List`] - An ordered collection with rational number positioning
//! - [`doc::list::Position`] - Rational number-based positions for stable list ordering
//! - [`Map`] - Per-key composition of arbitrary CRDT operations
//! - [`Lww`] - Ordered register operations with an explicit identity and tombstone
//!
//! `Map` and `Lww` provide standalone merge algebra, without built-in encoding
//! or Store integration.
//!
//! # Traits
//!
//! - [`Codec`] - Explicit byte encoding at persistence boundaries
//! - [`CRDT`] - Core trait defining merge semantics for conflict resolution

// Core modules
pub mod doc;
pub mod errors;
pub mod lww;
pub mod map;
pub mod traits;

// Re-export core types
pub use doc::Doc;
pub use errors::CRDTError;
pub use lww::Lww;
pub use map::Map;
pub use traits::{CRDT, Codec};
