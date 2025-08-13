//! Tree-based Map CRDT implementation.
//!
//! This module provides a Map CRDT that aligns with Eidetica's tree-based architecture,
//! replacing the legacy Map implementation with cleaner semantics and better performance.
//! The implementation uses conflict-free replicated data types (CRDTs) to enable
//! distributed collaboration without requiring coordination between nodes.
//!
//! # Core Types
//!
//! - [`Map`] - The root tree structure containing child nodes
//! - [`Value`] - Values that can be stored in tree nodes (leaf or branch values)
//! - [`List`] - Ordered collections with stable positioning using rational numbers
//! - [`list::Position`] - Rational number-based positions for stable list ordering
//!
//! # CRDT Architecture
//!
//! ## Conflict Resolution
//! The Map CRDT implements several conflict resolution strategies:
//! - **Last-write-wins** for scalar values (text, numbers, booleans)
//! - **Structural merging** for nested nodes and lists
//! - **Tombstone deletion** for preserving CRDT merge semantics
//! - **Stable ordering** for lists using rational number positions
//!
//! ## List Ordering with Rational Numbers
//! The [`List`] uses a unique approach to maintain stable ordering across
//! concurrent insertions. Instead of traditional list indices, each list item
//! has a [`list::Position`] containing:
//! - A rational number (numerator/denominator) for ordering
//! - A unique UUID for deterministic tie-breaking
//!
//! This allows insertion between any two existing elements without reordering:
//! ```
//! # use eidetica::crdt::map::{List, list::Position};
//! let mut list = List::new();
//!
//! // Simple index-based operations
//! list.push("first");   // Returns index 0
//! list.push("third");   // Returns index 1
//!
//! // Insert between them using index
//! list.insert(1, "second").unwrap();
//!
//! // List maintains order: ["first", "second", "third"]
//! // Advanced users can use Position for precise control
//! let pos1 = Position::new(10, 1);
//! let pos2 = Position::new(20, 1);
//! let between = Position::between(&pos1, &pos2);
//! list.insert_at_position(between, "advanced");
//! ```
//!
//! # API Design
//!
//! The Map API provides multiple levels of ergonomics:
//!
//! ## Level 1: Basic Access
//! ```
//! # use eidetica::crdt::map::{Map, Value};
//! let mut map = Map::new();
//! map.set("name", "Alice");
//!
//! // Traditional approach
//! let name = map.get("name").and_then(|v| v.as_text());
//! ```
//!
//! ## Level 2: Typed Getters
//! ```
//! # use eidetica::crdt::map::Map;
//! # let mut map = Map::new();
//! # map.set("name", "Alice");
//! # map.set("age", 30);
//! // Direct typed access
//! let name = map.get_text("name");           // Option<&str>
//! let age = map.get_int("age");              // Option<i64>
//! let bio = map.get_text_at_path("user.bio"); // Option<&str>
//! ```
//!
//! ## Level 3: Direct Comparisons
//! ```
//! # use eidetica::crdt::map::Map;
//! # let mut map = Map::new();
//! # map.set("name", "Alice");
//! # map.set("age", 30);
//! // Direct comparison with PartialEq
//! assert!(*map.get("name").unwrap() == "Alice");
//! assert!(*map.get("age").unwrap() == 30);
//! ```
//!
//! # Design Principles
//!
//! - **Tree-based naming**: Aligns with Eidetica's forest/tree metaphor
//! - **Direct storage**: No JSON serialization overhead
//! - **Predictable behavior**: Lists maintain stable order, paths work naturally
//! - **Clean API**: Multiple ergonomic levels for different use cases
//! - **Full path support**: Multi-level get/set operations with dot notation
//! - **CRDT semantics**: Proper conflict resolution and merge behavior
//! - **Tombstone hiding**: Internal deletion markers are hidden from public API

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use super::list::Position;
use uuid::Uuid;

use crate::crdt::CRDTError;
use crate::crdt::Doc;
use crate::crdt::traits::{CRDT, Data};

// Type alias for backwards compatibility within the map module
pub type Map = Node;

