//! Algebra composes independently of persistence; explicit codecs preserve state.

use eidetica::crdt::{
    CRDT, Codec, Doc, Lww, LwwMap, Map,
    doc::{List, Value},
};
use serde_bytes::ByteBuf;

// Neither the key nor the inner algebra implements Serde or Codec.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Key(&'static str);

#[derive(Clone, Default, Debug, PartialEq, Eq)]
struct Sum(i64);

impl CRDT for Sum {
    fn merge(&self, other: &Self) -> eidetica::Result<Self> {
        Ok(Self(self.0 + other.0))
    }
}

#[test]
fn test_nested_non_serde_algebra() {
    let mut first = Map::new();
    first.insert_delta(Key("x"), Sum(2));
    let mut second = Map::new();
    second.insert_delta(Key("x"), Sum(3));
    second.insert_delta(Key("y"), Sum(7));
    let mut a = Map::new();
    a.insert_delta(Key("outer"), first.clone());
    let mut b = Map::new();
    b.insert_delta(Key("outer"), second.clone());
    let merged = a.merge(&b).unwrap();
    assert_eq!(
        merged.get(&Key("outer")).unwrap().get(&Key("x")),
        Some(&Sum(5))
    );
    assert_eq!(
        merged.get(&Key("outer")).unwrap().get(&Key("y")),
        Some(&Sum(7))
    );
    assert_eq!(merged.merge(&Map::default()).unwrap(), merged);
    assert_eq!(Map::default().merge(&merged).unwrap(), merged);
    assert_eq!(
        a.merge(&b).unwrap().merge(&a).unwrap(),
        a.merge(&b.merge(&a).unwrap()).unwrap()
    );

    // Registers replace entire non-Serde values rather than merging their inners.
    let register = Lww::Set(first.clone());
    assert_eq!(
        register.merge(&Lww::Set(second.clone())).unwrap(),
        Lww::Set(second.clone())
    );
    assert_eq!(register.merge(&Lww::NoOp).unwrap(), register);
    assert_eq!(register.merge(&Lww::Delete).unwrap(), Lww::Delete);

    let mut live = LwwMap::new();
    live.set(Key("a"), first);
    live.set(Key("a.b"), second.clone());
    let mut delete = LwwMap::new();
    delete.delete(Key("a"));
    let deleted = live.merge(&delete).unwrap();
    assert_eq!(deleted.operation(&Key("a")), Some(&Lww::Delete));
    assert_eq!(deleted.get(&Key("a.b")), Some(&second));
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted.operations().count(), 2);
    let mut resurrect = LwwMap::new();
    resurrect.set(Key("a"), second.clone());
    assert_eq!(
        deleted.merge(&resurrect).unwrap().get(&Key("a")),
        Some(&second)
    );
}

#[test]
fn test_codec_has_no_clone_or_crdt_requirement() {
    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct EncodeOnly(u128);
    // Generic encodings require Serde on their particular representation, not Clone.
    let value = Lww::Set(EncodeOnly(u128::MAX));
    let bytes = value.encode().unwrap();
    assert_eq!(bytes, format!("{{\"set\":{}}}", u128::MAX).as_bytes());
    assert_eq!(Lww::<EncodeOnly>::decode(&bytes).unwrap(), value);
    let mut map = Map::new();
    map.insert_delta(42u64, EncodeOnly(u128::MAX));
    assert_eq!(
        Map::<u64, EncodeOnly>::decode(&map.encode().unwrap()).unwrap(),
        map
    );
    let mut live = LwwMap::new();
    live.set(42u64, EncodeOnly(u128::MAX));
    assert_eq!(
        LwwMap::<u64, EncodeOnly>::decode(&live.encode().unwrap()).unwrap(),
        live
    );
}

#[test]
fn test_opaque_rows_roundtrip_tombstones_and_exact_keys() {
    type Rows = LwwMap<String, ByteBuf>;
    let mut rows = Rows::new();
    rows.set("".into(), ByteBuf::new());
    rows.set("a".into(), ByteBuf::from(vec![1]));
    rows.set("a.b".into(), ByteBuf::from(vec![255, 0, 128]));
    let mut delete = Rows::new();
    delete.delete("a".into());
    rows = rows.merge(&delete).unwrap();
    let encoded = rows.encode().unwrap();
    assert_eq!(
        encoded,
        br#"[["",{"set":[]}],["a","delete"],["a.b",{"set":[255,0,128]}]]"#
    );
    let decoded = Rows::decode(&encoded).unwrap();
    assert_eq!(decoded, rows);
    assert_eq!(decoded.operation(&"a".into()), Some(&Lww::Delete));
    assert_eq!(decoded.get(&"a.b".into()).unwrap().as_ref(), &[255, 0, 128]);
    assert!(decoded.get(&"".into()).unwrap().is_empty());
    assert_eq!(decoded.merge(&Rows::default()).unwrap(), rows);
    assert_eq!(Rows::default().merge(&decoded).unwrap(), rows);

    let mut earlier = Rows::new();
    earlier.set("a".into(), ByteBuf::from(vec![99]));
    assert!(earlier.merge(&decoded).unwrap().get(&"a".into()).is_none());
    assert_eq!(
        rows.merge(&earlier).unwrap(),
        decoded.merge(&earlier).unwrap()
    );
    assert_eq!(
        Rows::decode(&Rows::default().encode().unwrap()).unwrap(),
        Rows::default()
    );

    // Optional binary Serde retains byte strings, not parsed application JSON.
    let cbor = serde_ipld_dagcbor::to_vec(&rows).unwrap();
    assert!(cbor.windows(4).any(|w| w == [0x43, 255, 0, 128]));
    assert_eq!(serde_ipld_dagcbor::from_slice::<Rows>(&cbor).unwrap(), rows);
    assert_eq!(
        Lww::<ByteBuf>::decode(&Lww::<ByteBuf>::Delete.encode().unwrap()).unwrap(),
        Lww::Delete
    );
    assert_eq!(
        Lww::<ByteBuf>::decode(&Lww::<ByteBuf>::default().encode().unwrap()).unwrap(),
        Lww::NoOp
    );
}

