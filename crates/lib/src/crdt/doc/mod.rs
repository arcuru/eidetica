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
#[cfg(test)]
mod node_tests;
pub mod path;
pub mod value;

// Convenience re-exports for core Doc types
pub use list::List;
pub use path::{Path, PathBuf, PathError};
pub use value::Value;

// Re-export the macro from crate root
pub use crate::path;

/// The main CRDT document type for Eidetica.
///
/// `Doc` provides the primary interface for CRDT document operations,
/// providing a unified tree structure for CRDT operations.
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
/// assert_eq!(doc.get_as::<&str>("name"), Some("Alice"));
/// assert_eq!(doc.get_as::<i64>("age"), Some(30));
/// ```
///
/// ## Path Operations
/// ```
/// # use eidetica::crdt::Doc;
/// # use eidetica::crdt::doc::path;
/// let mut doc = Doc::new();
/// doc.set("user.profile.name", "Alice");
///
/// assert_eq!(doc.get_as::<&str>("user.profile.name"), Some("Alice"));
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
/// assert_eq!(merged.get_as::<&str>("name"), Some("Bob")); // Last write wins
/// assert_eq!(merged.get_as::<i64>("age"), Some(30));        // Preserved from doc1
/// assert_eq!(merged.get_as::<&str>("city"), Some("NYC"));   // Added from doc2
/// ```
/// Current CRDT format version for Doc.
pub const DOC_VERSION: u8 = 0;

/// Helper to check if version is default (0) for serde skip_serializing_if
fn is_v0(v: &u8) -> bool {
    *v == 0
}

/// Validates the Doc version during deserialization.
fn validate_doc_version<'de, D>(deserializer: D) -> std::result::Result<u8, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let version = u8::deserialize(deserializer)?;
    if version != DOC_VERSION {
        return Err(serde::de::Error::custom(format!(
            "unsupported Doc version {version}; only version {DOC_VERSION} is supported"
        )));
    }
    Ok(version)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Doc {
    /// CRDT format version. v0 indicates unstable format.
    #[serde(
        rename = "_v",
        default,
        skip_serializing_if = "is_v0",
        deserialize_with = "validate_doc_version"
    )]
    version: u8,
    /// Child nodes indexed by string keys
    children: HashMap<String, Value>,
}

impl Doc {
    /// Creates a new empty document
    pub fn new() -> Self {
        Self {
            version: DOC_VERSION,
            children: HashMap::new(),
        }
    }

    /// Returns the CRDT format version of this document.
    pub fn version(&self) -> u8 {
        self.version
    }

    /// Returns true if this document has no data (excluding tombstones)
    pub fn is_empty(&self) -> bool {
        self.children.values().all(|v| matches!(v, Value::Deleted))
    }

    /// Returns the number of direct keys (excluding tombstones)
    pub fn len(&self) -> usize {
        self.children
            .values()
            .filter(|v| !matches!(v, Value::Deleted))
            .count()
    }