/// Position identifier for list elements that enables stable ordering in distributed systems.
///
/// `Position` uses rational numbers (fractions) to create a dense ordering system
/// that allows insertion between any two existing positions without requiring
/// coordination or reordering of existing elements. This is crucial for CRDTs
/// where concurrent insertions must maintain consistent ordering across all replicas.
///
/// # How Rational Number Positioning Works
///
/// Traditional list indices (0, 1, 2, 3...) don't work well for distributed
/// systems because inserting between positions requires reordering. Rational numbers
/// solve this by creating infinite space between any two positions:
///
/// ```
/// # use eidetica::crdt::map::{List, list::Position};
/// let pos1 = Position::new(1, 1);  // 1.0
/// let pos2 = Position::new(2, 1);  // 2.0
///
/// // Insert between them at 1.5
/// let middle = Position::between(&pos1, &pos2);
/// // Creates position with value ~1.5
///
/// // Can insert again at 1.25, 1.75, etc.
/// let quarter = Position::between(&pos1, &middle);
/// ```
///
/// # Components
///
/// - **numerator/denominator**: The rational number representing order
/// - **unique_id**: UUID for deterministic tie-breaking when rational values are equal
///
/// # Concurrent Insertion Example
///
/// ```text
/// Initial state: [A@1.0, C@2.0]
///
/// User 1 inserts B between A and C: B@1.5
/// User 2 inserts D between A and C: D@1.5 (same rational value!)
///
/// Final order determined by UUID comparison: [A@1.0, B@1.5, D@1.5, C@2.0]
/// (assuming B's UUID < D's UUID)
/// ```
///
/// Values that can be stored in Map tree structures.
///
/// `Value` provides tree-based naming aligned with Eidetica's forest metaphor,
/// supporting both leaf values (terminal nodes) and branch values (containing other nodes).
/// The enum implements CRDT semantics and provides ergonomic comparison operations.
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
/// - [`Value::Map`] - Nested tree structures
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
/// # use eidetica::crdt::map::{Map, Value};
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
/// - **Branch values**: Structural merging (recursive for Map, positional for List)
/// - **Tombstones**: Deletion markers that win over any non-deleted value
/// - **Resurrection**: Non-deleted values can overwrite tombstones
///
/// ```
/// # use eidetica::crdt::map::{Map, Value};
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

/// Ordered list with stable positioning for CRDT operations.
///
/// `List` maintains a stable ordering of elements using [`Position`] keys
/// in a `BTreeMap`. This design enables concurrent insertions without requiring
/// coordination between distributed replicas.
///
/// # Key Features
///
/// - **Stable ordering**: Elements maintain their relative positions even with concurrent modifications
/// - **Insertion anywhere**: Can insert between any two existing elements
/// - **CRDT semantics**: Proper merge behavior for distributed systems
/// - **Efficient access**: O(log n) access by position, O(1) by index for small lists
///
/// # Usage Patterns
///
/// ```
/// # use eidetica::crdt::map::{List, list::Position};
/// let mut list = List::new();
///
/// // Simple append operations
/// list.push("first");  // Returns index 0
/// list.push("second"); // Returns index 1
///
/// // Insert between existing elements using index
/// list.insert(1, "between").unwrap();
///
/// // Access by traditional index
/// assert_eq!(list.get(0).unwrap().as_text(), Some("first"));
///
/// // Advanced users can use Position for precise control
/// let pos1 = Position::new(1, 1);  // 1.0
/// let pos2 = Position::new(2, 1);  // 2.0
/// let middle = Position::between(&pos1, &pos2);
/// list.insert_at_position(middle, "advanced");
/// ```
///
/// # Concurrent Operations
///
/// When two replicas insert at the same logical position, the rational number
/// system ensures a consistent final order:
///
/// ```text
/// Replica A: ["item1", "item3"] -> inserts "item2" between them
/// Replica B: ["item1", "item3"] -> inserts "item4" between them
///
/// After merge: ["item1", "item2", "item4", "item3"]
/// (order determined by Position comparison)
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List {
    /// Internal storage using BTreeMap for ordered access
    items: BTreeMap<Position, Value>,
}

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
                    // For in-place merge, manually merge the children
                    for (key, other_value) in &other_node.children {
                        match self_node.children.get_mut(key) {
                            Some(self_value) => {
                                self_value.merge(other_value);
                            }
                            None => {
                                self_node.children.insert(key.clone(), other_value.clone());
                            }
                        }
                    }
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
    /// # use eidetica::crdt::map::Value;
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

impl From<Doc> for Value {
    fn from(value: Doc) -> Self {
        Value::Node(value.into())
    }
}

