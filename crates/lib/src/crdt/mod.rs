//! Conflict-free Replicated Data Types (CRDTs) for distributed data structures.
//!
//! This module provides CRDT implementations that enable automatic conflict resolution
//! in distributed systems. CRDTs guarantee that concurrent updates can be merged
//! deterministically, ensuring eventual consistency without coordination.
//!
//! # Core Types
//!
//! - [`Doc`] - The main CRDT document type for user interactions
//! - [`Value`] - The value type for nested structures  
//! - [`List`] - An ordered collection with rational number positioning
//! - [`doc::list::Position`] - Rational number-based positions for stable list ordering
//!
//! # Traits
//!
//! - [`Data`] - Marker trait for types that can be stored in Eidetica
//! - [`CRDT`] - Core trait defining merge semantics for conflict resolution

// Core modules
pub mod doc;
pub mod errors;
pub mod traits;

// Re-export core types
pub use doc::Doc;
pub use errors::CRDTError;
pub use traits::{CRDT, Data};
