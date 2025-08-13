//! Document-level CRDT API.
//!
//! This module provides the main public interface for CRDT documents in Eidetica.
//! The [`Doc`] type serves as the primary entry point for all CRDT operations,
//! providing a clean separation between document-level operations and internal
//! node structure management.
//!
//! # Design Philosophy
//!
//! The `Doc` type addresses the confusion in the original API where `crdt::Map`
//! served dual purposes as both the main document interface and internal node
//! representation. This separation provides:
//!
//! - **Clear API boundaries**: Users interact only with `Doc` for document operations
//! - **Better conceptual model**: Document vs internal node separation
//! - **Future flexibility**: Can optimize internal representation independently
//! - **Easier testing**: Document and node behaviors can be tested separately
//!
//! # Usage
//!
//! ```
//! use eidetica::crdt::{Doc, traits::CRDT};
//!
//! let mut doc = Doc::new();
//! doc.set("name", "Alice");
//! doc.set("age", 30);
//! doc.set_path("user.profile.bio", "Software developer").unwrap();
//!
//! // Merge with another document
//! let mut doc2 = Doc::new();
//! doc2.set("name", "Bob");
//! doc2.set("city", "New York");
//!
//! let merged = doc.merge(&doc2).unwrap();
//! ```

use std::{collections::HashMap, fmt};

use crate::crdt::CRDTError;
use crate::crdt::map::{List, Map, Node, Value};
use crate::crdt::traits::{CRDT, Data};

/// The main CRDT document type for Eidetica.
///
/// `Doc` provides the primary interface for CRDT document operations,
/// wrapping the internal [`Node`] structure to provide clear API boundaries.
/// This type handles document-level operations like merging, serialization,
/// and high-level data access patterns.
///
/// # Core Operations
///
/// - **Data access**: `get()`, `get_text()`, `get_int()`, etc.
/// - **Data modification**: `set()`, `remove()`, `set_path()`, etc.
/// - **CRDT operations**: `merge()` for conflict-free merging
/// - **Path operations**: Dot-notation access to nested structures
///
/// # Examples
///
/// ## Basic Operations
/// ```
/// # use eidetica::crdt::Doc;
/// let mut doc = Doc::new();
/// doc.set("name", "Alice");
/// doc.set("age", 30);
///
/// assert_eq!(doc.get_text("name"), Some("Alice"));
/// assert_eq!(doc.get_int("age"), Some(30));
/// ```
///
/// ## Path Operations
/// ```
/// # use eidetica::crdt::Doc;
/// let mut doc = Doc::new();
/// doc.set_path("user.profile.name", "Alice").unwrap();
///
/// assert_eq!(doc.get_text_at_path("user.profile.name"), Some("Alice"));
/// ```
///
/// ## CRDT Merging
/// ```
/// # use eidetica::crdt::{Doc, traits::CRDT};
/// let mut doc1 = Doc::new();
/// doc1.set("name", "Alice");
/// doc1.set("age", 30);
///
/// let mut doc2 = Doc::new();
/// doc2.set("name", "Bob");
/// doc2.set("city", "NYC");
///
/// let merged = doc1.merge(&doc2).unwrap();
/// assert_eq!(merged.get_text("name"), Some("Bob")); // Last write wins
/// assert_eq!(merged.get_int("age"), Some(30));        // Preserved from doc1
/// assert_eq!(merged.get_text("city"), Some("NYC"));   // Added from doc2
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Doc {
    /// Internal node structure handling the actual CRDT logic
    root: Node,
}

impl Doc {
    /// Creates a new empty document
    pub fn new() -> Self {
        Self { root: Node::new() }
    }

    /// Returns true if this document has no data (excluding tombstones)
    pub fn is_empty(&self) -> bool {
        self.root.is_empty()
    }

    /// Returns the number of direct keys (excluding tombstones)
    pub fn len(&self) -> usize {
        self.root.len()
    }

    /// Returns true if the document contains the given key
    pub fn contains_key(&self, key: impl AsRef<str>) -> bool {
        self.root.contains_key(key)
    }

