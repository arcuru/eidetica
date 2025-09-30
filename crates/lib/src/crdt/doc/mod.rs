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
//! use eidetica::crdt::doc::path;
//!
//! let mut doc = Doc::new();
//! doc.set("name", "Alice");
//! doc.set("age", 30);
//! doc.set_path(path!("user.profile.bio"), "Software developer").unwrap();
//!
//! // Merge with another document
//! let mut doc2 = Doc::new();
//! doc2.set("name", "Bob");
//! doc2.set("city", "New York");
//!
//! let merged = doc.merge(&doc2).unwrap();
//! ```

use std::{collections::HashMap, fmt, str::FromStr};

use crate::crdt::{
    CRDTError,
    traits::{CRDT, Data},
};

// Submodules
pub mod list;
pub mod node;
#[cfg(test)]
mod node_tests;
pub mod path;
pub mod value;

// Path re-exports (no conflicts)
// Convenience re-exports for core Doc types
pub use list::List;
pub use node::Node;
pub use path::{Path, PathBuf, PathError};
pub use value::Value;

// Re-export the macro from crate root
pub use crate::path;

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
/// # use eidetica::crdt::doc::path;
/// let mut doc = Doc::new();
/// doc.set_path(path!("user.profile.name"), "Alice").unwrap();
///
/// assert_eq!(doc.get_text_at_path(path!("user.profile.name")), Some("Alice"));
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
    /// testing, and debugging. See [`Doc::is_tombstone`] for detailed documentation.
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

    /// Gets a nested document by key (returns Doc wrapper)
    pub fn get_doc(&self, key: impl AsRef<str>) -> Option<Doc> {
        self.root
            .get_node(key)
            .map(|node| Doc::from_node(node.clone()))
    }

    /// Gets a list value by key
    pub fn get_list(&self, key: impl AsRef<str>) -> Option<&List> {
        self.root.get_list(key)
    }

    /// Gets a value by key with automatic type conversion using TryFrom
    ///
    /// This provides a generic interface that can convert to any type that implements
    /// `TryFrom<&Value>`, making the API more ergonomic by reducing type specification.
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::Doc;
    /// # use eidetica::Result;
    /// let mut doc = Doc::new();
    /// doc.set("name", "Alice");
    /// doc.set("age", 30);
    /// doc.set("active", true);
    ///
    /// // Type inference makes this clean
    /// let name: Result<String> = doc.get_as("name");
    /// let age: Result<i64> = doc.get_as("age");
    /// let active: Result<bool> = doc.get_as("active");
    ///
    /// assert_eq!(name.unwrap(), "Alice");
    /// assert_eq!(age.unwrap(), 30);
    /// assert_eq!(active.unwrap(), true);
    /// ```
    pub fn get_as<T>(&self, key: impl AsRef<str>) -> crate::Result<T>
    where
        T: for<'a> TryFrom<&'a Value, Error = CRDTError>,
    {
        let key_str = key.as_ref();
        match self.get(key_str) {
            Some(value) => T::try_from(value).map_err(Into::into),
            None => Err(CRDTError::ElementNotFound {
                key: key_str.to_string(),
            }
            .into()),
        }
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
    pub fn get_path(&self, path: impl AsRef<Path>) -> Option<&Value> {
        self.root.get_path(path)
    }

    /// Gets a value by path from a string, normalizing at runtime.
    pub fn get_path_str(&self, path: &str) -> Option<&Value> {
        let path_buf = PathBuf::from_str(path).unwrap(); // Infallible
        self.get_path(&path_buf)
    }

    /// Gets a text value by path
    pub fn get_text_at_path(&self, path: impl AsRef<Path>) -> Option<&str> {
        match self.get_path(path)? {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Gets an integer value by path
    pub fn get_int_at_path(&self, path: impl AsRef<Path>) -> Option<i64> {
        match self.get_path(path)? {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Gets a boolean value by path
    pub fn get_bool_at_path(&self, path: impl AsRef<Path>) -> Option<bool> {
        match self.get_path(path)? {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Gets a list value by path
    pub fn get_list_at_path(&self, path: impl AsRef<Path>) -> Option<&List> {
        match self.get_path(path)? {
            Value::List(list) => Some(list),
            _ => None,
        }
    }

    /// Gets a value by path with automatic type conversion using TryFrom
    ///
    /// Similar to `get_as()` but works with dot-notation paths for nested access.
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::Doc;
    /// # use eidetica::Result;
    /// # use eidetica::crdt::doc::path;
    /// let mut doc = Doc::new();
    /// doc.set_path(path!("user.profile.name"), "Alice").unwrap();
    /// doc.set_path(path!("user.profile.age"), 30).unwrap();
    ///
    /// // Type inference with path access
    /// let name: Result<String> = doc.get_path_as(path!("user.profile.name"));
    /// let age: Result<i64> = doc.get_path_as(path!("user.profile.age"));
    ///
    /// assert_eq!(name.unwrap(), "Alice");
    /// assert_eq!(age.unwrap(), 30);
    /// ```
    pub fn get_path_as<T>(&self, path: impl AsRef<Path>) -> crate::Result<T>
    where
        T: for<'a> TryFrom<&'a Value, Error = CRDTError>,
    {
        let path_ref = path.as_ref();
        match self.get_path(path_ref) {
            Some(value) => T::try_from(value).map_err(Into::into),
            None => Err(CRDTError::ElementNotFound {
                key: path_ref.as_str().to_string(),
            }
            .into()),
        }
    }

    /// Gets a mutable reference to a value by path
    pub fn get_path_mut(&mut self, path: impl AsRef<Path>) -> Option<&mut Value> {
        self.root.get_path_mut(path)
    }

    /// Sets a value at the given path, creating intermediate nodes as needed
    pub fn set_path(
        &mut self,
        path: impl AsRef<Path>,
        value: impl Into<Value>,
    ) -> Result<Option<Value>, CRDTError> {
        self.root.set_path(path, value)
    }

    /// Sets a value at the given path from a string, normalizing at runtime.
    pub fn set_path_str(
        &mut self,
        path: &str,
        value: impl Into<Value>,
    ) -> Result<Option<Value>, CRDTError> {
        let path_buf = PathBuf::from_str(path).unwrap(); // Infallible
        self.set_path(&path_buf, value)
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
    /// See [`Doc::to_json_string`] for detailed documentation.
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
    /// This method is useful when you need a Node copy for use in Value::Node
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
        let root = Node::from_pairs(iter);
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

    /// Builder method to set a list value
    pub fn with_list(self, key: impl Into<String>, value: impl Into<List>) -> Self {
        self.with(key, Value::List(value.into()))
    }

    /// Builder method to set a child node
    pub fn with_node<K, V>(self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Into<Node>,
    {
        self.with(key, Value::Node(value.into()))
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

    /// Set a key-value pair where the value is a nested node
    pub fn set_node<K>(&mut self, key: K, value: impl Into<Node>) -> &mut Self
    where
        K: Into<String>,
    {
        self.root.set_node(key, value.into());
        self
    }

    /// Get a mutable reference to a nested node by key
    pub fn get_node_mut(&mut self, key: &str) -> Option<&mut Node> {
        self.root.get_node_mut(key)
    }

    /// Get a reference to the internal HashMap for advanced access
    pub fn as_hashmap(&self) -> &HashMap<String, Value> {
        self.root.as_hashmap()
    }

    /// Get a mutable reference to the internal HashMap for advanced access
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
        K: Into<String> + AsRef<str>,
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

    /// Convenience methods for ergonomic access with better error handling
    ///
    /// These methods provide cleaner syntax for common CRDT operations.
    ///
    /// Mutable access methods for working with Values directly
    ///
    /// These methods provide cleaner access to mutable Values for in-place modification.
    ///
    /// Gets or inserts a value with a default, returns a mutable reference
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::Doc;
    /// # use eidetica::crdt::doc::Value;
    /// let mut doc = Doc::new();
    ///
    /// // Key doesn't exist - will insert default
    /// doc.get_or_insert("counter", 0);
    /// assert_eq!(doc.get_as::<i64>("counter").unwrap(), 0);
    ///
    /// // Key exists - will keep existing value
    /// doc.set("counter", 5);
    /// doc.get_or_insert("counter", 100);
    /// assert_eq!(doc.get_as::<i64>("counter").unwrap(), 5);
    /// ```
    pub fn get_or_insert(&mut self, key: impl AsRef<str>, default: impl Into<Value>) -> &mut Value {
        let key = key.as_ref();
        if !self.contains_key(key) {
            self.set(key, default);
        }
        self.get_mut(key).expect("Key should exist after insert")
    }

    /// Modifies a value in-place using a closure
    ///
    /// If the key exists and can be converted to type T, the closure is called
    /// with a mutable reference to the typed value. After the closure returns,
    /// the modified value is converted back and stored.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The key doesn't exist (`CRDTError::ElementNotFound`)
    /// - The value cannot be converted to type T (`CRDTError::TypeMismatch`)
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::Doc;
    /// let mut doc = Doc::new();
    /// doc.set("count", 5);
    /// doc.set("text", "hello");
    ///
    /// // Modify counter
    /// doc.modify::<i64, _>("count", |count| {
    ///     *count += 10;
    /// })?;
    /// assert_eq!(doc.get_as::<i64>("count")?, 15);
    ///
    /// // Modify string
    /// doc.modify::<String, _>("text", |text| {
    ///     text.push_str(" world");
    /// })?;
    /// assert_eq!(doc.get_as::<String>("text")?, "hello world");
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn modify<T, F>(&mut self, key: impl AsRef<str>, f: F) -> crate::Result<()>
    where
        T: for<'a> TryFrom<&'a Value, Error = CRDTError> + Into<Value>,
        F: FnOnce(&mut T),
    {
        let key = key.as_ref();

        // Try to get and convert the current value
        let mut value = self.get_as::<T>(key)?;

        // Apply the modification
        f(&mut value);

        // Store the modified value back
        self.set(key, value);
        Ok(())
    }

    /// Modifies a value at a path in-place using a closure
    ///
    /// Similar to `modify()` but works with dot-notation paths.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The path doesn't exist (`CRDTError::ElementNotFound`)
    /// - The value cannot be converted to type T (`CRDTError::TypeMismatch`)
    /// - Setting the path fails (`CRDTError::InvalidPath`)
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::Doc;
    /// # use eidetica::crdt::doc::path;
    /// let mut doc = Doc::new();
    /// doc.set_path(path!("user.score"), 100)?;
    ///
    /// doc.modify_path::<i64, _>(path!("user.score"), |score| {
    ///     *score += 50;
    /// })?;
    ///
    /// assert_eq!(doc.get_path_as::<i64>(path!("user.score"))?, 150);
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn modify_path<T, F>(&mut self, path: impl AsRef<Path>, f: F) -> crate::Result<()>
    where
        T: for<'a> TryFrom<&'a Value, Error = CRDTError> + Into<Value>,
        F: FnOnce(&mut T),
    {
        // Try to get and convert the current value
        let mut value = self.get_path_as::<T>(&path)?;

        // Apply the modification
        f(&mut value);

        // Store the modified value back
        self.set_path(path, value)?;
        Ok(())
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
        doc.root
    }
}

// Data trait implementation
impl Data for Doc {}
