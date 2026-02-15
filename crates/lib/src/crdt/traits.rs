//! Core traits for CRDT (Conflict-free Replicated Data Type) implementations.
//!
//! This module defines the fundamental traits that all CRDT implementations must satisfy:
//! - `Data`: A marker trait for types that can be stored in Eidetica
//! - `CRDT`: The core trait defining merge semantics for conflict resolution

use crate::Result;

/// Marker trait for data types that can be stored in Eidetica.
///
/// This trait requires serialization capabilities and cloning for data structures
/// that can be stored in the Eidetica database. All storable types must support
/// JSON serialization/deserialization and cloning for efficient data operations.
///
/// Implementing this trait signifies that a type can be safely used as the data component
/// of an Entry in the database.
///
/// # Examples
///
/// ```
/// use eidetica::crdt::Data;
///
/// #[derive(Clone, serde::Serialize, serde::Deserialize)]
/// struct MyData {
///     value: String,
/// }
///
/// impl Data for MyData {}
/// ```
pub trait Data: Clone + serde::Serialize + serde::de::DeserializeOwned {}

/// A trait for Conflict-free Replicated Data Types (CRDTs).
///
/// CRDTs are data structures that can be replicated across multiple nodes and automatically
/// resolve conflicts without requiring coordination between nodes. They guarantee that
/// concurrent updates can be merged deterministically, ensuring eventual consistency.
///
/// All CRDT types must also implement the `Data` trait, ensuring they can be stored
/// and serialized within the Eidetica database.
///
/// # Examples
///
/// ```
/// use eidetica::crdt::{CRDT, Data, Doc};
/// use eidetica::Result;
///
/// let mut kv1 = Doc::new();
/// kv1.set("key", "value1");
///
/// let mut kv2 = Doc::new();
/// kv2.set("key", "value2");
///
/// let merged = kv1.merge(&kv2).unwrap();
/// // Doc uses last-write-wins semantics for scalar values
/// ```
pub trait CRDT: Data + Default {
    /// Merge this CRDT with another instance, returning a new merged instance.
    ///
    /// This operation must be:
    /// - **Associative**: `(a.merge(b)).merge(c) == a.merge(b.merge(c))`
    ///
    /// Unlike traditional state-based CRDTs (which require a join-semilattice with
    /// commutativity and idempotency), Eidetica's Merkle-CRDT design relaxes these
    /// requirements. The Merkle DAG provides deterministic traversal order, eliminating
    /// the need for commutativity, and ensures each entry is applied exactly once,
    /// eliminating the need for idempotency.
    ///
    /// # Arguments
    ///
    /// * `other` - The other CRDT instance to merge with
    ///
    /// # Returns
    ///
    /// A new CRDT instance representing the merged state, or an error if the merge fails.
    fn merge(&self, other: &Self) -> Result<Self>
    where
        Self: Sized;
}
