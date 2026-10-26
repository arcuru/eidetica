//! Pointwise laws, delta composition and encoding through the public Map API.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use eidetica::{
    Error, Result,
    crdt::{CRDT, CRDTError, Codec, Doc, Lww, Map, doc::Value},
};

use super::helpers::{Opaque, assert_codec_fixture, assert_codec_rejects};

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

// No Clone, CRDT, Default or Serde: encoding must not require merge bounds.
#[derive(Debug, PartialEq, Eq)]
struct CodecOnly(bool);

impl Codec for CodecOnly {
    fn encode(&self) -> Result<Vec<u8>> {
        if self.0 {
            return Err(CRDTError::InvalidValue {
                reason: "payload encode rejected".into(),
            }
            .into());
        }
        Ok(vec![])
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        if !bytes.is_empty() {
            return Err(CRDTError::InvalidValue {
                reason: "payload decode rejected".into(),
            }
            .into());
        }
        Ok(Self(false))
    }
}

#[test]
fn test_operation_codecs_without_clone_or_merge_bounds() {
    assert_codec_fixture(&Lww::<CodecOnly>::NoOp, &[0x82, 0x00, 0x00]);
    assert_codec_fixture(&Lww::<CodecOnly>::Delete, &[0x82, 0x00, 0x02]);
    assert_codec_fixture(&Lww::Set(CodecOnly(false)), &[0x83, 0x00, 0x01, 0x40]);
    let mut map = Map::new();
    map.insert(String::new(), CodecOnly(false));
    assert_codec_fixture(&map, &[0x82, 0x00, 0x81, 0x82, 0x60, 0x40]);
}

#[test]
fn test_operation_codecs_preserve_payload_errors() {
    let mut map = Map::new();
    map.insert(String::new(), CodecOnly(true));
    for result in [Lww::Set(CodecOnly(true)).encode(), map.encode()] {
        assert!(matches!(result, Err(Error::CRDT(error))
            if matches!(*error, CRDTError::InvalidValue { ref reason }
                if reason == "payload encode rejected")));
    }
    for result in [
        Lww::<CodecOnly>::decode(&[0x83, 0x00, 0x01, 0x41, 0x00]).map(|_| ()),
        Map::<String, CodecOnly>::decode(&[0x82, 0x00, 0x81, 0x82, 0x60, 0x41, 0x00]).map(|_| ()),
    ] {
        assert!(matches!(result, Err(Error::CRDT(error))
            if matches!(*error, CRDTError::InvalidValue { ref reason }
                if reason == "payload decode rejected")));
    }
}

fn opaque_map(entries: &[(&str, Lww<Opaque>)]) -> Map<String, Lww<Opaque>> {
    let mut map = Map::new();
    for (key, value) in entries {
        map.insert((*key).to_owned(), value.clone());
    }
    map
}

fn round_trip<T: Codec>(value: &T) -> T {
    T::decode(&value.encode().unwrap()).unwrap()
}

#[test]
fn test_map_codec_stable_envelope_and_literal_keys() {
    assert_codec_fixture(&Map::<String, Opaque>::new(), &[0x82, 0x00, 0x80]);
    let mut map = Map::new();
    // Inserted out of order; encoded in ascending key order.
    map.insert("é".to_owned(), Opaque(vec![2]));
    map.insert("a.b".to_owned(), Opaque(vec![1]));
    map.insert(String::new(), Opaque(vec![]));
    #[rustfmt::skip]
    let bytes = [
        0x82, 0x00, 0x83,
        0x82, 0x60, 0x40,                         // ["", h'']
        0x82, 0x63, b'a', b'.', b'b', 0x41, 0x01, // ["a.b", h'01']
        0x82, 0x62, 0xc3, 0xa9, 0x41, 0x02,       // ["é", h'02']
    ];
    assert_codec_fixture(&map, &bytes);
    // Dotted keys stay literal, not paths.
    let decoded = Map::<String, Opaque>::decode(&bytes).unwrap();
    assert_eq!(decoded.get("a.b"), Some(&Opaque(vec![1])));
    assert_eq!(decoded.get("a"), None);
}

#[test]
fn test_map_codec_order_is_deterministic() {
    let keys = ["b", "", "ä", "a.b", "a", "\u{10ffff}", "B"];
    let forward = keys
        .iter()
        .map(|key| (*key, Lww::Set(Opaque(key.as_bytes().to_vec()))))
        .collect::<Vec<_>>();
    let reverse = forward.iter().rev().cloned().collect::<Vec<_>>();
    let encoded = opaque_map(&forward).encode().unwrap();
    assert_eq!(opaque_map(&reverse).encode().unwrap(), encoded);
    let decoded = Map::<String, Lww<Opaque>>::decode(&encoded).unwrap();
    let mut sorted = keys.map(str::to_owned);
    sorted.sort();
    assert!(decoded.iter().map(|(key, _)| key).eq(sorted.iter()));
}

