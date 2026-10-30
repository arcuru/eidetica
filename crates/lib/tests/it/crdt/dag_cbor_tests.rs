//! Opt-in structural persistence, strict admission, and pinned-adapter controls.

use std::collections::{BTreeMap, HashMap};

use eidetica::crdt::{
    CRDT, Doc, Lww, Map, dag_cbor,
    doc::{List, Value, list::Position},
};
use ipld_core::ipld::Ipld;
use serde::{Deserialize, Serialize, de::DeserializeOwned, ser::SerializeMap};
use serde_bytes::ByteBuf;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    #[serde(rename = "n")]
    count: u64,
    #[serde(rename = "data", with = "serde_bytes")]
    data: Vec<u8>,
}

fn roundtrip<T>(value: &T) -> T
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let bytes = dag_cbor::encode(value).unwrap();
    let decoded: T = dag_cbor::decode(&bytes).unwrap();
    assert_eq!(&decoded, value);
    assert_eq!(dag_cbor::encode(&decoded).unwrap(), bytes);
    decoded
}

fn golden<T>(value: &T, hex: &str)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_eq!(dag_cbor::encode(value).unwrap(), hex::decode(hex).unwrap());
    roundtrip(value);
}

#[test]
fn test_dag_cbor_golden_structural_variants_and_byte_leaves() {
    golden(&Lww::<Record>::NoOp, "644e6f4f70");
    golden(&Lww::<Record>::Delete, "6644656c657465");
    golden(
        &Lww::Set(Record {
            count: 7,
            data: vec![0, 255],
        }),
        "a163536574a2616e0764646174614200ff",
    );
    golden(&Lww::Set(ByteBuf::new()), "a16353657440");
    golden(&Lww::Set(Vec::<u8>::new()), "a16353657480");
    golden(&Map::<String, Lww<Record>>::new(), "a0");
    let mut map = Map::new();
    map.insert("aa".to_owned(), Lww::Set(ByteBuf::new()));
    map.insert("b".to_owned(), Lww::Delete);
    map.insert("".to_owned(), Lww::NoOp);
    golden(&map, "a360644e6f4f7061626644656c657465626161a16353657440");
    assert_eq!(map.get("missing"), None);
    assert_eq!(map.get(""), Some(&Lww::NoOp));
    assert_eq!(map.get("b"), Some(&Lww::Delete));
    assert_eq!(map.get("aa"), Some(&Lww::Set(ByteBuf::new())));
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
struct Text(Box<str>);

#[test]
fn test_dag_cbor_container_and_custom_text_equivalence() {
    // Rust lexicographic order puts "aa" before "b"; DAG-CBOR does not.
    let keys = ["aa", "b", "", ".", "a.b", "é", "e\u{301}", "日本"];
    let mut expected = None;
    for offset in 0..keys.len() {
        let mut map = Map::new();
        let mut tree = BTreeMap::new();
        let mut hash = HashMap::new();
        let mut custom = HashMap::new();
        for i in 0..keys.len() {
            let key = keys[(offset + i) % keys.len()];
            let value = Lww::Set(Record {
                count: key.len() as u64,
                data: key.as_bytes().to_vec(),
            });
            map.insert(key.to_owned(), value.clone());
            tree.insert(key.to_owned(), value.clone());
            hash.insert(key.to_owned(), value.clone());
            custom.insert(Text(key.into()), value);
        }
        let bytes = dag_cbor::encode(&map).unwrap();
        assert_eq!(dag_cbor::encode(&tree).unwrap(), bytes);
        assert_eq!(dag_cbor::encode(&hash).unwrap(), bytes);
        assert_eq!(dag_cbor::encode(&custom).unwrap(), bytes);
        assert_eq!(
            dag_cbor::decode::<Map<Text, Lww<Record>>>(&bytes)
                .unwrap()
                .len(),
            keys.len()
        );
        roundtrip(&map);
        match &expected {
            Some(expected) => assert_eq!(&bytes, expected),
            None => expected = Some(bytes),
        }
    }
    golden(
        &BTreeMap::from([("aa".to_owned(), 1u8), ("b".to_owned(), 2)]),
        "a261620262616101",
    );
}

#[test]
fn test_dag_cbor_native_numeric_domain() {
    for value in [
        -(u64::MAX as i128) - 1,
        i64::MIN as i128,
        -1,
        0,
        i64::MAX as i128,
        u64::MAX as i128,
    ] {
        roundtrip(&value);
    }
    roundtrip(&u64::MAX);
    roundtrip(&(u64::MAX as u128));
    roundtrip(&i64::MIN);
    roundtrip(&i32::MIN);
    roundtrip(&u32::MAX);
    roundtrip(&i16::MIN);
    roundtrip(&u16::MAX);
    roundtrip(&i8::MIN);
    roundtrip(&u8::MAX);
    for value in [
        i128::MIN,
        i128::MAX,
        -(u64::MAX as i128) - 2,
        u64::MAX as i128 + 1,
    ] {
        assert!(dag_cbor::encode(&value).is_err());
    }
    assert!(dag_cbor::encode(&u128::MAX).is_err());
    for value in [
        f64::MIN,
        f64::MAX,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        -1.5,
        0.0,
        1.5,
    ] {
        roundtrip(&value);
    }
    for value in [f32::MIN, f32::MAX, f32::from_bits(1), 0.0, 1.5] {
        roundtrip(&value);
    }
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(dag_cbor::encode(&value).is_err());
    }
    golden(&0.0f64, "fb0000000000000000");
    assert_eq!(
        dag_cbor::encode(&-0.0f64).unwrap(),
        dag_cbor::encode(&0.0f64).unwrap()
    );
    let nested = BTreeMap::from([("zero".to_owned(), vec![-0.0f64, 0.0])]);
    let positive = BTreeMap::from([("zero".to_owned(), vec![0.0f64, 0.0])]);
    assert_eq!(
        dag_cbor::encode(&nested).unwrap(),
        dag_cbor::encode(&positive).unwrap()
    );
    let encoded = dag_cbor::encode(&nested).unwrap();
    let decoded: BTreeMap<String, Vec<f64>> = dag_cbor::decode(&encoded).unwrap();
    assert!(decoded["zero"].iter().all(|v| v.is_sign_positive()));
}

