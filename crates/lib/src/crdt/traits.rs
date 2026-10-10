//! Core traits for CRDT (Conflict-free Replicated Data Type) implementations.
//!
//! This module defines the fundamental traits that all CRDT implementations must satisfy:
//! - `Codec`: The explicit byte-encoding contract for Store data
//! - `CRDT`: The core trait defining merge semantics for conflict resolution

use crate::Result;

/// Complete operation/state encoding at a Store's persistence boundary.
///
/// Implementations choose a stable byte format without performing storage I/O.
/// Decoding an encoded value must preserve all state affecting future merges,
/// including tombstones and default/identity behavior. Decoders must consume a
/// complete value and reject malformed or trailing data rather than returning
/// defaults. A durable format change requires a new Store type identity.
///
/// Encoding is independent of cloning and Serde. There is no blanket Serde
/// implementation, so custom binary formats can implement this trait directly.
///
/// # Examples
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
/// let state = MyData { value: "example".into() };
/// assert_eq!(MyData::decode(&state.encode()?)?.value, state.value);
/// # Ok::<(), eidetica::Error>(())
/// ```
pub trait Codec: Sized {
    /// Encode all merge-relevant operation/state data.
    fn encode(&self) -> Result<Vec<u8>>;
    /// Decode exactly one complete operation/state value.
    fn decode(bytes: &[u8]) -> Result<Self>;
}

/// A trait for Conflict-free Replicated Data Types (CRDTs).
///
/// CRDTs are data structures that can be replicated across multiple nodes and automatically
/// resolve conflicts without requiring coordination between nodes. They guarantee that
/// concurrent updates can be merged deterministically, ensuring eventual consistency.
///
/// Algebraic composition requires no encoding or Serde traits. Store data also
/// implements [`Codec`] at the persistence boundary.
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
pub trait CRDT: Clone + Default {
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
