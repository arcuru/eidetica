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
/// doc.set("user.profile.name", "Alice");
///
/// assert_eq!(doc.get_text("user.profile.name"), Some("Alice"));
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
    pub fn contains_key(&self, key: impl AsRef<Path>) -> bool {
        let path_buf = PathBuf::from_str(key.as_ref().as_str()).unwrap(); // Infallible
        self.root.get_path(&path_buf).is_some()
    }

    /// Returns true if the given key contains a tombstone (deleted value).
    ///
    /// This method provides access to CRDT tombstone information for advanced use cases,
    /// testing, and debugging. See [`Doc::is_tombstone`] for detailed documentation.
    pub fn is_tombstone(&self, key: impl AsRef<Path>) -> bool {
        // For path-based access, we need to check the final component
        let path_str = key.as_ref().as_str();
        if path_str.contains('.') {
            // For nested paths, we can't easily check tombstones
            // FIXME: This is a limitation of the current design
            false
        } else {
            // For direct keys, delegate to root
            self.root.is_tombstone(path_str)
        }
    }

    /// Gets a value by key or path (immutable reference)
    pub fn get(&self, key: impl AsRef<Path>) -> Option<&Value> {
        let path_buf = PathBuf::from_str(key.as_ref().as_str()).unwrap(); // Infallible
        self.root.get_path(&path_buf)
    }

    /// Gets a mutable reference to a value by key or path
    pub fn get_mut(&mut self, key: impl AsRef<Path>) -> Option<&mut Value> {
        let path_buf = PathBuf::from_str(key.as_ref().as_str()).unwrap(); // Infallible
        self.root.get_path_mut(&path_buf)
    }

    /// Gets a text value by key or path
    pub fn get_text(&self, key: impl AsRef<Path>) -> Option<&str> {
        match self.get(key)? {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Gets an integer value by key or path
    pub fn get_int(&self, key: impl AsRef<Path>) -> Option<i64> {
        match self.get(key)? {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Gets a boolean value by key or path
    pub fn get_bool(&self, key: impl AsRef<Path>) -> Option<bool> {
        match self.get(key)? {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Gets a nested document by key or path (returns Doc wrapper)
    pub fn get_doc(&self, key: impl AsRef<Path>) -> Option<Doc> {
        match self.get(key)? {
            Value::Node(node) => Some(Doc::from_node(node.clone())),
            _ => None,
        }
    }

    /// Gets a list value by key or path
    pub fn get_list(&self, key: impl AsRef<Path>) -> Option<&List> {
        match self.get(key)? {
            Value::List(list) => Some(list),
            _ => None,
        }
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
    pub fn get_as<T>(&self, key: impl AsRef<Path>) -> crate::Result<T>
    where
        T: for<'a> TryFrom<&'a Value, Error = CRDTError>,
    {
        let key_ref = key.as_ref();
        let key_str = key_ref.as_str();
        match self.get(key_ref) {
            Some(value) => T::try_from(value).map_err(Into::into),
            None => Err(CRDTError::ElementNotFound {
                key: key_str.to_string(),
            }
            .into()),
        }
    }

    /// Sets a value at the given key or path, returns the old value if present
    ///
    /// This method automatically creates intermediate nodes for nested paths.
    /// Path creation is always successful due to normalization, but setting
    /// through existing scalar values will fail.
    pub fn set(&mut self, key: impl AsRef<Path>, value: impl Into<Value>) -> Option<Value> {
        let path_str = key.as_ref().as_str();
        let path_buf = PathBuf::from_str(path_str).unwrap(); // Infallible

        // For simple keys (no dots), use direct assignment
        if !path_str.contains('.') {
            self.root.set(path_str, value)
        } else {
            // For paths, use set_path - errors should be handled by caller
            // but we return None for this non-Result interface
            // Path operations can fail (e.g., trying to set through scalar)
            // For this Option-returning interface, we return None
            // Use try_set() for Result-based error handling
            self.root.set_path(&path_buf, value).unwrap_or_default()
        }
    }

    /// Sets a value at the given key or path with Result error handling
    ///
    /// This provides a Result-based interface for cases where you need to
    /// handle path operation errors (e.g., setting through scalar values).
    pub fn try_set(
        &mut self,
        key: impl AsRef<Path>,
        value: impl Into<Value>,
    ) -> crate::Result<Option<Value>> {
        let path_str = key.as_ref().as_str();
        let path_buf = PathBuf::from_str(path_str).unwrap(); // Infallible

        // Check for empty key - this is not allowed
        if path_str.is_empty() {
            return Err(crate::crdt::CRDTError::InvalidPath {
                path: "empty path (not allowed for setting values)".to_string(),
            }
            .into());
        }

        // For simple keys (no dots), use direct assignment
        if !path_str.contains('.') {
            Ok(self.root.set(path_str, value))
        } else {
            // For paths, use set_path and return the Result
            self.root.set_path(&path_buf, value).map_err(Into::into)
        }
    }

    /// Removes a value by key or path, returns the old value if present.
    ///
    /// This method implements CRDT semantics by creating a tombstone marker.
    /// For paths with dots, this removes the value at the nested location.
    pub fn remove(&mut self, key: impl AsRef<Path>) -> Option<Value> {
        let path_str = key.as_ref().as_str();

        // For simple keys (no dots), use direct remove
        if !path_str.contains('.') {
            self.root.remove(path_str)
        } else {
            // For paths, get the current value first, then set to tombstone
            let _current = self.get(key)?.clone();
            // FIXME: We can't easily remove nested paths in the current CRDT design
            // This is a limitation we'll need to address in the future
            // For now, we'll just return None for nested paths
            None
        }
    }

    /// Marks a key as deleted by setting it to a tombstone.
    ///
    /// For paths with dots, this is limited to direct keys only.
    pub fn delete(&mut self, key: impl AsRef<Path>) -> bool {
        let path_str = key.as_ref().as_str();

        // For simple keys (no dots), use direct delete
        if !path_str.contains('.') {
            self.root.delete(path_str)
        } else {
            // Nested path deletion is not supported in current design
            false
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
    /// let mut doc = Doc::new();
    /// doc.set("user.profile.name", "Alice");
    /// doc.set("user.profile.age", 30);
    ///
    /// // Type inference with path access
    /// let name: Result<String> = doc.get_as("user.profile.name");
    /// let age: Result<i64> = doc.get_as("user.profile.age");
    ///
    /// assert_eq!(name.unwrap(), "Alice");
    /// assert_eq!(age.unwrap(), 30);
    /// ```
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

    // Deprecated wrapper methods for backward compatibility

    /// Sets a value at the given path (deprecated: use `try_set()` instead)
    #[deprecated(
        since = "0.1.0",
        note = "Use `try_set()` instead - it now accepts both keys and paths"
    )]
    pub fn set_path(
        &mut self,
        path: impl AsRef<Path>,
        value: impl Into<Value>,
    ) -> crate::Result<Option<Value>> {
        self.try_set(path, value)
    }

    /// Gets a value at the given path (deprecated: use `get()` instead)
    #[deprecated(
        since = "0.1.0",
        note = "Use `get()` instead - it now accepts both keys and paths"
    )]
    pub fn get_path(&self, path: impl AsRef<Path>) -> Option<&Value> {
        self.get(path)
    }

    /// Gets a text value at the given path (deprecated: use `get_text()` instead)
    #[deprecated(
        since = "0.1.0",
        note = "Use `get_text()` instead - it now accepts both keys and paths"
    )]
    pub fn get_text_at_path(&self, path: impl AsRef<Path>) -> Option<&str> {
        self.get_text(path)
    }

    /// Gets an integer value at the given path (deprecated: use `get_int()` instead)
    #[deprecated(
        since = "0.1.0",
        note = "Use `get_int()` instead - it now accepts both keys and paths"
    )]
    pub fn get_int_at_path(&self, path: impl AsRef<Path>) -> Option<i64> {
        self.get_int(path)
    }

    /// Gets a boolean value at the given path (deprecated: use `get_bool()` instead)
    #[deprecated(
        since = "0.1.0",
        note = "Use `get_bool()` instead - it now accepts both keys and paths"
    )]
    pub fn get_bool_at_path(&self, path: impl AsRef<Path>) -> Option<bool> {
        self.get_bool(path)
    }

    /// Gets a nested document at the given path (deprecated: use `get_doc()` instead)
    #[deprecated(
        since = "0.1.0",
        note = "Use `get_doc()` instead - it now accepts both keys and paths"
    )]
    pub fn get_doc_at_path(&self, path: impl AsRef<Path>) -> Option<Doc> {
        self.get_doc(path)
    }

    /// Gets a list value at the given path (deprecated: use `get_list()` instead)
    #[deprecated(
        since = "0.1.0",
        note = "Use `get_list()` instead - it now accepts both keys and paths"
    )]
    pub fn get_list_at_path(&self, path: impl AsRef<Path>) -> Option<&List> {
        self.get_list(path)
    }

    /// Gets a value with type conversion at the given path (deprecated: use `get_as()` instead)
    #[deprecated(
        since = "0.1.0",
        note = "Use `get_as()` instead - it now accepts both keys and paths"
    )]
    pub fn get_as_at_path<T>(&self, path: impl AsRef<Path>) -> crate::Result<T>
    where
        T: for<'a> TryFrom<&'a Value, Error = CRDTError>,
    {
        self.get_as(path)
    }

    /// Gets a mutable value at the given path (deprecated: use `get_mut()` instead)
    #[deprecated(
        since = "0.1.0",
        note = "Use `get_mut()` instead - it now accepts both keys and paths"
    )]
    pub fn get_path_mut(&mut self, path: impl AsRef<Path>) -> Option<&mut Value> {
        self.get_mut(path)
    }

    /// Gets a value with type conversion at the given path (deprecated: use `get_as()` instead)
    #[deprecated(
        since = "0.1.0",
        note = "Use `get_as()` instead - it now accepts both keys and paths"
    )]
    pub fn get_path_as<T>(&self, path: impl AsRef<Path>) -> crate::Result<T>
    where
        T: for<'a> TryFrom<&'a Value, Error = CRDTError>,
    {
        self.get_as(path)
    }

    /// Modifies a value in-place at the given path (deprecated: use `modify()` instead)
    #[deprecated(
        since = "0.1.0",
        note = "Use `modify()` instead - it now accepts both keys and paths"
    )]
    pub fn modify_path<T, F>(&mut self, path: impl AsRef<Path> + Clone, f: F) -> crate::Result<()>
    where
        T: for<'a> TryFrom<&'a Value, Error = CRDTError> + Into<Value>,
        F: FnOnce(&mut T),
    {
        self.modify(path, f)
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
    pub fn with(mut self, key: impl AsRef<Path>, value: impl Into<Value>) -> Self {
        self.set(key, value);
        self
    }

    /// Builder method to set a boolean value
    pub fn with_bool(self, key: impl AsRef<Path>, value: bool) -> Self {
        self.with(key, Value::Bool(value))
    }

    /// Builder method to set an integer value
    pub fn with_int(self, key: impl AsRef<Path>, value: i64) -> Self {
        self.with(key, Value::Int(value))
    }

    /// Builder method to set a text value
    pub fn with_text(self, key: impl AsRef<Path>, value: impl Into<String>) -> Self {
        self.with(key, Value::Text(value.into()))
    }

    /// Builder method to set a list value
    pub fn with_list(self, key: impl AsRef<Path>, value: impl Into<List>) -> Self {
        self.with(key, Value::List(value.into()))
    }

    /// Builder method to set a child node
    pub fn with_node(self, key: impl AsRef<Path>, value: impl Into<Node>) -> Self {
        self.with(key, Value::Node(value.into()))
    }
}

