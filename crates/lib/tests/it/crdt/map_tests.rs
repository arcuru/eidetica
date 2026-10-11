//! Pointwise laws and delta composition through the public Map API.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use eidetica::{
    Error, Result,
    crdt::{CRDT, CRDTError, Doc, Lww, Map},
};

// Concatenation is associative, but neither commutative nor idempotent.
// No Default, Codec or Serde: Map must not assume replacement-only values.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Sequence(Vec<i32>);

impl CRDT for Sequence {
    fn merge(&self, other: &Self) -> Result<Self> {
        Ok(Self([self.0.as_slice(), other.0.as_slice()].concat()))
    }
}

fn sequence_map(entries: &[(&str, &[i32])]) -> Map<String, Sequence> {
    let mut map = Map::new();
    for (key, values) in entries {
        map.insert((*key).to_owned(), Sequence(values.to_vec()));
    }
    map
}

#[test]
fn test_map_exhaustive_identity_and_associativity_with_custom_deltas() {
    let maps = [
        sequence_map(&[]),
        sequence_map(&[("x", &[])]),
        sequence_map(&[("x", &[1])]),
        sequence_map(&[("x", &[2])]),
        sequence_map(&[("y", &[3])]),
        sequence_map(&[("x", &[4]), ("y", &[5])]),
    ];
    for a in &maps {
        assert_eq!(a.merge(&Map::default()).unwrap(), *a);
        assert_eq!(Map::default().merge(a).unwrap(), *a);
        for b in &maps {
            for c in &maps {
                let left = a.merge(b).unwrap().merge(c).unwrap();
                let right = a.merge(&b.merge(c).unwrap()).unwrap();
                assert_eq!(left, right, "{a:?}, {b:?}, {c:?}");
                // Independent oracle retains every per-key event in input order.
                for key in ["x", "y"] {
                    let expected: Vec<_> = [a, b, c]
                        .into_iter()
                        .filter_map(|map| map.get(key))
                        .flat_map(|value| value.0.iter().copied())
                        .collect();
                    let present = [a, b, c].into_iter().any(|map| map.get(key).is_some());
                    assert_eq!(left.get(key), present.then_some(&Sequence(expected)));
                }
            }
        }
    }
}

#[test]
fn test_map_overlapping_disjoint_keys_and_order() {
    let a = sequence_map(&[("shared", &[1]), ("left", &[2]), ("", &[3])]);
    let b = sequence_map(&[("shared", &[4]), ("right", &[5]), ("a.b", &[6])]);
    let merged = a.merge(&b).unwrap();
    assert_eq!(
        merged,
        sequence_map(&[
            ("shared", &[1, 4]),
            ("left", &[2]),
            ("", &[3]),
            ("right", &[5]),
            ("a.b", &[6]),
        ])
    );
    assert_eq!(
        merged.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        ["", "a.b", "left", "right", "shared"]
    );
    assert_eq!(a.get("shared"), Some(&Sequence(vec![1])));
    assert_eq!(b.get("shared"), Some(&Sequence(vec![4])));
    assert_eq!(merged.get("missing"), None);
    assert_ne!(merged, b.merge(&a).unwrap());
    assert_eq!(
        a.merge(&a).unwrap().get("shared"),
        Some(&Sequence(vec![1, 1]))
    );
}

#[test]
fn test_map_repeated_insert_delta_composes_not_replaces() {
    let mut staged = sequence_map(&[]);
    staged
        .insert_delta("key".into(), Sequence(vec![1]))
        .unwrap();
    staged
        .insert_delta("key".into(), Sequence(vec![2]))
        .unwrap();
    staged
        .insert_delta("key".into(), Sequence(vec![3]))
        .unwrap();
    assert_eq!(staged.get("key"), Some(&Sequence(vec![1, 2, 3])));
    let composed = sequence_map(&[("key", &[1])])
        .merge(&sequence_map(&[("key", &[2])]))
        .unwrap()
        .merge(&sequence_map(&[("key", &[3])]))
        .unwrap();
    assert_eq!(staged, composed);

    // Literal insertion is deliberately a different, low-level operation.
    assert_eq!(
        staged.insert("key".into(), Sequence(vec![9])),
        Some(Sequence(vec![1, 2, 3]))
    );
    assert_eq!(staged.get("key"), Some(&Sequence(vec![9])));
}

#[test]
fn test_map_retains_lww_noop_and_tombstones() {
    let mut first = Map::new();
    first.insert_delta("gone", Lww::Set(1)).unwrap();
    first.insert_delta("keep", Lww::Set(2)).unwrap();
    first.insert_delta("identity", Lww::NoOp).unwrap();
    let mut later = Map::new();
    later.insert_delta("gone", Lww::Delete).unwrap();
    later.insert_delta("keep", Lww::NoOp).unwrap();
    later.insert_delta("absent", Lww::Delete).unwrap();
    let merged = first.merge(&later).unwrap();
    assert_eq!(merged.get("gone"), Some(&Lww::Delete));
    assert_eq!(merged.get("absent"), Some(&Lww::Delete));
    assert_eq!(merged.get("identity"), Some(&Lww::NoOp));
    assert_eq!(merged.get("keep"), Some(&Lww::Set(2)));
    assert_eq!(merged.len(), 4);
    assert_eq!(merged.iter().count(), 4);
    assert!(!merged.is_empty());
    assert!(Map::<String, Lww<i32>>::new().is_empty());
}

