//! A CRDT array implementation using UUID-sorted order.
//!
//! This module provides `Array` (formerly `CrdtArray`), which implements an ordered
//! collection where each element is assigned a unique UUID. Elements can be added,
//! removed, and accessed by their UUID, with deterministic ordering based on UUID sorting.

use crate::Result;
use crate::crdt::{CRDT, Data, value::Value};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// A CRDT array implementation using UUID-sorted order.
///
/// Each array element is assigned a UUIDv4 and can contain any Value.
/// Elements are always returned in UUID-sorted order for determinism.
/// Deletions are represented as tombstones (None values) to preserve causality.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Array {
    /// Map from UUIDv4 to value or tombstone
    items: HashMap<String, Option<Value>>,
}

impl Default for Array {
    fn default() -> Self {
        Self::new()
    }
}

impl Data for Array {}

impl CRDT for Array {
    fn merge(&self, other: &Self) -> Result<Self> {
        let mut merged_items = self.items.clone();

        // Merge items with LWW semantics
        for (id, value_opt) in &other.items {
            merged_items.insert(id.clone(), value_opt.clone());
        }

        Ok(Self {
            items: merged_items,
        })
    }
}

impl Array {
    /// Create a new empty array
    pub fn new() -> Self {
        Self {
            items: HashMap::new(),
        }
    }

    /// Add an element to the array
    /// Returns the unique ID of the added element
    pub fn add(&mut self, value: Value) -> String {
        let id = Uuid::new_v4().to_string();
        self.items.insert(id.clone(), Some(value));
        id
    }

    /// Remove an element by its unique ID
    /// Returns true if the element was found and removed, false otherwise
    pub fn remove(&mut self, id: &str) -> bool {
        match self.items.get_mut(id) {
            Some(slot) => {
                *slot = None; // Create tombstone
                true
            }
            None => false,
        }
    }

    /// Get an element by its ID
    pub fn get(&self, id: &str) -> Option<&Value> {
        self.items.get(id).and_then(|opt| opt.as_ref())
    }

    /// Get the number of live elements (excluding tombstones)
    pub fn len(&self) -> usize {
        self.items.values().filter(|opt| opt.is_some()).count()
    }

    /// Check if the array is empty (no live elements)
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterator over live elements (ID, value) in UUID-sorted order
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        let mut live_items: Vec<_> = self
            .items
            .iter()
            .filter_map(|(id, opt)| opt.as_ref().map(|value| (id.as_str(), value)))
            .collect();
        live_items.sort_by_key(|(id, _)| *id);
        live_items.into_iter()
    }

    /// Get all live element IDs in UUID-sorted order
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<_> = self
            .items
            .iter()
            .filter_map(|(id, opt)| {
                if opt.is_some() {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect();
        ids.sort();
        ids
    }

    /// Clear all elements (tombstone everything)
    pub fn clear(&mut self) {
        for value_opt in self.items.values_mut() {
            *value_opt = None;
        }
    }

    /// Compact the array by removing old tombstones
    /// Returns the number of tombstones removed
    pub fn compact(&mut self) -> usize {
        let tombstone_ids: Vec<_> = self
            .items
            .iter()
            .filter_map(|(id, opt)| {
                if opt.is_none() {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect();

        let removed_count = tombstone_ids.len();

        for id in tombstone_ids {
            self.items.remove(&id);
        }

        removed_count
    }

    /// Get access to the internal items map (for testing)
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn items(&self) -> &HashMap<String, Option<Value>> {
        &self.items
    }
}

// Type alias for backward compatibility
pub type CrdtArray = Array;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_option_nested_value_serialization() {
        let mut array = Array::new();

        // Add a live value and create a tombstone
        let id1 = array.add(Value::String("hello".to_string()));
        let id2 = array.add(Value::String("world".to_string()));
        array.remove(&id2); // This should create a tombstone (None)

        // Serialize
        let serialized = serde_json::to_string(&array).unwrap();
        println!("Serialized Array: {serialized}");

        // Deserialize
        let deserialized: Array = serde_json::from_str(&serialized).unwrap();

        // Verify the live value survived
        assert_eq!(
            deserialized.get(&id1),
            Some(&Value::String("hello".to_string()))
        );

        // Verify the tombstone survived
        assert_eq!(deserialized.get(&id2), None);

        // But the tombstone ID should still exist in the internal structure
        assert!(deserialized.items.contains_key(&id2));
        assert_eq!(deserialized.items.get(&id2), Some(&None));

        // Length should be 1 (only live elements)
        assert_eq!(deserialized.len(), 1);
    }
}
