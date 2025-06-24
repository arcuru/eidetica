//! A simple key-value CRDT using last-write-wins semantics.
//!
//! This module provides `Map` (formerly `KVOverWrite`), which implements a basic
//! key-value store where concurrent updates to the same key are resolved by
//! taking the "last" write based on the merge order.

use crate::Result;
use crate::crdt::{CRDT, Data};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A simple key-value CRDT using last-write-wins semantics.
///
/// This implementation stores key-value pairs where each key maps to a string value.
/// When conflicts occur (same key updated concurrently), the "last write wins" based
/// on the order of merging operations.
///
/// # Examples
///
/// ```
/// use eidetica::crdt::{CRDT, Map};
///
/// let mut kv = Map::new();
/// kv.set("name", "Alice");
/// kv.set("age", "30");
///
/// println!("Name: {:?}", kv.get("name")); // Some("Alice")
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Map {
    data: HashMap<String, String>,
}

impl Data for Map {}

impl CRDT for Map {
    fn merge(&self, other: &Self) -> Result<Self> {
        let mut merged_data = self.data.clone();
        merged_data.extend(other.data.clone());
        Ok(Self { data: merged_data })
    }
}

impl Map {
    /// Create a new empty `Map`.
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
        }
    }

    /// Set a key-value pair.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.data.insert(key.into(), value.into());
        self
    }

    /// Get a value by key.
    pub fn get(&self, key: &str) -> Option<&String> {
        self.data.get(key)
    }

    /// Remove a key-value pair.
    pub fn remove(&mut self, key: &str) -> Option<String> {
        self.data.remove(key)
    }

    /// Get an iterator over all key-value pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.data.iter()
    }

    /// Get the number of key-value pairs.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Check if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Get a reference to the internal HashMap.
    pub fn as_hashmap(&self) -> &HashMap<String, String> {
        &self.data
    }

    /// Get a mutable reference to the internal HashMap.
    pub fn as_hashmap_mut(&mut self) -> &mut HashMap<String, String> {
        &mut self.data
    }
}

impl Default for Map {
    fn default() -> Self {
        Self::new()
    }
}

impl From<HashMap<String, String>> for Map {
    fn from(data: HashMap<String, String>) -> Self {
        Self { data }
    }
}

impl From<Map> for HashMap<String, String> {
    fn from(kv: Map) -> Self {
        kv.data
    }
}

impl From<&Map> for HashMap<String, String> {
    fn from(kv: &Map) -> Self {
        kv.data.clone()
    }
}

impl AsRef<HashMap<String, String>> for Map {
    fn as_ref(&self) -> &HashMap<String, String> {
        &self.data
    }
}

impl AsMut<HashMap<String, String>> for Map {
    fn as_mut(&mut self) -> &mut HashMap<String, String> {
        &mut self.data
    }
}

// Type alias for backward compatibility
pub type KVOverWrite = Map;