#[test]
fn test_dag_cbor_rejects_malformed_and_noncanonical() {
    // Ipld has no typed-schema rejection that could mask an absent generic check.
    for hex in [
        "",
        "ff",
        "9fff",
        "bfff",
        "f7",
        "61ff",
        "0000",
        "00ff",
        "1800",
        "3800",
        "780161",
        "580100",
        "980100",
        "b801616100",
        "a262616101616202",     // keys in Rust string order, not DAG-CBOR order
        "a2616101616102",       // duplicate text keys
        "a10102",               // non-text key
        "a16178a2616101616102", // nested duplicate
        "f93c00",
        "fa3f800000",         // half/single instead of f64
        "fb8000000000000000", // negative zero
        "fb7ff0000000000000",
        "fbfff0000000000000",
        "fb7ff8000000000000",
        "c001",
        "d82a4101", // unsupported tag and malformed CID
    ] {
        let bytes = hex::decode(hex).unwrap();
        assert!(dag_cbor::decode::<Ipld>(&bytes).is_err(), "admitted {hex}");
    }
    // A valid canonical control must also get through this exact probe.
    roundtrip(&Ipld::Map(BTreeMap::from([(
        "a".to_owned(),
        Ipld::Integer(1),
    )])));
    for hex in ["63426164", "a16342616401", "a1644e6f4f70f6", "a163536574f6"] {
        assert!(
            dag_cbor::decode::<Lww<Record>>(&hex::decode(hex).unwrap()).is_err(),
            "admitted variant {hex}"
        );
    }
}

struct Duplicate;
impl Serialize for Duplicate {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("a", &1)?;
        map.serialize_entry("a", &2)?;
        map.end()
    }
}

