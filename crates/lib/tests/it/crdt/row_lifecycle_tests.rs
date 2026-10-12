//! Lifecycle laws through public APIs, without encoding or Table policy.

use eidetica::{
    Error, Result,
    crdt::{CRDT, CRDTError, Lww, Map, RowLifecycle},
};

// Concatenation is associative, noncommutative and non-idempotent.
// This application enum has no Default, Codec or Serde implementation.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Events {
    Valid(Vec<u8>),
    Rejected,
}

impl CRDT for Events {
    fn merge(&self, other: &Self) -> Result<Self> {
        match (self, other) {
            (Self::Valid(a), Self::Valid(b)) => {
                Ok(Self::Valid([a.as_slice(), b.as_slice()].concat()))
            }
            _ => Err(CRDTError::MergeFailed {
                reason: "rejected events".into(),
            }
            .into()),
        }
    }
}

use RowLifecycle::{Delete, NoOp, Reset, Update};

fn events(values: &[u8]) -> Events {
    Events::Valid(values.to_vec())
}

#[test]
fn test_row_lifecycle_all_pairings_and_identity() {
    let first = [NoOp, Update(events(&[1])), Reset(events(&[1])), Delete];
    let later = [NoOp, Update(events(&[2])), Reset(events(&[2])), Delete];
    let expected = [
        [NoOp, Update(events(&[2])), Reset(events(&[2])), Delete],
        [
            Update(events(&[1])),
            Update(events(&[1, 2])),
            Reset(events(&[2])),
            Delete,
        ],
        [
            Reset(events(&[1])),
            Reset(events(&[1, 2])),
            Reset(events(&[2])),
            Delete,
        ],
        [Delete, Delete, Reset(events(&[2])), Delete],
    ];
    for (i, a) in first.iter().enumerate() {
        assert_eq!(a.merge(&RowLifecycle::default()).unwrap(), *a);
        assert_eq!(RowLifecycle::default().merge(a).unwrap(), *a);
        for (j, b) in later.iter().enumerate() {
            assert_eq!(a.merge(b).unwrap(), expected[i][j], "{a:?}, {b:?}");
        }
    }
    assert_ne!(
        first[1].merge(&later[1]).unwrap(),
        later[1].merge(&first[1]).unwrap()
    );
    assert_ne!(
        Delete.merge(&later[2]).unwrap(),
        later[2].merge(&Delete).unwrap()
    );
}

// Independent sequential interpreter: append events directly, tracking the
// visible payload, deletion and whether a fresh boundary has been encountered.
fn reference(operations: &[RowLifecycle<Events>]) -> RowLifecycle<Events> {
    let mut payload: Option<Vec<u8>> = None;
    let mut deleted = false;
    let mut reset = false;
    for operation in operations {
        match operation {
            NoOp => (),
            Update(Events::Valid(values)) => {
                if !deleted {
                    payload.get_or_insert_with(Vec::new).extend(values);
                }
            }
            Reset(Events::Valid(values)) => {
                payload = Some(values.clone());
                deleted = false;
                reset = true;
            }
            Delete => {
                payload = None;
                deleted = true;
                reset = false;
            }
            _ => panic!("oracle requires valid operations"),
        }
    }
    if deleted {
        Delete
    } else if let Some(values) = payload {
        if reset {
            Reset(events(&values))
        } else {
            Update(events(&values))
        }
    } else {
        NoOp
    }
}

fn valid_operations() -> [RowLifecycle<Events>; 6] {
    [
        NoOp,
        Update(events(&[])),
        Update(events(&[1])),
        Update(events(&[2])),
        Reset(events(&[3])),
        Delete,
    ]
}

#[test]
fn test_row_lifecycle_exhaustive_triples_against_reference() {
    let operations = valid_operations();
    for a in &operations {
        for b in &operations {
            for c in &operations {
                let expected = reference(&[a.clone(), b.clone(), c.clone()]);
                assert_eq!(
                    a.merge(b).unwrap().merge(c).unwrap(),
                    expected,
                    "left: {a:?}, {b:?}, {c:?}"
                );
                assert_eq!(
                    a.merge(&b.merge(c).unwrap()).unwrap(),
                    expected,
                    "right: {a:?}, {b:?}, {c:?}"
                );
            }
        }
    }
}

// Enumerate every binary parenthesization without reordering operations.
fn regroupings(operations: &[RowLifecycle<Events>]) -> Vec<RowLifecycle<Events>> {
    if operations.len() == 1 {
        return vec![operations[0].clone()];
    }
    let mut results = Vec::new();
    for split in 1..operations.len() {
        for left in regroupings(&operations[..split]) {
            for right in regroupings(&operations[split..]) {
                results.push(left.merge(&right).unwrap());
            }
        }
    }
    results
}

#[test]
fn test_row_lifecycle_longer_regroupings_preserve_reset_boundaries() {
    let operations = valid_operations();
    // 6^5 sequences, each with all 14 binary parenthesizations.
    for mut index in 0..operations.len().pow(5) {
        let sequence: Vec<_> = (0..5)
            .map(|_| {
                let operation = operations[index % operations.len()].clone();
                index /= operations.len();
                operation
            })
            .collect();
        let expected = reference(&sequence);
        let results = regroupings(&sequence);
        assert_eq!(results.len(), 14);
        for result in results {
            assert_eq!(result, expected, "{sequence:?}");
        }
    }
}