// Additional methods for compatibility and advanced operations
impl Doc {
    /// Set a key-value pair with a raw Value (for advanced use)
    pub fn set_raw(&mut self, key: impl AsRef<Path>, value: Value) -> &mut Self {
        self.set(key, value);
        self
    }

    /// Set a key-value pair with automatic JSON serialization for any Serialize type
    pub fn set_json<T>(&mut self, key: impl AsRef<Path>, value: T) -> crate::Result<&mut Self>
    where
        T: serde::Serialize,
    {
        let path_str = key.as_ref().as_str();

        // For simple keys (no dots), delegate to Node's set_json
        if !path_str.contains('.') {
            self.root.set_json(path_str, value)?;
        } else {
            // For paths, serialize to JSON string and set as text
            let json =
                serde_json::to_string(&value).map_err(|e| CRDTError::SerializationFailed {
                    reason: e.to_string(),
                })?;
            self.set(key, Value::Text(json));
        }
        Ok(self)
    }

    /// Get a value by key with automatic JSON deserialization for any Deserialize type
    pub fn get_json<T>(&self, key: impl AsRef<Path>) -> crate::Result<T>
    where
        T: for<'de> serde::Deserialize<'de>,
    {
        let path_str = key.as_ref().as_str();

        // For simple keys (no dots), delegate to Node's get_json
        if !path_str.contains('.') {
            self.root.get_json(path_str)
        } else {
            // For paths, get the value and try to deserialize as JSON
            let key_ref = key.as_ref();
            let value = self
                .get(key_ref)
                .ok_or_else(|| CRDTError::ElementNotFound {
                    key: path_str.to_string(),
                })?;

            match value {
                Value::Text(json) => serde_json::from_str(json).map_err(|e| {
                    CRDTError::DeserializationFailed {
                        reason: format!(
                            "Failed to deserialize JSON for path '{}': {}",
                            path_str, e
                        ),
                    }
                    .into()
                }),
                _ => Err(CRDTError::TypeMismatch {
                    expected: "JSON string".to_string(),
                    actual: format!("{:?}", value),
                }
                .into()),
            }
        }
    }

