//! Value types for nested CRDT structures.
//!
//! This module provides `Value` (formerly `NestedValue`), which represents the possible
//! value types that can be stored in nested CRDT structures like `Nested`.

use serde::{Deserialize, Serialize};

/// Represents a value within a `Nested` structure, which can be either a String, another `Nested` map, an Array, or a tombstone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Value {
    String(String),
    Map(crate::crdt::nested::Nested),
    Array(crate::crdt::array::Array),
    Deleted, // Tombstone
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_string())
    }
}

impl From<crate::crdt::nested::Nested> for Value {
    fn from(map: crate::crdt::nested::Nested) -> Self {
        Value::Map(map)
    }
}

impl From<crate::crdt::array::Array> for Value {
    fn from(array: crate::crdt::array::Array) -> Self {
        Value::Array(array)
    }
}

// Type alias for backward compatibility
pub type NestedValue = Value;
