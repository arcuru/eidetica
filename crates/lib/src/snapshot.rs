//! Snapshot — an immutable identifier of a database state at a point in time.
//!
//! A `Snapshot` is the canonical (sorted, deduplicated) set of DAG tip IDs
//! that fully identifies the state of a `Database` or a `Store` within one.
//! Content-addressing makes the mapping bijective: given a snapshot and the
//! database root it belongs to, the entries (and therefore all reachable
//! content) are uniquely determined.
//!
//! Use a `Snapshot` to pin a read view, anchor a transaction, or describe
//! a state transition (e.g. `WriteEvent { from, to }`).
//!
//! `Snapshot` is intentionally scope-free: it carries *only* a set of tips.
//! The scope — a database root, or a `(database root, store name)` pair — is
//! contextual and supplied alongside the snapshot at API boundaries. A
//! snapshot can therefore describe either a database state or a store state;
//! the distinction lives in the API method consuming it, not on the snapshot
//! itself. Keeping `Snapshot` as just a canonical tip-set means no optional
//! fields, no runtime "is the root set?" checks, and a wire format identical
//! to `Vec<ID>`.

use serde::{Deserialize, Serialize, Serializer};

use crate::entry::ID;

/// Identifier for a database state — a sorted, deduplicated set of DAG tips.
///
/// Equality and hashing are set-equality on the tips. Serialization is
/// transparent: the wire form is a bare array of IDs, identical to a
/// `Vec<ID>`. Deserialization normalizes (sort + dedup), so unsorted or
/// duplicated wire input is canonicalized on read.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Sorted, deduplicated set of tip IDs.
    tips: Vec<ID>,
}

impl Serialize for Snapshot {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.tips.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Snapshot {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let tips = Vec::<ID>::deserialize(deserializer)?;
        Ok(Self::new(tips))
    }
}

impl Snapshot {
    /// A snapshot containing no tips — the state of a database with no entries.
    pub const EMPTY: Snapshot = Snapshot { tips: Vec::new() };

    /// Construct a snapshot from a vector of tips.
    ///
    /// Tips are sorted and deduplicated.
    pub fn new(mut tips: Vec<ID>) -> Self {
        tips.sort();
        tips.dedup();
        Self { tips }
    }

    /// Borrow the tips as a sorted, deduplicated slice.
    pub fn tips(&self) -> &[ID] {
        &self.tips
    }

    /// Consume the snapshot and return the underlying tips.
    pub fn into_tips(self) -> Vec<ID> {
        self.tips
    }

    /// Returns true if this snapshot contains no tips.
    pub fn is_empty(&self) -> bool {
        self.tips.is_empty()
    }

    /// Number of tips in this snapshot.
    pub fn len(&self) -> usize {
        self.tips.len()
    }
}

/// Equality is set-equality on tips.
impl PartialEq for Snapshot {
    fn eq(&self, other: &Self) -> bool {
        // Set-equality holds because `tips` is canonical (sorted + deduped on construction).
        self.tips == other.tips
    }
}

impl Eq for Snapshot {}

impl std::hash::Hash for Snapshot {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.tips.hash(state);
    }
}

impl From<Vec<ID>> for Snapshot {
    fn from(tips: Vec<ID>) -> Self {
        Self::new(tips)
    }
}

impl From<&[ID]> for Snapshot {
    fn from(tips: &[ID]) -> Self {
        Self::new(tips.to_vec())
    }
}

impl<const N: usize> From<[ID; N]> for Snapshot {
    fn from(tips: [ID; N]) -> Self {
        Self::new(tips.to_vec())
    }
}

impl AsRef<[ID]> for Snapshot {
    fn as_ref(&self) -> &[ID] {
        &self.tips
    }
}

impl<'a> IntoIterator for &'a Snapshot {
    type Item = &'a ID;
    type IntoIter = std::slice::Iter<'a, ID>;

