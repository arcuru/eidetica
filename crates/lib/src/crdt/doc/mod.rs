//! Document CRDT
//!
//! This module provides the main public interface for a CRDT "Document" in Eidetica.
//! The [`Doc`] type serves as the primary entry point for accessing and editing it.
//! It is a json-ish nested type.
//!
//! # Usage
//!
//! ```
//! use eidetica::crdt::{Doc, traits::CRDT};
//!
//! let mut doc = Doc::new();
//! doc.set("name", "Alice");
//! doc.set("age", 30);
//! doc.set("user.profile.bio", "Software developer"); // Creates nested structure
//!
//! // Type-safe retrieval
//! let name: Option<&str> = doc.get_as("name");
//! let age: Option<i64> = doc.get_as("age");
//!
//! // Merge with another document
//! let mut doc2 = Doc::new();
//! doc2.set("name", "Bob");
//! doc2.set("city", "New York");
//!
//! let merged = doc.merge(&doc2).unwrap();
//! ```

use std::{collections::HashMap, fmt};

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
/// `Doc` is a hierarchical key-value store with Last-Write-Wins (LWW) merge semantics.
/// Keys can be simple strings or dot-separated paths for nested access.
///
/// # Examples
///
/// ```
/// # use eidetica::crdt::Doc;
/// let mut doc = Doc::new();
///
/// // Simple key-value
/// doc.set("name", "Alice");
/// doc.set("age", 30);
///
/// // Nested paths (creates intermediate Doc nodes automatically)
/// doc.set("user.profile.bio", "Developer");
///
/// // Type-safe retrieval
/// assert_eq!(doc.get_as::<&str>("name"), Some("Alice"));
/// assert_eq!(doc.get_as::<i64>("age"), Some(30));
/// assert_eq!(doc.get_as::<&str>("user.profile.bio"), Some("Developer"));
/// ```
///
/// # CRDT Merging
///
/// ```
/// # use eidetica::crdt::{Doc, traits::CRDT};
/// let mut doc1 = Doc::new();
/// doc1.set("name", "Alice");
///
/// let mut doc2 = Doc::new();
/// doc2.set("name", "Bob");
/// doc2.set("city", "NYC");
///
/// let merged = doc1.merge(&doc2).unwrap();
/// assert_eq!(merged.get_as::<&str>("name"), Some("Bob")); // Last write wins
/// assert_eq!(merged.get_as::<&str>("city"), Some("NYC")); // Added from doc2
/// ```
/// Current CRDT format version for Doc.
pub const DOC_VERSION: u8 = 0;

/// Helper to check if version is default (0) for serde skip_serializing_if
fn is_v0(v: &u8) -> bool {
    *v == 0
}

/// Helper for serde skip_serializing_if on bool fields
fn is_false(v: &bool) -> bool {
    !v
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
    /// When true, the document merges as a single unit (LWW) rather than
    /// recursively merging individual fields.
    #[serde(rename = "_a", default, skip_serializing_if = "is_false")]
    atomic: bool,
    /// Child nodes indexed by string keys
    children: HashMap<String, Value>,
}

impl Doc {
    /// Creates a new empty document.
    pub fn new() -> Self {
        Self {
            version: DOC_VERSION,
            atomic: false,
            children: HashMap::new(),
        }
    }