    /// Returns true if the given key contains a tombstone (deleted value).
    ///
    /// This method provides access to CRDT tombstone information for advanced use cases,
    /// testing, and debugging. See [`Map::is_tombstone`] for detailed documentation.
    pub fn is_tombstone(&self, key: impl AsRef<str>) -> bool {
        self.root.is_tombstone(key)
    }

    /// Gets a value by key (immutable reference)
    pub fn get(&self, key: impl AsRef<str>) -> Option<&Value> {
        self.root.get(key)
    }

    /// Gets a mutable reference to a value by key
    pub fn get_mut(&mut self, key: impl AsRef<str>) -> Option<&mut Value> {
        self.root.get_mut(key)
    }

    /// Gets a text value by key
    pub fn get_text(&self, key: impl AsRef<str>) -> Option<&str> {
        self.root.get_text(key)
    }

    /// Gets an integer value by key
    pub fn get_int(&self, key: impl AsRef<str>) -> Option<i64> {
        self.root.get_int(key)
    }

    /// Gets a boolean value by key
    pub fn get_bool(&self, key: impl AsRef<str>) -> Option<bool> {
        self.root.get_bool(key)
    }

    /// Gets a map value by key
    pub fn get_map(&self, key: impl AsRef<str>) -> Option<&Node> {
        self.root.get_node(key)
    }

    /// Gets a nested document by key (returns Doc instead of Node)
    pub fn get_node(&self, key: impl AsRef<str>) -> Option<Doc> {
        self.root
            .get_node(key)
            .map(|node| Doc::from_node(node.clone()))
    }

    /// Gets a list value by key
    pub fn get_list(&self, key: impl AsRef<str>) -> Option<&List> {
        self.root.get_list(key)
    }

    /// Sets a value at the given key, returns the old value if present
    pub fn set<K, V>(&mut self, key: K, value: V) -> Option<Value>
    where
        K: Into<String>,
        V: Into<Value>,
    {
        self.root.set(key, value)
    }

    /// Removes a value by key, returns the old value if present.
    ///
    /// This method implements CRDT semantics by creating a tombstone marker.
    /// See [`Node::remove`] for detailed documentation.
    pub fn remove(&mut self, key: impl Into<String>) -> Option<Value> {
        self.root.remove(key)
    }

    /// Marks a key as deleted by setting it to a tombstone.
    ///
    /// See [`Node::delete`] for detailed documentation.
    pub fn delete(&mut self, key: impl Into<String>) -> bool {
        self.root.delete(key)
    }

    /// Gets a value by path using dot notation (e.g., "users.123.name").
    ///
    /// See [`Node::get_path`] for detailed documentation and examples.
    pub fn get_path(&self, path: impl AsRef<str>) -> Option<&Value> {
        self.root.get_path(path)
    }

    /// Gets a text value by path
    pub fn get_text_at_path(&self, path: impl AsRef<str>) -> Option<&str> {
        self.root.get_text_at_path(path)
    }

    /// Gets an integer value by path
    pub fn get_int_at_path(&self, path: impl AsRef<str>) -> Option<i64> {
        self.root.get_int_at_path(path)
    }

    /// Gets a boolean value by path
    pub fn get_bool_at_path(&self, path: impl AsRef<str>) -> Option<bool> {
        self.root.get_bool_at_path(path)
    }

    /// Gets a map value by path
    pub fn get_map_at_path(&self, path: impl AsRef<str>) -> Option<&Node> {
        self.root.get_node_at_path(path)
    }

    /// Gets a list value by path
    pub fn get_list_at_path(&self, path: impl AsRef<str>) -> Option<&List> {
        self.root.get_list_at_path(path)
    }

    /// Gets a mutable reference to a value by path
    pub fn get_path_mut(&mut self, path: impl AsRef<str>) -> Option<&mut Value> {
        self.root.get_path_mut(path)
    }

    /// Sets a value at the given path, creating intermediate maps as needed
    pub fn set_path(
        &mut self,
        path: impl AsRef<str>,
        value: impl Into<Value>,
    ) -> Result<Option<Value>, CRDTError> {
        self.root.set_path(path, value)
    }

