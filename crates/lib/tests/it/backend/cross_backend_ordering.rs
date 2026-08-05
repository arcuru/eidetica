//! Traversal order must be identical across backends.
//!
//! Entries are ordered by height, then by ID as a tiebreak. The normative ID
//! order is `ID`'s `Ord` — the CID tuple (version, codec, multihash code,
//! length, digest bytes). Any backend that orders by the base32lower *string*
//! form instead will disagree with that on some inputs: base32lower encodes
//! values 26-31 as the digits `2`-`7`, which sort *before* letters in ASCII
//! while representing larger 5-bit values. The encoding is therefore not
//! order-preserving.
//!
//! This matters beyond aesthetics: traversal order feeds the CRDT merge, so two
//! backends holding the same DAG can resolve last-writer-wins differently and
//! materialize different state.

use eidetica::{
    backend::BackendImpl,
    backend::database::{InMemory, Sqlite},
    entry::{Entry, ID},
};

use crate::helpers::TestVerify;

/// Number of same-height siblings to build.
///
/// Order divergence needs one pair whose first differing base32 character has a
/// digit on one side and a letter on the other. That is common enough that a
/// handful of random CIDs is nearly certain to contain such a pair, but the
/// count is generous so the test does not become flaky on an unlucky draw.
const SIBLINGS: usize = 24;

const STORE: &str = "data";

/// Build a root plus `SIBLINGS` children that all sit at the same store height,
/// writing every entry into each backend so both hold a byte-identical DAG.
async fn build_fan_out(backends: &[&dyn BackendImpl]) -> ID {
    let root = Entry::root_builder()
        .set_subtree_data(STORE, r#"{"seed":0}"#)
        .build()
        .expect("root entry should build");
    let root_id = root.id();

    for backend in backends {
        backend.put_verified(root.clone()).await.unwrap();
    }

    // Every child has the same single parent, so all share one store height.
    // Distinct payloads give distinct content-addressed IDs.
    for i in 0..SIBLINGS {
        let child = Entry::builder(root_id.clone())
            .add_parent(root_id.clone())
            .set_subtree_data(STORE, format!(r#"{{"n":{i}}}"#).as_str())
            .build()
            .expect("child entry should build");

        for backend in backends {
            backend.put_verified(child.clone()).await.unwrap();
        }
    }

    root_id
}

/// The two reference backends must return store entries in the same order.
#[tokio::test]
async fn test_get_store_order_matches_across_backends() {
    let mem = InMemory::new();
    let sql = Sqlite::in_memory().await.expect("sqlite backend");

    let root_id = build_fan_out(&[&mem, &sql]).await;

    let mem_order: Vec<ID> = mem
        .get_store(&root_id, STORE)
        .await
        .unwrap()
        .iter()
        .map(|e| e.id())
        .collect();
    let sql_order: Vec<ID> = sql
        .get_store(&root_id, STORE)
        .await
        .unwrap()
        .iter()
        .map(|e| e.id())
        .collect();

    assert_eq!(
        mem_order.len(),
        SIBLINGS + 1,
        "both the root and every sibling should be in the store"
    );
    assert_eq!(
        mem_order, sql_order,
        "backends disagree on store traversal order; \
         this feeds CRDT merge order and so can diverge materialized state"
    );
}

/// The same requirement for whole-tree traversal.
#[tokio::test]
async fn test_get_tree_order_matches_across_backends() {
    let mem = InMemory::new();
    let sql = Sqlite::in_memory().await.expect("sqlite backend");

    let root_id = build_fan_out(&[&mem, &sql]).await;

    let mem_order: Vec<ID> = mem
        .get_tree(&root_id)
        .await
        .unwrap()
        .iter()
        .map(|e| e.id())
        .collect();
    let sql_order: Vec<ID> = sql
        .get_tree(&root_id)
        .await
        .unwrap()
        .iter()
        .map(|e| e.id())
        .collect();

    assert_eq!(
        mem_order, sql_order,
        "backends disagree on tree traversal order"
    );
}

/// Pin the normative order itself, so a future change that makes both backends
/// agree on the *wrong* order still fails.
///
/// Sibling entries share a height, so the tiebreak is `ID`'s `Ord`.
#[tokio::test]
async fn test_sibling_order_follows_cid_ord() {
    let mem = InMemory::new();
    let sql = Sqlite::in_memory().await.expect("sqlite backend");

    let root_id = build_fan_out(&[&mem, &sql]).await;

    for (label, backend) in [
        ("in-memory", &mem as &dyn BackendImpl),
        ("sqlite", &sql as &dyn BackendImpl),
    ] {
        let siblings: Vec<ID> = backend
            .get_store(&root_id, STORE)
            .await
            .unwrap()
            .iter()
            .map(|e| e.id())
            .filter(|id| *id != root_id)
            .collect();

        let mut expected = siblings.clone();
        expected.sort();

        assert_eq!(
            siblings, expected,
            "{label} backend does not order same-height siblings by CID Ord"
        );
    }
}