impl From<List> for Value {
    fn from(value: List) -> Self {
        Value::List(value)
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

// Data trait implementations
impl Data for Position {}
impl Data for Value {}
impl Data for List {}
impl Data for Node {}

impl List {
    /// Creates a new empty list
    pub fn new() -> Self {
        Self {
            items: BTreeMap::new(),
        }
    }

    /// Returns the number of items in the list (excluding tombstones)
    pub fn len(&self) -> usize {
        self.items
            .values()
            .filter(|v| !matches!(v, Value::Deleted))
            .count()
    }

    /// Returns the total number of items including tombstones
    pub fn total_len(&self) -> usize {
        self.items.len()
    }

    /// Returns true if the list is empty
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Pushes a value to the end of the list
    /// Returns the index of the newly added element
    pub fn push(&mut self, value: impl Into<Value>) -> usize {
        let value = value.into();
        let position = if let Some((last_pos, _)) = self.items.last_key_value() {
            // Create a position after the last element
            Position::new(last_pos.numerator.saturating_add(1), 1)
        } else {
            // First element
            Position::beginning()
        };

        self.items.insert(position, value);
        // Return the index (count of non-tombstone elements - 1)
        self.len() - 1
    }

    /// Inserts a value at a specific index
    pub fn insert(&mut self, index: usize, value: impl Into<Value>) -> Result<(), CRDTError> {
        let len = self.len();
        if index > len {
            return Err(CRDTError::ListIndexOutOfBounds { index, len });
        }

        let position = if index == 0 {
            // Insert at beginning
            if let Some((first_pos, _)) = self.items.first_key_value() {
                Position::new(first_pos.numerator - 1, first_pos.denominator)
            } else {
                Position::beginning()
            }
        } else if index == len {
            // Insert at end (same as push)
            if let Some((last_pos, _)) = self.items.last_key_value() {
                Position::new(last_pos.numerator + 1, last_pos.denominator)
            } else {
                Position::beginning()
            }
        } else {
            // Insert between two existing positions
            let positions: Vec<_> = self.items.keys().collect();
            let left_pos = positions[index - 1];
            let right_pos = positions[index];
            Position::between(left_pos, right_pos)
        };

        self.items.insert(position, value.into());
        Ok(())
    }

    /// Gets a value by index (0-based), filtering out tombstones
    pub fn get(&self, index: usize) -> Option<&Value> {
        self.items
            .values()
            .filter(|v| !matches!(v, Value::Deleted))
            .nth(index)
    }

    /// Gets a mutable reference to a value by index (0-based), filtering out tombstones
    pub fn get_mut(&mut self, index: usize) -> Option<&mut Value> {
        // Find the position of the nth non-tombstone element
        let mut current_index = 0;
        let mut target_position = None;

        for (pos, value) in &self.items {
            if !matches!(value, Value::Deleted) {
                if current_index == index {
                    target_position = Some(pos.clone());
                    break;
                }
                current_index += 1;
            }
        }

        if let Some(pos) = target_position {
            self.items.get_mut(&pos)
        } else {
            None
        }
    }

    /// Inserts a value at a specific position (advanced API)
    pub fn insert_at_position(&mut self, position: Position, value: impl Into<Value>) {
        self.items.insert(position, value.into());
    }

    /// Gets a value by position
    pub fn get_by_position(&self, position: &Position) -> Option<&Value> {
        self.items.get(position)
    }

    /// Gets a mutable reference to a value by position
    pub fn get_by_position_mut(&mut self, position: &Position) -> Option<&mut Value> {
        self.items.get_mut(position)
    }

    /// Sets a value at a specific index, returns the old value if present
    /// Only considers non-tombstone elements for indexing
    pub fn set(&mut self, index: usize, value: impl Into<Value>) -> Option<Value> {
        let value = value.into();
        // Find the position of the nth non-tombstone element
        let mut current_index = 0;
        let mut target_position = None;

        for (pos, val) in &self.items {
            if !matches!(val, Value::Deleted) {
                if current_index == index {
                    target_position = Some(pos.clone());
                    break;
                }
                current_index += 1;
            }
        }

        if let Some(pos) = target_position {
            self.items.insert(pos, value)
        } else {
            None
        }
    }

    /// Removes a value by index (tombstones it for CRDT semantics)
    /// Only considers non-tombstone elements for indexing
    pub fn remove(&mut self, index: usize) -> Option<Value> {
        // Find the position of the nth non-tombstone element
        let mut current_index = 0;
        let mut target_position = None;

        for (pos, val) in &self.items {
            if !matches!(val, Value::Deleted) {
                if current_index == index {
                    target_position = Some(pos.clone());
                    break;
                }
                current_index += 1;
            }
        }

        if let Some(pos) = target_position {
            let old_value = self.items.get(&pos).cloned();
            self.items.insert(pos, Value::Deleted);
            old_value
        } else {
            None
        }
    }

    /// Removes a value by position
    pub fn remove_by_position(&mut self, position: &Position) -> Option<Value> {
        self.items.remove(position)
    }

    /// Returns an iterator over the values in order (excluding tombstones)
    pub fn iter(&self) -> impl Iterator<Item = &Value> {
        self.items.values().filter(|v| !matches!(v, Value::Deleted))
    }

    /// Returns an iterator over all values including tombstones
    pub fn iter_all(&self) -> impl Iterator<Item = &Value> {
        self.items.values()
    }

    /// Returns an iterator over position-value pairs in order
    pub fn iter_with_positions(&self) -> impl Iterator<Item = (&Position, &Value)> {
        self.items.iter()
    }

    /// Returns a mutable iterator over the values in order
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Value> {
        self.items.values_mut()
    }

    /// Merges another List into this one (CRDT merge operation)
    pub fn merge(&mut self, other: &List) {
        for (position, value) in &other.items {
            match self.items.get_mut(position) {
                Some(existing_value) => {
                    // If both lists have the same position, merge the values
                    existing_value.merge(value);
                }
                None => {
                    // Position doesn't exist, add it
                    self.items.insert(position.clone(), value.clone());
                }
            }
        }
    }

    /// Clears all items from the list
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Converts to a Vec of values (loses position information)
    pub fn to_vec(&self) -> Vec<Value> {
        self.items.values().cloned().collect()
    }
}

impl Default for List {
    fn default() -> Self {
        Self::new()
    }
}

impl FromIterator<Value> for List {
    fn from_iter<T: IntoIterator<Item = Value>>(iter: T) -> Self {
        let mut list = List::new();
        for value in iter {
            list.push(value);
        }
        list
    }
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

    /// Returns true if the node contains the given key
    pub fn contains_key(&self, key: impl AsRef<str>) -> bool {
        match self.children.get(key.as_ref()) {
            Some(Value::Deleted) => false,
            Some(_) => true,
            None => false,
        }
    }

    /// Returns true if the given key contains a tombstone (deleted value).
    ///
    /// This method provides access to CRDT tombstone information for advanced use cases,
    /// testing, and debugging. Tombstones are internal markers used to track deletions
    /// in CRDT systems and are normally hidden from the public API.
    ///
    /// # Use Cases
    ///
    /// - **Testing**: Verify that deletions create proper tombstones
    /// - **Debugging**: Inspect internal CRDT state
    /// - **Advanced CRDT operations**: Check deletion history for merge operations
    /// - **Serialization verification**: Ensure tombstones survive round-trip serialization
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::map::Map;
    /// let mut map = Map::new();
    /// map.set("key", "value");
    ///
    /// // Normal key - not a tombstone
    /// assert!(!map.is_tombstone("key"));
    /// assert!(!map.is_tombstone("nonexistent"));
    ///
    /// // Remove key - creates tombstone
    /// map.remove("key");
    /// assert!(map.is_tombstone("key"));
    /// assert!(!map.contains_key("key")); // Hidden from normal API
    /// assert!(map.get("key").is_none());  // Hidden from normal API
    /// ```
    ///
    /// # Returns
    ///
    /// - `true` if the key exists and contains a tombstone (`Value::Deleted`)
    /// - `false` if the key doesn't exist or contains a non-deleted value
    pub fn is_tombstone(&self, key: impl AsRef<str>) -> bool {
        matches!(self.children.get(key.as_ref()), Some(Value::Deleted))
    }

    /// Gets a value by key (immutable reference)
    pub fn get(&self, key: impl AsRef<str>) -> Option<&Value> {
        match self.children.get(key.as_ref()) {
            Some(Value::Deleted) => None,
            other => other,
        }
    }

    /// Gets a mutable reference to a value by key
    pub fn get_mut(&mut self, key: impl AsRef<str>) -> Option<&mut Value> {
        match self.children.get_mut(key.as_ref()) {
            Some(Value::Deleted) => None,
            other => other,
        }
    }

    /// Gets a text value by key.
    ///
    /// This is a convenience method that combines `get()` and `as_text()`
    /// for more ergonomic access to text values.
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::map::Map;
    /// let mut map = Map::new();
    /// map.set("name", "Alice");
    ///
    /// // Concise access
    /// assert_eq!(map.get_text("name"), Some("Alice"));
    ///
    /// // Equivalent to:
    /// // assert_eq!(map.get("name").and_then(|v| v.as_text()), Some("Alice"));
    /// ```
    pub fn get_text(&self, key: impl AsRef<str>) -> Option<&str> {
        self.get(key).and_then(|v| v.as_text())
    }

    /// Gets an integer value by key
    pub fn get_int(&self, key: impl AsRef<str>) -> Option<i64> {
        self.get(key).and_then(|v| v.as_int())
    }

    /// Gets a boolean value by key
    pub fn get_bool(&self, key: impl AsRef<str>) -> Option<bool> {
        self.get(key).and_then(|v| v.as_bool())
    }

    /// Gets a node value by key
    pub fn get_node(&self, key: impl AsRef<str>) -> Option<&Node> {
        self.get(key).and_then(|v| v.as_node())
    }

    /// Gets a list value by key
    pub fn get_list(&self, key: impl AsRef<str>) -> Option<&List> {
        self.get(key).and_then(|v| v.as_list())
    }

    /// Sets a value at the given key, returns the old value if present
    pub fn set<K, V>(&mut self, key: K, value: V) -> Option<Value>
    where
        K: Into<String>,
        V: Into<Value>,
    {
        self.children.insert(key.into(), value.into())
    }

    /// Removes a value by key, returns the old value if present.
    ///
    /// This method implements CRDT semantics by creating a tombstone (deletion marker)
    /// to ensure the removal is preserved during merge operations.
    ///
    /// # Parameters
    /// * `key` - The key to remove. Accepts any string type (`&str`, `String`, `&String`)
    ///   for ergonomic usage.
    ///
    /// # Returns
    /// The previous value if it existed, `None` if the key was not present or already deleted.
    pub fn remove(&mut self, key: impl Into<String>) -> Option<Value> {
        let key_string = key.into();
        let key_ref = &key_string;
        match self.children.get(key_ref) {
            Some(Value::Deleted) => {
                // Already deleted, return None and don't modify anything
                None
            }
            Some(_) => {
                // Key exists with real value, remove it and create tombstone
                let existing = self.children.remove(key_ref);
                self.children.insert(key_string.clone(), Value::Deleted);
                existing
            }
            None => {
                // Key doesn't exist, create tombstone and return None
                self.children.insert(key_string, Value::Deleted);
                None
            }
        }
    }

    /// Marks a key as deleted by setting it to `Value::Deleted` tombstone.
    ///
    /// Unlike `remove()`, this method doesn't return the previous value and only
    /// sets the deletion marker if the key currently exists.
    ///
    /// # Parameters
    /// * `key` - The key to mark as deleted. Accepts any string type (`&str`, `String`, `&String`)
    ///   for ergonomic usage.
    ///
    /// # Returns
    /// `true` if the key existed and was marked for deletion, `false` if the key didn't exist.
    pub fn delete(&mut self, key: impl Into<String>) -> bool {
        let key_string = key.into();
        let key_ref = &key_string;
        if self.children.contains_key(key_ref) {
            self.children.insert(key_string, Value::Deleted);
            true
        } else {
            false
        }
    }

    /// Gets a value by path using dot notation (e.g., "users.123.name").
    ///
    /// Traverses the tree structure following the path segments separated by dots.
    /// Each segment navigates deeper into the tree structure.
    ///
    /// # Path Syntax
    ///
    /// - **Nodes**: Navigate by key name (e.g., "user.profile.name")
    /// - **Lists**: Navigate by index (e.g., "items.0.title")
    /// - **Mixed**: Combine both (e.g., "users.0.tags.1")
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::map::{Map, List};
    /// let mut map = Map::new();
    /// map.set_path("user.profile.name", "Alice").unwrap();
    ///
    /// // Navigate nested structure
    /// let name = map.get_path("user.profile.name");
    /// assert_eq!(name.and_then(|v| v.as_text()), Some("Alice"));
    ///
    /// // Or use typed getter
    /// assert_eq!(map.get_text_at_path("user.profile.name"), Some("Alice"));
    /// ```
    ///
    /// # Returns
    ///
    /// - `Some(&Value)` if the path exists
    /// - `None` if any segment of the path doesn't exist or has wrong type
    pub fn get_path(&self, path: impl AsRef<str>) -> Option<&Value> {
        let path = path.as_ref();
        let parts: Vec<&str> = path.split('.').collect();
        if parts.is_empty() {
            return None;
        }

        let mut current_value = self.children.get(parts[0])?;

        for part in parts.iter().skip(1) {
            match current_value {
                Value::Node(node) => {
                    current_value = node.get(part)?;
                }
                Value::List(list) => {
                    // Try to parse as index
                    if let Ok(index) = part.parse::<usize>() {
                        current_value = list.get(index)?;
                    } else {
                        return None;
                    }
                }
                _ => return None,
            }
        }

        Some(current_value)
    }

    /// Gets a text value by path
    pub fn get_text_at_path(&self, path: impl AsRef<str>) -> Option<&str> {
        self.get_path(path).and_then(|v| v.as_text())
    }

    /// Gets an integer value by path
    pub fn get_int_at_path(&self, path: impl AsRef<str>) -> Option<i64> {
        self.get_path(path).and_then(|v| v.as_int())
    }

    /// Gets a boolean value by path
    pub fn get_bool_at_path(&self, path: impl AsRef<str>) -> Option<bool> {
        self.get_path(path).and_then(|v| v.as_bool())
    }

    /// Gets a node value by path
    pub fn get_node_at_path(&self, path: impl AsRef<str>) -> Option<&Node> {
        self.get_path(path).and_then(|v| v.as_node())
    }

    /// Gets a list value by path
    pub fn get_list_at_path(&self, path: impl AsRef<str>) -> Option<&List> {
        self.get_path(path).and_then(|v| v.as_list())
    }

    /// Gets a mutable reference to a value by path
    pub fn get_path_mut(&mut self, path: impl AsRef<str>) -> Option<&mut Value> {
        let path = path.as_ref();
        let parts: Vec<&str> = path.split('.').collect();
        if parts.is_empty() {
            return None;
        }

        let mut current_value = self.children.get_mut(parts[0])?;

        for part in parts.iter().skip(1) {
            match current_value {
                Value::Node(node) => {
                    current_value = node.get_mut(part)?;
                }
                Value::List(list) => {
                    // Try to parse as index
                    if let Ok(index) = part.parse::<usize>() {
                        current_value = list.get_mut(index)?;
                    } else {
                        return None;
                    }
                }
                _ => return None,
            }
        }

        Some(current_value)
    }

    /// Sets a value at the given path, creating intermediate nodes as needed
    pub fn set_path(
        &mut self,
        path: impl AsRef<str>,
        value: impl Into<Value>,
    ) -> Result<Option<Value>, CRDTError> {
        let path = path.as_ref();
        let value = value.into();
        let parts: Vec<&str> = path.split('.').collect();
        if parts.is_empty() {
            return Err(CRDTError::InvalidPath {
                path: "Empty path".to_string(),
            });
        }

        if parts.len() == 1 {
            // Simple key set
            return Ok(self.set(parts[0], value));
        }

        // Navigate to the parent, creating intermediate nodes as needed
        let mut current_map = self;
        for part in parts.iter().take(parts.len() - 1) {
            let part_owned = part.to_string();

            // Check if we need to create a new node
            let needs_new_node = match current_map.children.get(*part) {
                Some(Value::Node(_)) => false,
                Some(_) => {
                    // Existing non-node value, can't navigate further
                    return Err(CRDTError::InvalidPath {
                        path: format!("Cannot navigate through non-node value at '{part}'"),
                    });
                }
                None => true,
            };

            if needs_new_node {
                current_map
                    .children
                    .insert(part_owned.clone(), Value::Node(Node::new()));
            }

            // Navigate to the node
            match current_map.children.get_mut(&part_owned) {
                Some(Value::Node(node)) => {
                    current_map = node;
                }
                _ => unreachable!(), // We just ensured this is a Map
            }
        }

        // Set the final value
        let final_key = parts[parts.len() - 1];
        Ok(current_map.set(final_key, value))
    }

    /// Returns an iterator over all key-value pairs (excluding tombstones)
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.children
            .iter()
            .filter(|(_, v)| !matches!(v, Value::Deleted))
    }

    /// Returns a mutable iterator over all key-value pairs (excluding tombstones)
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut Value)> {
        self.children
            .iter_mut()
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

    /// Returns a mutable iterator over all values (excluding tombstones)
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Value> {
        self.children
            .values_mut()
            .filter(|v| !matches!(v, Value::Deleted))
    }

    /// Clears all children from this node
    pub fn clear(&mut self) {
        self.children.clear();
    }

    /// Converts to a JSON-like string representation for human-readable output.
    ///
    /// This method produces clean JSON output intended for display, debugging, and export.
    /// It differs from serde serialization in important ways:
    ///
    /// - **Tombstones**: Deleted entries are completely excluded from JSON output
    /// - **Purpose**: Human-readable output, not CRDT state preservation
    /// - **Use cases**: Display, debugging, export to external systems
    ///
    /// For complete CRDT state preservation including tombstones, use serde serialization instead.
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::map::Map;
    /// let mut map = Map::new();
    /// map.set("name", "Alice");
    /// map.set("age", 30);
    /// map.delete("age"); // Creates a tombstone
    ///
    /// // JSON output excludes the deleted key
    /// let json = map.to_json_string();
    /// assert!(json.contains("name"));
    /// assert!(!json.contains("age")); // Deleted keys are excluded
    /// ```
    pub fn to_json_string(&self) -> String {
        let mut items = Vec::new();
        for (key, value) in &self.children {
            // Skip tombstones (deleted values) - they should not appear in human-readable JSON output
            if !matches!(value, Value::Deleted) {
                items.push(format!("\"{}\":{}", key, value.to_json_string()));
            }
        }
        format!("{{{}}}", items.join(","))
    }

    /// Returns a copy of the internal HashMap (for testing/debugging)
    pub fn children(&self) -> &HashMap<String, Value> {
        &self.children
    }
}

impl CRDT for Node {
    /// Merges another Map into this one using CRDT semantics.
    ///
    /// This method implements the core CRDT merge operation for Map structures.
    /// It recursively merges all child nodes while preserving CRDT properties:
    /// - Associativity: (A ∪ B) ∪ C = A ∪ (B ∪ C)
    /// - Idempotency: A ∪ A = A
    ///
    /// Note: This merge operation is NOT commutative (A ∪ B ≠ B ∪ A) due to
    /// last-write-wins semantics for conflicting scalar values.
    ///
    /// # Merge Strategy
    ///
    /// - **Structural merging**: Child nodes are merged recursively
    /// - **Additive**: Keys present in either node appear in the result
    /// - **Value merging**: Conflicting values use Value merge semantics
    /// - **Tombstone handling**: Deletion markers are preserved for consistency
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::map::Map;
    /// # use eidetica::crdt::traits::CRDT;
    /// let mut map1 = Map::new();
    /// map1.set("name", "Alice");
    /// map1.set("age", 30);
    ///
    /// let mut map2 = Map::new();
    /// map2.set("name", "Bob");     // Conflict: will use last-write-wins
    /// map2.set("city", "NYC");     // New key: will be added
    ///
    /// let merged = map1.merge(&map2).unwrap();
    /// assert_eq!(merged.get_text("name"), Some("Bob"));  // Last write wins
    /// assert_eq!(merged.get_int("age"), Some(30));        // Preserved
    /// assert_eq!(merged.get_text("city"), Some("NYC"));   // Added
    /// ```
    fn merge(&self, other: &Self) -> crate::Result<Self> {
        let mut merged = self.clone();
        for (key, other_value) in &other.children {
            match merged.children.get_mut(key) {
                Some(self_value) => {
                    // Both nodes have this key, merge the values
                    self_value.merge(other_value);
                }
                None => {
                    // Only other node has this key, add it
                    merged.children.insert(key.clone(), other_value.clone());
                }
            }
        }
        Ok(merged)
    }
}

impl Default for Node {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_json_string())
    }
}

