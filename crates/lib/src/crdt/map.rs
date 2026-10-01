//! Pointwise composition of ordered CRDT operations.

use std::collections::BTreeMap;

use serde::de::{DeserializeOwned, SeqAccess, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{CRDT, Data, Lww};

/// Per-key CRDT composition; a value inserted here is a **delta**, not
/// necessarily a replacement. There is no implicit deletion policy.
///
/// ```
/// use eidetica::crdt::{CRDT, Lww, Map};
/// let mut a = Map::new();
/// a.insert_delta("x".to_owned(), Lww::Set(1));
/// let mut b = Map::new();
/// b.insert_delta("x".to_owned(), Lww::Delete);
/// assert_eq!(a.merge(&b).unwrap().get(&"x".to_owned()), Some(&Lww::Delete));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Map<K, V> {
    entries: BTreeMap<K, V>,
}

impl<K: Ord, V> Default for Map<K, V> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }
}

impl<K: Ord, V> Map<K, V> {
    /// Construct an empty operation map.
    pub fn new() -> Self {
        Self::default()
    }
    /// Inspect a per-key operation (which may represent a tombstone).
    pub fn get(&self, key: &K) -> Option<&V> {
        self.entries.get(key)
    }
    /// Mutably inspect a per-key operation.
    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.entries.get_mut(key)
    }
    /// Test whether an operation is present for this key.
    pub fn contains_key(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }
    /// Visit operations in deterministic key order.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.iter()
    }
    /// Insert a per-key CRDT delta, replacing a previous operation in this map.
    pub fn insert_delta(&mut self, key: K, delta: V) -> Option<V> {
        self.entries.insert(key, delta)
    }
}

impl<K: Ord + Serialize, V: Serialize> Serialize for Map<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.entries.len()))?;
        for (key, value) in &self.entries {
            seq.serialize_element(&(key, value))?;
        }
        seq.end()
    }
}

impl<'de, K, V> Deserialize<'de> for Map<K, V>
where
    K: Ord + Deserialize<'de>,
    V: Deserialize<'de>,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entries<K, V>(std::marker::PhantomData<(K, V)>);
        impl<'de, K: Ord + Deserialize<'de>, V: Deserialize<'de>> Visitor<'de> for Entries<K, V> {
            type Value = Map<K, V>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a sequence of unique key/delta pairs")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut map = Map::new();
                while let Some((key, value)) = seq.next_element::<(K, V)>()? {
                    if map.entries.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate map key"));
                    }
                }
                Ok(map)
            }
        }
        deserializer.deserialize_seq(Entries(std::marker::PhantomData))
    }
}

impl<K, V> Data for Map<K, V>
where
    K: Ord + Clone + Serialize + DeserializeOwned,
    V: CRDT,
{
}
impl<K, V> CRDT for Map<K, V>
where
    K: Ord + Clone + Serialize + DeserializeOwned,
    V: CRDT,
{
    fn merge(&self, other: &Self) -> crate::Result<Self> {
        let mut merged = self.clone();
        for (key, delta) in &other.entries {
            let value = match merged.entries.get(key) {
                Some(current) => current.merge(delta)?,
                None => delta.clone(),
            };
            merged.entries.insert(key.clone(), value);
        }
        Ok(merged)
    }
}

/// A live-value map whose canonical operations are per-key [`Lww`] deltas.
/// Tombstones remain in reduction, but are hidden from normal iteration.
/// "Last" follows deterministic Entry order, not wall-clock time.
///
/// ```
/// use eidetica::crdt::{CRDT, LwwMap};
/// let mut first = LwwMap::new();
/// first.set("name".to_owned(), "before".to_owned());
/// let mut later = LwwMap::new();
/// later.delete("name".to_owned());
/// assert!(first.merge(&later).unwrap().get(&"name".to_owned()).is_none());
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LwwMap<K, V> {
    inner: Map<K, Lww<V>>,
}

impl<K: Ord, V> Default for LwwMap<K, V> {
    fn default() -> Self {
        Self { inner: Map::new() }
    }
}

impl<K: Ord, V> LwwMap<K, V> {
    /// Construct an empty live map.
    pub fn new() -> Self {
        Self::default()
    }
    /// Read a visible value.
    pub fn get(&self, key: &K) -> Option<&V> {
        match self.inner.get(key) {
            Some(Lww::Set(value)) => Some(value),
            _ => None,
        }
    }
    /// Check whether a key has a visible value.
    pub fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }
    /// Visit only visible values in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.inner.iter().filter_map(|(key, op)| match op {
            Lww::Set(v) => Some((key, v)),
            _ => None,
        })
    }
    /// Visit visible keys.
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.iter().map(|(key, _)| key)
    }
    /// Visit visible values.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, v)| v)
    }
    /// Count visible values.
    pub fn len(&self) -> usize {
        self.iter().count()
    }
    /// Check whether there are no visible values.
    pub fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }
    /// Stage a complete replacement for a key.
    pub fn set(&mut self, key: K, value: V) {
        self.inner.insert_delta(key, Lww::Set(value));
    }
    /// Stage a deletion for a key.
    pub fn delete(&mut self, key: K) {
        self.inner.insert_delta(key, Lww::Delete);
    }
    /// Inspect a key's canonical operation, including a tombstone.
    pub fn operation(&self, key: &K) -> Option<&Lww<V>> {
        self.inner.get(key)
    }
    /// Visit canonical operations, including tombstones.
    pub fn operations(&self) -> impl Iterator<Item = (&K, &Lww<V>)> {
        self.inner.iter()
    }
}

