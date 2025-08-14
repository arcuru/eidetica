//! Map-based CRDT implementation.
//!
//! This module provides a Map CRDT that aligns with Eidetica's tree-based architecture,
//! replacing the legacy "Nested/Node" implementation with cleaner semantics and better performance.
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
//! - **Structural merging** for nested maps and lists
//! - **Tombstone deletion** for preserving CRDT merge semantics
//! - **Stable ordering** for lists using rational number positions
//!
//! ## List Ordering with Rational Numbers
//! The [`List`] uses Rational Numbers to maintain stable ordering across
//! concurrent insertions. Instead of traditional indices, each list item
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
//! # use eidetica::crdt::{Doc, map::Value};
//! let mut map = Doc::new();
//! map.set("name", "Alice");
//!
//! // Traditional approach
//! let name = map.get("name").and_then(|v| v.as_text());
//! ```
//!
//! ## Level 2: Typed Getters
//! ```
//! # use eidetica::crdt::Doc;
//! # let mut map = Doc::new();
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
//! # use eidetica::crdt::Doc;
//! # let mut map = Doc::new();
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

mod implementation;
pub mod list;
mod tests;

pub use implementation::{List, Node, Value};