    /// Returns an iterator over all key-value pairs (excluding tombstones)
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.root.iter()
    }

    /// Returns a mutable iterator over all key-value pairs (excluding tombstones)
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut Value)> {
        self.root.iter_mut()
    }

    /// Returns an iterator over all keys (excluding tombstones)
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.root.keys()
    }

    /// Returns an iterator over all values (excluding tombstones)
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.root.values()
    }

    /// Returns a mutable iterator over all values (excluding tombstones)
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Value> {
        self.root.values_mut()
    }

    /// Clears all data from this document
    pub fn clear(&mut self) {
        self.root.clear()
    }

    /// Converts to a JSON-like string representation for human-readable output.
    ///
    /// See [`Map::to_json_string`] for detailed documentation.
    pub fn to_json_string(&self) -> String {
        self.root.to_json_string()
    }

    /// Provides access to the internal node structure for advanced operations
    ///
    /// This method is primarily intended for:
    /// - Testing and debugging
    /// - Integration with existing code during migration
    /// - Advanced operations not yet exposed in the Doc API
    ///
    /// Most users should prefer the document-level methods instead.
    pub fn as_node(&self) -> &Node {
        &self.root
    }

    /// Provides mutable access to the internal node structure for advanced operations
    ///
    /// This method is primarily intended for:
    /// - Testing and debugging
    /// - Integration with existing code during migration
    /// - Advanced operations not yet exposed in the Doc API
    ///
    /// Most users should prefer the document-level methods instead.
    pub fn as_node_mut(&mut self) -> &mut Node {
        &mut self.root
    }

    /// Provides access to the internal structure as Map for backwards compatibility
    ///
    /// This method provides the Map type alias which points to Node, maintaining
    /// backwards compatibility during the migration period.
    pub fn as_map(&self) -> &Map {
        &self.root
    }

    /// Provides mutable access to the internal structure as Map for backwards compatibility
    ///
    /// This method provides the Map type alias which points to Node, maintaining
    /// backwards compatibility during the migration period.
    pub fn as_map_mut(&mut self) -> &mut Map {
        &mut self.root
    }

    /// Extracts the internal root node, consuming the Doc
    ///
    /// This method is useful for converting from Doc back to Node when needed
    /// for internal operations or backwards compatibility.
    pub fn into_root(self) -> Node {
        self.root
    }

    /// Creates a new Doc from a Node
    ///
    /// This method is useful for converting from Node to Doc when transitioning
    /// between the internal representation and the public API.
    pub fn from_node(node: Node) -> Self {
        Doc { root: node }
    }

    /// Clones the internal Node structure
    ///
    /// This method is useful when you need a Node copy for use in Value::Map
    /// or other internal CRDT operations.
    pub fn clone_as_node(&self) -> Node {
        self.root.clone()
    }
}

impl CRDT for Doc {
    /// Merges another document into this one using CRDT semantics.
    ///
    /// This implements the core CRDT merge operation at the document level,
    /// delegating to the internal Node's merge implementation while providing
    /// a clean document-level interface.
    ///
    /// See [`Node::merge`] for detailed merge semantics and examples.
    fn merge(&self, other: &Self) -> crate::Result<Self> {
        let merged_root = self.root.merge(&other.root)?;
        Ok(Doc { root: merged_root })
    }
}

impl Default for Doc {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Doc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.root)
    }
}

impl FromIterator<(String, Value)> for Doc {
    fn from_iter<T: IntoIterator<Item = (String, Value)>>(iter: T) -> Self {
        let root = Node::from_iter(iter);
        Doc { root }
    }
}

// Builder pattern methods
impl Doc {
    /// Builder method to set a value and return self
    pub fn with(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.set(key, value);
        self
    }

    /// Builder method to set a boolean value
    pub fn with_bool(self, key: impl Into<String>, value: bool) -> Self {
        self.with(key, Value::Bool(value))
    }

    /// Builder method to set an integer value
    pub fn with_int(self, key: impl Into<String>, value: i64) -> Self {
        self.with(key, Value::Int(value))
    }