    /// Creates a new empty atomic document.
    ///
    /// The atomic flag means "this data is a complete replacement — take all of
    /// it." During merge (left ⊕ right):
    ///
    /// - `right.atomic` → LWW: return right (always replaces left entirely)
    /// - `left.atomic`, `!right.atomic` → structural field merge, result stays
    ///   atomic (the flag is **contagious**)
    /// - Neither atomic → structural field merge, result non-atomic
    ///
    /// The contagious property preserves associativity: in a chain `1⊕2⊕3⊕4`
    /// where 3 is atomic and 4 edits subfields, `3⊕4` produces an atomic
    /// result, so `(1⊕2) ⊕ (3⊕4)` correctly overwrites everything before 3.
    ///
    /// Use this for config or metadata that must always be written as a
    /// consistent whole. Types that need atomicity should convert into
    /// `Doc::atomic()` (e.g., via `impl From<MyType> for Doc`) to declare
    /// replacement semantics.
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::{Doc, traits::CRDT};
    /// let mut doc1 = Doc::atomic();
    /// doc1.set("x", 1);
    /// doc1.set("y", 2);
    ///
    /// let mut doc2 = Doc::atomic();
    /// doc2.set("x", 10);
    /// doc2.set("z", 30);
    ///
    /// // Atomic merge replaces entirely (LWW), no field-level merge
    /// let merged = doc1.merge(&doc2).unwrap();
    /// assert_eq!(merged.get_as::<i64>("x"), Some(10));
    /// assert_eq!(merged.get_as::<i64>("z"), Some(30));
    /// assert_eq!(merged.get_as::<i64>("y"), None); // Not carried from doc1
    /// ```
    pub fn atomic() -> Self {
        Self {
            version: DOC_VERSION,
            atomic: true,
            children: HashMap::new(),
        }
    }

    /// Returns true if this document uses atomic merge semantics.
    pub fn is_atomic(&self) -> bool {
        self.atomic
    }

    /// Returns true if this document has no data (excluding tombstones).
    pub fn is_empty(&self) -> bool {
        self.children.values().all(|v| matches!(v, Value::Deleted))
    }

    /// Returns the number of direct keys (excluding tombstones).
    pub fn len(&self) -> usize {
        self.children
            .values()
            .filter(|v| !matches!(v, Value::Deleted))
            .count()
    }

    /// Returns true if the document contains the given key.
    pub fn contains_key(&self, key: impl AsRef<Path>) -> bool {
        self.get(key).is_some()
    }

    /// Returns true if the exact path points to a tombstone (Value::Deleted).
    ///
    /// This method checks if the specific key has been deleted. Note that this
    /// only returns true if the exact path is a tombstone - it does not check
    /// if an ancestor was deleted (which would make the path inaccessible).
    ///
    /// To check if a path is inaccessible (either deleted or has a deleted ancestor),
    /// use `get(path).is_none()` instead.
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::Doc;
    /// let mut doc = Doc::new();
    /// doc.set("user.profile.name", "Alice");
    /// doc.remove("user.profile.name");
    ///
    /// assert!(doc.is_tombstone("user.profile.name"));
    /// assert!(!doc.is_tombstone("user.profile")); // parent is not tombstoned
    ///
    /// // Deleting a parent makes children inaccessible but not directly tombstoned
    /// doc.set("settings.theme.color", "blue");
    /// doc.remove("settings.theme");
    /// assert!(doc.is_tombstone("settings.theme")); // exact path is tombstoned
    /// assert!(!doc.is_tombstone("settings.theme.color")); // child path is NOT a tombstone
    /// assert!(doc.get("settings.theme.color").is_none()); // but it's still inaccessible
    /// ```
    pub fn is_tombstone(&self, key: impl AsRef<Path>) -> bool {
        matches!(self.get_raw(key), Some(Value::Deleted))
    }

    /// Gets a value by key or path without filtering tombstones.
    fn get_raw(&self, key: impl AsRef<Path>) -> Option<&Value> {
        let path = key.as_ref();
        let path_str: &str = path.as_ref();

        // For simple keys (no dots), use direct access
        // This handles empty keys ("") and regular simple keys ("foo")
        if !path_str.contains('.') {
            return self.children.get(path_str);
        }

        // For paths with dots, use components (which filters empty strings)
        let segments: Vec<_> = path.components().collect();

        if segments.is_empty() {
            return None;
        }

        let first_segment = segments.first()?;
        let mut current_value = self.children.get(*first_segment)?;

        for segment in &segments[1..] {
            match current_value {
                Value::Doc(doc) => {
                    current_value = doc.children.get(*segment)?;
                }
                Value::List(list) => {
                    // Try to parse segment as list index
                    let index: usize = segment.parse().ok()?;
                    current_value = list.get(index)?;
                }
                // Can't navigate through Deleted, scalars, etc.
                _ => return None,
            }
        }

        Some(current_value)
    }

