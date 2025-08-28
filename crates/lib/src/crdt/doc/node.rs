//! Internal node implementation for CRDT documents.
//!
//! This module provides the Node type that represents the internal tree-like
//! structure of CRDT documents. This type is primarily internal and users
//! should interact with the Doc type instead.

use std::collections::HashMap;
use std::fmt;

use crate::Result;
use crate::crdt::CRDTError;
use crate::crdt::doc::path::Path;
use crate::crdt::traits::{CRDT, Data};

// Import types from other modules
use super::list::List;
use super::value::Value;

/// Internal node structure for CRDT trees.
///
/// `Node` represents the internal tree-like structure where each node can contain
/// multiple named children. This type is now internal to the CRDT implementation,
/// with the public API provided through the [`Doc`] type.
///
/// # CRDT Behavior
///
/// Nodes implement CRDT semantics for distributed collaboration:
/// - **Structural merging**: Child nodes are merged recursively
/// - **Tombstone deletion**: Deleted keys are marked with tombstones for proper merge behavior
/// - **Last-write-wins**: Conflicting scalar values use last-write-wins resolution
///
/// # Internal Use Only
///
/// This type is primarily for internal use within the CRDT system. External users
/// should interact with the [`Doc`] type instead, which provides a cleaner API
/// and proper separation of concerns.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Node {
    /// Child nodes indexed by string keys
    children: HashMap<String, Value>,
}

impl Node {
    /// Creates a new empty node
    pub fn new() -> Self {
        Self {
            children: HashMap::new(),
        }
    }

    /// Returns true if this node has no children (excluding tombstones)
    pub fn is_empty(&self) -> bool {
        self.children.values().all(|v| matches!(v, Value::Deleted))
    }

    /// Returns the number of direct children (excluding tombstones)
    pub fn len(&self) -> usize {
        self.children
            .values()
            .filter(|v| !matches!(v, Value::Deleted))
            .count()
    }

    /// Returns true if the given key contains a tombstone (deleted value).
    ///
    /// This method provides access to CRDT tombstone information for advanced use cases,
    /// testing, and debugging. In normal operation, tombstones are hidden from the public API
    /// but are essential for CRDT merge correctness.
    ///
    /// # Purpose
    ///
    /// Tombstones serve several critical functions in CRDT systems:
    /// - **Merge correctness**: Preserve deletion information across distributed replicas
    /// - **Conflict resolution**: Ensure deletions "win" over concurrent modifications
    /// - **Eventual consistency**: Enable proper state synchronization without coordination
    ///
    /// # Usage
    ///
    /// This method is primarily useful for:
    /// - **Testing**: Verifying that deletions create appropriate tombstones
    /// - **Debugging**: Understanding internal CRDT state and merge behavior
    /// - **Advanced integrations**: Building higher-level abstractions that need tombstone awareness
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::Doc;
    /// let mut doc = Doc::new();
    /// doc.set("key", "value");
    /// assert!(!doc.is_tombstone("key"));
    ///
    /// doc.remove("key");
    /// assert!(doc.is_tombstone("key"));
    ///
    /// // Tombstones are hidden from normal access
    /// assert_eq!(doc.get("key"), None);
    /// ```
    pub fn is_tombstone(&self, key: impl AsRef<str>) -> bool {
        matches!(self.children.get(key.as_ref()), Some(Value::Deleted))
    }

    /// Returns true if the node contains the given key (excluding tombstones)
    pub fn contains_key(&self, key: impl AsRef<str>) -> bool {
        match self.children.get(key.as_ref()) {
            Some(Value::Deleted) => false, // Hide tombstones
            Some(_) => true,
            None => false,
        }
    }

    /// Gets a value by key (immutable reference)
    pub fn get(&self, key: impl AsRef<str>) -> Option<&Value> {
        match self.children.get(key.as_ref()) {
            Some(Value::Deleted) => None, // Hide tombstones
            value => value,
        }
    }

