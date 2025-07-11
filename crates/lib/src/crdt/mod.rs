//! Conflict-free Replicated Data Types (CRDTs) for distributed data structures.
//!
//! This module provides CRDT implementations that enable automatic conflict resolution
//! in distributed systems. CRDTs guarantee that concurrent updates can be merged
//! deterministically, ensuring eventual consistency without coordination.
//!
//! # Core Types
//!
//! - [`Map`] - A recursive key-value structure supporting nested maps and lists
//! - [`map::Array`] - An ordered collection with rational number positioning
//! - [`map::Value`] - The value type for nested structures
//! - [`map::array::Position`] - Rational number-based positions for stable list ordering
//!
//! # Traits
//!
//! - [`Data`] - Marker trait for types that can be stored in Eidetica
//! - [`CRDT`] - Core trait defining merge semantics for conflict resolution

// Core modules
pub mod errors;
pub mod map;
pub mod traits;

// Re-export core types
pub use errors::CRDTError;
pub use map::Map;
pub use traits::{CRDT, Data};

// Backward compatibility aliases - Map is now the main type
pub use map::Array as NodeList;
pub use map::Map as Node;
pub use map::Map as Nested;
pub use map::Value as NodeValue;

// Legacy aliases for backward compatibility
pub use map::Array as CrdtArray; // Use Array instead of removed Array
pub use map::Map as KVNested;
pub use map::Value as NestedValue;
pub use map::array::Position as ListPosition; // Backward compatibility for ListPosition