    /// Gets a value by key or path (immutable reference).
    ///
    /// Supports both simple keys and dot-separated paths for nested access.
    /// Returns `None` if the key doesn't exist or has been deleted (tombstone).
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::Doc;
    /// let mut doc = Doc::new();
    /// doc.set("name", "Alice");
    /// doc.set("user.profile.age", 30);
    ///
    /// assert!(doc.get("name").is_some());
    /// assert!(doc.get("user.profile.age").is_some());
    /// assert!(doc.get("nonexistent").is_none());
    ///
    /// // Deleted keys return None
    /// doc.remove("name");
    /// assert!(doc.get("name").is_none());
    /// ```
    pub fn get(&self, key: impl AsRef<Path>) -> Option<&Value> {
        match self.get_raw(key) {
            Some(Value::Deleted) => None,
            value => value,
        }
    }

    /// Gets a mutable reference to a value by key or path
    pub fn get_mut(&mut self, key: impl AsRef<Path>) -> Option<&mut Value> {
        let path = key.as_ref();
        let path_str: &str = path.as_ref();

        // For simple keys (no dots), use direct access
        // This handles empty keys ("") and regular simple keys ("foo")
        if !path_str.contains('.') {
            return match self.children.get_mut(path_str) {
                Some(Value::Deleted) => None, // Hide tombstones
                value => value,
            };
        }

        // For paths with dots, use components (which filters empty strings)
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

    /// Sets a value at the given key or path, returns the old value if present.
    ///
    /// This method automatically creates intermediate `Doc` nodes for nested paths.
    /// For example, `doc.set("a.b.c", value)` will create `a` and `b` as `Doc` nodes
    /// if they don't exist.
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::Doc;
    /// let mut doc = Doc::new();
    ///
    /// // Simple key
    /// doc.set("name", "Alice");
    ///
    /// // Nested path - creates intermediate nodes automatically
    /// doc.set("user.profile.age", 30);
    ///
    /// assert_eq!(doc.get_as("name"), Some("Alice"));
    /// assert_eq!(doc.get_as("user.profile.age"), Some(30));
    /// ```
    pub fn set(&mut self, key: impl AsRef<Path>, value: impl Into<Value>) -> Option<Value> {
        let path = key.as_ref();
        let path_str: &str = path.as_ref();

        // For simple keys (no dots), use direct assignment
        // This handles empty keys ("") and regular simple keys ("foo")
        if !path_str.contains('.') {
            let old = self.children.insert(path_str.to_string(), value.into());
            return match old {
                Some(Value::Deleted) => None, // Don't return tombstones
                v => v,
            };
        }

        // For paths with dots, use components (which filters empty strings)
        let segments: Vec<_> = path.components().collect();

        if segments.is_empty() {
            return None;
        }

        // Single segment after filtering - direct assignment
        if segments.len() == 1 {
            let old = self.children.insert(segments[0].to_string(), value.into());
            return match old {
                Some(Value::Deleted) => None, // Don't return tombstones
                v => v,
            };
        }

        // Navigate to the parent, creating intermediate nodes as needed
        let mut current = self;
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

        match old {
            Some(Value::Deleted) => None, // Don't return tombstones
            v => v,
        }
    }

    /// Removes a value by key or path, returns the old value if present.
    ///
    /// This method implements CRDT semantics by always creating a tombstone marker.
    /// For nested paths, intermediate Doc nodes are created if they don't exist.
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::Doc;
    /// let mut doc = Doc::new();
    /// doc.set("user.profile.name", "Alice");
    ///
    /// let old = doc.remove("user.profile.name");
    /// assert_eq!(old.and_then(|v| v.as_text().map(|s| s.to_string())), Some("Alice".to_string()));
    /// assert!(doc.get("user.profile.name").is_none());
    /// ```
    pub fn remove(&mut self, key: impl AsRef<Path>) -> Option<Value> {
        // Delegate to set with Value::Deleted
        self.set(key, Value::Deleted)
    }

    /// Returns an iterator over all key-value pairs (excluding tombstones)
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.children
            .iter()
            .filter(|(_, v)| !matches!(v, Value::Deleted))
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

    /// Converts this Doc to a JSON string representation.
    ///
    /// This produces a valid JSON object string from the document's contents,
    /// excluding tombstones.
    pub fn to_json_string(&self) -> String {
        // 64-byte preallocated buffer to avoid reallocs for simple cases
        let mut result = String::with_capacity(64);
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
        if other.atomic {
            return Ok(other.clone());
        }
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

// JSON serialization methods
impl Doc {
    /// Set a key-value pair with automatic JSON serialization for any Serialize type.
    ///
    /// The value is serialized to a JSON string and stored as `Value::Text`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::Doc;
    /// use serde::{Serialize, Deserialize};
    ///
    /// #[derive(Serialize, Deserialize, PartialEq, Debug)]
    /// struct User { name: String, age: i32 }
    ///
    /// let mut doc = Doc::new();
    /// doc.set_json("user", User { name: "Alice".into(), age: 30 })?;
    ///
    /// let user: User = doc.get_json("user")?;
    /// assert_eq!(user, User { name: "Alice".into(), age: 30 });
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn set_json<T>(&mut self, key: impl AsRef<Path>, value: T) -> crate::Result<&mut Self>
    where
        T: serde::Serialize,
    {
        let json = serde_json::to_string(&value).map_err(|e| CRDTError::SerializationFailed {
            reason: e.to_string(),
        })?;
        self.set(key, Value::Text(json));
        Ok(self)
    }

    /// Get a value by key with automatic JSON deserialization for any Deserialize type.
    ///
    /// The value must be a `Value::Text` containing valid JSON.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The key doesn't exist
    /// - The value is not a `Value::Text`
    /// - The JSON deserialization fails
    pub fn get_json<T>(&self, key: impl AsRef<Path>) -> crate::Result<T>
    where
        T: for<'de> serde::Deserialize<'de>,
    {
        let path_str = key.as_ref().as_str().to_string();
        let value = self.get(key).ok_or_else(|| CRDTError::ElementNotFound {
            key: path_str.clone(),
        })?;

        match value {
            Value::Text(json) => serde_json::from_str(json).map_err(|e| {
                CRDTError::DeserializationFailed {
                    reason: format!("Failed to deserialize JSON for key '{path_str}': {e}"),
                }
                .into()
            }),
            _ => Err(CRDTError::TypeMismatch {
                expected: "Text (JSON string)".to_string(),
                actual: value.type_name().to_string(),
            }
            .into()),
        }
    }

    /// Gets or inserts a value with a default, returns a mutable reference.
    ///
    /// If the key doesn't exist, the default value is inserted. Returns a mutable
    /// reference to the value (existing or newly inserted).
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::Doc;
    /// let mut doc = Doc::new();
    ///
    /// // Key doesn't exist - will insert default
    /// doc.get_or_insert("counter", 0);
    /// assert_eq!(doc.get_as::<i64>("counter"), Some(0));
    ///
    /// // Key exists - will keep existing value
    /// doc.set("counter", 5);
    /// doc.get_or_insert("counter", 100);
    /// assert_eq!(doc.get_as::<i64>("counter"), Some(5));
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
}

// Conversion implementations
// Node is now an alias for Doc, so no conversion implementations needed
// The standard From<T> for T implementation in core handles this automatically

// Data trait implementation
impl Data for Doc {}