#[test]
fn test_map_nested_composition() {
    let mut a = Map::new();
    a.insert("group", sequence_map(&[("x", &[1]), ("left", &[2])]));
    let mut b = Map::new();
    b.insert("group", sequence_map(&[("x", &[3]), ("right", &[4])]));
    b.insert("other", sequence_map(&[("z", &[5])]));
    let mut c = Map::new();
    c.insert("group", sequence_map(&[("x", &[6])]));
    let merged = a.merge(&b).unwrap().merge(&c).unwrap();
    assert_eq!(merged, a.merge(&b.merge(&c).unwrap()).unwrap());
    assert_eq!(
        merged.get("group"),
        Some(&sequence_map(&[
            ("x", &[1, 3, 6]),
            ("left", &[2]),
            ("right", &[4])
        ]))
    );
    assert_eq!(merged.get("other"), b.get("other"));
    a.insert_delta("group", b.get("group").unwrap().clone())
        .unwrap();
    a.insert_delta("group", c.get("group").unwrap().clone())
        .unwrap();
    assert_eq!(a.get("group"), merged.get("group"));
}

#[test]
fn test_map_doc_structural_and_atomic_composition() {
    let mut before = Doc::new();
    before.set("old", 1);
    before.set("shared", 2);
    let mut delta = Doc::new();
    delta.set("new", 3);
    delta.set("shared", 4);
    let mut reset = Doc::atomic();
    reset.set("reset", 5);
    let mut a = Map::new();
    a.insert("row", before);
    let mut b = Map::new();
    b.insert("row", delta);
    let mut c = Map::new();
    c.insert("row", reset.clone());
    let structural = a.merge(&b).unwrap();
    let row = structural.get("row").unwrap();
    assert_eq!(row.get_as::<i64>("old"), Some(1));
    assert_eq!(row.get_as::<i64>("new"), Some(3));
    assert_eq!(row.get_as::<i64>("shared"), Some(4));
    assert_eq!(structural.merge(&c).unwrap().get("row"), Some(&reset));
    assert_eq!(
        structural.merge(&c).unwrap(),
        a.merge(&b.merge(&c).unwrap()).unwrap()
    );
}

#[derive(Clone, Debug)]
struct MergeProbe {
    calls: Arc<AtomicUsize>,
    reject: bool,
}

impl CRDT for MergeProbe {
    fn merge(&self, other: &Self) -> Result<Self> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.reject || other.reject {
            return Err(CRDTError::MergeFailed {
                reason: "probe rejection".into(),
            }
            .into());
        }
        Ok(self.clone())
    }
}

fn assert_probe_error(error: Error) {
    match error {
        Error::CRDT(error) => assert!(
            matches!(*error, CRDTError::MergeFailed { ref reason } if reason == "probe rejection")
        ),
        error => panic!("unexpected error: {error}"),
    }
}

#[test]
fn test_map_merges_only_overlapping_keys_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let probe = MergeProbe {
        calls: calls.clone(),
        reject: false,
    };
    let mut a = Map::new();
    a.insert("shared", probe.clone());
    a.insert("left", probe.clone());
    let mut b = Map::new();
    b.insert("shared", probe.clone());
    b.insert("right", probe);
    let merged = a.merge(&b).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(merged.len(), 3);
    assert!(merged.get("left").is_some());
    assert!(merged.get("right").is_some());
    a.merge(&Map::new()).unwrap();
    Map::new().merge(&b).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn test_map_propagates_errors_and_retains_stored_operation() {
    let calls = Arc::new(AtomicUsize::new(0));
    let accepted = MergeProbe {
        calls: calls.clone(),
        reject: false,
    };
    let rejected = MergeProbe {
        calls: calls.clone(),
        reject: true,
    };
    let mut a = Map::new();
    a.insert_delta("shared", accepted.clone()).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let mut b = Map::new();
    b.insert("shared", rejected.clone());
    assert_probe_error(a.merge(&b).unwrap_err());
    assert!(!a.get("shared").unwrap().reject);
    assert!(b.get("shared").unwrap().reject);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_probe_error(a.insert_delta("shared", rejected.clone()).unwrap_err());
    assert!(!a.get("shared").unwrap().reject);
    assert_eq!(a.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // A one-sided value is retained without calling its fallible merge.
    a.insert_delta("one-sided", rejected).unwrap();
    assert!(
        Map::new()
            .merge(&a)
            .unwrap()
            .get("one-sided")
            .unwrap()
            .reject
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn test_map_construction_and_insertion_without_clone_or_encoding() {
    #[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
    struct Key(u8);
    #[derive(Debug, PartialEq, Eq)]
    struct Delta(u32);
    impl CRDT for Delta {
        fn merge(&self, other: &Self) -> Result<Self> {
            Ok(Self(self.0 + other.0))
        }
    }
    let mut map = Map::new();
    map.insert_delta(Key(1), Delta(2)).unwrap();
    map.insert_delta(Key(1), Delta(3)).unwrap();
    assert_eq!(map.get(&Key(1)), Some(&Delta(5)));
    assert_eq!(map.insert(Key(1), Delta(7)), Some(Delta(5)));
    assert_eq!(map.len(), 1);

    // Empty construction/access imposes no Ord or value trait requirement.
    struct Unordered;
    let empty = Map::<Unordered, Unordered>::default();
    assert!(empty.is_empty());
    assert_eq!(empty.len(), 0);
    assert_eq!(empty.iter().count(), 0);
}
