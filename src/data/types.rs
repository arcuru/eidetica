use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Marker trait for data types that can be stored in EideticaDB.
///
/// Requires `Serialize` and `Deserialize` for conversion to/from [`RawData`](crate::entry::RawData).
/// Users can implement this for any type they wish to store, typically alongside `serde::Serialize` and `serde::Deserialize`.
pub trait Data: Serialize + for<'de> Deserialize<'de> {}

/// Trait for Conflict-free Replicated Data Types (CRDTs).
///
/// CRDTs define a deterministic `merge` operation that combines two states
/// into a new state, resolving conflicts automatically. EideticaDB uses this
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
    data: HashMap<String, String>,
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

    /// Create a KVOverWrite from an existing HashMap
    pub fn from_hashmap(data: HashMap<String, String>) -> Self {
        Self { data }
    }

    /// Get a value by key
    pub fn get(&self, key: &str) -> Option<&String> {
        self.data.get(key)
    }

    /// Set a key-value pair
    pub fn set(&mut self, key: String, value: String) -> &mut Self {
        self.data.insert(key, value);
        self
    }

    /// Remove a key-value pair
    pub fn remove(&mut self, key: &str) -> Option<String> {
        self.data.remove(key)
    }

    /// Get the underlying HashMap
    pub fn as_hashmap(&self) -> &HashMap<String, String> {
        &self.data
    }

    /// Get a mutable reference to the underlying HashMap
    pub fn as_hashmap_mut(&mut self) -> &mut HashMap<String, String> {
        &mut self.data
    }
}
