//! Public table:v0.1 building-block tests, without Table or backend integration.

use std::collections::BTreeMap;

use eidetica::crdt::{CRDT, CRDTError, Codec, Lww, LwwMap};
use eidetica::store::TableData;
use ipld_core::ipld::Ipld;
use serde_bytes::ByteBuf;

fn golden_state() -> TableData {
    let mut data = TableData::default();
    data.0.set("é".into(), ByteBuf::from(vec![0xfe, 0]));
    data.0.set("z".into(), ByteBuf::new());
    data.0.delete("a.b".into());
    data.0.set("".into(), ByteBuf::from(vec![0, 0xff, 0x80]));
    data
}

fn roundtrip(data: &TableData) -> TableData {
    let bytes = data.encode().unwrap();
    let decoded = TableData::decode(&bytes).unwrap();
    assert_eq!(&decoded, data);
    assert_eq!(decoded.encode().unwrap(), bytes);
    decoded
}

fn assert_rejected(bytes: &[u8]) {
    let error = TableData::decode(bytes).expect_err("invalid envelope was accepted");
    assert!(
        matches!(error, eidetica::Error::CRDT(ref inner)
            if matches!(**inner, CRDTError::DeserializationFailed { .. })),
        "unexpected error: {error}"
    );
}

fn pair(key: Ipld, operation: Ipld) -> Ipld {
    Ipld::List(vec![key, operation])
}

fn set(payload: Ipld) -> Ipld {
    Ipld::Map(BTreeMap::from([("set".into(), payload)]))
}

#[test]
fn test_table_data_golden_dag_cbor() {
    let empty = TableData::default();
    assert_eq!(empty.encode().unwrap(), vec![0x80]);
    roundtrip(&empty);

    let data = golden_state();
    // [ ["", {"set": h'00ff80'}], ["a.b", "delete"],
    //   ["z", {"set": h''}], ["é", {"set": h'fe00'}] ]
    let expected = hex::decode(concat!(
        "84",
        "8260a1637365744300ff80",
        "8263612e626664656c657465",
        "82617aa16373657440",
        "8262c3a9a16373657442fe00"
    ))
    .unwrap();
    assert_eq!(data.encode().unwrap(), expected);
    let envelope: Ipld = serde_ipld_dagcbor::from_slice(&expected).unwrap();
    assert_eq!(
        envelope,
        Ipld::List(vec![
            pair(
                Ipld::String("".into()),
                set(Ipld::Bytes(vec![0, 0xff, 0x80]))
            ),
            pair(Ipld::String("a.b".into()), Ipld::String("delete".into())),
            pair(Ipld::String("z".into()), set(Ipld::Bytes(vec![]))),
            pair(Ipld::String("é".into()), set(Ipld::Bytes(vec![0xfe, 0]))),
        ])
    );
    roundtrip(&data);

    let mut delete_empty = TableData::default();
    delete_empty.0.delete("".into());
    assert_eq!(
        delete_empty.encode().unwrap(),
        hex::decode("8182606664656c657465").unwrap()
    );
    assert_ne!(delete_empty.encode().unwrap(), empty.encode().unwrap());
    roundtrip(&delete_empty);
}

#[test]
fn test_table_data_insertion_order_and_exact_utf8_keys() {
    // UTF-8 orders these last two keys differently from UTF-16; dots have no
    // hierarchy, and composed/decomposed Unicode keys must remain distinct.
    let keys = ["", "a", "a.b", "e\u{301}", "é", "\u{e000}", "\u{10000}"];
    let mut expected = TableData::default();
    for key in keys {
        expected.0.set(key.into(), ByteBuf::from(key.as_bytes()));
    }
    assert_eq!(
        expected
            .0
            .operations()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        keys
    );
    for reverse in [false, true] {
        for offset in 0..keys.len() {
            let mut permuted = keys.to_vec();
            permuted.rotate_left(offset);
            if reverse {
                permuted.reverse();
            }
            let mut data = TableData::default();
            for key in permuted {
                data.0.set(key.into(), ByteBuf::from(key.as_bytes()));
            }
            assert_eq!(data.encode().unwrap(), expected.encode().unwrap());
            roundtrip(&data);
        }
    }
}