    /// Gets a mutable reference to a value by key
    pub fn get_mut(&mut self, key: impl AsRef<str>) -> Option<&mut Value> {
        match self.children.get_mut(key.as_ref()) {
            Some(Value::Deleted) => None, // Hide tombstones
            value => value,
        }
    }

    /// Gets a text value by key
    pub fn get_text(&self, key: impl AsRef<str>) -> Option<&str> {
        self.get(key)?.as_text()
    }

    /// Gets an integer value by key
    pub fn get_int(&self, key: impl AsRef<str>) -> Option<i64> {
        self.get(key)?.as_int()
    }

    /// Gets a boolean value by key
    pub fn get_bool(&self, key: impl AsRef<str>) -> Option<bool> {
        self.get(key)?.as_bool()
    }

    /// Gets a node value by key
    pub fn get_node(&self, key: impl AsRef<str>) -> Option<&Node> {
        self.get(key)?.as_node()
    }

    /// Gets a list value by key
    pub fn get_list(&self, key: impl AsRef<str>) -> Option<&List> {
        self.get(key)?.as_list()
    }

    /// Gets a typed value by key
    pub fn get_as<T>(&self, key: impl AsRef<str>) -> Result<T, CRDTError>
    where
        T: for<'a> TryFrom<&'a Value, Error = CRDTError>,
    {
        match self.get(key.as_ref()) {
            Some(value) => T::try_from(value),
            None => Err(CRDTError::ElementNotFound {
                key: key.as_ref().to_string(),
            }),
        }
    }

    /// Sets a value for a key, returning the previous value if present
    pub fn set<K, V>(&mut self, key: K, value: V) -> Option<Value>
    where
        K: Into<String>,
        V: Into<Value>,
    {
        let old = self.children.insert(key.into(), value.into());
        match old {
            Some(Value::Deleted) => None, // Don't return tombstones
            value => value,
        }
    }

    /// Removes a value by key, returning it if present (tombstones for CRDT semantics)
    pub fn remove(&mut self, key: impl Into<String>) -> Option<Value> {
        let key = key.into();
        let old_value = self.children.get(&key).cloned();
        self.children.insert(key, Value::Deleted);
        match old_value {
            Some(Value::Deleted) => None, // Don't return tombstones
            value => value,
        }
    }

    /// Gets a value at a path using dot notation
    pub fn get_path(&self, path: impl AsRef<Path>) -> Option<&Value> {
        let path = path.as_ref();
        let segments: Vec<_> = path.components().collect();

        if segments.is_empty() {
            return None;
        }

        // Start with the first segment to get into our node structure
        let first_segment = segments.first()?;
        let mut current_value = self.get(*first_segment)?;

        // Navigate through remaining segments
        for segment in &segments[1..] {
            match current_value {
                Value::Node(node) => {
                    current_value = node.get(*segment)?;
                }
                Value::List(list) => {
                    // Try to parse segment as list index
                    let index: usize = segment.parse().ok()?;
                    current_value = list.get(index)?;
                }
                _ => return None, // Can't navigate further
            }
        }

        Some(current_value)
    }

