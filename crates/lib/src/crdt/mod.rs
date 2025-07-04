//! Conflict-free Replicated Data Types (CRDTs) for distributed data structures.
//!
//! This module provides CRDT implementations that enable automatic conflict resolution
//! in distributed systems. CRDTs guarantee that concurrent updates can be merged
//! deterministically, ensuring eventual consistency without coordination.
//!
//! # Core Types
//!
//! - [`Map`] - A simple last-write-wins key-value store
//! - [`Array`] - An ordered collection with UUID-based element identification
//! - [`Nested`] - A recursive key-value structure supporting nested maps and arrays
//! - [`Value`] - The value type for nested structures
//!
//! # Traits
//!
//! - [`Data`] - Marker trait for types that can be stored in Eidetica
//! - [`CRDT`] - Core trait defining merge semantics for conflict resolution

// First declare the value module to break circular dependencies
pub mod value;

// Then other modules that depend on value
pub mod array;
pub mod map;
pub mod nested;
pub mod traits;

// Re-export core types with new names
pub use array::Array;
pub use map::Map;
pub use nested::Nested;
pub use traits::{CRDT, Data};
pub use value::Value;

// Legacy aliases for backward compatibility
pub use array::Array as CrdtArray;
pub use map::Map as KVOverWrite;
pub use nested::Nested as KVNested;
pub use value::Value as NestedValue;
