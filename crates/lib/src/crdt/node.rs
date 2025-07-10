//! Tree-based Node CRDT implementation.
//!
//! This module provides a Node CRDT that aligns with Eidetica's tree-based architecture,
//! replacing the legacy Nested implementation with cleaner semantics and better performance.
//! The implementation uses conflict-free replicated data types (CRDTs) to enable
//! distributed collaboration without requiring coordination between nodes.
//!
//! # Core Types
//!
//! - [`Node`] - The root tree structure containing child nodes
//! - [`NodeValue`] - Values that can be stored in tree nodes (leaf or branch values)
//! - [`NodeList`] - Ordered collections with stable positioning using rational numbers
//! - [`ListPosition`] - Rational number-based positions for stable list ordering
//!
//! # CRDT Architecture
//!
//! ## Conflict Resolution
//! The Node CRDT implements several conflict resolution strategies:
//! - **Last-write-wins** for scalar values (text, numbers, booleans)
//! - **Structural merging** for nested nodes and lists
//! - **Tombstone deletion** for preserving CRDT merge semantics
//! - **Stable ordering** for lists using rational number positions
//!
//! ## List Ordering with Rational Numbers
//! The [`NodeList`] uses a unique approach to maintain stable ordering across
//! concurrent insertions. Instead of traditional array indices, each list item
//! has a [`ListPosition`] containing:
//! - A rational number (numerator/denominator) for ordering
//! - A unique UUID for deterministic tie-breaking
//!
//! This allows insertion between any two existing elements without reordering:
//! ```
//! # use eidetica::crdt::node::{NodeList, ListPosition};
//! let mut list = NodeList::new();
//!
//! // Simple index-based operations
//! list.push("first");   // Returns index 0
//! list.push("third");   // Returns index 1
//!
//! // Insert between them using index
//! list.insert(1, "second").unwrap();
//!
//! // List maintains order: ["first", "second", "third"]
//! // Advanced users can use ListPosition for precise control
//! let pos1 = ListPosition::new(10, 1);
//! let pos2 = ListPosition::new(20, 1);
//! let between = ListPosition::between(&pos1, &pos2);
//! list.insert_at_position(between, "advanced");
//! ```
//!
//! # API Design
//!
//! The Node API provides multiple levels of ergonomics:
//!
//! ## Level 1: Basic Access
//! ```
//! # use eidetica::crdt::node::{Node, NodeValue};
//! let mut node = Node::new();
//! node.set("name", "Alice");
//!
//! // Traditional approach
//! let name = node.get("name").and_then(|v| v.as_text());
//! ```
//!
//! ## Level 2: Typed Getters
//! ```
//! # use eidetica::crdt::node::Node;
//! # let mut node = Node::new();
//! # node.set("name", "Alice");
//! # node.set("age", 30);
//! // Direct typed access
//! let name = node.get_text("name");           // Option<&str>
//! let age = node.get_int("age");              // Option<i64>
//! let bio = node.get_text_at_path("user.bio"); // Option<&str>
//! ```
//!
//! ## Level 3: Direct Comparisons
//! ```
//! # use eidetica::crdt::node::Node;
//! # let mut node = Node::new();
//! # node.set("name", "Alice");
//! # node.set("age", 30);
//! // Direct comparison with PartialEq
//! assert!(*node.get("name").unwrap() == "Alice");
//! assert!(*node.get("age").unwrap() == 30);
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

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use uuid::Uuid;

use crate::crdt::CRDTError;
use crate::crdt::traits::{CRDT, Data};

/// Position identifier for list elements that enables stable ordering in distributed systems.
///
/// `ListPosition` uses rational numbers (fractions) to create a dense ordering system
/// that allows insertion between any two existing positions without requiring
/// coordination or reordering of existing elements. This is crucial for CRDTs
/// where concurrent insertions must maintain consistent ordering across all replicas.
///
/// # How Rational Number Positioning Works
///
/// Traditional array indices (0, 1, 2, 3...) don't work well for distributed
/// systems because inserting between positions requires reordering. Rational numbers
/// solve this by creating infinite space between any two positions:
///
/// ```
/// # use eidetica::crdt::node::ListPosition;
/// let pos1 = ListPosition::new(1, 1);  // 1.0
/// let pos2 = ListPosition::new(2, 1);  // 2.0
///
/// // Insert between them at 1.5
/// let middle = ListPosition::between(&pos1, &pos2);
/// // Creates position with value ~1.5
///
/// // Can insert again at 1.25, 1.75, etc.
/// let quarter = ListPosition::between(&pos1, &middle);
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
/// This ensures that all replicas converge to the same order without coordination.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ListPosition {
    /// Numerator of the rational position
    pub numerator: i64,
    /// Denominator of the rational position (always positive)
    pub denominator: u64,
    /// Unique identifier for deterministic ordering
    pub unique_id: Uuid,
}

impl ListPosition {
    /// Creates a new position with the given rational value
    pub fn new(numerator: i64, denominator: u64) -> Self {
        assert!(denominator > 0, "Denominator must be positive");
        Self {
            numerator,
            denominator,
            unique_id: Uuid::new_v4(),
        }
    }

    /// Creates a position at the beginning (before all existing positions)
    pub fn beginning() -> Self {
        Self::new(0, 1)
    }

    /// Creates a position at the end (after all existing positions)
    pub fn end() -> Self {
        Self::new(i64::MAX, 1)
    }

    /// Creates a position between two existing positions.
    ///
    /// This is the core method that enables insertion anywhere in a list
    /// without requiring coordination. It calculates the midpoint between
    /// two rational numbers using precise arithmetic.
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::node::ListPosition;
    /// let pos1 = ListPosition::new(1, 1);  // 1.0
    /// let pos2 = ListPosition::new(2, 1);  // 2.0
    ///
    /// let middle = ListPosition::between(&pos1, &pos2);
    /// // Creates position with value 1.5
    ///
    /// assert!(pos1 < middle);
    /// assert!(middle < pos2);
    /// ```
    ///
    /// # Concurrent Safety
    ///
    /// Multiple replicas can independently create positions between the same
    /// two points. The unique UUID ensures deterministic ordering even when
    /// rational values are identical.
    pub fn between(left: &ListPosition, right: &ListPosition) -> Self {
        // Calculate the midpoint using rational arithmetic
        let left_num = left.numerator as i128 * right.denominator as i128;
        let right_num = right.numerator as i128 * left.denominator as i128;
        let new_denominator = left.denominator as u128 * right.denominator as u128 * 2;

        let new_numerator = (left_num + right_num) as i64;
        let new_denominator = new_denominator as u64;

        Self::new(new_numerator, new_denominator)
    }

    /// Returns the rational value as f64 for comparison
    pub fn as_f64(&self) -> f64 {
        self.numerator as f64 / self.denominator as f64
    }
}

impl PartialOrd for ListPosition {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ListPosition {
    fn cmp(&self, other: &Self) -> Ordering {
        // First compare by rational value
        let self_val = self.numerator as i128 * other.denominator as i128;
        let other_val = other.numerator as i128 * self.denominator as i128;

        match self_val.cmp(&other_val) {
            Ordering::Equal => {
                // If rational values are equal, compare by unique_id for deterministic ordering
                self.unique_id.cmp(&other.unique_id)
            }
            other => other,
        }
    }
}

/// Values that can be stored in Node tree structures.
///
/// `NodeValue` provides tree-based naming aligned with Eidetica's forest metaphor,
/// supporting both leaf values (terminal nodes) and branch values (containing other nodes).
/// The enum implements CRDT semantics and provides ergonomic comparison operations.
///
/// # Value Types
///
/// ## Leaf Values (Terminal Nodes)
/// - [`NodeValue::Null`] - Represents null/empty values
/// - [`NodeValue::Bool`] - Boolean values (true/false)
/// - [`NodeValue::Int`] - 64-bit signed integers
/// - [`NodeValue::Text`] - UTF-8 text strings
///
/// ## Branch Values (Container Nodes)
/// - [`NodeValue::Node`] - Nested tree structures
/// - [`NodeValue::List`] - Ordered collections with stable positioning
///
/// ## CRDT Semantics
/// - [`NodeValue::Deleted`] - Tombstone marker for deleted values
///
/// # Direct Comparisons
///
/// `NodeValue` implements `PartialEq` with primitive types for ergonomic comparisons:
///
/// ```
/// # use eidetica::crdt::node::NodeValue;
/// let text = NodeValue::Text("hello".to_string());
/// let number = NodeValue::Int(42);
/// let flag = NodeValue::Bool(true);
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
/// # use eidetica::crdt::node::NodeValue;
/// let mut val1 = NodeValue::Int(42);
/// let val2 = NodeValue::Int(100);
/// val1.merge(&val2);  // val1 becomes 100 (last-write-wins)
///
/// let mut val3 = NodeValue::Text("hello".to_string());
/// let deleted = NodeValue::Deleted;
/// val3.merge(&deleted);  // val3 becomes Deleted (tombstone wins)
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NodeValue {
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
    List(NodeList),

    // CRDT semantics
    /// Tombstone marker for deleted values
    Deleted,
}

/// Ordered list with stable positioning for CRDT operations.
///
/// `NodeList` maintains a stable ordering of elements using [`ListPosition`] keys
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
/// # use eidetica::crdt::node::{NodeList, ListPosition};
/// let mut list = NodeList::new();
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
/// // Advanced users can use ListPosition for precise control
/// let pos1 = ListPosition::new(1, 1);  // 1.0
/// let pos2 = ListPosition::new(2, 1);  // 2.0
/// let middle = ListPosition::between(&pos1, &pos2);
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
/// (order determined by ListPosition comparison)
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeList {
    /// Internal storage using BTreeMap for ordered access
    items: BTreeMap<ListPosition, NodeValue>,
}

/// The root tree structure containing child nodes.
///
/// `Node` represents a tree-like structure where each node can contain
/// multiple named children, aligned with Eidetica's forest metaphor.
/// Each child is identified by a string key and can contain any [`NodeValue`].
///
/// # CRDT Behavior
///
/// Nodes implement CRDT semantics for distributed collaboration:
/// - **Structural merging**: Child nodes are merged recursively
/// - **Tombstone deletion**: Deleted keys are marked with tombstones for proper merge behavior
/// - **API hiding**: Tombstones are hidden from public API methods
/// - **Last-write-wins**: Conflicting scalar values use last-write-wins resolution
///
/// # API Levels
///
/// The Node API provides multiple levels of ergonomics:
///
/// ## Basic Access
/// ```
/// # use eidetica::crdt::node::Node;
/// let mut node = Node::new();
/// node.set("name", "Alice");
///
/// // Traditional verbose approach
/// let name = node.get("name").and_then(|v| v.as_text());
/// assert_eq!(name, Some("Alice"));
/// ```
///
/// ## Typed Getters
/// ```
/// # use eidetica::crdt::node::Node;
/// # let mut node = Node::new();
/// # node.set("name", "Alice");
/// # node.set("age", 30);
/// // Direct typed access
/// let name = node.get_text("name");     // Option<&str>
/// let age = node.get_int("age");        // Option<i64>
/// assert_eq!(name, Some("Alice"));
/// assert_eq!(age, Some(30));
/// ```
///
/// ## Direct Comparisons
/// ```
/// # use eidetica::crdt::node::Node;
/// # let mut node = Node::new();
/// # node.set("name", "Alice");
/// # node.set("age", 30);
/// // Direct comparison with PartialEq
/// assert!(*node.get("name").unwrap() == "Alice");
/// assert!(*node.get("age").unwrap() == 30);
/// ```
///
/// ## Path-based Access
/// ```
/// # use eidetica::crdt::node::Node;
/// let mut node = Node::new();
/// node.set_path("user.profile.name", "Alice").unwrap();
///
/// // Access nested values with dot notation
/// let name = node.get_text_at_path("user.profile.name");
/// assert_eq!(name, Some("Alice"));
/// ```
///
/// # Builder Pattern
/// ```
/// # use eidetica::crdt::node::{Node, NodeList};
/// let node = Node::new()
///     .with_text("name", "Alice")
///     .with_int("age", 30)
///     .with_bool("active", true)
///     .with_list("tags", NodeList::new());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Node {
    /// Child nodes indexed by string keys
    children: HashMap<String, NodeValue>,
}