    /// Gets a text value at a path
    pub fn get_text_at_path(&self, path: impl AsRef<Path>) -> Option<&str> {
        match self.get_path(path)? {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Gets an integer value at a path
    pub fn get_int_at_path(&self, path: impl AsRef<Path>) -> Option<i64> {
        match self.get_path(path)? {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Gets a boolean value at a path
    pub fn get_bool_at_path(&self, path: impl AsRef<Path>) -> Option<bool> {
        match self.get_path(path)? {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Gets a node value at a path
    pub fn get_node_at_path(&self, path: impl AsRef<Path>) -> Option<&Node> {
        match self.get_path(path)? {
            Value::Node(node) => Some(node),
            _ => None,
        }
    }

    /// Gets a list value at a path
    pub fn get_list_at_path(&self, path: impl AsRef<Path>) -> Option<&List> {
        match self.get_path(path)? {
            Value::List(list) => Some(list),
            _ => None,
        }
    }

    /// Gets a typed value at a path
    pub fn get_path_as<T>(&self, path: impl AsRef<Path>) -> Result<T, CRDTError>
    where
        T: for<'a> TryFrom<&'a Value, Error = CRDTError>,
    {
        let path_ref = path.as_ref();
        match self.get_path(path_ref) {
            Some(value) => T::try_from(value),
            None => Err(CRDTError::ElementNotFound {
                key: path_ref.as_str().to_string(),
            }),
        }
    }

    /// Gets a mutable reference to a value at a path
    pub fn get_path_mut(&mut self, path: impl AsRef<Path>) -> Option<&mut Value> {
        let path = path.as_ref();
        let segments: Vec<_> = path.components().collect();

        if segments.is_empty() {
            return None;
        }

        let mut current = self;

        // Navigate to the parent of the target
        for segment in &segments[..segments.len() - 1] {
            match current.children.get_mut(*segment) {
                Some(Value::Node(node)) => {
                    current = node;
                }
                _ => return None, // Can't navigate further
            }
        }

        // Get the final value
        let final_key = segments.last()?;
        match current.children.get_mut(*final_key) {
            Some(Value::Deleted) => None, // Hide tombstones
            value => value,
        }
    }

    /// Sets a value at a path using dot notation, creating intermediate nodes as needed
    pub fn set_path(
        &mut self,
        path: impl AsRef<Path>,
        value: impl Into<Value>,
    ) -> Result<Option<Value>, CRDTError> {
        let path = path.as_ref();
        let segments: Vec<_> = path.components().collect();

        if segments.is_empty() {
            return Err(CRDTError::InvalidPath {
                path: "(empty path)".to_string(),
            });
        }

        let mut current = self;

        // Navigate to the parent, creating intermediate nodes as needed
        for segment in &segments[..segments.len() - 1] {
            let entry = current
                .children
                .entry(segment.to_string())
                .or_insert_with(|| Value::Node(Node::new()));
            match entry {
                Value::Node(node) => {
                    current = node;
                }
                Value::Deleted => {
                    // Replace tombstone with new node
                    *entry = Value::Node(Node::new());
                    match entry {
                        Value::Node(node) => current = node,
                        _ => unreachable!(),
                    }
                }
                _ => {
                    // Can't navigate through non-node value
                    return Err(CRDTError::InvalidPath {
                        path: path.to_string(),
                    });
                }
            }
        }

        // Set the final value
        let final_key = segments.last().unwrap();
        let old = current.children.insert(final_key.to_string(), value.into());

        Ok(match old {
            Some(Value::Deleted) => None, // Don't return tombstones
            value => value,
        })
    }

    /// Returns an iterator over key-value pairs (excluding tombstones)
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.children
            .iter()
            .filter(|(_, v)| !matches!(v, Value::Deleted))
    }

    /// Returns a mutable iterator over key-value pairs
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut Value)> {
        self.children.iter_mut()
    }

    /// Returns an iterator over keys (excluding tombstones)
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.children
            .iter()
            .filter(|(_, v)| !matches!(v, Value::Deleted))
            .map(|(k, _)| k)
    }

    /// Returns an iterator over values (excluding tombstones)
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.children
            .values()
            .filter(|v| !matches!(v, Value::Deleted))
    }

    /// Returns a mutable iterator over values
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Value> {
        self.children.values_mut()
    }

    /// Returns the underlying HashMap (including tombstones)
    ///
    /// This provides direct access to the internal HashMap representation,
    /// including tombstone entries. This method is primarily for testing,
    /// debugging, and advanced use cases that need to examine the complete
    /// CRDT state including deleted entries.
    pub fn as_hashmap(&self) -> &HashMap<String, Value> {
        &self.children
    }

    /// Returns a mutable reference to the underlying HashMap
    ///
    /// This provides direct mutable access to the internal HashMap representation.
    /// Use with caution as it bypasses normal CRDT semantics and can potentially
    /// break invariants if used incorrectly.
    pub fn as_hashmap_mut(&mut self) -> &mut HashMap<String, Value> {
        &mut self.children
    }

    /// Set a key-value pair with automatic JSON serialization for any Serialize type.
    pub fn set_json<K, T>(&mut self, key: K, value: T) -> crate::Result<&mut Self>
    where
        K: Into<String>,
        T: serde::Serialize,
    {
        let json = serde_json::to_string(&value)?;
        self.set(key.into(), Value::Text(json));
        Ok(self)
    }

    /// Get a value by key with automatic JSON deserialization for any Deserialize type.
    pub fn get_json<T>(&self, key: &str) -> crate::Result<T>
    where
        T: for<'de> serde::Deserialize<'de>,
    {
        match self.get(key) {
            Some(Value::Text(json)) => serde_json::from_str::<T>(json).map_err(|e| {
                CRDTError::DeserializationFailed {
                    reason: format!("Failed to deserialize JSON for key '{key}': {e}"),
                }
                .into()
            }),
            Some(other) => Err(CRDTError::TypeMismatch {
                expected: "Text (JSON string)".to_string(),
                actual: format!("{other:?}"),
            }
            .into()),
            None => Err(CRDTError::ElementNotFound {
                key: key.to_string(),
            }
            .into()),
        }
    }

    /// Merge another node into this one (in-place)
    pub fn merge_in_place(&mut self, other: &Node) {
        for (key, other_value) in &other.children {
            match self.children.get_mut(key) {
                Some(self_value) => {
                    self_value.merge(other_value);
                }
                None => {
                    self.children.insert(key.clone(), other_value.clone());
                }
            }
        }
    }

    /// Deletes a key-value pair, creating a tombstone for CRDT semantics
    pub fn delete(&mut self, key: impl Into<String>) -> bool {
        self.remove(key).is_some()
    }

    /// Clears all key-value pairs by creating tombstones
    pub fn clear(&mut self) {
        let keys: Vec<_> = self.children.keys().cloned().collect();
        for key in keys {
            self.children.insert(key, Value::Deleted);
        }
    }

    /// Creates a Node from an iterator of key-value pairs
    pub fn from_pairs<I, K, V>(iter: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<Value>,
    {
        let mut node = Self::new();
        for (key, value) in iter {
            node.set(key, value);
        }
        node
    }

    /// Sets a raw value for a key (alias for set)
    pub fn set_raw<K, V>(&mut self, key: K, value: V) -> Option<Value>
    where
        K: Into<String>,
        V: Into<Value>,
    {
        self.set(key, value)
    }

    /// Sets a string value for a key
    pub fn set_string<K, V>(&mut self, key: K, value: V) -> Option<Value>
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.set(key, Value::Text(value.into()))
    }

    /// Sets a node value for a key
    pub fn set_node<K>(&mut self, key: K, value: Node) -> Option<Value>
    where
        K: Into<String>,
    {
        self.set(key, Value::Node(value))
    }

    /// Gets a mutable reference to a node value by key
    pub fn get_node_mut(&mut self, key: impl AsRef<str>) -> Option<&mut Node> {
        match self.get_mut(key)? {
            Value::Node(node) => Some(node),
            _ => None,
        }
    }

    /// Adds an item to a list at the specified key
    pub fn list_add(
        &mut self,
        key: impl Into<String>,
        value: impl Into<Value>,
    ) -> crate::Result<String> {
        let key = key.into();
        let value = value.into();

        // Get or create the list
        let list = match self.children.get_mut(&key) {
            Some(Value::List(list)) => list,
            Some(Value::Deleted) => {
                // Replace tombstone with new list
                let mut new_list = super::list::List::new();
                let index = new_list.push(value);
                self.children.insert(key, Value::List(new_list));
                return Ok(index.to_string());
            }
            Some(_) => {
                return Err(CRDTError::TypeMismatch {
                    expected: "List".to_string(),
                    actual: "other type".to_string(),
                }
                .into());
            }
            None => {
                // Create new list
                let mut new_list = super::list::List::new();
                let index = new_list.push(value);
                self.children.insert(key, Value::List(new_list));
                return Ok(index.to_string());
            }
        };

        Ok(list.push(value).to_string())
    }

    /// Removes an item from a list at the specified key and index
    pub fn list_remove(&mut self, key: impl AsRef<str>, id: &str) -> crate::Result<bool> {
        let index: usize = id.parse().map_err(|_| CRDTError::InvalidPath {
            path: format!("Invalid list index: {id}"),
        })?;

        match self.get_mut(key.as_ref()) {
            Some(Value::List(list)) => Ok(list.remove(index).is_some()),
            Some(_) => Err(CRDTError::TypeMismatch {
                expected: "List".to_string(),
                actual: "other type".to_string(),
            }
            .into()),
            None => Ok(false), // Key doesn't exist
        }
    }

    /// Gets an item from a list at the specified key and index
    pub fn list_get(&self, key: impl AsRef<str>, id: &str) -> Option<&Value> {
        let index: usize = id.parse().ok()?;
        match self.get(key)? {
            Value::List(list) => list.get(index),
            _ => None,
        }
    }

    /// Gets all position IDs from a list at the specified key
    pub fn list_ids(&self, key: impl AsRef<str>) -> Vec<String> {
        match self.get(key) {
            Some(Value::List(list)) => {
                // Return index-based IDs as strings
                (0..list.len()).map(|i| i.to_string()).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Gets the length of a list at the specified key
    pub fn list_len(&self, key: impl AsRef<str>) -> usize {
        match self.get(key) {
            Some(Value::List(list)) => list.len(),
            _ => 0,
        }
    }

    /// Checks if a list at the specified key is empty
    pub fn list_is_empty(&self, key: impl AsRef<str>) -> bool {
        match self.get(key) {
            Some(Value::List(list)) => list.is_empty(),
            _ => true, // Non-existent lists are considered empty
        }
    }

    /// Clears all items from a list at the specified key
    pub fn list_clear(&mut self, key: impl AsRef<str>) -> crate::Result<()> {
        match self.get_mut(key.as_ref()) {
            Some(Value::List(list)) => {
                list.clear();
                Ok(())
            }
            Some(_) => Err(CRDTError::TypeMismatch {
                expected: "List".to_string(),
                actual: "other type".to_string(),
            }
            .into()),
            None => Ok(()), // Nothing to clear
        }
    }

    /// Converts to a JSON-like string representation for human-readable output
    pub fn to_json_string(&self) -> String {
        let mut result = String::with_capacity(self.children.len() * 16);
        result.push('{');
        let mut first = true;
        for (key, value) in self.iter() {
            if !first {
                result.push(',');
            }
            result.push_str(&format!("\"{}\":{}", key, value.to_json_string()));
            first = false;
        }
        result.push('}');
        result
    }
}

impl Default for Node {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{")?;
        let mut first = true;
        for (key, value) in self.iter() {
            if !first {
                write!(f, ", ")?;
            }
            write!(f, "{key}: {value}")?;
            first = false;
        }
        write!(f, "}}")
    }
}

// CRDT implementation
impl CRDT for Node {
    fn merge(&self, other: &Self) -> Result<Self> {
        let mut result = self.clone();
        for (key, other_value) in &other.children {
            match result.children.get_mut(key) {
                Some(self_value) => {
                    self_value.merge(other_value);
                }
                None => {
                    result.children.insert(key.clone(), other_value.clone());
                }
            }
        }
        Ok(result)
    }
}

// Data trait implementation
impl Data for Node {}

// FromIterator implementation for ergonomic Node creation from key-value pairs
impl FromIterator<(String, Value)> for Node {
    fn from_iter<T: IntoIterator<Item = (String, Value)>>(iter: T) -> Self {
        Node::from_pairs(iter)
    }
}