    /// Builder method to set a text value
    pub fn with_text(self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.with(key, Value::Text(value.into()))
    }

    /// Builder method to set a nested map
    pub fn with_map<K, V>(self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Into<Node>,
    {
        self.with(key, Value::Node(value.into()))
    }

    /// Builder method to set a list value
    pub fn with_list(self, key: impl Into<String>, value: impl Into<List>) -> Self {
        self.with(key, Value::List(value.into()))
    }

    /// Builder method to set a child node (alias for with_map for compatibility)
    pub fn with_node<K, V>(self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Into<Node>,
    {
        self.with_map(key, value)
    }
}

// Additional methods for compatibility and advanced operations
impl Doc {
    /// Set a key-value pair with a raw Value (for advanced use)
    pub fn set_raw<K>(&mut self, key: K, value: Value) -> &mut Self
    where
        K: Into<String>,
    {
        self.root.set_raw(key, value);
        self
    }

    /// Set a key-value pair with automatic JSON serialization for any Serialize type
    pub fn set_json<K, T>(&mut self, key: K, value: T) -> crate::Result<&mut Self>
    where
        K: Into<String>,
        T: serde::Serialize,
    {
        self.root.set_json(key, value)?;
        Ok(self)
    }

    /// Get a value by key with automatic JSON deserialization for any Deserialize type
    pub fn get_json<T>(&self, key: &str) -> crate::Result<T>
    where
        T: for<'de> serde::Deserialize<'de>,
    {
        self.root.get_json(key)
    }

    /// Set a key-value pair where the value is a string
    pub fn set_string<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.root.set_string(key, value);
        self
    }

    /// Set a key-value pair where the value is a nested map
    pub fn set_map<K>(&mut self, key: K, value: impl Into<Node>) -> &mut Self
    where
        K: Into<String>,
    {
        self.root.set_map(key, value.into());
        self
    }

    /// Get a mutable reference to a nested map by key
    pub fn get_map_mut(&mut self, key: &str) -> Option<&mut Node> {
        self.root.get_map_mut(key)
    }

    /// Get a reference to the internal HashMap compatible with Map API
    pub fn as_hashmap(&self) -> &HashMap<String, Value> {
        self.root.as_hashmap()
    }

    /// Get a mutable reference to the internal HashMap (compatibility method)
    pub fn as_hashmap_mut(&mut self) -> &mut HashMap<String, Value> {
        self.root.as_hashmap_mut()
    }

    /// List operations compatibility methods
    pub fn list_add<K>(&mut self, key: K, value: Value) -> crate::Result<String>
    where
        K: Into<String>,
    {
        self.root.list_add(key, value)
    }

    /// List remove operation - tombstones element by position ID
    pub fn list_remove<K>(&mut self, key: K, id: &str) -> crate::Result<bool>
    where
        K: Into<String>,
    {
        self.root.list_remove(key, id)
    }

    /// Get an element by its ID from a list
    pub fn list_get<K>(&self, key: K, id: &str) -> Option<&Value>
    where
        K: AsRef<str>,
    {
        self.root.list_get(key, id)
    }

    /// Get all element IDs from a list in order
    pub fn list_ids<K>(&self, key: K) -> Vec<String>
    where
        K: AsRef<str>,
    {
        self.root.list_ids(key)
    }

    /// Get list length (excluding tombstones)
    pub fn list_len<K>(&self, key: K) -> usize
    where
        K: AsRef<str>,
    {
        self.root.list_len(key)
    }

    /// Check if list is empty
    pub fn list_is_empty<K>(&self, key: K) -> bool
    where
        K: AsRef<str>,
    {
        self.root.list_is_empty(key)
    }

    /// Clear list
    pub fn list_clear<K>(&mut self, key: K) -> crate::Result<()>
    where
        K: AsRef<str>,
    {
        self.root.list_clear(key)
    }
}

// Conversion implementations
impl From<Node> for Doc {
    fn from(node: Node) -> Self {
        Doc { root: node }
    }
}

impl From<Doc> for Node {
    fn from(doc: Doc) -> Self {
        doc.into_root()
    }
}

// Data trait implementation
impl Data for Doc {}