impl FromIterator<(String, Value)> for Node {
    fn from_iter<T: IntoIterator<Item = (String, Value)>>(iter: T) -> Self {
        let mut map = Node::new();
        for (key, value) in iter {
            map.set(key, value);
        }
        map
    }
}

// Convenient builder pattern methods
impl Node {
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

    /// Builder method to set a child node
    pub fn with_node<K, V>(self, key: K, value: V) -> Self
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
}

// JSON serialization methods
impl Node {
    /// Set a key-value pair with a raw Value (for advanced use).
    pub fn set_raw<K>(&mut self, key: K, value: Value) -> &mut Self
    where
        K: Into<String>,
    {
        self.set(key.into(), value);
        self
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
            Some(Value::Deleted) | None => Err(CRDTError::ElementNotFound {
                key: key.to_string(),
            }
            .into()),
            Some(value) => Err(CRDTError::TypeMismatch {
                expected: "Text containing JSON data for deserialization".to_string(),
                actual: format!("{} value", value.type_name()),
            }
            .into()),
        }
    }

    /// Set a key-value pair where the value is a string.
    pub fn set_string<K, V>(&mut self, key: K, value: V) -> &mut Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.set(key.into(), Value::Text(value.into()));
        self
    }

    /// Set a key-value pair where the value is a nested map.
    pub fn set_map<K>(&mut self, key: K, value: impl Into<Node>) -> &mut Self
    where
        K: Into<String>,
    {
        self.set(key.into(), Value::Node(value.into()));
        self
    }

    /// Get a nested map by key.
    pub fn get_map(&self, key: &str) -> Option<&Node> {
        match self.get(key) {
            Some(Value::Node(node)) => Some(node),
            _ => None,
        }
    }

    /// Get a mutable reference to a nested map by key.
    pub fn get_map_mut(&mut self, key: &str) -> Option<&mut Node> {
        match self.get_mut(key) {
            Some(Value::Node(node)) => Some(node),
            _ => None,
        }
    }

    /// Get a reference to the internal HashMap compatible with Map API.
    /// Returns a converted HashMap<String, Value> for compatibility.
    /// Get a reference to the internal HashMap compatible with Map API.
    /// Returns a reference to the underlying Value storage.
    pub fn as_hashmap(&self) -> &HashMap<String, Value> {
        &self.children
    }

    /// Get a mutable reference to the internal HashMap (compatibility method).
    /// Note: This creates a new HashMap each time due to the conversion required.
    pub fn as_hashmap_mut(&mut self) -> &mut HashMap<String, Value> {
        &mut self.children
    }

    /// List operations compatibility methods
    pub fn list_add<K>(&mut self, key: K, value: Value) -> crate::Result<String>
    where
        K: Into<String>,
    {
        let key_str = key.into();
        let map_value = value;

        match self.children.get_mut(&key_str) {
            Some(Value::List(list)) => {
                let index = list.push(map_value);
                // Return a string representation of the index for compatibility
                Ok(index.to_string())
            }
            Some(_) => Err(CRDTError::TypeMismatch {
                expected: "List for adding elements".to_string(),
                actual: "Non-list value".to_string(),
            }
            .into()),
            None => {
                let mut list = List::new();
                let index = list.push(map_value);
                self.set(key_str, Value::List(list));
                Ok(index.to_string())
            }
        }
    }

    /// List remove operation - tombstones element by position ID
    pub fn list_remove<K>(&mut self, key: K, id: &str) -> crate::Result<bool>
    where
        K: Into<String>,
    {
        match self.children.get_mut(&key.into()) {
            Some(Value::List(list)) => {
                // Parse the position ID (format: "numerator:denominator")
                let parts: Vec<&str> = id.split(':').collect();
                if parts.len() >= 2 {
                    if let (Ok(numerator), Ok(denominator)) =
                        (parts[0].parse::<i64>(), parts[1].parse::<u64>())
                    {
                        // Find and tombstone the element with matching position
                        let mut found = false;

                        for (pos, value) in list.items.iter_mut() {
                            if pos.numerator == numerator && pos.denominator == denominator {
                                // Check if already tombstoned
                                if !matches!(value, Value::Deleted) {
                                    *value = Value::Deleted;
                                    found = true;
                                }
                                break;
                            }
                        }

                        Ok(found)
                    } else {
                        Ok(false)
                    }
                } else {
                    Ok(false)
                }
            }
            Some(_) => Err(CRDTError::TypeMismatch {
                expected: "List for removing elements".to_string(),
                actual: "Non-list value".to_string(),
            }
            .into()),
            None => Ok(false),
        }
    }

    /// Get an element by its ID from a list
    pub fn list_get<K>(&self, key: K, id: &str) -> Option<&Value>
    where
        K: AsRef<str>,
    {
        match self.children.get(key.as_ref()) {
            Some(Value::List(list)) => {
                // Parse the position ID (format: "numerator:denominator")
                let parts: Vec<&str> = id.split(':').collect();
                if parts.len() >= 2
                    && let (Ok(numerator), Ok(denominator)) =
                        (parts[0].parse::<i64>(), parts[1].parse::<u64>())
                {
                    // Find the element with matching position
                    for (pos, value) in list.iter_with_positions() {
                        if pos.numerator == numerator && pos.denominator == denominator {
                            // Return None if it's a tombstone
                            return match value {
                                Value::Deleted => None,
                                _ => Some(value),
                            };
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Get all element IDs from a list in order
    /// Returns string representations of list positions for compatibility
    /// Filters out tombstones (deleted elements)
    pub fn list_ids<K>(&self, key: K) -> Vec<String>
    where
        K: AsRef<str>,
    {
        match self.children.get(key.as_ref()) {
            Some(Value::List(list)) => list
                .iter_with_positions()
                .filter_map(|(pos, value)| {
                    // Filter out tombstones
                    match value {
                        Value::Deleted => None,
                        _ => Some(format!("{}:{}", pos.numerator, pos.denominator)),
                    }
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Get list length (excluding tombstones)
    pub fn list_len<K>(&self, key: K) -> usize
    where
        K: AsRef<str>,
    {
        match self.children.get(key.as_ref()) {
            Some(Value::List(list)) => list
                .iter_with_positions()
                .filter(|(_, value)| !matches!(value, Value::Deleted))
                .count(),
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
            _ => true,
        }
    }

    /// Clear list
    pub fn list_clear<K>(&mut self, key: K) -> crate::Result<()>
    where
        K: AsRef<str>,
    {
        match self.get_mut(key.as_ref()) {
            Some(Value::List(list)) => {
                list.clear();
                Ok(())
            }
            Some(_) => Err(CRDTError::TypeMismatch {
                expected: "List for clearing elements".to_string(),
                actual: "Non-list value".to_string(),
            }
            .into()),
            None => Ok(()),
        }
    }
}

// Custom serialization for List to handle Position keys
impl serde::Serialize for List {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.items.len()))?;
        for (pos, value) in &self.items {
            let key = format!("{}:{}:{}", pos.numerator, pos.denominator, pos.unique_id);
            map.serialize_entry(&key, value)?;
        }
        map.end()
    }
}

impl<'de> serde::Deserialize<'de> for List {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{MapAccess, Visitor};
        use std::fmt;

        struct ListVisitor;

        impl<'de> Visitor<'de> for ListVisitor {
            type Value = List;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a map with position keys")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut items = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, Value>()? {
                    let parts: Vec<&str> = key.split(':').collect();
                    if parts.len() == 3 {
                        let numerator =
                            parts[0].parse::<i64>().map_err(serde::de::Error::custom)?;
                        let denominator =
                            parts[1].parse::<u64>().map_err(serde::de::Error::custom)?;
                        let unique_id =
                            parts[2].parse::<Uuid>().map_err(serde::de::Error::custom)?;
                        let position = Position {
                            numerator,
                            denominator,
                            unique_id,
                        };
                        items.insert(position, value);
                    }
                }
                Ok(List { items })
            }
        }

        deserializer.deserialize_map(ListVisitor)
    }
}
