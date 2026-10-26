//! Ordered register laws and complete operation encoding.

use eidetica::crdt::{CRDT, Codec, Lww};

use super::helpers::{Opaque, assert_codec_fixture, assert_codec_rejects};

#[test]
fn test_lww_exhaustive_laws_and_rightmost_winner() {
    let operations = [Lww::NoOp, Lww::Set(1), Lww::Set(2), Lww::Delete];
    for a in &operations {
        assert_eq!(a.merge(&Lww::default()).unwrap(), *a);
        assert_eq!(Lww::default().merge(a).unwrap(), *a);
        for b in &operations {
            for c in &operations {
                let left = a.merge(b).unwrap().merge(c).unwrap();
                let right = a.merge(&b.merge(c).unwrap()).unwrap();
                let expected = [a, b, c]
                    .into_iter()
                    .rfind(|op| !matches!(op, Lww::NoOp))
                    .unwrap_or(&Lww::NoOp);
                assert_eq!(left, right, "associativity: {a:?}, {b:?}, {c:?}");
                assert_eq!(&left, expected, "winner: {a:?}, {b:?}, {c:?}");
            }
        }
    }
}

#[test]
fn test_lww_order_and_value_level_delete_replacement() {
    let first = Lww::Set(1);
    let later = Lww::Set(2);
    assert_eq!(first.merge(&later).unwrap(), later);
    assert_eq!(later.merge(&first).unwrap(), first);
    assert_ne!(first.merge(&later).unwrap(), later.merge(&first).unwrap());
    assert_eq!(first.merge(&Lww::Delete).unwrap(), Lww::Delete);
    assert_eq!(Lww::Delete.merge(&later).unwrap(), later);
    assert_eq!(Lww::<i32>::Delete.merge(&Lww::NoOp).unwrap(), Lww::Delete);
}

#[test]
fn test_lww_payload_without_default_crdt_or_encoding() {
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Payload(&'static str);

    let empty = Lww::<Payload>::default();
    assert_eq!(empty, Lww::NoOp);
    let value = Lww::Set(Payload("opaque"));
    assert_eq!(empty.merge(&value).unwrap(), value);
    assert_eq!(value.merge(&Lww::NoOp).unwrap(), value);
    let deleted = Lww::<Payload>::Delete;
    assert_eq!(
        deleted.merge(&Payload("after").into()).unwrap(),
        Lww::Set(Payload("after"))
    );

    // Construction and Default do not require Clone either.
    struct NonClone;
    assert!(matches!(Lww::<NonClone>::default(), Lww::NoOp));
    assert!(matches!(Lww::Set(NonClone), Lww::Set(_)));
}

#[test]
fn test_lww_from_payload_without_clone_or_other_traits() {
    #[derive(Debug, PartialEq, Eq)]
    struct Payload(&'static str);

    assert_eq!(Lww::from(Payload("from")), Lww::Set(Payload("from")));
    let converted: Lww<Payload> = Payload("into").into();
    assert_eq!(converted, Lww::Set(Payload("into")));
}

#[test]
fn test_lww_codec_stable_envelopes_for_every_operation() {
    // [0, 0], [0, 1, payload bytes] and [0, 2] in DAG-CBOR.
    assert_codec_fixture(&Lww::<Opaque>::NoOp, &[0x82, 0x00, 0x00]);
    assert_codec_fixture(
        &Lww::Set(Opaque(b"ab".to_vec())),
        &[0x83, 0x00, 0x01, 0x42, b'a', b'b'],
    );
    assert_codec_fixture(&Lww::Set(Opaque(vec![])), &[0x83, 0x00, 0x01, 0x40]);
    assert_codec_fixture(&Lww::<Opaque>::Delete, &[0x82, 0x00, 0x02]);
    // The payload's own bytes are embedded verbatim, not re-serialized.
    let nested = Lww::Set(Lww::Set(Opaque(vec![7])));
    assert_codec_fixture(
        &nested,
        &[0x83, 0x00, 0x01, 0x45, 0x83, 0x00, 0x01, 0x41, 7],
    );
}

#[test]
fn test_lww_codec_keeps_delete_and_noop_distinct() {
    let set = Lww::Set(Opaque(vec![1]));
    let deleted = Lww::<Opaque>::decode(&Lww::<Opaque>::Delete.encode().unwrap()).unwrap();
    let noop = Lww::<Opaque>::decode(&Lww::<Opaque>::NoOp.encode().unwrap()).unwrap();
    assert_eq!(deleted, Lww::Delete);
    assert_eq!(noop, Lww::NoOp);
    // A decoded tombstone still removes the earlier value; NoOp does not.
    assert_eq!(set.merge(&deleted).unwrap(), Lww::Delete);
    assert_eq!(set.merge(&noop).unwrap(), set);
}

#[test]
fn test_lww_codec_composition_agrees_after_round_trip() {
    let operations = [
        Lww::NoOp,
        Lww::Set(Opaque(vec![])),
        Lww::Set(Opaque(vec![1])),
        Lww::Delete,
    ];
    let round_trip = |op: &Lww<Opaque>| Lww::decode(&op.encode().unwrap()).unwrap();
    for a in &operations {
        for b in &operations {
            let merged = a.merge(b).unwrap();
            let decoded = round_trip(a).merge(&round_trip(b)).unwrap();
            assert_eq!(decoded, merged, "{a:?}, {b:?}");
            assert_eq!(decoded.encode().unwrap(), merged.encode().unwrap());
        }
    }
}

#[test]
fn test_lww_codec_rejects_malformed_envelopes() {
    for bytes in [
        &[][..],                               // empty input
        &[0xff],                               // not CBOR
        &[0x00],                               // not an array
        &[0x80],                               // missing revision
        &[0x81, 0x00],                         // missing tag
        &[0x82, 0x01, 0x00],                   // unknown revision
        &[0x82, 0x20, 0x00],                   // negative revision
        &[0x82, 0x60, 0x00],                   // revision is not an integer
        &[0x82, 0x00, 0x03],                   // unknown tag
        &[0x82, 0x00, 0x61, b'0'],             // tag is not an integer
        &[0x83, 0x00, 0x00, 0x40],             // NoOp with a payload
        &[0x82, 0x00, 0x01],                   // Set without a payload
        &[0x84, 0x00, 0x01, 0x40, 0x40],       // Set with two payloads
        &[0x83, 0x00, 0x01, 0x61, b'a'],       // payload is text, not bytes
        &[0x83, 0x00, 0x02, 0x40],             // Delete with a payload
        &[0x82, 0x00, 0x00, 0x00],             // trailing data
        &[0x82, 0x00, 0x02, 0x82, 0x00, 0x02], // two concatenated values
    ] {
        assert_codec_rejects::<Lww<Opaque>>(bytes);
    }
    // Payload bytes the payload codec rejects are not accepted.
    assert_codec_rejects::<Lww<Opaque>>(&[0x83, 0x00, 0x01, 0x41, 0xff]);
}