    /// Set a key-value pair where the value is a string
    pub fn set_string(&mut self, key: impl AsRef<Path>, value: impl Into<String>) -> &mut Self {
        self.set(key, Value::Text(value.into()));
        self
    }

    /// Set a key-value pair where the value is a nested node
    pub fn set_node(&mut self, key: impl AsRef<Path>, value: impl Into<Node>) -> &mut Self {
        self.set(key, Value::Node(value.into()));
        self
    }

    /// Get a mutable reference to a nested node by key
    pub fn get_node_mut(&mut self, key: impl AsRef<Path>) -> Option<&mut Node> {
        match self.get_mut(key)? {
            Value::Node(node) => Some(node),
            _ => None,
        }
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
    pub fn get_or_insert(
        &mut self,
        key: impl AsRef<Path> + Clone,
        default: impl Into<Value>,
    ) -> &mut Value {
        if !self.contains_key(key.clone()) {
            self.set(key.clone(), default);
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
    pub fn modify<T, F>(&mut self, key: impl AsRef<Path> + Clone, f: F) -> crate::Result<()>
    where
        T: for<'a> TryFrom<&'a Value, Error = CRDTError> + Into<Value>,
        F: FnOnce(&mut T),
    {
        // Try to get and convert the current value
        let mut value = self.get_as::<T>(key.clone())?;

        // Apply the modification
        f(&mut value);

        // Store the modified value back
        self.set(key, value);
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