#[test]
fn test_dag_cbor_encoder_domain_and_raw_adapter_controls() {
    let raw = serde_ipld_dagcbor::to_vec(&Duplicate).unwrap();
    assert!(serde_ipld_dagcbor::from_slice::<BTreeMap<String, u8>>(&raw).is_ok());
    assert!(serde_ipld_dagcbor::from_slice::<Ipld>(&raw).is_err());
    assert!(dag_cbor::encode(&Duplicate).is_err());
    let nontext = BTreeMap::from([(1u8, 2u8)]);
    assert!(serde_ipld_dagcbor::to_vec(&nontext).is_ok());
    assert!(dag_cbor::encode(&nontext).is_err());
    for hex in [
        "1800",
        "780161",
        "980100",
        "a262616101616202",
        "fa3f800000",
        "fb8000000000000000",
        "fb7ff0000000000000",
    ] {
        let bytes = hex::decode(hex).unwrap();
        assert!(
            serde_ipld_dagcbor::from_slice::<Ipld>(&bytes).is_ok(),
            "raw adapter changed: {hex}"
        );
        assert!(
            dag_cbor::decode::<Ipld>(&bytes).is_err(),
            "strict admission lost: {hex}"
        );
    }
    assert!(serde_ipld_dagcbor::from_slice::<Ipld>(&[0, 0]).is_err());
    let bytes = ByteBuf::from(vec![0, 255]);
    let array = vec![0u8, 255];
    golden(&bytes, "4200ff");
    golden(&array, "820018ff");
    assert!(dag_cbor::decode::<ByteBuf>(&dag_cbor::encode(&array).unwrap()).is_err());
    let cid = ipld_core::cid::Cid::new_v1(
        0x55,
        ipld_core::cid::multihash::Multihash::wrap(0x00, &[0x42]).unwrap(),
    );
    roundtrip(&Ipld::Link(cid));
}

fn position(numerator: i64) -> Position {
    Position {
        numerator,
        denominator: 1,
        unique_id: Uuid::nil(),
    }
}

#[test]
fn test_dag_cbor_rejects_typed_schema_loss() {
    let pairs = vec![(position(1), Value::Int(1)), (position(1), Value::Deleted)];
    let bytes = dag_cbor::encode(&pairs).unwrap();
    // Generic CBOR validity cannot see that List's BTreeMap drops a position.
    roundtrip(&dag_cbor::decode::<Ipld>(&bytes).unwrap());
    assert_eq!(
        serde_ipld_dagcbor::from_slice::<List>(&bytes)
            .unwrap()
            .iter_all()
            .count(),
        1
    );
    assert!(dag_cbor::decode::<List>(&bytes).is_err());
    let reversed = vec![(position(2), Value::Int(2)), (position(1), Value::Int(1))];
    assert!(dag_cbor::decode::<List>(&dag_cbor::encode(&reversed).unwrap()).is_err());
    // Ignored Doc fields/default aliases must not silently disappear either.
    for extra in [("extra", Ipld::Bool(true)), ("_v", Ipld::Integer(0))] {
        let value = Ipld::Map(BTreeMap::from([
            ("children".to_owned(), Ipld::Map(BTreeMap::new())),
            (extra.0.to_owned(), extra.1),
        ]));
        let bytes = dag_cbor::encode(&value).unwrap();
        assert!(serde_ipld_dagcbor::from_slice::<Doc>(&bytes).is_ok());
        assert!(dag_cbor::decode::<Doc>(&bytes).is_err());
    }
    let lossy = dag_cbor::encode(&1.0000000000000002f64).unwrap();
    // The pinned adapter already guards f64-to-f32 precision loss; retain it.
    assert!(serde_ipld_dagcbor::from_slice::<f32>(&lossy).is_err());
    assert!(dag_cbor::decode::<f32>(&lossy).is_err());
    roundtrip(&dag_cbor::decode::<f64>(&lossy).unwrap());
}