#[test]
fn test_row_lifecycle_map_delta_composition_and_independent_rows() {
    let mut before = Map::new();
    before.insert("row", Update(events(&[1])));
    before.insert("untouched", Update(events(&[9])));
    let mut delta = Map::new();
    for operation in [Reset(events(&[2])), Update(events(&[3]))] {
        delta.insert_delta("row", operation).unwrap();
    }
    delta.insert_delta("gone", Delete).unwrap();
    delta.insert_delta("gone", Update(events(&[4]))).unwrap();
    delta.insert_delta("noop", NoOp).unwrap();
    let merged = before.merge(&delta).unwrap();
    assert_eq!(merged.get("row"), Some(&Reset(events(&[2, 3]))));
    assert_eq!(merged.get("gone"), Some(&Delete));
    assert_eq!(merged.get("noop"), Some(&NoOp));
    assert_eq!(merged.get("untouched"), before.get("untouched"));
    assert_eq!(merged.len(), 4);
    for (key, operation) in delta.iter() {
        before.insert_delta(*key, operation.clone()).unwrap();
    }
    assert_eq!(before, merged);
}

fn assert_merge_error(error: Error) {
    match error {
        Error::CRDT(error) => assert!(
            matches!(*error, CRDTError::MergeFailed { ref reason } if reason == "rejected events")
        ),
        error => panic!("unexpected error: {error}"),
    }
}

#[test]
fn test_row_lifecycle_payload_errors_propagate_without_changing_map() {
    let rejected = Update(Events::Rejected);
    for live in [Update(events(&[1])), Reset(events(&[1]))] {
        assert_merge_error(live.merge(&rejected).unwrap_err());
        let mut stored = Map::new();
        stored.insert("row", live);
        let original = stored.clone();
        assert_merge_error(stored.insert_delta("row", rejected.clone()).unwrap_err());
        assert_eq!(stored, original);
        let mut delta = Map::new();
        delta.insert("row", rejected.clone());
        assert_merge_error(stored.merge(&delta).unwrap_err());
        assert_eq!(stored, original);
        assert_eq!(delta.get("row"), Some(&rejected));
    }
    // NoOp, suppression, Reset and Delete do not call the payload merge.
    assert_eq!(NoOp.merge(&rejected).unwrap(), rejected);
    assert_eq!(Delete.merge(&rejected).unwrap(), Delete);
    assert_eq!(rejected.merge(&Delete).unwrap(), Delete);
    assert_eq!(
        rejected.merge(&Reset(events(&[2]))).unwrap(),
        Reset(events(&[2]))
    );
}

#[test]
fn test_row_lifecycle_fallible_evaluation_is_not_associative() {
    // The payload itself agrees under regrouping, including success vs error.
    let payloads = [events(&[]), events(&[1]), events(&[2]), Events::Rejected];
    for a in &payloads {
        for b in &payloads {
            for c in &payloads {
                let left = a.merge(b).and_then(|ab| ab.merge(c));
                let right = b.merge(c).and_then(|bc| a.merge(&bc));
                match (left, right) {
                    (Ok(left), Ok(right)) => assert_eq!(left, right),
                    (Err(left), Err(right)) => {
                        assert_merge_error(left);
                        assert_merge_error(right);
                    }
                    results => {
                        panic!("payload grouping disagrees: {a:?}, {b:?}, {c:?}: {results:?}")
                    }
                }
            }
        }
    }
    let a = Update(Events::Rejected);
    let b = Update(events(&[1]));
    // Even an associative partial payload cannot guarantee the same error
    // domain: regrouping may evaluate a merge that deletion/reset would skip.
    for terminal in [Delete, Reset(events(&[2]))] {
        assert_merge_error(a.merge(&b).and_then(|ab| ab.merge(&terminal)).unwrap_err());
        assert_eq!(a.merge(&b.merge(&terminal).unwrap()).unwrap(), terminal);
    }
    assert_eq!(Delete.merge(&a).unwrap().merge(&b).unwrap(), Delete);
    assert_merge_error(a.merge(&b).and_then(|ab| Delete.merge(&ab)).unwrap_err());
}

#[test]
fn test_row_lifecycle_inner_lww_tombstone_is_not_row_deletion() {
    let live = Update(Lww::Set(1));
    let inner_deleted = live.merge(&Update(Lww::Delete)).unwrap();
    assert_eq!(inner_deleted, Update(Lww::Delete));
    assert_eq!(
        inner_deleted.merge(&Update(2.into())).unwrap(),
        Update(Lww::Set(2))
    );
    let row_deleted = live.merge(&Delete).unwrap();
    assert_eq!(row_deleted.merge(&Update(Lww::Set(2))).unwrap(), Delete);
    assert_eq!(
        row_deleted.merge(&Reset(Lww::Set(3))).unwrap(),
        Reset(Lww::Set(3))
    );
}

#[test]
fn test_row_lifecycle_unbounded_construction_and_downstream_matching() {
    struct Opaque; // No Clone, CRDT, Default or encoding traits.
    assert!(matches!(RowLifecycle::<Opaque>::default(), NoOp));
    assert!(matches!(Update(Opaque), Update(_)));
    assert!(matches!(Reset(Opaque), Reset(_)));
    let row = RowLifecycle::<Opaque>::Delete;
    let row_kind = match row {
        NoOp => "noop",
        Update(_) => "update",
        Reset(_) => "reset",
        Delete => "delete",
        _ => "future",
    };
    let value_kind = match Lww::<Opaque>::Delete {
        Lww::NoOp => "noop",
        Lww::Set(_) => "set",
        Lww::Delete => "delete",
        _ => "future",
    };
    assert_eq!(row_kind, value_kind);
}