    fn into_iter(self) -> Self::IntoIter {
        self.tips.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> ID {
        ID::from_bytes([byte])
    }

    #[test]
    fn empty_const_is_empty() {
        assert!(Snapshot::EMPTY.is_empty());
        assert_eq!(Snapshot::EMPTY.len(), 0);
        assert_eq!(Snapshot::EMPTY.tips(), &[] as &[ID]);
    }

    #[test]
    fn default_equals_empty() {
        assert_eq!(Snapshot::default(), Snapshot::EMPTY);
    }

    #[test]
    fn new_sorts_input() {
        let a = id(1);
        let b = id(2);
        let c = id(3);
        let unsorted = Snapshot::new(vec![c.clone(), a.clone(), b.clone()]);
        let sorted = Snapshot::new(vec![a, b, c]);
        assert_eq!(unsorted.tips(), sorted.tips());
    }

    #[test]
    fn new_dedups_input() {
        let a = id(1);
        let b = id(2);
        let with_dupes = Snapshot::new(vec![a.clone(), b.clone(), a.clone(), b.clone(), a.clone()]);
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(with_dupes.len(), 2);
        assert_eq!(with_dupes.tips(), expected.as_slice());
    }

    #[test]
    fn set_equality_holds_via_canonical_form() {
        let a = id(1);
        let b = id(2);
        let s1: Snapshot = vec![a.clone(), b.clone()].into();
        let s2: Snapshot = vec![b, a].into();
        assert_eq!(s1, s2);
    }

    #[test]
    fn hash_matches_for_set_equal_snapshots() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let a = id(1);
        let b = id(2);
        let s1: Snapshot = vec![a.clone(), b.clone()].into();
        let s2: Snapshot = vec![b, a].into();
        let mut h1 = DefaultHasher::new();
        s1.hash(&mut h1);
        let mut h2 = DefaultHasher::new();
        s2.hash(&mut h2);
        assert_eq!(h1.finish(), h2.finish());
    }

    #[test]
    fn from_slice_sorts_and_dedups() {
        let a = id(1);
        let b = id(2);
        let snap = Snapshot::from(&[b.clone(), a.clone(), a.clone()][..]);
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(snap.tips(), expected.as_slice());
    }

    #[test]
    fn from_array_sorts_and_dedups() {
        let a = id(1);
        let b = id(2);
        let snap = Snapshot::from([b.clone(), a.clone(), a.clone()]);
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(snap.tips(), expected.as_slice());
    }

    #[test]
    fn into_tips_returns_sorted_vec() {
        let mut expected = vec![id(1), id(2), id(3)];
        expected.sort();
        let snap = Snapshot::new(vec![id(3), id(1), id(2)]);
        assert_eq!(snap.into_tips(), expected);
    }

    #[test]
    fn iter_yields_sorted_tips() {
        let mut expected = [id(1), id(2), id(3)];
        expected.sort();
        let snap = Snapshot::new(vec![id(3), id(1), id(2)]);
        let collected: Vec<&ID> = (&snap).into_iter().collect();
        let expected_refs: Vec<&ID> = expected.iter().collect();
        assert_eq!(collected, expected_refs);
    }

    #[test]
    fn as_ref_exposes_sorted_slice() {
        let a = id(1);
        let b = id(2);
        let snap = Snapshot::new(vec![b.clone(), a.clone()]);
        let slice: &[ID] = snap.as_ref();
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(slice, expected.as_slice());
    }

    #[test]
    fn serde_roundtrip_preserves_invariant() {
        let a = id(1);
        let b = id(2);
        let snap = Snapshot::new(vec![b, a]);
        let json = serde_json::to_string(&snap).unwrap();
        let parsed: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, snap);
    }

    #[test]
    fn deserialize_normalizes_unsorted_wire_data() {
        // Simulate wire data written without the sorted invariant
        // (e.g. by an older client). The Snapshot deserializer must canonicalize.
        let a = id(1);
        let b = id(2);
        let canonical = Snapshot::new(vec![a.clone(), b.clone()]);
        let unsorted_json = serde_json::to_string(&vec![b, a]).unwrap();
        let parsed: Snapshot = serde_json::from_str(&unsorted_json).unwrap();
        assert_eq!(parsed, canonical);
    }

    #[test]
    fn deserialize_dedups_wire_data() {
        let a = id(1);
        let b = id(2);
        let canonical = Snapshot::new(vec![a.clone(), b.clone()]);
        let duped_json = serde_json::to_string(&vec![a.clone(), b, a]).unwrap();
        let parsed: Snapshot = serde_json::from_str(&duped_json).unwrap();
        assert_eq!(parsed, canonical);
    }

    #[test]
    fn serializes_as_bare_id_array() {
        // Snapshot must be wire-compatible with Vec<ID> — same JSON shape.
        // This matters for in-place migration of fields that were previously
        // typed `Vec<ID>` (e.g. EntryMetadata.settings_tips).
        let a = id(1);
        let b = id(2);
        let snap = Snapshot::new(vec![a.clone(), b.clone()]);
        let snap_json = serde_json::to_string(&snap).unwrap();
        let vec_json = serde_json::to_string(snap.tips()).unwrap();
        assert_eq!(snap_json, vec_json);
    }
}