fn assert_continuation<T>(initial: &T, suffix: &[T])
where
    T: CRDT + Serialize + DeserializeOwned + PartialEq + std::fmt::Debug + Clone,
{
    let mut original = initial.clone();
    let mut decoded = roundtrip(initial);
    for delta in suffix {
        original = original.merge(delta).unwrap();
        decoded = decoded.merge(&roundtrip(delta)).unwrap();
        assert_eq!(decoded, original);
        decoded = roundtrip(&decoded);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Events(Vec<u8>);
impl CRDT for Events {
    fn merge(&self, other: &Self) -> eidetica::Result<Self> {
        Ok(Self([self.0.as_slice(), other.0.as_slice()].concat()))
    }
}

#[test]
fn test_dag_cbor_complete_state_continuation_and_lawful_merges() {
    let mut doc = Doc::atomic();
    doc.set("gone", Value::Deleted);
    golden(
        &doc,
        "a2625f61f5686368696c6472656ea164676f6e656744656c65746564",
    );
    let mut list = List::new();
    list.insert_at_position(position(1), Value::Deleted);
    list.insert_at_position(position(2), Value::Int(i64::MIN));
    let fractional = Position {
        numerator: -3,
        denominator: u64::MAX,
        unique_id: Uuid::from_u128(0x123456789abcdef0123456789abcdef0),
    };
    let tied = Position {
        unique_id: Uuid::from_u128(0x123456789abcdef0123456789abcdef1),
        ..fractional.clone()
    };
    list.insert_at_position(fractional.clone(), Value::Text("before".to_owned()));
    list.insert_at_position(tied.clone(), Value::Deleted);
    doc.set("list", list);
    let mut nested = Doc::atomic();
    nested.set("deleted", Value::Deleted);
    doc.set("nested", nested);
    roundtrip(&doc);
    let mut later = Doc::new();
    later.set("after", true);
    let mut list_delta = List::new();
    list_delta.insert_at_position(fractional, Value::Deleted);
    list_delta.insert_at_position(tied, Value::Text("after".to_owned()));
    later.set("list", list_delta);
    // Exact state preservation for this fixture, NOT a repair/qualification of
    // Doc's separate delete/scalar/descendant-write batching defect.
    assert_continuation(&doc, &[later, Doc::atomic()]);

    let mut initial = Map::new();
    initial.insert("".to_owned(), Lww::NoOp);
    initial.insert("a.b".to_owned(), Lww::Set(doc));
    initial.insert("é".to_owned(), Lww::Delete);
    let mut delete = Map::new();
    delete.insert("a.b".to_owned(), Lww::Delete);
    let mut noop = Map::new();
    noop.insert("a.b".to_owned(), Lww::NoOp);
    let mut recreate = Map::new();
    recreate.insert("a.b".to_owned(), Lww::Set(Doc::new()));
    assert_continuation(&initial, &[delete.clone(), noop.clone(), recreate.clone()]);
    assert_eq!(
        initial.merge(&delete).unwrap().merge(&noop).unwrap(),
        initial.merge(&delete.merge(&noop).unwrap()).unwrap()
    );
    assert_eq!(
        initial.merge(&delete).unwrap().get("a.b"),
        Some(&Lww::Delete)
    );

    let events = |values: &[u8]| {
        let mut map = Map::new();
        map.insert("events".to_owned(), Events(values.to_vec()));
        map
    };
    let (a, b, c) = (events(&[1]), events(&[2]), events(&[3]));
    assert_continuation(&a, &[b.clone(), c.clone()]);
    assert_eq!(
        a.merge(&b).unwrap().merge(&c).unwrap(),
        a.merge(&b.merge(&c).unwrap()).unwrap()
    );
    assert_eq!(
        roundtrip(&a.merge(&b).unwrap().merge(&c).unwrap()).get("events"),
        Some(&Events(vec![1, 2, 3]))
    );
    let mut outer = Map::new();
    outer.insert("nested".to_owned(), a);
    let mut suffix = Map::new();
    suffix.insert("nested".to_owned(), b);
    assert_continuation(&outer, &[suffix]);
}
