//! Value types for CRDT documents.
//!
//! This module provides the Value enum that represents all possible values
//! that can be stored within a CRDT document. Values can be either leaf values
//! (primitives like integers, strings, booleans) or branch values (nested
//! structures like nodes and lists).

use std::fmt;

use crate::crdt::CRDTError;
use crate::crdt::traits::Data;

// Forward declarations for types defined in other modules
use super::list::List;
use super::node::Node;

/// Values that can be stored in CRDT documents.
///
/// `Value` represents all possible data types that can be stored within
/// a CRDT document structure. Values can be either leaf values (terminal data)
/// or branch values (containing other structures).
///
/// # Value Types
///
/// ## Leaf Values (Terminal Nodes)
/// - [`Value::Null`] - Represents null/empty values
/// - [`Value::Bool`] - Boolean values (true/false)
/// - [`Value::Int`] - 64-bit signed integers
/// - [`Value::Text`] - UTF-8 text strings
///
/// ## Branch Values (Container Nodes)
/// - [`Value::Node`] - Nested document structures
/// - [`Value::List`] - Ordered collections with stable positioning
///
/// ## CRDT Semantics
/// - [`Value::Deleted`] - Tombstone marker for deleted values
///
/// # Direct Comparisons
///
/// `Value` implements `PartialEq` with primitive types for ergonomic comparisons:
///
/// ```
/// # use eidetica::crdt::doc::Value;
/// let text = Value::Text("hello".to_string());
/// let number = Value::Int(42);
/// let flag = Value::Bool(true);
///
/// // Direct comparison with primitives
/// assert!(text == "hello");
/// assert!(number == 42);
/// assert!(flag == true);
///
/// // Reverse comparisons also work
/// assert!("hello" == text);
/// assert!(42 == number);
/// assert!(true == flag);
///
/// // Type mismatches return false
/// assert!(!(text == 42));
/// assert!(!(number == "hello"));
/// ```
///
/// # CRDT Merge Behavior
///
/// - **Leaf values**: Last-write-wins semantics
/// - **Branch values**: Structural merging (recursive for Node, positional for List)
/// - **Tombstones**: Deletion markers that win over any non-deleted value
/// - **Resurrection**: Non-deleted values can overwrite tombstones
///
/// ```
/// # use eidetica::crdt::doc::Value;
/// let mut val1 = Value::Int(42);
/// let val2 = Value::Int(100);
/// val1.merge(&val2);  // val1 becomes 100 (last-write-wins)
///
/// let mut val3 = Value::Text("hello".to_string());
/// let deleted = Value::Deleted;
/// val3.merge(&deleted);  // val3 becomes Deleted (tombstone wins)
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Value {
    // Leaf values (terminal nodes)
    /// Null/empty value
    Null,
    /// Boolean value
    Bool(bool),
    /// Integer value
    Int(i64),
    /// Text string value
    Text(String),

    // Branch values (can contain other nodes)
    /// Sub-tree containing other nodes
    Node(Node),
    /// Ordered collection of values
    List(List),

    // CRDT semantics
    /// Tombstone marker for deleted values
    Deleted,
}

impl Value {
    /// Returns true if this is a leaf value (terminal node)
    pub fn is_leaf(&self) -> bool {
        matches!(
            self,
            Value::Null | Value::Bool(_) | Value::Int(_) | Value::Text(_) | Value::Deleted
        )
    }

    /// Returns true if this is a branch value (can contain other nodes)
    pub fn is_branch(&self) -> bool {
        matches!(self, Value::Node(_) | Value::List(_))
    }

    /// Returns true if this value represents a deletion
    pub fn is_deleted(&self) -> bool {
        matches!(self, Value::Deleted)
    }