    /// Returns true if the document contains the given key
    pub fn contains_key(&self, key: impl AsRef<Path>) -> bool {
        let path_buf = PathBuf::from_str(key.as_ref().as_str()).unwrap(); // Infallible
        self.get(&path_buf).is_some()
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
            // For direct keys, check our children directly
            matches!(self.children.get(path_str), Some(Value::Deleted))
        }
    }

    /// Gets a value by key or path (immutable reference)
    pub fn get(&self, key: impl AsRef<Path>) -> Option<&Value> {
        let path = key.as_ref();
        let segments: Vec<_> = path.components().collect();

        if segments.is_empty() {
            return None;
        }

        // Start with the first segment to get into our structure
        let first_segment = segments.first()?;
        let mut current_value = match self.children.get(*first_segment) {
            Some(Value::Deleted) => return None, // Hide tombstones
            value => value?,
        };

        // Navigate through remaining segments
        for segment in &segments[1..] {
            match current_value {
                Value::Doc(doc) => {
                    current_value = match doc.children.get(*segment) {
                        Some(Value::Deleted) => return None, // Hide tombstones
                        value => value?,
                    };
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

    /// Gets a mutable reference to a value by key or path
    pub fn get_mut(&mut self, key: impl AsRef<Path>) -> Option<&mut Value> {
        let path = key.as_ref();
        let segments: Vec<_> = path.components().collect();

        if segments.is_empty() {
            return None;
        }

        let mut current = self;

        // Navigate to the parent of the target
        for segment in &segments[..segments.len() - 1] {
            match current.children.get_mut(*segment) {
                Some(Value::Doc(doc)) => {
                    // Now Value::Doc contains a Doc, so we can navigate
                    current = doc;
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

    /// Gets a value by key with automatic type conversion using TryFrom
    ///
    /// Returns Some(T) if the value exists and can be converted to type T.
    /// Returns None if the key doesn't exist or type conversion fails.
    ///
    /// This is the recommended method for type-safe value retrieval as it provides
    /// a cleaner Option-based interface compared to the Result-based `get_as()` method.
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::Doc;
    /// let mut doc = Doc::new();
    /// doc.set("name", "Alice");
    /// doc.set("age", 30);
    /// doc.set("active", true);
    ///
    /// // Returns Some when value exists and type matches
    /// assert_eq!(doc.get_as::<&str>("name"), Some("Alice"));
    /// assert_eq!(doc.get_as::<i64>("age"), Some(30));
    /// assert_eq!(doc.get_as::<bool>("active"), Some(true));
    ///
    /// // Returns None when key doesn't exist
    /// assert_eq!(doc.get_as::<String>("missing"), None);
    ///
    /// // Returns None when type doesn't match
    /// assert_eq!(doc.get_as::<i64>("name"), None);
    /// ```
    pub fn get_as<'a, T>(&'a self, key: impl AsRef<Path>) -> Option<T>
    where
        T: TryFrom<&'a Value, Error = CRDTError>,
    {
        let value = self.get(key)?;
        T::try_from(value).ok()
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
            let old = self.children.insert(path_str.to_string(), value.into());
            match old {
                Some(Value::Deleted) => None, // Don't return tombstones
                value => value,
            }
        } else {
            // For paths, use set_path - errors should be handled by caller
            // but we return None for this non-Result interface
            // Path operations can fail (e.g., trying to set through scalar)
            // For this Option-returning interface, we return None
            // Use try_set() for Result-based error handling
            self.set_path(&path_buf, value).unwrap_or_default()
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
            let old = self.children.insert(path_str.to_string(), value.into());
            Ok(match old {
                Some(Value::Deleted) => None, // Don't return tombstones
                value => value,
            })
        } else {
            // For paths, use set_path and return the Result
            self.set_path(&path_buf, value).map_err(Into::into)
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
                .or_insert_with(|| Value::Doc(Doc::new()));
            match entry {
                Value::Doc(doc) => {
                    current = doc;
                }
                Value::Deleted => {
                    // Replace tombstone with new node
                    *entry = Value::Doc(Doc::new());
                    match entry {
                        Value::Doc(doc) => current = doc,
                        _ => unreachable!(),
                    }
                }
                _ => {
                    // Replace scalar value with new node to allow navigation
                    *entry = Value::Doc(Doc::new());
                    match entry {
                        Value::Doc(doc) => current = doc,
                        _ => unreachable!(),
                    }
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

    /// Removes a value by key or path, returns the old value if present.
    ///
    /// This method implements CRDT semantics by creating a tombstone marker.
    /// For paths with dots, this removes the value at the nested location.
    pub fn remove(&mut self, key: impl AsRef<Path>) -> Option<Value> {
        let path_str = key.as_ref().as_str();

        // For simple keys (no dots), use direct remove
        if !path_str.contains('.') {
            let key = path_str.to_string();
            let old_value = self.children.get(&key).cloned();
            self.children.insert(key, Value::Deleted);
            match old_value {
                Some(Value::Deleted) => None, // Don't return tombstones
                value => value,
            }
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
            self.remove(path_str).is_some()
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
    /// let mut doc = Doc::new();
    /// doc.set("user.profile.name", "Alice");
    /// doc.set("user.profile.age", 30);
    ///
    /// // Type inference with path access
    /// let name = doc.get_as::<String>("user.profile.name");
    /// let age = doc.get_as::<i64>("user.profile.age");
    ///
    /// assert_eq!(name, Some("Alice".to_string()));
    /// assert_eq!(age, Some(30));
    /// ```
    /// Returns an iterator over all key-value pairs (excluding tombstones)
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.children
            .iter()
            .filter(|(_, v)| !matches!(v, Value::Deleted))
    }

    /// Returns a mutable iterator over all key-value pairs (excluding tombstones)
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut Value)> {
        self.children.iter_mut()
    }

    /// Returns an iterator over all keys (excluding tombstones)
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.children
            .iter()
            .filter(|(_, v)| !matches!(v, Value::Deleted))
            .map(|(k, _)| k)
    }

    /// Returns an iterator over all values (excluding tombstones)
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.children
            .values()
            .filter(|v| !matches!(v, Value::Deleted))
    }

    /// Returns a mutable iterator over all values (excluding tombstones)
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Value> {
        self.children.values_mut()
    }

    /// Clears all data from this document
    pub fn clear(&mut self) {
        let keys: Vec<_> = self.children.keys().cloned().collect();
        for key in keys {
            self.children.insert(key, Value::Deleted);
        }
    }

    /// Converts to a JSON-like string representation for human-readable output.
    ///
    /// See [`Doc::to_json_string`] for detailed documentation.
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

impl CRDT for Doc {
    /// Merges another document into this one using CRDT semantics.
    ///
    /// This implements the core CRDT merge operation at the document level,
    /// providing deterministic conflict resolution following CRDT semantics.
    ///
    /// Merge combines the key-value pairs from both documents, resolving
    /// conflicts deterministically using CRDT rules.
    fn merge(&self, other: &Self) -> crate::Result<Self> {
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

impl Default for Doc {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Doc {
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

impl FromIterator<(String, Value)> for Doc {
    fn from_iter<T: IntoIterator<Item = (String, Value)>>(iter: T) -> Self {
        let mut doc = Doc::new();
        for (key, value) in iter {
            doc.set(key, value);
        }
        doc
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

    /// Builder method to set a nested Doc
    pub fn with_doc(self, key: impl AsRef<Path>, value: impl Into<Doc>) -> Self {
        self.with(key, Value::Doc(value.into()))
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

        // For simple keys (no dots), set as JSON string
        if !path_str.contains('.') {
            let json =
                serde_json::to_string(&value).map_err(|e| CRDTError::SerializationFailed {
                    reason: e.to_string(),
                })?;
            self.set(path_str, Value::Text(json));
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

        // For simple keys (no dots), get and deserialize JSON
        if !path_str.contains('.') {
            match self.children.get(path_str) {
                Some(Value::Text(json)) => serde_json::from_str::<T>(json).map_err(|e| {
                    CRDTError::DeserializationFailed {
                        reason: format!("Failed to deserialize JSON for key '{path_str}': {e}"),
                    }
                    .into()
                }),
                Some(Value::Deleted) => Err(CRDTError::ElementNotFound {
                    key: path_str.to_string(),
                }
                .into()),
                Some(other) => Err(CRDTError::TypeMismatch {
                    expected: "Text (JSON string)".to_string(),
                    actual: format!("{other:?}"),
                }
                .into()),
                None => Err(CRDTError::ElementNotFound {
                    key: path_str.to_string(),
                }
                .into()),
            }
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
                        reason: format!("Failed to deserialize JSON for path '{path_str}': {e}"),
                    }
                    .into()
                }),
                _ => Err(CRDTError::TypeMismatch {
                    expected: "JSON string".to_string(),
                    actual: format!("{value:?}"),
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

    /// Set a key-value pair where the value is a nested Doc
    pub fn set_doc(&mut self, key: impl AsRef<Path>, value: Doc) -> &mut Self {
        self.set(key, Value::Doc(value));
        self
    }

    /// Get a reference to a nested Doc by key
    pub fn get_doc(&self, key: impl AsRef<Path>) -> Option<&Doc> {
        match self.get(key)? {
            Value::Doc(node) => Some(node),
            _ => None,
        }
    }

    /// Get a mutable reference to a nested Doc by key
    pub fn get_doc_mut(&mut self, key: impl AsRef<Path>) -> Option<&mut Doc> {
        match self.get_mut(key)? {
            Value::Doc(node) => Some(node),
            _ => None,
        }
    }

    /// Get a reference to the internal HashMap for advanced access
    pub fn as_hashmap(&self) -> &HashMap<String, Value> {
        &self.children
    }

    /// Get a mutable reference to the internal HashMap for advanced access
    pub fn as_hashmap_mut(&mut self) -> &mut HashMap<String, Value> {
        &mut self.children
    }

    /// List operations compatibility methods
    pub fn list_add<K>(&mut self, key: K, value: Value) -> crate::Result<String>
    where
        K: Into<String>,
    {
        let key = key.into();

        // Get or create the list
        let list = match self.children.get_mut(&key) {
            Some(Value::List(list)) => list,
            Some(Value::Deleted) => {
                // Replace tombstone with new list
                let mut new_list = List::new();
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
                let mut new_list = List::new();
                let index = new_list.push(value);
                self.children.insert(key, Value::List(new_list));
                return Ok(index.to_string());
            }
        };

        Ok(list.push(value).to_string())
    }

    /// List remove operation - tombstones element by position ID
    pub fn list_remove<K>(&mut self, key: K, id: &str) -> crate::Result<bool>
    where
        K: Into<String> + AsRef<str>,
    {
        let index: usize = id.parse().map_err(|_| CRDTError::InvalidPath {
            path: format!("Invalid list index: {id}"),
        })?;

        match self.children.get_mut(key.as_ref()) {
            Some(Value::List(list)) => Ok(list.remove(index).is_some()),
            Some(_) => Err(CRDTError::TypeMismatch {
                expected: "List".to_string(),
                actual: "other type".to_string(),
            }
            .into()),
            None => Ok(false), // Key doesn't exist
        }
    }

    /// Get an element by its ID from a list
    pub fn list_get<K>(&self, key: K, id: &str) -> Option<&Value>
    where
        K: AsRef<str>,
    {
        let index: usize = id.parse().ok()?;
        match self.children.get(key.as_ref()) {
            Some(Value::List(list)) => list.get(index),
            Some(Value::Deleted) => None, // Hide tombstones
            _ => None,
        }
    }

    /// Get all element IDs from a list in order
    pub fn list_ids<K>(&self, key: K) -> Vec<String>
    where
        K: AsRef<str>,
    {
        match self.children.get(key.as_ref()) {
            Some(Value::List(list)) => {
                // Return index-based IDs as strings
                (0..list.len()).map(|i| i.to_string()).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Get list length (excluding tombstones)
    pub fn list_len<K>(&self, key: K) -> usize
    where
        K: AsRef<str>,
    {
        match self.children.get(key.as_ref()) {
            Some(Value::List(list)) => list.len(),
            _ => 0,
        }
    }

    /// Check if list is empty
    pub fn list_is_empty<K>(&self, key: K) -> bool
    where
        K: AsRef<str>,
    {
        match self.children.get(key.as_ref()) {
            Some(Value::List(list)) => list.is_empty(),
            _ => true, // Non-existent lists are considered empty
        }
    }

    /// Clear list
    pub fn list_clear<K>(&mut self, key: K) -> crate::Result<()>
    where
        K: AsRef<str>,
    {
        match self.children.get_mut(key.as_ref()) {
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
    /// assert_eq!(doc.get_as::<i64>("count"), Some(15));
    ///
    /// // Modify string
    /// doc.modify::<String, _>("text", |text| {
    ///     text.push_str(" world");
    /// })?;
    /// assert_eq!(doc.get_as::<String>("text"), Some("hello world".to_string()));
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn modify<T, F>(&mut self, key: impl AsRef<Path> + Clone, f: F) -> crate::Result<()>
    where
        T: for<'a> TryFrom<&'a Value, Error = CRDTError> + Into<Value>,
        F: FnOnce(&mut T),
    {
        // Try to get and convert the current value
        let mut value = self.get_as::<T>(key.clone()).ok_or_else(|| {
            crate::Error::CRDT(CRDTError::ElementNotFound {
                key: key.as_ref().as_str().to_string(),
            })
        })?;

        // Apply the modification
        f(&mut value);

        // Store the modified value back
        self.set(key, value);
        Ok(())
    }
}

// Conversion implementations
// Node is now an alias for Doc, so no conversion implementations needed
// The standard From<T> for T implementation in core handles this automatically

// Data trait implementation
impl Data for Doc {}