#[test]
fn test_map_codec_retains_tombstones_noops_and_nested_values() {
    let lww = opaque_map(&[
        ("deleted", Lww::Delete),
        ("noop", Lww::NoOp),
        ("set", Lww::Set(Opaque(vec![]))),
    ]);
    assert_eq!(round_trip(&lww), lww);
    assert_eq!(round_trip(&lww).len(), 3);

    let mut nested = Map::new();
    nested.insert("inner".to_owned(), lww.clone());
    nested.insert("empty".to_owned(), Map::new());
    assert_eq!(round_trip(&nested), nested);

    // Doc keeps its atomic flag and tombstones through its own codec.
    let mut atomic = Doc::atomic();
    atomic.set("gone", Value::Deleted);
    atomic.set("kept", "value");
    let mut docs = Map::new();
    docs.insert("row".to_owned(), atomic.clone());
    let decoded = round_trip(&docs);
    assert_eq!(decoded, docs);
    let mut previous = Doc::new();
    previous.set("old", "value");
    assert_eq!(
        previous.merge(decoded.get("row").unwrap()).unwrap(),
        previous.merge(&atomic).unwrap()
    );
    assert_eq!(previous.merge(&atomic).unwrap(), atomic);
}

// A non-LWW value: associative concatenation with an explicit codec.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Events(Vec<u8>);

impl CRDT for Events {
    fn merge(&self, other: &Self) -> Result<Self> {
        Ok(Self([self.0.as_slice(), other.0.as_slice()].concat()))
    }
}

impl Codec for Events {
    fn encode(&self) -> Result<Vec<u8>> {
        Ok(self.0.clone())
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(Self(bytes.to_vec()))
    }
}

#[test]
fn test_map_codec_composition_agrees_after_round_trip() {
    let lww_maps = [
        opaque_map(&[]),
        opaque_map(&[("x", Lww::Set(Opaque(vec![1])))]),
        opaque_map(&[("x", Lww::Delete), ("y", Lww::NoOp)]),
        opaque_map(&[("x", Lww::NoOp), ("", Lww::Set(Opaque(vec![])))]),
    ];
    for a in &lww_maps {
        for b in &lww_maps {
            let merged = a.merge(b).unwrap();
            assert_eq!(round_trip(a).merge(&round_trip(b)).unwrap(), merged);
            assert_eq!(round_trip(&merged), merged);
        }
    }

    let event_maps = [("x", vec![1]), ("x", vec![2]), ("y", vec![3])].map(|(key, events)| {
        let mut map = Map::new();
        map.insert(key.to_owned(), Events(events));
        map
    });
    for a in &event_maps {
        for b in &event_maps {
            let merged = a.merge(b).unwrap();
            assert_eq!(round_trip(a).merge(&round_trip(b)).unwrap(), merged);
        }
    }
}

#[test]
fn test_map_codec_rejects_malformed_envelopes() {
    type Ops = Map<String, Lww<Opaque>>;
    #[rustfmt::skip]
    let cases: &[&[u8]] = &[
        &[],                                     // empty input
        &[0xa0],                                 // map, not an array
        &[0x81, 0x00],                           // missing entries
        &[0x82, 0x01, 0x80],                     // unknown revision
        &[0x83, 0x00, 0x80, 0x80],               // extra envelope field
        &[0x82, 0x00, 0xa0],                     // entries are a CBOR map
        &[0x82, 0x00, 0x81, 0x60],               // entry is not an array
        &[0x82, 0x00, 0x81, 0x81, 0x60],         // entry without value
        &[0x82, 0x00, 0x81, 0x83, 0x60, 0x40, 0x40], // entry with extra field
        &[0x82, 0x00, 0x81, 0x82, 0x40, 0x40],   // byte-string key
        &[0x82, 0x00, 0x81, 0x82, 0x00, 0x40],   // integer key
        &[0x82, 0x00, 0x81, 0x82, 0x60, 0x60],   // text value, not bytes
        &[0x82, 0x00, 0x80, 0x00],               // trailing data
        &[0x82, 0x00, 0x81, 0x82, 0x60, 0x40],   // value bytes are not an Lww envelope
        // Value envelope has trailing data.
        &[0x82, 0x00, 0x81, 0x82, 0x60, 0x44, 0x82, 0x00, 0x00, 0x00],
    ];
    for bytes in cases {
        assert_codec_rejects::<Ops>(bytes);
    }
    #[rustfmt::skip]
    let misordered: [&[u8]; 2] = [
        // Duplicate "" keys.
        &[0x82, 0x00, 0x82,
          0x82, 0x60, 0x43, 0x82, 0x00, 0x00,
          0x82, 0x60, 0x43, 0x82, 0x00, 0x00],
        // "b" before "a".
        &[0x82, 0x00, 0x82,
          0x82, 0x61, b'b', 0x43, 0x82, 0x00, 0x00,
          0x82, 0x61, b'a', 0x43, 0x82, 0x00, 0x00],
    ];
    for bytes in misordered {
        let error = Ops::decode(bytes).unwrap_err().to_string();
        assert!(error.contains("duplicate or out of order"), "{error}");
    }
    // Payload bytes that the nested payload codec rejects.
    assert_codec_rejects::<Ops>(&[
        0x82, 0x00, 0x81, 0x82, 0x60, 0x44, 0x83, 0x00, 0x01, 0x41, 0xff,
    ]);
    // A lone well-formed entry decodes, confirming the fixtures above are minimal.
    assert_eq!(
        Ops::decode(&[0x82, 0x00, 0x81, 0x82, 0x60, 0x43, 0x82, 0x00, 0x00]).unwrap(),
        opaque_map(&[("", Lww::NoOp)])
    );
}