impl<K: Ord + Serialize, V: Serialize> Serialize for LwwMap<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.inner.serialize(serializer)
    }
}

impl<'de, K, V> Deserialize<'de> for LwwMap<K, V>
where
    K: Ord + Deserialize<'de>,
    V: Deserialize<'de>,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let inner = Map::<K, Lww<V>>::deserialize(deserializer)?;
        if inner
            .entries
            .values()
            .any(|value| matches!(value, Lww::NoOp))
        {
            return Err(serde::de::Error::custom(
                "keyed NoOp is not a canonical map operation",
            ));
        }
        Ok(Self { inner })
    }
}

impl<K, V> Data for LwwMap<K, V>
where
    K: Ord + Clone + Serialize + DeserializeOwned,
    V: Clone + Serialize + DeserializeOwned,
{
}
impl<K, V> CRDT for LwwMap<K, V>
where
    K: Ord + Clone + Serialize + DeserializeOwned,
    V: Clone + Serialize + DeserializeOwned,
{
    fn merge(&self, other: &Self) -> crate::Result<Self> {
        Ok(Self {
            inner: self.inner.merge(&other.inner)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_roundtrip_and_duplicate_rejection() {
        let mut map = LwwMap::new();
        map.set("beta", 2);
        map.set("alpha", 1);
        map.delete("beta");
        let json = serde_json::to_string(&map).unwrap();
        assert_eq!(json, r#"[["alpha",{"set":1}],["beta","delete"]]"#);
        assert_eq!(
            serde_json::from_str::<LwwMap<String, i32>>(&json)
                .unwrap()
                .len(),
            1
        );
        assert!(
            serde_json::from_str::<LwwMap<String, i32>>(r#"[["a","delete"],["a",{"set":1}]]"#)
                .is_err()
        );
        assert!(serde_json::from_str::<LwwMap<String, i32>>(r#"[["a","no_op"]]"#).is_err());
        let mut numeric = Map::new();
        numeric.insert_delta(3, Lww::Set(5));
        assert_eq!(
            serde_json::from_str::<Map<i32, Lww<i32>>>(&serde_json::to_string(&numeric).unwrap())
                .unwrap(),
            numeric
        );
    }

    #[test]
    fn reference_projection_obeys_identity_merge_and_composition() {
        use crate::crdt::CanonicalJson;
        type Delta = LwwMap<String, CanonicalJson>;
        fn apply(rows: &mut BTreeMap<Vec<u8>, Vec<u8>>, delta: &Delta) {
            for (key, operation) in delta.operations() {
                match operation {
                    Lww::Set(value) => {
                        rows.insert(key.as_bytes().to_vec(), value.as_bytes().to_vec());
                    }
                    Lww::Delete => {
                        rows.remove(key.as_bytes());
                    }
                    Lww::NoOp => unreachable!("keyed NoOp is rejected"),
                }
            }
        }
        let empty = Delta::new();
        let mut set = Delta::new();
        set.set(
            "a".into(),
            CanonicalJson::parse(br#"{"b":2,"a":1}"#).unwrap(),
        );
        set.set("a.b".into(), CanonicalJson::parse(b"true").unwrap());
        set.set("".into(), CanonicalJson::parse(b"null").unwrap());
        let mut delete = Delta::new();
        delete.delete("a".into());
        let mut resurrect = Delta::new();
        resurrect.set("a".into(), CanonicalJson::parse(b"3").unwrap());
        for first in [&empty, &set, &delete, &resurrect] {
            for second in [&empty, &set, &delete, &resurrect] {
                let merged = first.merge(second).unwrap();
                let mut sequential = BTreeMap::new();
                apply(&mut sequential, first);
                apply(&mut sequential, second);
                let mut collapsed = BTreeMap::new();
                apply(&mut collapsed, &merged);
                assert_eq!(sequential, collapsed);
                let mut previous = BTreeMap::from([(b"keep".to_vec(), b"old".to_vec())]);
                let mut from_merged = previous.clone();
                apply(&mut previous, first);
                apply(&mut previous, second);
                apply(&mut from_merged, &merged);
                assert_eq!(previous, from_merged);
            }
        }
        let mut unchanged = BTreeMap::from([(b"keep".to_vec(), b"old".to_vec())]);
        apply(&mut unchanged, &empty);
        assert_eq!(unchanged.get(b"keep".as_slice()), Some(&b"old".to_vec()));
    }

    #[test]
    fn merge_laws_and_tombstones() {
        let mut a = LwwMap::new();
        a.set("a".to_owned(), 1);
        a.set("a.b".to_owned(), 2);
        let mut b = LwwMap::new();
        b.delete("a".to_owned());
        let mut c = LwwMap::new();
        c.set("a".to_owned(), 3);
        let empty = LwwMap::new();
        assert_eq!(a.merge(&empty).unwrap(), a);
        assert_eq!(empty.merge(&a).unwrap(), a);
        assert_eq!(
            a.merge(&b).unwrap().merge(&c).unwrap(),
            a.merge(&b.merge(&c).unwrap()).unwrap()
        );
        assert_eq!(a.merge(&b).unwrap().get(&"a".to_owned()), None);
        assert_eq!(a.merge(&b).unwrap().get(&"a.b".to_owned()), Some(&2));
        assert_eq!(a.merge(&b).unwrap().operations().count(), 2);
        assert_eq!(a.merge(&b).unwrap().iter().count(), 1);
        assert_eq!(
            a.merge(&b).unwrap().merge(&c).unwrap().get(&"a".to_owned()),
            Some(&3)
        );
    }
}
