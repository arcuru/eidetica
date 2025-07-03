//! Database-style backend implementations
//!
//! These backends provide persistent, queryable storage similar to traditional databases.

mod in_memory;

pub use in_memory::InMemory;
