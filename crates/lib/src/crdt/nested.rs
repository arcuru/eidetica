//! A nested key-value CRDT implementation.
//!
//! This module provides `Nested` (formerly `KVNested`), which implements a recursive
//! key-value store where values can be strings, other nested maps, arrays, or tombstones.
//! This enables building complex hierarchical data structures with CRDT semantics.

use crate::crdt::{Array, CRDT, Data, Value};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A nested key-value CRDT implementation using a last-write-wins (LWW) strategy.
///
/// Values can be either strings or other `Nested` instances, allowing for arbitrary nesting.
/// Deletions are represented using tombstones (`Value::Deleted`) to maintain causality.
///
/// # Examples
///
/// ```
/// use eidetica::crdt::{CRDT, Nested, Value};
///
/// let mut kv = Nested::new();
/// kv.set("user.name", "Alice");
/// kv.set("user.age", "30");
///
/// // Access nested values
/// if let Some(Value::String(name)) = kv.get("user.name") {
///     println!("User name: {}", name);
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Nested {
    data: HashMap<String, Value>,
}

impl Data for Nested {}

impl CRDT for Nested {
    fn merge(&self, other: &Self) -> Result<Self> {
        let mut new_data = self.data.clone();

        for (key, other_value) in &other.data {
            match other_value {
                // If other has a string or tombstone, it overwrites:
                Value::String(_) | Value::Deleted => {
                    new_data.insert(key.clone(), other_value.clone());
                }
                // If other has a map, merge recursively:
                Value::Map(other_map) => {
                    if let Some(self_value) = new_data.get_mut(key) {
                        // Use get_mut to potentially update in place
                        match self_value {
                            Value::Map(self_map_mut) => {
                                // Both are maps, merge them
                                let merged_inner_map = self_map_mut.merge(other_map)?;
                                *self_value = Value::Map(merged_inner_map);
                            }
                            // Self is String, Array, or Deleted, other_map overwrites
                            _ => {
                                new_data.insert(key.clone(), Value::Map(other_map.clone()));
                            }
                        }
                    } else {
                        // Key only exists in other, so add it
                        new_data.insert(key.clone(), Value::Map(other_map.clone()));
                    }
                }
                // If other has an array, merge recursively
                Value::Array(other_array) => {
                    if let Some(self_value) = new_data.get_mut(key) {
                        match self_value {
                            Value::Array(self_array_mut) => {
                                // Both are arrays, merge them
                                let merged_array = self_array_mut.merge(other_array)?;
                                *self_value = Value::Array(merged_array);
                            }
                            // Self is String, Map, or Deleted, other_array overwrites
                            _ => {
                                new_data.insert(key.clone(), Value::Array(other_array.clone()));
                            }
                        }
                    } else {
                        // Key only exists in other, so add it
                        new_data.insert(key.clone(), Value::Array(other_array.clone()));
                    }
                }
            }
        }
        // Handle keys present in self but not in other (they are preserved)

        Ok(Self { data: new_data })
    }
}

