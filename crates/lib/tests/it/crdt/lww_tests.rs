//! Laws for ordered register operations, independent of payload encoding.

use eidetica::crdt::{CRDT, Lww};

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