    /// Returns true if this is a null value
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Returns the type name as a string
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Text(_) => "text",
            Value::Node(_) => "node",
            Value::List(_) => "list",
            Value::Deleted => "deleted",
        }
    }

    /// Attempts to convert to a boolean
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Attempts to convert to a boolean, returning default if not a bool
    pub fn as_bool_or(&self, default: bool) -> bool {
        self.as_bool().unwrap_or(default)
    }

    /// Attempts to convert to a boolean, returning false if not a bool
    pub fn as_bool_or_false(&self) -> bool {
        self.as_bool().unwrap_or(false)
    }

    /// Attempts to convert to an integer
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// Attempts to convert to an integer, returning default if not an int
    pub fn as_int_or(&self, default: i64) -> i64 {
        self.as_int().unwrap_or(default)
    }

    /// Attempts to convert to an integer, returning 0 if not an int
    pub fn as_int_or_zero(&self) -> i64 {
        self.as_int().unwrap_or(0)
    }

    /// Attempts to convert to a string
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Attempts to convert to a string, returning empty string if not text
    pub fn as_text_or_empty(&self) -> &str {
        self.as_text().unwrap_or("")
    }

    /// Attempts to convert to a node (returns immutable reference)
    pub fn as_node(&self) -> Option<&Node> {
        match self {
            Value::Node(node) => Some(node),
            _ => None,
        }
    }

    /// Attempts to convert to a mutable node reference
    pub fn as_node_mut(&mut self) -> Option<&mut Node> {
        match self {
            Value::Node(node) => Some(node),
            _ => None,
        }
    }

    /// Attempts to convert to a list (returns immutable reference)
    pub fn as_list(&self) -> Option<&List> {
        match self {
            Value::List(list) => Some(list),
            _ => None,
        }
    }

    /// Attempts to convert to a mutable list reference
    pub fn as_list_mut(&mut self) -> Option<&mut List> {
        match self {
            Value::List(list) => Some(list),
            _ => None,
        }
    }

    /// Merges another Value into this one (CRDT merge operation)
    pub fn merge(&mut self, other: &Value) {
        if matches!(self, Value::Deleted) {
            // If self is deleted, other value wins (resurrection)
            *self = other.clone();
            return;
        }

        if matches!(other, Value::Deleted) {
            // If other is deleted, the tombstone wins (deletion)
            *self = Value::Deleted;
            return;
        }

        // Handle specific cases without moving self
        match other {
            Value::Node(other_node) => {
                if let Value::Node(self_node) = self {
                    // For in-place merge, use the Node's merge method
                    self_node.merge_in_place(other_node);
                } else {
                    // Different types, replace with other
                    *self = other.clone();
                }
            }
            Value::List(other_list) => {
                if let Value::List(self_list) = self {
                    self_list.merge(other_list);
                } else {
                    // Different types, replace with other
                    *self = other.clone();
                }
            }
            _ => {
                // For leaf values, implement last-write-wins
                *self = other.clone();
            }
        }
    }

    /// Converts to a JSON-like string representation for human-readable output.
    ///
    /// This method produces clean JSON output intended for display, debugging, and export.
    /// It differs from serde serialization in important ways:
    ///
    /// - **Tombstones**: Deleted values appear as `null` instead of being preserved as tombstones
    /// - **Purpose**: Human-readable output, not CRDT state preservation
    /// - **Use cases**: Display, debugging, export to external systems
    ///
    /// For complete CRDT state preservation including tombstones, use serde serialization instead.
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::doc::Value;
    /// let value = Value::Text("hello".to_string());
    /// assert_eq!(value.to_json_string(), "\"hello\"");
    ///
    /// let deleted = Value::Deleted;
    /// assert_eq!(deleted.to_json_string(), "null"); // Tombstones become null
    /// ```
    pub fn to_json_string(&self) -> String {
        match self {
            Value::Null => "null".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Int(n) => n.to_string(),
            Value::Text(s) => format!("\"{}\"", s.replace('\"', "\\\"")),
            Value::Node(node) => node.to_json_string(),
            Value::List(list) => {
                let mut result = String::with_capacity(list.len() * 8); // Reasonable initial capacity
                result.push('[');
                for (i, item) in list.iter().enumerate() {
                    if i > 0 {
                        result.push(',');
                    }
                    result.push_str(&item.to_json_string());
                }
                result.push(']');
                result
            }
            Value::Deleted => "null".to_string(), // Deleted values appear as null
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "null"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(n) => write!(f, "{n}"),
            Value::Text(s) => write!(f, "{s}"),
            Value::Node(node) => write!(f, "{node}"),
            Value::List(list) => {
                write!(f, "[")?;
                for (i, item) in list.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "]")
            }
            Value::Deleted => write!(f, "<deleted>"),
        }
    }
}

// Convenient From implementations for common types
impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Value::Bool(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Value::Int(value)
    }
}

impl From<u64> for Value {
    fn from(value: u64) -> Self {
        // Convert to i64, clamping if necessary
        Value::Int(value as i64)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        // Convert to i64, truncating the fractional part
        Value::Int(value as i64)
    }
}