#[test]
fn test_table_data_roundtrip_preserves_future_merge_and_tombstones() {
    let empty = TableData::default();
    let initial = golden_state();
    let mut delete = TableData::default();
    delete.0.delete("".into());
    delete.0.delete("é".into());
    let mut resurrect = TableData::default();
    resurrect
        .0
        .set("".into(), ByteBuf::from(vec![0xff, 0, 0x80]));
    let states = [&empty, &initial, &delete, &resurrect];
    for first in states {
        assert_eq!(roundtrip(first).merge(&roundtrip(&empty)).unwrap(), *first);
        assert_eq!(roundtrip(&empty).merge(&roundtrip(first)).unwrap(), *first);
        for second in states {
            let merged = first.merge(second).unwrap();
            assert_eq!(
                roundtrip(first).merge(&roundtrip(second)).unwrap(),
                roundtrip(&merged)
            );
            for third in states {
                let sequential = roundtrip(&merged).merge(&roundtrip(third)).unwrap();
                let grouped = roundtrip(first)
                    .merge(&roundtrip(&second.merge(third).unwrap()))
                    .unwrap();
                assert_eq!(sequential, grouped);
                roundtrip(&sequential);
            }
        }
    }
    let deleted = roundtrip(&initial.merge(&delete).unwrap());
    assert!(deleted.0.get(&"".into()).is_none());
    assert_eq!(deleted.0.operation(&"".into()), Some(&Lww::Delete));
    assert_eq!(deleted.0.operation(&"a.b".into()), Some(&Lww::Delete));
    let alive = roundtrip(&deleted.merge(&resurrect).unwrap());
    assert_eq!(alive.0.get(&"".into()).unwrap().as_ref(), &[0xff, 0, 0x80]);
    assert_eq!(alive.0.operation(&"é".into()), Some(&Lww::Delete));
}

#[test]
fn test_table_data_payloads_remain_opaque() {
    // Neither noncanonical JSON nor CBOR nor invalid application data is an
    // envelope error when it is carried inside the row byte string.
    let payloads: &[&[u8]] = &[
        b"",
        b"{ \"z\": 1, \"a\": 2, \"a\": 3 }",
        b"1 2",
        b"\x98\x00",
        b"\xff\x80\x00\x00",
    ];
    for payload in payloads {
        let mut data = TableData::default();
        data.0.set("opaque".into(), ByteBuf::from(*payload));
        let decoded = roundtrip(&data);
        assert_eq!(decoded.0.get(&"opaque".into()).unwrap().as_ref(), *payload);
        assert_eq!(decoded.merge(&TableData::default()).unwrap(), data);
    }
}

#[test]
fn test_table_data_reject_duplicate_keys_and_keyed_noop() {
    let duplicate = Ipld::List(vec![
        pair(Ipld::String("a".into()), set(Ipld::Bytes(vec![]))),
        pair(Ipld::String("a".into()), Ipld::String("delete".into())),
    ]);
    let noop = Ipld::List(vec![pair(
        Ipld::String("a".into()),
        Ipld::String("no_op".into()),
    )]);
    for (envelope, reason) in [(duplicate, "duplicate map key"), (noop, "keyed NoOp")] {
        let bytes = serde_ipld_dagcbor::to_vec(&envelope).unwrap();
        let error = serde_ipld_dagcbor::from_slice::<LwwMap<String, ByteBuf>>(&bytes)
            .expect_err("operation-map validation must reject before canonicality check");
        assert!(error.to_string().contains(reason), "{error}");
        assert_rejected(&bytes);
    }
}