fn reject_incomplete<D: Codec>(valid: &[u8]) {
    assert!(D::decode(b"").is_err());
    assert!(D::decode(b"not a value").is_err());
    assert!(D::decode(&valid[..valid.len() - 1]).is_err());
    let mut trailing = valid.to_vec();
    trailing.extend_from_slice(b" null");
    assert!(D::decode(&trailing).is_err());
}

#[test]
fn test_algebra_codecs_reject_malformed_and_trailing_data() {
    reject_incomplete::<Lww<i64>>(br#"{"set":42}"#);
    reject_incomplete::<Map<String, Lww<i64>>>(br#"[["a",{"set":42}]]"#);
    reject_incomplete::<LwwMap<String, ByteBuf>>(br#"[["a",{"set":[255,0]}]]"#);
    assert!(Map::<String, Lww<i64>>::decode(br#"[["a","delete"],["a","delete"]]"#).is_err());
    assert!(LwwMap::<String, ByteBuf>::decode(br#"[["a","no_op"]]"#).is_err());
    assert!(LwwMap::<String, ByteBuf>::decode(br#"[["a",{"set":[256]}]]"#).is_err());
}

#[test]
fn test_doc_codec_preserves_old_bytes_and_atomic_tombstones() {
    let default = br#"{"children":{}}"#;
    assert_eq!(Doc::default().encode().unwrap(), default);
    assert_eq!(Doc::decode(default).unwrap(), Doc::default());
    let old = br#"{"_a":true,"children":{"gone":"Deleted"}}"#;
    let mut deleted = Doc::atomic();
    deleted.remove("gone");
    assert_eq!(deleted.encode().unwrap(), old);
    let decoded = Doc::decode(old).unwrap();
    assert_eq!(decoded, deleted);
    let mut before = Doc::new();
    before.set("other", 5);
    assert_eq!(before.merge(&decoded).unwrap(), deleted);
    let mut next = Doc::new();
    next.set("gone", 8);
    assert_eq!(decoded.merge(&next).unwrap(), deleted.merge(&next).unwrap());
    reject_incomplete::<Doc>(old);
    assert!(Doc::decode(br#"{"_v":1,"children":{}}"#).is_err());
}

#[test]
fn test_value_and_list_codecs_preserve_old_bytes() {
    for (value, bytes) in [
        (Value::Int(42), br#"{"Int":42}"#.as_slice()),
        (Value::Deleted, br#""Deleted""#.as_slice()),
    ] {
        assert_eq!(value.encode().unwrap(), bytes);
        assert_eq!(Value::decode(bytes).unwrap(), value);
        reject_incomplete::<Value>(bytes);
    }
    assert_eq!(List::default().encode().unwrap(), b"[]");
    let old = br#"[[{"numerator":1,"denominator":1,"unique_id":"00000000-0000-0000-0000-000000000000"},"Deleted"]]"#;
    let decoded = List::decode(old).unwrap();
    assert_eq!(decoded.encode().unwrap(), old);
    assert_eq!(decoded.total_len(), 1);
    assert_eq!(decoded.len(), 0);
    let mut later = List::new();
    later.push(3);
    let mut original = serde_json::from_slice::<List>(old).unwrap();
    original.merge(&later);
    let mut restored = decoded;
    restored.merge(&later);
    assert_eq!(original, restored);
    reject_incomplete::<List>(old);
}

#[cfg(feature = "y-crdt")]
#[test]
fn test_yrs_codec_preserves_old_json_update_bytes() {
    use eidetica::store::YrsBinary;
    let old = br#"{"data":[0,0]}"#;
    let update = YrsBinary::new(vec![0, 0]);
    assert_eq!(update.encode().unwrap(), old);
    let decoded = YrsBinary::decode(old).unwrap();
    assert_eq!(decoded.as_bytes(), update.as_bytes());
    assert_eq!(
        decoded.merge(&YrsBinary::default()).unwrap().as_bytes(),
        update.merge(&YrsBinary::default()).unwrap().as_bytes()
    );
    assert_eq!(YrsBinary::default().encode().unwrap(), br#"{"data":[]}"#);
    reject_incomplete::<YrsBinary>(old);
    assert!(YrsBinary::decode(br#"{"data":[256]}"#).is_err());
}
