use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Marker trait for data types that can be stored in Eidetica.
///
/// Requires `Serialize` and `Deserialize` for conversion to/from [`RawData`](crate::entry::RawData).
/// Users can implement this for any type they wish to store, typically alongside `serde::Serialize` and `serde::Deserialize`.
pub trait Data: Serialize + for<'de> Deserialize<'de> {}

/// Trait for Conflict-free Replicated Data Types (CRDTs).
///
/// CRDTs define a deterministic `merge` operation that combines two states
/// into a new state, resolving conflicts automatically. Eidetica uses this
/// trait to merge data from different branches of the history.
///
/// Implementors must also implement `Default` and `Data`.
pub trait CRDT: Default + Data {
    /// Merge another CRDT into this one.
    ///
    /// The order matters, `self` is the older value, and we are adding `other` on top of it.
    fn merge(&self, other: &Self) -> Result<Self>
    where
        Self: Sized;
}

/// A simple key-value CRDT implementation using a last-write-wins (LWW) strategy.
///
/// When merging two `KVOverWrite` instances, keys present in the `other` instance
/// overwrite keys in the `self` instance. Keys unique to either instance are preserved.
/// This is suitable for configuration or metadata where the latest update should prevail.
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct KVOverWrite {
    data: HashMap<String, Option<String>>,
}

impl Data for KVOverWrite {}

impl CRDT for KVOverWrite {
    fn merge(&self, other: &Self) -> Result<Self> {
        let mut data = self.data.clone();
        data.extend(other.data.clone());
        Ok(KVOverWrite { data })
    }
}

// Additional helper methods for KVOverWrite for ease of use
impl KVOverWrite {
    /// Create a new empty KVOverWrite
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a KVOverWrite from an existing HashMap.
    /// Keys and values should be convertible to Strings via `Into<String>`.
    /// Values will be wrapped in Some().
    pub fn from_hashmap<K, V>(initial_data: HashMap<K, V>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        let data = initial_data
            .into_iter()
            .map(|(k, v)| (k.into(), Some(v.into())))
            .collect();
        Self { data }
    }

    /// Get a value by key. Returns None if the key does not exist or has been deleted (tombstone).
    pub fn get(&self, key: &str) -> Option<&str> {
        self.data.get(key).and_then(Option::as_deref)
    }

    /// Set a key-value pair. This will overwrite any existing value or tombstone.
    pub fn set<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.data.insert(key.into(), Some(value.into()));
        self
    }

    /// Remove a key-value pair by inserting a tombstone.
    /// Returns the value if it existed before removal, otherwise None.
    pub fn remove(&mut self, key: &str) -> Option<String> {
        self.data.insert(key.to_string(), None).flatten()
    }

    /// Get the underlying HashMap, including tombstones (None values).
    pub fn as_hashmap(&self) -> &HashMap<String, Option<String>> {
        &self.data
    }

    /// Get a mutable reference to the underlying HashMap, including tombstones (None values).
    pub fn as_hashmap_mut(&mut self) -> &mut HashMap<String, Option<String>> {
        &mut self.data
    }
}

/// Represents a value within a `KVNested` structure, which can be either a String, another `KVNested` map, or a tombstone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum NestedValue {
    String(String),
    Map(KVNested),
    Deleted, // Tombstone
}

impl From<String> for NestedValue {
    fn from(s: String) -> Self {
        NestedValue::String(s)
    }
}

impl From<KVNested> for NestedValue {
    fn from(nested: KVNested) -> Self {
        NestedValue::Map(nested)
    }
}

/// A nested key-value CRDT implementation using a last-write-wins (LWW) strategy.
///
/// Values can be either strings or other `KVNested` instances, allowing for arbitrary nesting.
/// When merging, values from the `other` instance overwrite values in `self`.
/// If both `self` and `other` have a `Map` at the same key, their maps are recursively merged.
/// If one has a `Map` and the other a `String` at the same key, the `other` value (be it `Map` or `String`) overwrites.
#[derive(Default, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KVNested {
    data: HashMap<String, NestedValue>,
}

impl Data for KVNested {}

impl CRDT for KVNested {
    fn merge(&self, other: &Self) -> Result<Self> {
        let mut new_data = self.data.clone();

        for (key, other_value) in &other.data {
            match other_value {
                // If other has a tombstone, it always wins
                NestedValue::Deleted => {
                    new_data.insert(key.clone(), NestedValue::Deleted);
                }
                // If other has a string, it always wins
                NestedValue::String(_) => {
                    new_data.insert(key.clone(), other_value.clone());
                }
                // If other has a map, merge recursively:w
                NestedValue::Map(other_map) => {
                    if let Some(self_value) = new_data.get_mut(key) {
                        // Use get_mut to potentially update in place
                        match self_value {
                            NestedValue::Map(self_map_mut) => {
                                // Both are maps, recursive merge
                                // We need to merge into a new map, then replace the entry
                                let merged_inner_map = self_map_mut.merge(other_map)?;
                                *self_value = NestedValue::Map(merged_inner_map);
                            }
                            // Self is String or Deleted, other_map overwrites
                            _ => {
                                new_data.insert(key.clone(), NestedValue::Map(other_map.clone()));
                            }
                        }
                    } else {
                        // Key only exists in other, so add it
                        new_data.insert(key.clone(), NestedValue::Map(other_map.clone()));
                    }
                }
            }
        }
        // Handle keys present in self but not in other (they are preserved)
        // This is implicitly handled because we cloned self.data initially.
        // However, the above loop might have inserted tombstones for keys that were only in self if other also had a tombstone for it.
        // To be perfectly clear: the current logic is that other fully dictates the state for shared keys.
        // Keys only in self remain as they are.
        Ok(KVNested { data: new_data })
    }
}

impl KVNested {
    /// Create a new empty KVNested
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a value by key. Returns None if the key does not exist or has been deleted (tombstone).
    pub fn get(&self, key: &str) -> Option<&NestedValue> {
        self.data.get(key).and_then(|val| match val {
            NestedValue::Deleted => None,
            _ => Some(val),
        })
    }

    /// Set a key-value pair where the value is a NestedValue
    pub fn set<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: Into<String>,
        V: Into<NestedValue>,
    {
        self.data.insert(key.into(), value.into());
        self
    }

    /// Set a key-value pair where the value is a String
    /// Key and value should be convertible to Strings via `Into<String>`.
    pub fn set_string<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.data
            .insert(key.into(), NestedValue::String(value.into()));
        self
    }

    /// Set a key-value pair where the value is a nested KVNested map
    pub fn set_map<K>(&mut self, key: K, value: KVNested) -> &mut Self
    where
        K: Into<String>,
    {
        self.data.insert(key.into(), NestedValue::Map(value));
        self
    }

    /// Remove a key-value pair by inserting a tombstone.
    /// Returns the value if it existed (and wasn't already a tombstone) before removal, otherwise None.
    pub fn remove(&mut self, key: &str) -> Option<NestedValue> {
        match self.data.insert(key.to_string(), NestedValue::Deleted) {
            Some(NestedValue::Deleted) => None, // It was already a tombstone
            Some(old_value) => Some(old_value), // It was String or Map
            None => None,                       // Key didn't exist
        }
    }

    /// Get the underlying HashMap, including tombstones.
    pub fn as_hashmap(&self) -> &HashMap<String, NestedValue> {
        &self.data
    }

    /// Get a mutable reference to the underlying HashMap, including tombstones.
    pub fn as_hashmap_mut(&mut self) -> &mut HashMap<String, NestedValue> {
        &mut self.data
    }
}