impl NodeValue {
    /// Returns true if this is a leaf value (terminal node)
    pub fn is_leaf(&self) -> bool {
        matches!(
            self,
            NodeValue::Null
                | NodeValue::Bool(_)
                | NodeValue::Int(_)
                | NodeValue::Text(_)
                | NodeValue::Deleted
        )
    }

    /// Returns true if this is a branch value (can contain other nodes)
    pub fn is_branch(&self) -> bool {
        matches!(self, NodeValue::Node(_) | NodeValue::List(_))
    }

    /// Returns true if this value represents a deletion
    pub fn is_deleted(&self) -> bool {
        matches!(self, NodeValue::Deleted)
    }

    /// Returns true if this is a null value
    pub fn is_null(&self) -> bool {
        matches!(self, NodeValue::Null)
    }

    /// Returns the type name as a string
    pub fn type_name(&self) -> &'static str {
        match self {
            NodeValue::Null => "null",
            NodeValue::Bool(_) => "bool",
            NodeValue::Int(_) => "int",
            NodeValue::Text(_) => "text",
            NodeValue::Node(_) => "node",
            NodeValue::List(_) => "list",
            NodeValue::Deleted => "deleted",
        }
    }

    /// Attempts to convert to a boolean
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            NodeValue::Bool(b) => Some(*b),
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
            NodeValue::Int(n) => Some(*n),
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
            NodeValue::Text(s) => Some(s),
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
            NodeValue::Node(node) => Some(node),
            _ => None,
        }
    }

    /// Attempts to convert to a mutable node reference
    pub fn as_node_mut(&mut self) -> Option<&mut Node> {
        match self {
            NodeValue::Node(node) => Some(node),
            _ => None,
        }
    }

    /// Attempts to convert to a list (returns immutable reference)
    pub fn as_list(&self) -> Option<&NodeList> {
        match self {
            NodeValue::List(list) => Some(list),
            _ => None,
        }
    }

    /// Attempts to convert to a mutable list reference
    pub fn as_list_mut(&mut self) -> Option<&mut NodeList> {
        match self {
            NodeValue::List(list) => Some(list),
            _ => None,
        }
    }

    /// Merges another NodeValue into this one (CRDT merge operation)
    pub fn merge(&mut self, other: &NodeValue) {
        if matches!(self, NodeValue::Deleted) {
            // If self is deleted, other value wins (resurrection)
            *self = other.clone();
            return;
        }

        if matches!(other, NodeValue::Deleted) {
            // If other is deleted, the tombstone wins (deletion)
            *self = NodeValue::Deleted;
            return;
        }

        // Handle specific cases without moving self
        match other {
            NodeValue::Node(other_node) => {
                if let NodeValue::Node(self_node) = self {
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
            NodeValue::List(other_list) => {
                if let NodeValue::List(self_list) = self {
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

    /// Converts to a JSON-like string representation
    pub fn to_json_string(&self) -> String {
        match self {
            NodeValue::Null => "null".to_string(),
            NodeValue::Bool(b) => b.to_string(),
            NodeValue::Int(n) => n.to_string(),
            NodeValue::Text(s) => format!("\"{}\"", s.replace('\"', "\\\"")),
            NodeValue::Node(node) => node.to_json_string(),
            NodeValue::List(list) => {
                let items: Vec<String> = list.iter().map(|v| v.to_json_string()).collect();
                format!("[{}]", items.join(","))
            }
            NodeValue::Deleted => "null".to_string(), // Deleted values appear as null
        }
    }
}

impl fmt::Display for NodeValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeValue::Null => write!(f, "null"),
            NodeValue::Bool(b) => write!(f, "{b}"),
            NodeValue::Int(n) => write!(f, "{n}"),
            NodeValue::Text(s) => write!(f, "{s}"),
            NodeValue::Node(node) => write!(f, "{node}"),
            NodeValue::List(list) => {
                let items: Vec<String> = list.iter().map(|v| v.to_string()).collect();
                write!(f, "[{}]", items.join(", "))
            }
            NodeValue::Deleted => write!(f, "<deleted>"),
        }
    }
}

// Convenient From implementations for common types
impl From<bool> for NodeValue {
    fn from(value: bool) -> Self {
        NodeValue::Bool(value)
    }
}

impl From<i64> for NodeValue {
    fn from(value: i64) -> Self {
        NodeValue::Int(value)
    }
}

impl From<u64> for NodeValue {
    fn from(value: u64) -> Self {
        // Convert to i64, clamping if necessary
        NodeValue::Int(value as i64)
    }
}

impl From<f64> for NodeValue {
    fn from(value: f64) -> Self {
        // Convert to i64, truncating the fractional part
        NodeValue::Int(value as i64)
    }
}

impl From<i32> for NodeValue {
    fn from(value: i32) -> Self {
        NodeValue::Int(value as i64)
    }
}

impl From<u32> for NodeValue {
    fn from(value: u32) -> Self {
        NodeValue::Int(value as i64)
    }
}

impl From<f32> for NodeValue {
    fn from(value: f32) -> Self {
        NodeValue::Int(value as i64)
    }
}

impl From<String> for NodeValue {
    fn from(value: String) -> Self {
        NodeValue::Text(value)
    }
}

impl From<&str> for NodeValue {
    fn from(value: &str) -> Self {
        NodeValue::Text(value.to_string())
    }
}

impl From<Node> for NodeValue {
    fn from(value: Node) -> Self {
        NodeValue::Node(value)
    }
}

impl From<NodeList> for NodeValue {
    fn from(value: NodeList) -> Self {
        NodeValue::List(value)
    }
}

// PartialEq implementations for comparing NodeValue with other types
impl PartialEq<str> for NodeValue {
    fn eq(&self, other: &str) -> bool {
        match self {
            NodeValue::Text(s) => s == other,
            _ => false,
        }
    }
}

impl PartialEq<&str> for NodeValue {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

impl PartialEq<String> for NodeValue {
    fn eq(&self, other: &String) -> bool {
        match self {
            NodeValue::Text(s) => s == other,
            _ => false,
        }
    }
}

impl PartialEq<i64> for NodeValue {
    fn eq(&self, other: &i64) -> bool {
        match self {
            NodeValue::Int(n) => n == other,
            _ => false,
        }
    }
}

impl PartialEq<i32> for NodeValue {
    fn eq(&self, other: &i32) -> bool {
        match self {
            NodeValue::Int(n) => *n == *other as i64,
            _ => false,
        }
    }
}

impl PartialEq<u32> for NodeValue {
    fn eq(&self, other: &u32) -> bool {
        match self {
            NodeValue::Int(n) => *n == *other as i64,
            _ => false,
        }
    }
}

impl PartialEq<bool> for NodeValue {
    fn eq(&self, other: &bool) -> bool {
        match self {
            NodeValue::Bool(b) => b == other,
            _ => false,
        }
    }
}

// Reverse implementations for symmetry
impl PartialEq<NodeValue> for str {
    fn eq(&self, other: &NodeValue) -> bool {
        other == self
    }
}

impl PartialEq<NodeValue> for &str {
    fn eq(&self, other: &NodeValue) -> bool {
        other == *self
    }
}

impl PartialEq<NodeValue> for String {
    fn eq(&self, other: &NodeValue) -> bool {
        other == self
    }
}

impl PartialEq<NodeValue> for i64 {
    fn eq(&self, other: &NodeValue) -> bool {
        other == self
    }
}

impl PartialEq<NodeValue> for i32 {
    fn eq(&self, other: &NodeValue) -> bool {
        other == self
    }
}

impl PartialEq<NodeValue> for u32 {
    fn eq(&self, other: &NodeValue) -> bool {
        other == self
    }
}

impl PartialEq<NodeValue> for bool {
    fn eq(&self, other: &NodeValue) -> bool {
        other == self
    }
}

impl From<crate::crdt::Value> for NodeValue {
    fn from(value: crate::crdt::Value) -> Self {
        match value {
            crate::crdt::Value::String(s) => NodeValue::Text(s),
            crate::crdt::Value::Map(nested) => {
                // Convert Nested to Node
                let mut node = Node::new();
                for (key, val) in nested.as_hashmap() {
                    node.set(key.clone(), val.clone());
                }
                NodeValue::Node(node)
            }
            crate::crdt::Value::Array(array) => {
                // Convert Array to NodeList
                let mut list = NodeList::new();
                for id in array.ids() {
                    if let Some(array_value) = array.get(&id) {
                        let node_value = NodeValue::from(array_value.clone());
                        list.push(node_value);
                    }
                }
                NodeValue::List(list)
            }
            crate::crdt::Value::Deleted => NodeValue::Deleted,
        }
    }
}

// Data trait implementations
impl Data for ListPosition {}
impl Data for NodeValue {}
impl Data for NodeList {}
impl Data for Node {}

impl NodeList {
    /// Creates a new empty list
    pub fn new() -> Self {
        Self {
            items: BTreeMap::new(),
        }
    }

    /// Returns the number of items in the list
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Returns true if the list is empty
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Pushes a value to the end of the list
    pub fn push(&mut self, value: impl Into<NodeValue>) -> usize {
        let value = value.into();
        let position = if let Some((last_pos, _)) = self.items.last_key_value() {
            // Create a position after the last element
            ListPosition::new(last_pos.numerator.saturating_add(1), 1)
        } else {
            // First element
            ListPosition::beginning()
        };

        self.items.insert(position, value);
        self.len() - 1 // Return the index of the newly inserted item
    }

    /// Inserts a value at a specific index
    pub fn insert(&mut self, index: usize, value: impl Into<NodeValue>) -> Result<(), CRDTError> {
        let len = self.len();
        if index > len {
            return Err(CRDTError::ListIndexOutOfBounds { index, len });
        }

        let position = if index == 0 {
            // Insert at beginning
            if let Some((first_pos, _)) = self.items.first_key_value() {
                ListPosition::new(first_pos.numerator - 1, first_pos.denominator)
            } else {
                ListPosition::beginning()
            }
        } else if index == len {
            // Insert at end (same as push)
            if let Some((last_pos, _)) = self.items.last_key_value() {
                ListPosition::new(last_pos.numerator + 1, last_pos.denominator)
            } else {
                ListPosition::beginning()
            }
        } else {
            // Insert between two existing positions
            let positions: Vec<_> = self.items.keys().collect();
            let left_pos = positions[index - 1];
            let right_pos = positions[index];
            ListPosition::between(left_pos, right_pos)
        };

        self.items.insert(position, value.into());
        Ok(())
    }

    /// Gets a value by index (0-based)
    pub fn get(&self, index: usize) -> Option<&NodeValue> {
        self.items.values().nth(index)
    }

    /// Gets a mutable reference to a value by index (0-based)
    pub fn get_mut(&mut self, index: usize) -> Option<&mut NodeValue> {
        // This is inefficient but necessary for mutable access by index
        let position = self.items.keys().nth(index).cloned()?;
        self.items.get_mut(&position)
    }

    /// Inserts a value at a specific position (advanced API)
    pub fn insert_at_position(&mut self, position: ListPosition, value: impl Into<NodeValue>) {
        self.items.insert(position, value.into());
    }

    /// Gets a value by position
    pub fn get_by_position(&self, position: &ListPosition) -> Option<&NodeValue> {
        self.items.get(position)
    }

    /// Gets a mutable reference to a value by position
    pub fn get_by_position_mut(&mut self, position: &ListPosition) -> Option<&mut NodeValue> {
        self.items.get_mut(position)
    }

    /// Sets a value at a specific index, returns the old value if present
    pub fn set(&mut self, index: usize, value: impl Into<NodeValue>) -> Option<NodeValue> {
        let position = self.items.keys().nth(index).cloned()?;
        self.items.insert(position, value.into())
    }

    /// Removes a value by index
    pub fn remove(&mut self, index: usize) -> Option<NodeValue> {
        let position = self.items.keys().nth(index).cloned()?;
        self.items.remove(&position)
    }

    /// Removes a value by position
    pub fn remove_by_position(&mut self, position: &ListPosition) -> Option<NodeValue> {
        self.items.remove(position)
    }

    /// Returns an iterator over the values in order
    pub fn iter(&self) -> impl Iterator<Item = &NodeValue> {
        self.items.values()
    }

    /// Returns an iterator over position-value pairs in order
    pub fn iter_with_positions(&self) -> impl Iterator<Item = (&ListPosition, &NodeValue)> {
        self.items.iter()
    }

    /// Returns a mutable iterator over the values in order
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut NodeValue> {
        self.items.values_mut()
    }

    /// Merges another NodeList into this one (CRDT merge operation)
    pub fn merge(&mut self, other: &NodeList) {
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
    pub fn to_vec(&self) -> Vec<NodeValue> {
        self.items.values().cloned().collect()
    }
}

impl Default for NodeList {
    fn default() -> Self {
        Self::new()
    }
}

impl FromIterator<NodeValue> for NodeList {
    fn from_iter<T: IntoIterator<Item = NodeValue>>(iter: T) -> Self {
        let mut list = NodeList::new();
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
        self.children
            .values()
            .all(|v| matches!(v, NodeValue::Deleted))
    }

    /// Returns the number of direct children (excluding tombstones)
    pub fn len(&self) -> usize {
        self.children
            .values()
            .filter(|v| !matches!(v, NodeValue::Deleted))
            .count()
    }

    /// Returns true if the node contains the given key
    pub fn contains_key(&self, key: impl AsRef<str>) -> bool {
        match self.children.get(key.as_ref()) {
            Some(NodeValue::Deleted) => false,
            Some(_) => true,
            None => false,
        }
    }

    /// Gets a value by key (immutable reference)
    pub fn get(&self, key: impl AsRef<str>) -> Option<&NodeValue> {
        match self.children.get(key.as_ref()) {
            Some(NodeValue::Deleted) => None,
            other => other,
        }
    }

    /// Gets a mutable reference to a value by key
    pub fn get_mut(&mut self, key: impl AsRef<str>) -> Option<&mut NodeValue> {
        match self.children.get_mut(key.as_ref()) {
            Some(NodeValue::Deleted) => None,
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
    /// # use eidetica::crdt::node::Node;
    /// let mut node = Node::new();
    /// node.set("name", "Alice");
    ///
    /// // Concise access
    /// assert_eq!(node.get_text("name"), Some("Alice"));
    ///
    /// // Equivalent to:
    /// // assert_eq!(node.get("name").and_then(|v| v.as_text()), Some("Alice"));
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
    pub fn get_list(&self, key: impl AsRef<str>) -> Option<&NodeList> {
        self.get(key).and_then(|v| v.as_list())
    }

    /// Sets a value at the given key, returns the old value if present
    pub fn set<K, V>(&mut self, key: K, value: V) -> Option<NodeValue>
    where
        K: Into<String>,
        V: Into<NodeValue>,
    {
        self.children.insert(key.into(), value.into())
    }

    /// Removes a value by key, returns the old value if present
    pub fn remove(&mut self, key: impl AsRef<str>) -> Option<NodeValue> {
        let key_ref = key.as_ref();
        match self.children.get(key_ref) {
            Some(NodeValue::Deleted) => {
                // Already deleted, return None and don't modify anything
                None
            }
            Some(_) => {
                // Key exists with real value, remove it and create tombstone
                let existing = self.children.remove(key_ref);
                self.children
                    .insert(key_ref.to_string(), NodeValue::Deleted);
                existing
            }
            None => {
                // Key doesn't exist, create tombstone and return None
                self.children
                    .insert(key_ref.to_string(), NodeValue::Deleted);
                None
            }
        }
    }

    /// Marks a key as deleted (sets to NodeValue::Deleted)
    pub fn delete(&mut self, key: impl AsRef<str>) -> bool {
        let key_ref = key.as_ref();
        if self.children.contains_key(key_ref) {
            self.children
                .insert(key_ref.to_string(), NodeValue::Deleted);
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
    /// # use eidetica::crdt::node::{Node, NodeList};
    /// let mut node = Node::new();
    /// node.set_path("user.profile.name", "Alice").unwrap();
    ///
    /// // Navigate nested structure
    /// let name = node.get_path("user.profile.name");
    /// assert_eq!(name.and_then(|v| v.as_text()), Some("Alice"));
    ///
    /// // Or use typed getter
    /// assert_eq!(node.get_text_at_path("user.profile.name"), Some("Alice"));
    /// ```
    ///
    /// # Returns
    ///
    /// - `Some(&NodeValue)` if the path exists
    /// - `None` if any segment of the path doesn't exist or has wrong type
    pub fn get_path(&self, path: impl AsRef<str>) -> Option<&NodeValue> {
        let path = path.as_ref();
        let parts: Vec<&str> = path.split('.').collect();
        if parts.is_empty() {
            return None;
        }

        let mut current_value = self.children.get(parts[0])?;

        for part in parts.iter().skip(1) {
            match current_value {
                NodeValue::Node(node) => {
                    current_value = node.get(part)?;
                }
                NodeValue::List(list) => {
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
    pub fn get_list_at_path(&self, path: impl AsRef<str>) -> Option<&NodeList> {
        self.get_path(path).and_then(|v| v.as_list())
    }

    /// Gets a mutable reference to a value by path
    pub fn get_path_mut(&mut self, path: impl AsRef<str>) -> Option<&mut NodeValue> {
        let path = path.as_ref();
        let parts: Vec<&str> = path.split('.').collect();
        if parts.is_empty() {
            return None;
        }

        let mut current_value = self.children.get_mut(parts[0])?;

        for part in parts.iter().skip(1) {
            match current_value {
                NodeValue::Node(node) => {
                    current_value = node.get_mut(part)?;
                }
                NodeValue::List(list) => {
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
        value: impl Into<NodeValue>,
    ) -> Result<Option<NodeValue>, CRDTError> {
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
        let mut current_node = self;
        for part in parts.iter().take(parts.len() - 1) {
            let part_owned = part.to_string();

            // Check if we need to create a new node
            let needs_new_node = match current_node.children.get(*part) {
                Some(NodeValue::Node(_)) => false,
                Some(_) => {
                    // Existing non-node value, can't navigate further
                    return Err(CRDTError::InvalidPath {
                        path: format!("Cannot navigate through non-node value at '{part}'"),
                    });
                }
                None => true,
            };

            if needs_new_node {
                current_node
                    .children
                    .insert(part_owned.clone(), NodeValue::Node(Node::new()));
            }

            // Navigate to the node
            match current_node.children.get_mut(&part_owned) {
                Some(NodeValue::Node(node)) => {
                    current_node = node;
                }
                _ => unreachable!(), // We just ensured this is a Node
            }
        }

        // Set the final value
        let final_key = parts[parts.len() - 1];
        Ok(current_node.set(final_key, value))
    }

    /// Returns an iterator over all key-value pairs (excluding tombstones)
    pub fn iter(&self) -> impl Iterator<Item = (&String, &NodeValue)> {
        self.children
            .iter()
            .filter(|(_, v)| !matches!(v, NodeValue::Deleted))
    }

    /// Returns a mutable iterator over all key-value pairs (excluding tombstones)
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut NodeValue)> {
        self.children
            .iter_mut()
            .filter(|(_, v)| !matches!(v, NodeValue::Deleted))
    }

    /// Returns an iterator over all keys (excluding tombstones)
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.children
            .iter()
            .filter(|(_, v)| !matches!(v, NodeValue::Deleted))
            .map(|(k, _)| k)
    }

    /// Returns an iterator over all values (excluding tombstones)
    pub fn values(&self) -> impl Iterator<Item = &NodeValue> {
        self.children
            .values()
            .filter(|v| !matches!(v, NodeValue::Deleted))
    }

    /// Returns a mutable iterator over all values (excluding tombstones)
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut NodeValue> {
        self.children
            .values_mut()
            .filter(|v| !matches!(v, NodeValue::Deleted))
    }

    /// Clears all children from this node
    pub fn clear(&mut self) {
        self.children.clear();
    }

    /// Converts to a JSON-like string representation
    pub fn to_json_string(&self) -> String {
        let mut items = Vec::new();
        for (key, value) in &self.children {
            items.push(format!("\"{}\":{}", key, value.to_json_string()));
        }
        format!("{{{}}}", items.join(","))
    }

    /// Returns a copy of the internal HashMap (for testing/debugging)
    pub fn children(&self) -> &HashMap<String, NodeValue> {
        &self.children
    }
}

impl CRDT for Node {
    /// Merges another Node into this one using CRDT semantics.
    ///
    /// This method implements the core CRDT merge operation for Node structures.
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
    /// - **Value merging**: Conflicting values use NodeValue merge semantics
    /// - **Tombstone handling**: Deletion markers are preserved for consistency
    ///
    /// # Examples
    ///
    /// ```
    /// # use eidetica::crdt::node::Node;
    /// # use eidetica::crdt::traits::CRDT;
    /// let mut node1 = Node::new();
    /// node1.set("name", "Alice");
    /// node1.set("age", 30);
    ///
    /// let mut node2 = Node::new();
    /// node2.set("name", "Bob");     // Conflict: will use last-write-wins
    /// node2.set("city", "NYC");     // New key: will be added
    ///
    /// let merged = node1.merge(&node2).unwrap();
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

impl FromIterator<(String, NodeValue)> for Node {
    fn from_iter<T: IntoIterator<Item = (String, NodeValue)>>(iter: T) -> Self {
        let mut node = Node::new();
        for (key, value) in iter {
            node.set(key, value);
        }
        node
    }
}

// Convenient builder pattern methods
impl Node {
    /// Builder method to set a value and return self
    pub fn with(mut self, key: impl Into<String>, value: impl Into<NodeValue>) -> Self {
        self.set(key, value);
        self
    }

    /// Builder method to set a boolean value
    pub fn with_bool(self, key: impl Into<String>, value: bool) -> Self {
        self.with(key, NodeValue::Bool(value))
    }

    /// Builder method to set an integer value
    pub fn with_int(self, key: impl Into<String>, value: i64) -> Self {
        self.with(key, NodeValue::Int(value))
    }

    /// Builder method to set a text value
    pub fn with_text(self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.with(key, NodeValue::Text(value.into()))
    }

    /// Builder method to set a child node
    pub fn with_node(self, key: impl Into<String>, value: impl Into<Node>) -> Self {
        self.with(key, NodeValue::Node(value.into()))
    }

    /// Builder method to set a list value
    pub fn with_list(self, key: impl Into<String>, value: impl Into<NodeList>) -> Self {
        self.with(key, NodeValue::List(value.into()))
    }
}

// Compatibility methods for Nested API
impl Node {
    /// Set a key-value pair with automatic conversion from Value enum.
    /// This provides compatibility with the existing Nested API.
    pub fn set_value<K>(&mut self, key: K, value: crate::crdt::Value) -> &mut Self
    where
        K: Into<String>,
    {
        let node_value = Self::value_to_node_value(value);
        self.set(key.into(), node_value);
        self
    }

    /// Set a key-value pair with a raw NodeValue (for advanced use).
    pub fn set_raw<K>(&mut self, key: K, value: NodeValue) -> &mut Self
    where
        K: Into<String>,
    {
        self.set(key.into(), value);
        self
    }

    // /// Get a raw Value by key (for compatibility with Nested API).
    /*pub fn get_raw(&self, key: &str) -> Option<crate::crdt::Value> {
        self.get(key).map(Self::node_value_to_value)
    }*/

    /// Set a key-value pair with automatic JSON serialization for any Serialize type.
    pub fn set_json<K, T>(&mut self, key: K, value: T) -> crate::Result<&mut Self>
    where
        K: Into<String>,
        T: serde::Serialize,
    {
        let json = serde_json::to_string(&value)?;
        self.set(key.into(), NodeValue::Text(json));
        Ok(self)
    }

    /// Get a value by key with automatic JSON deserialization for any Deserialize type.
    pub fn get_json<T>(&self, key: &str) -> crate::Result<T>
    where
        T: for<'de> serde::Deserialize<'de>,
    {
        match self.get(key) {
            Some(NodeValue::Text(json)) => serde_json::from_str::<T>(json).map_err(|e| {
                CRDTError::DeserializationFailed {
                    reason: format!("Failed to deserialize JSON for key '{key}': {e}"),
                }
                .into()
            }),
            Some(NodeValue::Deleted) | None => Err(CRDTError::ElementNotFound {
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
        self.set(key.into(), NodeValue::Text(value.into()));
        self
    }

    /// Set a key-value pair where the value is a nested map.
    pub fn set_map<K>(&mut self, key: K, value: Node) -> &mut Self
    where
        K: Into<String>,
    {
        self.set(key.into(), NodeValue::Node(value));
        self
    }

    /// Get a nested map by key.
    pub fn get_map(&self, key: &str) -> Option<&Node> {
        match self.get(key) {
            Some(NodeValue::Node(node)) => Some(node),
            _ => None,
        }
    }

    /// Get a mutable reference to a nested map by key.
    pub fn get_map_mut(&mut self, key: &str) -> Option<&mut Node> {
        match self.get_mut(key) {
            Some(NodeValue::Node(node)) => Some(node),
            _ => None,
        }
    }

    /// Set a key-value pair where the value is an array.
    pub fn set_array<K>(&mut self, key: K, value: crate::crdt::Array) -> &mut Self
    where
        K: Into<String>,
    {
        // Convert Array to NodeList
        let mut node_list = NodeList::new();
        for id in value.ids() {
            if let Some(array_value) = value.get(&id) {
                let node_value = Self::value_to_node_value(array_value.clone());
                node_list.push(node_value);
            }
        }
        self.set(key.into(), NodeValue::List(node_list));
        self
    }

    // /// Get an array by key.
    /*pub fn get_array(&self, key: &str) -> Option<crate::crdt::Array> {
        match self.get(key) {
            Some(NodeValue::List(list)) => {
                // Convert NodeList to Array
                let mut array = crate::crdt::Array::new();
                for value in list.iter() {
                    let array_value = Self::node_value_to_value(value);
                    array.add(array_value);
                }
                Some(array)
            }
            _ => None,
        }
    }*/

    /// Get a reference to the internal HashMap compatible with Nested API.
    /// Returns a converted HashMap<String, Value> for compatibility.
    /// Get a reference to the internal HashMap compatible with Nested API.
    /// Returns a reference to the underlying NodeValue storage.
    pub fn as_hashmap(&self) -> &HashMap<String, NodeValue> {
        &self.children
    }

    /// Get a mutable reference to the internal HashMap (compatibility method).
    /// Note: This creates a new HashMap each time due to the conversion required.
    pub fn as_hashmap_mut(&mut self) -> &mut HashMap<String, NodeValue> {
        &mut self.children
    }

    /// Array operations compatibility methods
    pub fn array_add<K>(&mut self, key: K, value: crate::crdt::Value) -> crate::Result<String>
    where
        K: Into<String>,
    {
        let key_str = key.into();
        let node_value = Self::value_to_node_value(value);

        match self.children.get_mut(&key_str) {
            Some(NodeValue::List(list)) => {
                let index = list.push(node_value);
                // Return a string representation of the index for compatibility
                Ok(format!("index:{index}"))
            }
            Some(_) => Err(CRDTError::TypeMismatch {
                expected: "List for adding elements".to_string(),
                actual: "Non-list value".to_string(),
            }
            .into()),
            None => {
                let mut list = NodeList::new();
                let index = list.push(node_value);
                self.set(key_str, NodeValue::List(list));
                Ok(format!("index:{index}"))
            }
        }
    }

    /// Array remove operation - tombstones element by position ID
    pub fn array_remove<K>(&mut self, key: K, id: &str) -> crate::Result<bool>
    where
        K: Into<String>,
    {
        match self.children.get_mut(&key.into()) {
            Some(NodeValue::List(list)) => {
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
                                if !matches!(value, NodeValue::Deleted) {
                                    *value = NodeValue::Deleted;
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

    /// Get an element by its ID from an array
    pub fn array_get<K>(&self, key: K, id: &str) -> Option<&NodeValue>
    where
        K: AsRef<str>,
    {
        match self.children.get(key.as_ref()) {
            Some(NodeValue::List(list)) => {
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
                                NodeValue::Deleted => None,
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

    /// Get all element IDs from an array in order
    /// Returns string representations of list positions for compatibility
    /// Filters out tombstones (deleted elements)
    pub fn array_ids<K>(&self, key: K) -> Vec<String>
    where
        K: AsRef<str>,
    {
        match self.children.get(key.as_ref()) {
            Some(NodeValue::List(list)) => list
                .iter_with_positions()
                .filter_map(|(pos, value)| {
                    // Filter out tombstones
                    match value {
                        NodeValue::Deleted => None,
                        _ => Some(format!("{}:{}", pos.numerator, pos.denominator)),
                    }
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Get array length (excluding tombstones)
    pub fn array_len<K>(&self, key: K) -> usize
    where
        K: AsRef<str>,
    {
        match self.children.get(key.as_ref()) {
            Some(NodeValue::List(list)) => list
                .iter_with_positions()
                .filter(|(_, value)| !matches!(value, NodeValue::Deleted))
                .count(),
            _ => 0,
        }
    }

    /// Check if array is empty
    pub fn array_is_empty<K>(&self, key: K) -> bool
    where
        K: AsRef<str>,
    {
        match self.children.get(key.as_ref()) {
            Some(NodeValue::List(list)) => list.is_empty(),
            _ => true,
        }
    }

    /// Clear array
    pub fn array_clear<K>(&mut self, key: K) -> crate::Result<()>
    where
        K: AsRef<str>,
    {
        match self.get_mut(key.as_ref()) {
            Some(NodeValue::List(list)) => {
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

    // Helper methods for converting between Value and NodeValue
    fn value_to_node_value(value: crate::crdt::Value) -> NodeValue {
        match value {
            crate::crdt::Value::String(s) => NodeValue::Text(s),
            crate::crdt::Value::Map(nested) => {
                // Convert Nested to Node
                let mut node = Node::new();
                for (key, val) in nested.as_hashmap() {
                    node.set(key.clone(), val.clone());
                }
                NodeValue::Node(node)
            }
            crate::crdt::Value::Array(array) => {
                // Convert Array to NodeList
                let mut list = NodeList::new();
                for id in array.ids() {
                    if let Some(array_value) = array.get(&id) {
                        let node_value = Self::value_to_node_value(array_value.clone());
                        list.push(node_value);
                    }
                }
                NodeValue::List(list)
            }
            crate::crdt::Value::Deleted => NodeValue::Deleted,
        }
    }

    /*fn node_value_to_value(node_value: &NodeValue) -> crate::crdt::Value {
        match node_value {
            NodeValue::Text(s) => crate::crdt::Value::String(s.clone()),
            NodeValue::Node(node) => {
                // Convert Node back to Nested for compatibility
                let mut nested = crate::crdt::Nested::new();
                for (key, val) in &node.children {
                    let value = Self::node_value_to_value(val);
                    nested.set_raw(key, value);
                }
                crate::crdt::Value::Map(nested)
            }
            NodeValue::List(list) => {
                // Convert NodeList back to Array for compatibility
                let mut array = crate::crdt::Array::new();
                for val in list.iter() {
                    let value = Self::node_value_to_value(val);
                    array.add(value);
                }
                crate::crdt::Value::Array(array)
            }
            NodeValue::Bool(b) => crate::crdt::Value::String(b.to_string()),
            NodeValue::Int(n) => crate::crdt::Value::String(n.to_string()),
            NodeValue::Null => crate::crdt::Value::String("null".to_string()),
            NodeValue::Deleted => crate::crdt::Value::Deleted,
        }
    }*/
}

// Custom serialization for NodeList to handle ListPosition keys
impl serde::Serialize for NodeList {
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

impl<'de> serde::Deserialize<'de> for NodeList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{MapAccess, Visitor};
        use std::fmt;

        struct NodeListVisitor;

        impl<'de> Visitor<'de> for NodeListVisitor {
            type Value = NodeList;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a map with position keys")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut items = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, NodeValue>()? {
                    let parts: Vec<&str> = key.split(':').collect();
                    if parts.len() == 3 {
                        let numerator =
                            parts[0].parse::<i64>().map_err(serde::de::Error::custom)?;
                        let denominator =
                            parts[1].parse::<u64>().map_err(serde::de::Error::custom)?;
                        let unique_id =
                            parts[2].parse::<Uuid>().map_err(serde::de::Error::custom)?;
                        let position = ListPosition {
                            numerator,
                            denominator,
                            unique_id,
                        };
                        items.insert(position, value);
                    }
                }
                Ok(NodeList { items })
            }
        }

        deserializer.deserialize_map(NodeListVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crdt::Value;

    // NodeValue tests
    #[test]
    fn test_node_value_basic_types() {
        let null_val = NodeValue::Null;
        let bool_val = NodeValue::Bool(true);
        let int_val = NodeValue::Int(42);
        let text_val = NodeValue::Text("hello".to_string());
        let deleted_val = NodeValue::Deleted;

        assert!(null_val.is_leaf());
        assert!(bool_val.is_leaf());
        assert!(int_val.is_leaf());
        assert!(text_val.is_leaf());
        assert!(deleted_val.is_leaf());

        assert!(!null_val.is_branch());
        assert!(!bool_val.is_branch());
        assert!(!int_val.is_branch());
        assert!(!text_val.is_branch());
        assert!(!deleted_val.is_branch());

        assert!(null_val.is_null());
        assert!(!bool_val.is_null());
        assert!(!int_val.is_null());
        assert!(!text_val.is_null());
        assert!(!deleted_val.is_null());

        assert!(!null_val.is_deleted());
        assert!(!bool_val.is_deleted());
        assert!(!int_val.is_deleted());
        assert!(!text_val.is_deleted());
        assert!(deleted_val.is_deleted());
    }

    #[test]
    fn test_node_value_branch_types() {
        let node_val = NodeValue::Node(Node::new());
        let list_val = NodeValue::List(NodeList::new());

        assert!(!node_val.is_leaf());
        assert!(!list_val.is_leaf());

        assert!(node_val.is_branch());
        assert!(list_val.is_branch());

        assert!(!node_val.is_null());
        assert!(!list_val.is_null());

        assert!(!node_val.is_deleted());
        assert!(!list_val.is_deleted());
    }

    #[test]
    fn test_node_value_type_names() {
        assert_eq!(NodeValue::Null.type_name(), "null");
        assert_eq!(NodeValue::Bool(true).type_name(), "bool");
        assert_eq!(NodeValue::Int(42).type_name(), "int");
        assert_eq!(NodeValue::Text("hello".to_string()).type_name(), "text");
        assert_eq!(NodeValue::Node(Node::new()).type_name(), "node");
        assert_eq!(NodeValue::List(NodeList::new()).type_name(), "list");
        assert_eq!(NodeValue::Deleted.type_name(), "deleted");
    }

    #[test]
    fn test_node_value_accessors() {
        let bool_val = NodeValue::Bool(true);
        let int_val = NodeValue::Int(42);
        let text_val = NodeValue::Text("hello".to_string());
        let node_val = NodeValue::Node(Node::new());
        let list_val = NodeValue::List(NodeList::new());

        // Test as_bool
        assert_eq!(bool_val.as_bool(), Some(true));
        assert_eq!(int_val.as_bool(), None);

        // Test as_int
        assert_eq!(int_val.as_int(), Some(42));
        assert_eq!(bool_val.as_int(), None);

        // Test as_text
        assert_eq!(text_val.as_text(), Some("hello"));
        assert_eq!(bool_val.as_text(), None);

        // Test direct comparisons
        assert!(bool_val == true);
        assert!(int_val == 42);
        assert!(text_val == "hello");

        // Test as_node
        assert!(node_val.as_node().is_some());
        assert!(bool_val.as_node().is_none());

        // Test as_list
        assert!(list_val.as_list().is_some());
        assert!(bool_val.as_list().is_none());
    }

    #[test]
    fn test_node_value_from_impls() {
        let from_bool: NodeValue = true.into();
        let from_i64: NodeValue = 42i64.into();
        let from_string: NodeValue = "hello".into();
        let from_node: NodeValue = Node::new().into();
        let from_list: NodeValue = NodeList::new().into();

        assert_eq!(from_bool.as_bool(), Some(true));
        assert_eq!(from_i64.as_int(), Some(42));
        assert_eq!(from_string.as_text(), Some("hello"));
        assert!(from_node.as_node().is_some());
        assert!(from_list.as_list().is_some());
    }

    #[test]
    fn test_node_value_merge_leafs() {
        let mut val1 = NodeValue::Int(42);
        let val2 = NodeValue::Int(100);

        val1.merge(&val2);
        assert_eq!(val1.as_int(), Some(100)); // Last write wins

        let mut val3 = NodeValue::Text("hello".to_string());
        let val4 = NodeValue::Text("world".to_string());

        val3.merge(&val4);
        assert_eq!(val3.as_text(), Some("world")); // Last write wins
    }

    #[test]
    fn test_node_value_merge_with_deleted() {
        let mut val1 = NodeValue::Int(42);
        let val2 = NodeValue::Deleted;

        val1.merge(&val2);
        assert!(val1.is_deleted()); // Deletion wins

        let mut val3 = NodeValue::Deleted;
        let val4 = NodeValue::Int(100);

        val3.merge(&val4);
        assert_eq!(val3.as_int(), Some(100)); // Resurrection
    }

    // NodeList tests
    #[test]
    fn test_node_list_basic_operations() {
        let mut list = NodeList::new();

        assert!(list.is_empty());
        assert_eq!(list.len(), 0);

        // Test push with flexible input
        let idx1 = list.push("hello");
        let idx2 = list.push(42);
        let idx3 = list.push(true);

        assert!(!list.is_empty());
        assert_eq!(list.len(), 3);

        // Test get
        assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("hello"));
        assert_eq!(list.get(1).and_then(|v| v.as_int()), Some(42));
        assert_eq!(list.get(2).and_then(|v| v.as_bool()), Some(true));
        assert!(list.get(3).is_none());

        // Test indexes returned by push
        assert_eq!(idx1, 0);
        assert_eq!(idx2, 1);
        assert_eq!(idx3, 2);
    }

    #[test]
    fn test_node_list_set_operations() {
        let mut list = NodeList::new();

        list.push("original");
        list.push(100);

        // Test set with flexible input
        let old_val = list.set(0, "modified");
        assert_eq!(old_val.as_ref().and_then(|v| v.as_text()), Some("original"));
        assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("modified"));

        let old_val2 = list.set(1, 200);
        assert_eq!(old_val2.as_ref().and_then(|v| v.as_int()), Some(100));
        assert_eq!(list.get(1).and_then(|v| v.as_int()), Some(200));

        // Test set on non-existent index
        let result = list.set(10, "nonexistent");
        assert!(result.is_none());
    }

    #[test]
    fn test_node_list_remove_operations() {
        let mut list = NodeList::new();

        list.push("first");
        list.push("second");
        list.push("third");

        // Test remove
        let removed = list.remove(1);
        assert_eq!(removed.as_ref().and_then(|v| v.as_text()), Some("second"));
        assert_eq!(list.len(), 2);

        // Verify remaining elements
        assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("first"));
        assert_eq!(list.get(1).and_then(|v| v.as_text()), Some("third"));

        // Test remove on non-existent index
        let result = list.remove(10);
        assert!(result.is_none());
    }

    #[test]
    fn test_node_list_insert_at_position() {
        let mut list = NodeList::new();

        let pos1 = ListPosition::new(10, 1);
        let pos2 = ListPosition::new(20, 1);
        let pos3 = ListPosition::new(15, 1); // Between pos1 and pos2

        list.insert_at_position(pos1, "first");
        list.insert_at_position(pos2, "third");
        list.insert_at_position(pos3, "second");

        // Should be ordered by position
        assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("first"));
        assert_eq!(list.get(1).and_then(|v| v.as_text()), Some("second"));
        assert_eq!(list.get(2).and_then(|v| v.as_text()), Some("third"));
    }

    #[test]
    fn test_node_list_iterators() {
        let mut list = NodeList::new();

        list.push("a");
        list.push("b");
        list.push("c");

        // Test iter
        let values: Vec<_> = list.iter().collect();
        assert_eq!(values.len(), 3);

        // Test iter_with_positions
        let pairs: Vec<_> = list.iter_with_positions().collect();
        assert_eq!(pairs.len(), 3);

        // Test iter_mut
        for value in list.iter_mut() {
            if let NodeValue::Text(s) = value {
                s.push_str("_modified");
            }
        }

        assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("a_modified"));
        assert_eq!(list.get(1).and_then(|v| v.as_text()), Some("b_modified"));
        assert_eq!(list.get(2).and_then(|v| v.as_text()), Some("c_modified"));
    }

    #[test]
    fn test_node_list_merge() {
        let mut list1 = NodeList::new();
        let mut list2 = NodeList::new();

        let pos1 = ListPosition::new(10, 1);
        let pos2 = ListPosition::new(20, 1);
        let pos3 = ListPosition::new(15, 1);

        list1.insert_at_position(pos1.clone(), "first");
        list1.insert_at_position(pos2.clone(), "second");

        list2.insert_at_position(pos2.clone(), "second_modified"); // Conflict
        list2.insert_at_position(pos3.clone(), "middle");

        list1.merge(&list2);

        // Should have merged
        assert_eq!(list1.len(), 3);
        assert_eq!(
            list1.get_by_position(&pos1).and_then(|v| v.as_text()),
            Some("first")
        );
        assert_eq!(
            list1.get_by_position(&pos2).and_then(|v| v.as_text()),
            Some("second_modified")
        );
        assert_eq!(
            list1.get_by_position(&pos3).and_then(|v| v.as_text()),
            Some("middle")
        );
    }

    #[test]
    fn test_node_list_from_iterator() {
        let values = vec![
            NodeValue::Text("a".to_string()),
            NodeValue::Int(42),
            NodeValue::Bool(true),
        ];

        let list: NodeList = values.into_iter().collect();
        assert_eq!(list.len(), 3);
        assert_eq!(list.get(0).and_then(|v| v.as_text()), Some("a"));
        assert_eq!(list.get(1).and_then(|v| v.as_int()), Some(42));
        assert_eq!(list.get(2).and_then(|v| v.as_bool()), Some(true));
    }

    // Node tests
    #[test]
    fn test_node_basic_operations() {
        let mut node = Node::new();

        assert!(node.is_empty());
        assert_eq!(node.len(), 0);

        // Test set with flexible input
        let old_val = node.set("name", "Alice");
        assert!(old_val.is_none());
        assert!(!node.is_empty());
        assert_eq!(node.len(), 1);

        let old_val2 = node.set("age", 30);
        assert!(old_val2.is_none());
        assert_eq!(node.len(), 2);

        // Test contains_key with flexible input
        assert!(node.contains_key("name"));
        assert!(node.contains_key("age"));
        assert!(!node.contains_key("nonexistent"));

        // Test get with flexible input
        assert_eq!(node.get_text("name"), Some("Alice"));
        assert_eq!(node.get_int("age"), Some(30));
        assert!(node.get("nonexistent").is_none());
    }

    #[test]
    fn test_node_overwrite_values() {
        let mut node = Node::new();

        node.set("key", "original");
        let old_val = node.set("key", "modified");

        assert_eq!(old_val.as_ref().and_then(|v| v.as_text()), Some("original"));
        assert_eq!(node.get_text("key"), Some("modified"));
        assert_eq!(node.len(), 1); // Should still be 1
    }

    #[test]
    fn test_node_remove_operations() {
        let mut node = Node::new();

        node.set("name", "Alice");
        node.set("age", 30);
        node.set("active", true);

        // Test remove with flexible input
        let removed = node.remove("age");
        assert_eq!(removed.as_ref().and_then(|v| v.as_int()), Some(30));
        assert!(!node.contains_key("age")); // Key no longer exists (tombstone hidden)
        assert!(node.get("age").is_none()); // get returns None
        assert_eq!(node.len(), 2); // Tombstones excluded from len

        // Test remove on non-existent key
        let result = node.remove("nonexistent");
        assert!(result.is_none());
        assert!(!node.contains_key("nonexistent")); // Tombstone hidden from API
        assert_eq!(node.len(), 2); // Tombstones excluded from len
    }

    #[test]
    fn test_node_delete_operations() {
        let mut node = Node::new();

        node.set("name", "Alice");
        node.set("age", 30);

        // Test delete with flexible input
        let result = node.delete("age");
        assert!(result);
        assert!(!node.contains_key("age")); // Key no longer exists (tombstone hidden)
        assert!(node.get("age").is_none()); // Returns None (filtered out)

        // Test delete on non-existent key
        let result2 = node.delete("nonexistent");
        assert!(!result2);
    }

    #[test]
    fn test_node_get_mut() {
        let mut node = Node::new();

        node.set("name", "Alice");
        node.set("age", 30);

        // Test get_mut with flexible input
        if let Some(NodeValue::Text(name)) = node.get_mut("name") {
            name.push_str(" Smith");
        }

        assert_eq!(node.get_text("name"), Some("Alice Smith"));

        // Test get_mut on non-existent key
        assert!(node.get_mut("nonexistent").is_none());
    }

    #[test]
    fn test_node_path_operations() {
        let mut node = Node::new();

        // Test set_path creating intermediate nodes
        let result = node.set_path("user.profile.name", "Alice");
        assert!(result.is_ok());

        let result2 = node.set_path("user.profile.age", 30);
        assert!(result2.is_ok());

        let result3 = node.set_path("user.settings.theme", "dark");
        assert!(result3.is_ok());

        // Test get_path
        assert_eq!(node.get_text_at_path("user.profile.name"), Some("Alice"));
        assert_eq!(node.get_int_at_path("user.profile.age"), Some(30));
        assert_eq!(node.get_text_at_path("user.settings.theme"), Some("dark"));
        assert!(node.get_path("nonexistent.path").is_none());

        // Test get_path_mut
        if let Some(NodeValue::Text(name)) = node.get_path_mut("user.profile.name") {
            name.push_str(" Smith");
        }

        assert_eq!(
            node.get_text_at_path("user.profile.name"),
            Some("Alice Smith")
        );
    }

    #[test]
    fn test_node_path_with_lists() {
        let mut node = Node::new();

        // Create a node with a list
        let mut list = NodeList::new();
        list.push("item1");
        list.push("item2");
        node.set("items", list);

        // Test path access with list indices
        assert_eq!(node.get_text_at_path("items.0"), Some("item1"));
        assert_eq!(node.get_text_at_path("items.1"), Some("item2"));
        assert!(node.get_path("items.2").is_none());
        assert!(node.get_path("items.invalid").is_none());
    }

    #[test]
    fn test_node_path_errors() {
        let mut node = Node::new();

        node.set("scalar", "value");

        // Test setting path through scalar value
        let result = node.set_path("scalar.nested", "should_fail");
        assert!(result.is_err());

        // Test empty path - this actually works, it just sets at root level
        let result2 = node.set_path("", "value");
        assert!(result2.is_ok()); // Empty path is treated as root level

        // Test path with single component
        let result3 = node.set_path("single", "value");
        assert!(result3.is_ok());
        assert_eq!(node.get_text("single"), Some("value"));
    }

    #[test]
    fn test_node_iterators() {
        let mut node = Node::new();

        node.set("name", "Alice");
        node.set("age", 30);
        node.set("active", true);

        // Test iter
        let pairs: Vec<_> = node.iter().collect();
        assert_eq!(pairs.len(), 3);

        // Test keys
        let keys: Vec<_> = node.keys().collect();
        assert_eq!(keys.len(), 3);
        assert!(keys.contains(&&"name".to_string()));
        assert!(keys.contains(&&"age".to_string()));
        assert!(keys.contains(&&"active".to_string()));

        // Test values
        let values: Vec<_> = node.values().collect();
        assert_eq!(values.len(), 3);

        // Test iter_mut
        for (key, value) in node.iter_mut() {
            if key == "name"
                && let NodeValue::Text(s) = value
            {
                s.push_str(" Smith");
            }
        }

        assert_eq!(node.get_text("name"), Some("Alice Smith"));
    }

    #[test]
    fn test_node_builder_pattern() {
        let node = Node::new()
            .with_text("name", "Alice")
            .with_int("age", 30)
            .with_bool("active", true)
            .with_node("profile", Node::new().with_text("bio", "Developer"))
            .with_list("tags", NodeList::new());

        assert_eq!(node.get_text("name"), Some("Alice"));
        assert_eq!(node.get_int("age"), Some(30));
        assert_eq!(node.get_bool("active"), Some(true));
        assert!(node.get_node("profile").is_some());
        assert!(node.get_list("tags").is_some());

        // Test nested access
        assert_eq!(node.get_text_at_path("profile.bio"), Some("Developer"));
    }

    #[test]
    fn test_node_clear() {
        let mut node = Node::new();

        node.set("name", "Alice");
        node.set("age", 30);

        assert_eq!(node.len(), 2);

        node.clear();

        assert!(node.is_empty());
        assert_eq!(node.len(), 0);
    }

    #[test]
    fn test_node_crdt_merge() {
        let mut node1 = Node::new();
        let mut node2 = Node::new();

        node1.set("name", "Alice");
        node1.set("age", 30);

        node2.set("name", "Bob"); // Conflict
        node2.set("city", "NYC");

        let merged = node1.merge(&node2).unwrap();

        assert_eq!(merged.get_text("name"), Some("Bob")); // Last write wins
        assert_eq!(merged.get_int("age"), Some(30));
        assert_eq!(merged.get_text("city"), Some("NYC"));
    }

    #[test]
    fn test_node_from_iterator() {
        let pairs = vec![
            ("name".to_string(), NodeValue::Text("Alice".to_string())),
            ("age".to_string(), NodeValue::Int(30)),
            ("active".to_string(), NodeValue::Bool(true)),
        ];

        let node: Node = pairs.into_iter().collect();

        assert_eq!(node.get_text("name"), Some("Alice"));
        assert_eq!(node.get_int("age"), Some(30));
        assert_eq!(node.get_bool("active"), Some(true));
    }

    #[test]
    fn test_list_position_ordering() {
        let pos1 = ListPosition::new(1, 2); // 0.5
        let pos2 = ListPosition::new(3, 4); // 0.75
        let pos3 = ListPosition::new(1, 1); // 1.0

        assert!(pos1 < pos2);
        assert!(pos2 < pos3);
        assert!(pos1 < pos3);

        // Test between
        let between = ListPosition::between(&pos1, &pos3);
        assert!(pos1 < between);
        assert!(between < pos3);
    }

    #[test]
    fn test_list_position_beginning_end() {
        let beginning = ListPosition::beginning();
        let end = ListPosition::end();
        let middle = ListPosition::new(100, 1);

        assert!(beginning < middle);
        assert!(middle < end);
        assert!(beginning < end);
    }

    // Legacy compatibility test (just one to verify the conversion works)
    #[test]
    fn test_cleaner_api_examples() {
        let mut node = Node::new();

        // Set some values
        node.set("name", "Alice");
        node.set("age", 30);
        node.set("active", true);

        // Old verbose way (still works)
        assert_eq!(node.get("name").and_then(|v| v.as_text()), Some("Alice"));
        assert_eq!(node.get("age").and_then(|v| v.as_int()), Some(30));
        assert_eq!(node.get("active").and_then(|v| v.as_bool()), Some(true));

        // New clean way with typed getters
        assert_eq!(node.get_text("name"), Some("Alice"));
        assert_eq!(node.get_int("age"), Some(30));
        assert_eq!(node.get_bool("active"), Some(true));

        // Even cleaner with direct comparisons on NodeValue!
        assert!(*node.get("name").unwrap() == "Alice");
        assert!(*node.get("age").unwrap() == 30);
        assert!(*node.get("active").unwrap() == true);

        // Path-based access
        node.set_path("user.profile.bio", "Developer").unwrap();

        // Old verbose way (still works)
        assert_eq!(
            node.get_path("user.profile.bio").and_then(|v| v.as_text()),
            Some("Developer")
        );

        // New clean way with typed getters
        assert_eq!(node.get_text_at_path("user.profile.bio"), Some("Developer"));

        // Even cleaner with direct comparisons on NodeValue!
        assert!(*node.get_path("user.profile.bio").unwrap() == "Developer");

        // Convenience methods for NodeValue
        let value = NodeValue::Text("hello".to_string());
        assert_eq!(value.as_text_or_empty(), "hello");

        let value = NodeValue::Int(42);
        assert_eq!(value.as_int_or_zero(), 42);
        assert!(!value.as_bool_or_false()); // not a bool, returns false
    }

    #[test]
    fn test_partial_eq_nodevalue() {
        let text_val = NodeValue::Text("hello".to_string());
        let int_val = NodeValue::Int(42);
        let bool_val = NodeValue::Bool(true);

        // Test NodeValue comparisons with primitive types
        assert!(text_val == "hello");
        assert!(text_val == "hello");
        assert!(int_val == 42i64);
        assert!(int_val == 42i32);
        assert!(int_val == 42u32);
        assert!(bool_val == true);

        // Test reverse comparisons
        assert!("hello" == text_val);
        assert!("hello" == text_val);
        assert!(42i64 == int_val);
        assert!(42i32 == int_val);
        assert!(42u32 == int_val);
        assert!(true == bool_val);

        // Test non-matching types
        assert!(!(text_val == 42));
        assert!(!(int_val == "hello"));
        assert!(!(bool_val == "hello"));
    }

    #[test]
    fn test_partial_eq_with_unwrap() {
        let mut node = Node::new();
        node.set("name", "Alice");
        node.set("age", 30);
        node.set("active", true);

        // Test NodeValue comparisons through unwrap
        assert!(*node.get("name").unwrap() == "Alice");
        assert!(*node.get("age").unwrap() == 30i64);
        assert!(*node.get("age").unwrap() == 30i32);
        assert!(*node.get("age").unwrap() == 30u32);
        assert!(*node.get("active").unwrap() == true);

        // Test reverse comparisons
        assert!("Alice" == *node.get("name").unwrap());
        assert!(30i64 == *node.get("age").unwrap());
        assert!(30i32 == *node.get("age").unwrap());
        assert!(30u32 == *node.get("age").unwrap());
        assert!(true == *node.get("active").unwrap());

        // Test non-matching types
        assert!(!(*node.get("name").unwrap() == 42));
        assert!(!(*node.get("age").unwrap() == "Alice"));
        assert!(!(*node.get("active").unwrap() == "Alice"));

        // Test with matches! macro for cleaner pattern
        assert!(matches!(node.get("name"), Some(v) if *v == "Alice"));
        assert!(matches!(node.get("age"), Some(v) if *v == 30));
        assert!(matches!(node.get("active"), Some(v) if *v == true));
        assert!(node.get("nonexistent").is_none());
    }

    #[test]
    fn test_node_array_serialization() {
        let mut node = Node::new();

        // Add an array element
        let result = node.array_add("fruits", Value::String("apple".to_string()));
        assert!(result.is_ok());

        // Check array length before serialization
        let length_before = node.array_len("fruits");
        assert_eq!(length_before, 1);

        // Serialize and deserialize
        let serialized = serde_json::to_string(&node).unwrap();
        let deserialized: Node = serde_json::from_str(&serialized).unwrap();

        // Check array length after deserialization
        let length_after = deserialized.array_len("fruits");
        assert_eq!(length_after, 1);

        // Check if they're equal
        assert_eq!(length_before, length_after);
    }

    // Additional tests for new API methods
    #[test]
    fn test_node_list_push_returns_index() {
        let mut list = NodeList::new();

        // Test push returns correct sequential indices
        let idx1 = list.push("first");
        let idx2 = list.push("second");
        let idx3 = list.push("third");

        assert_eq!(idx1, 0);
        assert_eq!(idx2, 1);
        assert_eq!(idx3, 2);
        assert_eq!(list.len(), 3);

        // Verify values are accessible by returned indices
        assert_eq!(list.get(idx1).unwrap().as_text(), Some("first"));
        assert_eq!(list.get(idx2).unwrap().as_text(), Some("second"));
        assert_eq!(list.get(idx3).unwrap().as_text(), Some("third"));
    }

    #[test]
    fn test_node_list_push_different_types() {
        let mut list = NodeList::new();

        let idx1 = list.push("hello");
        let idx2 = list.push(42);
        let idx3 = list.push(true);
        let idx4 = list.push(3.13); // Use non-pi value to avoid clippy warning

        assert_eq!(idx1, 0);
        assert_eq!(idx2, 1);
        assert_eq!(idx3, 2);
        assert_eq!(idx4, 3);

        assert_eq!(list.get(0).unwrap().as_text(), Some("hello"));
        assert_eq!(list.get(1).unwrap().as_int(), Some(42));
        assert_eq!(list.get(2).unwrap().as_bool(), Some(true));
        assert_eq!(list.get(3).unwrap().as_int(), Some(3)); // float converted to int
    }

    #[test]
    fn test_node_list_insert_at_valid_indices() {
        let mut list = NodeList::new();

        // Insert at beginning of empty list
        assert!(list.insert(0, "first").is_ok());
        assert_eq!(list.len(), 1);
        assert_eq!(list.get(0).unwrap().as_text(), Some("first"));

        // Insert at end
        assert!(list.insert(1, "last").is_ok());
        assert_eq!(list.len(), 2);
        assert_eq!(list.get(1).unwrap().as_text(), Some("last"));

        // Insert in middle
        assert!(list.insert(1, "middle").is_ok());
        assert_eq!(list.len(), 3);
        assert_eq!(list.get(0).unwrap().as_text(), Some("first"));
        assert_eq!(list.get(1).unwrap().as_text(), Some("middle"));
        assert_eq!(list.get(2).unwrap().as_text(), Some("last"));
    }

    #[test]
    fn test_node_list_insert_at_beginning() {
        let mut list = NodeList::new();

        list.push("second");
        list.push("third");

        // Insert at beginning
        assert!(list.insert(0, "first").is_ok());
        assert_eq!(list.len(), 3);
        assert_eq!(list.get(0).unwrap().as_text(), Some("first"));
        assert_eq!(list.get(1).unwrap().as_text(), Some("second"));
        assert_eq!(list.get(2).unwrap().as_text(), Some("third"));
    }

    #[test]
    fn test_node_list_insert_at_end() {
        let mut list = NodeList::new();

        list.push("first");
        list.push("second");

        // Insert at end (equivalent to push)
        assert!(list.insert(2, "third").is_ok());
        assert_eq!(list.len(), 3);
        assert_eq!(list.get(2).unwrap().as_text(), Some("third"));
    }

    #[test]
    fn test_node_list_insert_index_out_of_bounds() {
        let mut list = NodeList::new();

        // Insert beyond bounds in empty list
        let result = list.insert(1, "invalid");
        assert!(result.is_err());
        match result.unwrap_err() {
            CRDTError::ListIndexOutOfBounds { index, len } => {
                assert_eq!(index, 1);
                assert_eq!(len, 0);
            }
            _ => panic!("Expected ListIndexOutOfBounds error"),
        }

        // Add some items
        list.push("first");
        list.push("second");

        // Insert way beyond bounds
        let result = list.insert(10, "invalid");
        assert!(result.is_err());
        match result.unwrap_err() {
            CRDTError::ListIndexOutOfBounds { index, len } => {
                assert_eq!(index, 10);
                assert_eq!(len, 2);
            }
            _ => panic!("Expected ListIndexOutOfBounds error"),
        }
    }

    #[test]
    fn test_node_list_insert_mixed_with_push() {
        let mut list = NodeList::new();

        // Mix insert and push operations
        let idx1 = list.push("a");
        assert!(list.insert(1, "c").is_ok());
        assert!(list.insert(1, "b").is_ok());
        let idx4 = list.push("d");

        assert_eq!(idx1, 0);
        assert_eq!(idx4, 3);
        assert_eq!(list.len(), 4);

        // Verify order
        assert_eq!(list.get(0).unwrap().as_text(), Some("a"));
        assert_eq!(list.get(1).unwrap().as_text(), Some("b"));
        assert_eq!(list.get(2).unwrap().as_text(), Some("c"));
        assert_eq!(list.get(3).unwrap().as_text(), Some("d"));
    }

    #[test]
    fn test_node_list_insert_maintains_stable_ordering() {
        let mut list = NodeList::new();

        // Add initial items
        list.push("first");
        list.push("third");

        // Insert in middle
        assert!(list.insert(1, "second").is_ok());

        // Create another list with same operations
        let mut list2 = NodeList::new();
        list2.push("first");
        list2.push("third");
        assert!(list2.insert(1, "second").is_ok());

        // Both lists should have same order
        assert_eq!(list.len(), list2.len());
        for i in 0..list.len() {
            assert_eq!(
                list.get(i).unwrap().as_text(),
                list2.get(i).unwrap().as_text()
            );
        }
    }

    #[test]
    fn test_node_list_insert_with_nested_values() {
        let mut list = NodeList::new();

        // Insert nested structures
        let mut nested_node = Node::new();
        nested_node.set("name", "Alice");
        nested_node.set("age", 30);

        let mut nested_list = NodeList::new();
        nested_list.push(1);
        nested_list.push(2);
        nested_list.push(3);

        assert!(list.insert(0, nested_node).is_ok());
        assert!(list.insert(1, nested_list).is_ok());

        assert_eq!(list.len(), 2);
        assert!(list.get(0).unwrap().as_node().is_some());
        assert!(list.get(1).unwrap().as_list().is_some());

        // Verify nested content
        let node = list.get(0).unwrap().as_node().unwrap();
        assert_eq!(node.get_text("name"), Some("Alice"));
        assert_eq!(node.get_int("age"), Some(30));

        let inner_list = list.get(1).unwrap().as_list().unwrap();
        assert_eq!(inner_list.len(), 3);
        assert_eq!(inner_list.get(0).unwrap().as_int(), Some(1));
    }

    #[test]
    fn test_node_list_error_integration() {
        let mut list = NodeList::new();

        let result = list.insert(5, "test");
        assert!(result.is_err());

        let error = result.unwrap_err();
        assert!(error.is_list_error());
        assert!(!error.is_merge_error());
        assert!(!error.is_serialization_error());
        assert!(!error.is_type_error());
        assert!(!error.is_array_error());
        assert!(!error.is_map_error());
        assert!(!error.is_nested_error());
        assert!(!error.is_not_found_error());
    }

    #[test]
    fn test_node_list_push_after_removals() {
        let mut list = NodeList::new();

        // Add items
        list.push("a");
        list.push("b");
        list.push("c");

        // Remove middle item
        list.remove(1);
        assert_eq!(list.len(), 2);

        // Push should still return correct index
        let idx = list.push("d");
        assert_eq!(idx, 2);
        assert_eq!(list.len(), 3);
        assert_eq!(list.get(2).unwrap().as_text(), Some("d"));
    }

    #[test]
    fn test_node_list_insert_after_removals() {
        let mut list = NodeList::new();

        // Add items
        list.push("a");
        list.push("b");
        list.push("c");

        // Remove middle item
        list.remove(1);
        assert_eq!(list.len(), 2);

        // Insert should work correctly
        assert!(list.insert(1, "new").is_ok());
        assert_eq!(list.len(), 3);
        assert_eq!(list.get(1).unwrap().as_text(), Some("new"));
    }
}