impl From<i32> for Value {
    fn from(value: i32) -> Self {
        Value::Int(value as i64)
    }
}

impl From<u32> for Value {
    fn from(value: u32) -> Self {
        Value::Int(value as i64)
    }
}

impl From<f32> for Value {
    fn from(value: f32) -> Self {
        Value::Int(value as i64)
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Value::Text(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Value::Text(value.to_string())
    }
}

impl From<Node> for Value {
    fn from(value: Node) -> Self {
        Value::Node(value)
    }
}

impl From<List> for Value {
    fn from(value: List) -> Self {
        Value::List(value)
    }
}

// Note: Import Doc here to avoid circular dependencies
impl From<crate::crdt::Doc> for Value {
    fn from(doc: crate::crdt::Doc) -> Self {
        // Convert Doc to Node by extracting its root
        Value::Node(doc.into())
    }
}

// TryFrom implementations for better type coercion
impl TryFrom<&Value> for String {
    type Error = CRDTError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        match value {
            Value::Text(s) => Ok(s.clone()),
            _ => Err(CRDTError::TypeMismatch {
                expected: "String".to_string(),
                actual: format!("{value:?}"),
            }),
        }
    }
}

// Note: &str TryFrom is tricky due to lifetimes - users should use String or the existing as_text() method

impl TryFrom<&Value> for i64 {
    type Error = CRDTError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        match value {
            Value::Int(n) => Ok(*n),
            _ => Err(CRDTError::TypeMismatch {
                expected: "i64".to_string(),
                actual: format!("{value:?}"),
            }),
        }
    }
}

impl TryFrom<&Value> for bool {
    type Error = CRDTError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        match value {
            Value::Bool(b) => Ok(*b),
            _ => Err(CRDTError::TypeMismatch {
                expected: "bool".to_string(),
                actual: format!("{value:?}"),
            }),
        }
    }
}

// Note: Reference types (&Node, &List) have lifetime issues with TryFrom
// Users should use the existing as_node() and as_list() methods for references
// Or clone into owned types when needed

impl TryFrom<&Value> for Node {
    type Error = CRDTError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        match value {
            Value::Node(node) => Ok(node.clone()),
            _ => Err(CRDTError::TypeMismatch {
                expected: "Node".to_string(),
                actual: format!("{value:?}"),
            }),
        }
    }
}

impl TryFrom<&Value> for List {
    type Error = CRDTError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        match value {
            Value::List(list) => Ok(list.clone()),
            _ => Err(CRDTError::TypeMismatch {
                expected: "List".to_string(),
                actual: format!("{value:?}"),
            }),
        }
    }
}

// PartialEq implementations for comparing Value with other types
impl PartialEq<str> for Value {
    fn eq(&self, other: &str) -> bool {
        match self {
            Value::Text(s) => s == other,
            _ => false,
        }
    }
}

impl PartialEq<&str> for Value {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

impl PartialEq<String> for Value {
    fn eq(&self, other: &String) -> bool {
        match self {
            Value::Text(s) => s == other,
            _ => false,
        }
    }
}

impl PartialEq<i64> for Value {
    fn eq(&self, other: &i64) -> bool {
        match self {
            Value::Int(n) => n == other,
            _ => false,
        }
    }
}

impl PartialEq<i32> for Value {
    fn eq(&self, other: &i32) -> bool {
        match self {
            Value::Int(n) => *n == *other as i64,
            _ => false,
        }
    }
}

impl PartialEq<u32> for Value {
    fn eq(&self, other: &u32) -> bool {
        match self {
            Value::Int(n) => *n == *other as i64,
            _ => false,
        }
    }
}

impl PartialEq<bool> for Value {
    fn eq(&self, other: &bool) -> bool {
        match self {
            Value::Bool(b) => b == other,
            _ => false,
        }
    }
}

// Reverse implementations for symmetry
impl PartialEq<Value> for str {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

impl PartialEq<Value> for &str {
    fn eq(&self, other: &Value) -> bool {
        other == *self
    }
}

impl PartialEq<Value> for String {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

impl PartialEq<Value> for i64 {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

impl PartialEq<Value> for i32 {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

impl PartialEq<Value> for u32 {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

impl PartialEq<Value> for bool {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

// Data trait implementation
impl Data for Value {}