impl Nested {
    /// Create a new empty `Nested`.
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
        }
    }

    /// Set a key-value pair where the value is a string.
    pub fn set<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: Into<String>,
        V: Into<Value>,
    {
        self.data.insert(key.into(), value.into());
        self
    }

    /// Set a key-value pair where the value is a string (alias for set).
    pub fn set_string<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.data.insert(key.into(), Value::String(value.into()));
        self
    }

    /// Set a key-value pair where the value is any type that can convert to Value.
    pub fn set_nested<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: Into<String>,
        V: Into<Value>,
    {
        self.data.insert(key.into(), value.into());
        self
    }

    /// Set a key-value pair where the value is a nested map.
    pub fn set_map<K>(&mut self, key: K, value: Nested) -> &mut Self
    where
        K: Into<String>,
    {
        self.data.insert(key.into(), Value::Map(value));
        self
    }

    /// Set a key-value pair where the value is an array
    pub fn set_array<K>(&mut self, key: K, value: Array) -> &mut Self
    where
        K: Into<String>,
    {
        self.data.insert(key.into(), Value::Array(value));
        self
    }

    /// Remove a key-value pair by inserting a tombstone.
    /// Returns the value if it existed (and wasn't already a tombstone) before removal, otherwise None.
    pub fn remove(&mut self, key: impl AsRef<str>) -> Option<Value> {
        let old_value = self.data.get(key.as_ref()).cloned();
        self.data.insert(key.as_ref().to_string(), Value::Deleted);

        match old_value {
            Some(Value::Deleted) | None => None,
            Some(value) => Some(value),
        }
    }

    /// Get a value by key. Returns `None` if the key doesn't exist or has been deleted.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self.data.get(key) {
            Some(Value::Deleted) | None => None,
            Some(value) => Some(value),
        }
    }

    /// Get a reference to the internal HashMap.
    pub fn as_hashmap(&self) -> &HashMap<String, Value> {
        &self.data
    }

    /// Get a mutable reference to the internal HashMap.
    pub fn as_hashmap_mut(&mut self) -> &mut HashMap<String, Value> {
        &mut self.data
    }

    // Array operations - simple API that hides implementation details

    /// Add an element to an array at the given key
    /// Creates a new array if the key doesn't exist
    /// Returns the unique ID of the added element
    pub fn array_add<K>(&mut self, key: K, value: Value) -> Result<String>
    where
        K: Into<String>,
    {
        let key_str = key.into();

        match self.data.entry(key_str) {
            std::collections::hash_map::Entry::Occupied(mut entry) => match entry.get_mut() {
                Value::Array(array) => Ok(array.add(value)),
                _ => Err(Error::InvalidOperation(
                    "Expected Array, found other type".to_string(),
                )),
            },
            std::collections::hash_map::Entry::Vacant(entry) => {
                let mut array = Array::new();
                let id = array.add(value);
                entry.insert(Value::Array(array));
                Ok(id)
            }
        }
    }

    /// Remove an element by its ID from an array
    /// Returns true if an element was removed, false otherwise
    pub fn array_remove<K>(&mut self, key: K, id: &str) -> Result<bool>
    where
        K: Into<String>,
    {
        let key_str = key.into();

        match self.data.get_mut(&key_str) {
            Some(Value::Array(array)) => Ok(array.remove(id)),
            Some(_) => Err(Error::InvalidOperation(
                "Expected Array, found other type".to_string(),
            )),
            None => Ok(false),
        }
    }

    /// Get an element by its ID from an array
    pub fn array_get<K>(&self, key: K, id: &str) -> Option<&Value>
    where
        K: AsRef<str>,
    {
        match self.get(key.as_ref()) {
            Some(Value::Array(array)) => array.get(id),
            _ => None,
        }
    }

    /// Get all element IDs from an array in UUID-sorted order
    /// Returns an empty Vec if the key doesn't exist or isn't an array
    pub fn array_ids<K>(&self, key: K) -> Vec<String>
    where
        K: AsRef<str>,
    {
        match self.get(key.as_ref()) {
            Some(Value::Array(array)) => array.ids(),
            _ => Vec::new(),
        }
    }

    /// Get the length of an array
    /// Returns 0 if the key doesn't exist or isn't an array
    pub fn array_len<K>(&self, key: K) -> usize
    where
        K: AsRef<str>,
    {
        match self.get(key.as_ref()) {
            Some(Value::Array(array)) => array.len(),
            _ => 0,
        }
    }

    /// Check if an array is empty
    /// Returns true if the key doesn't exist, isn't an array, or the array is empty
    pub fn array_is_empty<K>(&self, key: K) -> bool
    where
        K: AsRef<str>,
    {
        match self.get(key.as_ref()) {
            Some(Value::Array(array)) => array.is_empty(),
            _ => true,
        }
    }

    /// Clear all elements from an array (tombstone them)
    /// Does nothing if the key doesn't exist or isn't an array
    pub fn array_clear<K>(&mut self, key: K) -> Result<()>
    where
        K: AsRef<str>,
    {
        match self.data.get_mut(key.as_ref()) {
            Some(Value::Array(array)) => {
                array.clear();
                Ok(())
            }
            Some(_) => Err(Error::InvalidOperation(
                "Expected Array, found other type".to_string(),
            )),
            None => Ok(()), // Nothing to clear
        }
    }
}

impl Default for Nested {
    fn default() -> Self {
        Self::new()
    }
}

// Type alias for backward compatibility
pub type KVNested = Nested;
