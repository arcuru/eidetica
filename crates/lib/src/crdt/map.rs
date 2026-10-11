//! Pointwise composition of ordered CRDT operations.

use std::{
    borrow::Borrow,
    collections::{BTreeMap, btree_map::Entry},
};

use super::CRDT;

/// Per-key CRDT composition over ordered keys.
///
/// An empty map is the identity. Merging preserves one-sided keys and calls
/// `V::merge` once for each overlapping key, in left-to-right operation order.
/// All value-level operations, including tombstones, remain accessible; this
/// map defines no deletion or row lifecycle policy. Keys are literal, not paths.
///
/// Use [`Self::insert_delta`] to compose repeated updates to a key. The
/// low-level [`Self::insert`] instead replaces the stored operation verbatim.
/// Values need not implement `Default` or encoding traits. This type has no
/// built-in [`super::Codec`] or Serde implementation.
///
/// ```
/// use eidetica::crdt::{CRDT, Lww, Map};
///
/// #[derive(Clone, Debug, PartialEq)]
/// struct Payload(u8);
///
/// let mut first = Map::new();
/// first.insert_delta("key".to_owned(), Lww::Set(Payload(1)))?;
/// let mut later = Map::new();
/// later.insert_delta("key".to_owned(), Lww::Delete)?;
/// let merged = first.merge(&later)?;
/// assert_eq!(merged.get("key"), Some(&Lww::Delete));
/// assert_eq!(merged.len(), 1); // The tombstone is still present.
/// # Ok::<(), eidetica::Error>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Map<K, V> {
    entries: BTreeMap<K, V>,
}

impl<K, V> Default for Map<K, V> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }
}

impl<K, V> Map<K, V> {
    /// Construct an empty operation map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Visit all stored operations in key order, including tombstones.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.iter()
    }

    /// Count stored operations, including tombstones.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check whether there are no stored operations.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<K: Ord, V> Map<K, V> {
    /// Inspect a per-key operation, which may represent a tombstone.
    pub fn get<Q: Ord + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.entries.get(key)
    }

    /// Replace a stored operation verbatim, returning the previous operation.
    ///
    /// This is literal backing-map insertion, not CRDT composition. Use
    /// [`Self::insert_delta`] for updates that must retain preceding deltas.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.entries.insert(key, value)
    }
}

impl<K: Ord, V: CRDT> Map<K, V> {
    /// Compose a delta after the current operation for this key.
    ///
    /// A missing key stores the delta as-is; an existing key uses `V::merge`
    /// exactly once. On a merge error, the stored operation is unchanged.
    /// Neither keys nor values need to implement `Clone` for insertion.
    pub fn insert_delta(&mut self, key: K, delta: V) -> crate::Result<()> {
        match self.entries.entry(key) {
            Entry::Occupied(mut entry) => {
                let composed = entry.get().merge(&delta)?;
                entry.insert(composed);
            }
            Entry::Vacant(entry) => {
                entry.insert(delta);
            }
        }
        Ok(())
    }
}

impl<K: Ord + Clone, V: CRDT + Clone> CRDT for Map<K, V> {
    fn merge(&self, other: &Self) -> crate::Result<Self> {
        let mut merged = self.clone();
        for (key, delta) in &other.entries {
            merged.insert_delta(key.clone(), delta.clone())?;
        }
        Ok(merged)
    }
}