#[test]
fn test_table_data_reject_wrong_tags_containers_and_payloads() {
    let mut invalid = vec![
        Ipld::Null,
        Ipld::Map(BTreeMap::new()),
        Ipld::List(vec![Ipld::String("not a pair".into())]),
        Ipld::List(vec![Ipld::List(vec![])]),
        Ipld::List(vec![Ipld::List(vec![Ipld::String("a".into())])]),
        Ipld::List(vec![Ipld::List(vec![
            Ipld::String("a".into()),
            Ipld::String("delete".into()),
            Ipld::Null,
        ])]),
        Ipld::List(vec![pair(Ipld::Integer(1), Ipld::String("delete".into()))]),
        Ipld::List(vec![pair(
            Ipld::Bytes(vec![b'a']),
            Ipld::String("delete".into()),
        )]),
    ];
    for operation in [
        Ipld::String("unknown".into()),
        Ipld::String("Set".into()),
        Ipld::String("set".into()),
        Ipld::Integer(1),
        Ipld::Null,
        Ipld::List(vec![Ipld::String("set".into()), Ipld::Bytes(vec![])]),
        Ipld::Map(BTreeMap::from([("put".into(), Ipld::Bytes(vec![]))])),
        Ipld::Map(BTreeMap::from([
            ("set".into(), Ipld::Bytes(vec![])),
            ("delete".into(), Ipld::Null),
        ])),
        set(Ipld::Integer(1)),
        set(Ipld::Null),
        set(Ipld::Bool(true)),
        set(Ipld::Map(BTreeMap::new())),
        set(Ipld::List(vec![Ipld::Integer(256)])),
    ] {
        invalid.push(Ipld::List(vec![pair(Ipld::String("a".into()), operation)]));
    }
    for envelope in invalid {
        assert_rejected(&serde_ipld_dagcbor::to_vec(&envelope).unwrap());
    }
    // The dependency itself requires the enum's map header to be exactly 0xa1.
    assert_rejected(&hex::decode("81826161b8016373657440").unwrap());
}

#[test]
fn test_table_data_reject_truncated_and_trailing_data() {
    let bytes = golden_state().encode().unwrap();
    for length in 0..bytes.len() {
        assert_rejected(&bytes[..length]);
    }
    for extra in [vec![0x00], vec![0x80], bytes.clone()] {
        let mut trailing = bytes.clone();
        trailing.extend(extra);
        assert_rejected(&trailing);
    }
    // A single complete CBOR value is required, including for empty state.
    assert_rejected(&[0x80, 0x80]);
}

#[test]
fn test_table_data_reject_byte_array_and_text_payload_aliases() {
    for hex_bytes in [
        "81826161a16373657480",       // empty integer array
        "81826161a1637365748218ff00", // [255, 0]
        "81826161a16373657463616263", // text "abc"
    ] {
        let bytes = hex::decode(hex_bytes).unwrap();
        // ByteBuf requests byte-string deserialization, which this dependency
        // already enforces. Pin that behavior as well as TableData's rejection.
        assert!(serde_ipld_dagcbor::from_slice::<LwwMap<String, ByteBuf>>(&bytes).is_err());
        assert_rejected(&bytes);
    }
}

#[test]
fn test_table_data_reject_noncanonical_dependency_aliases() {
    // Each witness is accepted by the dependency as a valid operation map,
    // proving TableData's stricter rejection is not a vacuous parse failure.
    let aliases = [
        (
            "reverse pair order",
            "828261626664656c6574658261616664656c657465",
        ),
        ("wide empty sequence length (u8)", "9800"),
        ("wide empty sequence length (u16)", "990000"),
        ("wide empty sequence length (u32)", "9a00000000"),
        ("wide empty sequence length (u64)", "9b0000000000000000"),
        ("wide pair length", "81980261616664656c657465"),
        ("wide key length", "81827801616664656c657465"),
        ("wide operation length", "81826161780664656c657465"),
        ("wide set tag length", "81826161a1780373657440"),
        ("wide byte-string length", "81826161a1637365745801ff"),
    ];
    for (name, hex_bytes) in aliases {
        let bytes = hex::decode(hex_bytes).unwrap();
        let permissive: LwwMap<String, ByteBuf> = serde_ipld_dagcbor::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("dependency did not accept {name}: {error}"));
        let canonical = TableData(permissive).encode().unwrap();
        assert_ne!(canonical, bytes, "witness must be noncanonical: {name}");
        roundtrip(&TableData::decode(&canonical).unwrap());
        assert_rejected(&bytes);
    }
}
