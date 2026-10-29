//! Core traits for CRDT (Conflict-free Replicated Data Type) implementations.
//!
//! Encoding and merging are independent contracts:
//! - `Codec`: Explicit byte encoding at persistence boundaries
//! - `CRDT`: The core trait defining merge semantics for conflict resolution

use crate::Result;

/// Explicit byte encoding for data at a Store's persistence boundary.
///
/// Encoding is independent of merging, cloning and Serde. Implementations must
/// preserve all state needed by future merges, including tombstones, and decode
/// one complete value rather than silently accepting malformed or trailing data.
/// Existing Store formats must remain readable; changing a durable format needs
/// a separate compatibility decision.
///
/// There is no blanket Serde implementation. A type can use JSON explicitly:
///
/// ```
/// use eidetica::{crdt::Codec, Result};
///
/// #[derive(serde::Serialize, serde::Deserialize)]
/// struct MyData { value: String }
///
/// impl Codec for MyData {
///     fn encode(&self) -> Result<Vec<u8>> {
///         Ok(serde_json::to_vec(self)?)
///     }
///     fn decode(bytes: &[u8]) -> Result<Self> {
///         Ok(serde_json::from_slice(bytes)?)
///     }
/// }
/// let data = MyData { value: "example".into() };
/// assert_eq!(MyData::decode(&data.encode()?)?.value, data.value);
/// # Ok::<(), eidetica::Error>(())
/// ```
pub trait Codec: Sized {
    /// Encode a complete value, including all merge-relevant state.
    fn encode(&self) -> Result<Vec<u8>>;
    /// Decode one complete value, rejecting malformed or trailing data.
    fn decode(bytes: &[u8]) -> Result<Self>;
}

/// A trait for Conflict-free Replicated Data Types (CRDTs).
///
/// CRDTs are data structures that can be replicated across multiple nodes and automatically
/// resolve conflicts without requiring coordination between nodes. They guarantee that
/// concurrent updates can be merged deterministically, ensuring eventual consistency.
///
/// This trait only defines merging. Store data also implements [`Codec`] and
/// `Default` where persistence and history reconstruction require them; merging
/// alone does not require encoding, Serde, cloning or a default value.
///
/// # Examples
///
/// ```
/// use eidetica::crdt::{CRDT, Doc};
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
pub trait CRDT: Sized {
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
    fn merge(&self, other: &Self) -> Result<Self>;
}
