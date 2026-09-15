//! A traversal over a partially-synced history must report the gap, not
//! silently return the part it can reach.
//!
//! Under partial sync a node can hold a child whose parents have not arrived.
//! Every ancestor walk here is the input to a CRDT fold whose result is cached
//! per entry, so a truncated walk does not merely return less — it produces a
//! state that is wrong and stays wrong after the missing entries arrive.
//!
//! Each test builds the same DAG twice: once whole, to pin that the check does
//! not fire on a complete history, and once with one entry withheld, to pin
//! that it does.

use eidetica::{Snapshot, entry::Entry, entry::ID};

use super::helpers::test_backend;
use crate::helpers::TestVerify;

const STORE: &str = "data";

/// A linear store history `root <- a <- b`, returned as (entries, ids) in
/// storage order.
fn linear_history() -> (Vec<Entry>, Vec<ID>) {
    let root = Entry::root_builder()
        .set_subtree_data(STORE, b"root")
        .set_subtree_height(STORE, Some(0))
        .build()
        .expect("root should build");
    let root_id = root.id();

    let a = Entry::builder(root_id.clone())
        .add_parent(root_id.clone())
        .set_subtree_data(STORE, b"a")
        .add_subtree_parent(STORE, root_id.clone())
        .set_subtree_height(STORE, Some(1))
        .build()
        .expect("a should build");
    let a_id = a.id();

    let b = Entry::builder(root_id.clone())
        .add_parent(a_id.clone())
        .set_subtree_data(STORE, b"b")
        .add_subtree_parent(STORE, a_id.clone())
        .set_subtree_height(STORE, Some(2))
        .build()
        .expect("b should build");
    let b_id = b.id();

    (vec![root, a, b], vec![root_id, a_id, b_id])
}

/// A forked store history: `root <- a <- {left, right}`.
fn forked_history() -> (Vec<Entry>, Vec<ID>) {
    let (mut entries, mut ids) = linear_history();
    let (root_id, a_id) = (ids[0].clone(), ids[1].clone());

    // `entries[2]` / `ids[2]` is the left branch from the linear history.
    let right = Entry::builder(root_id)
        .add_parent(a_id.clone())
        .set_subtree_data(STORE, b"right")
        .add_subtree_parent(STORE, a_id)
        .set_subtree_height(STORE, Some(2))
        .build()
        .expect("right should build");
    ids.push(right.id());
    entries.push(right);

    (entries, ids)
}

/// Store `entries`, skipping the one at `withhold` if given.
async fn backend_holding(
    entries: &[Entry],
    withhold: Option<&ID>,
) -> Box<dyn eidetica::backend::BackendImpl> {
    let backend = test_backend().await;
    for entry in entries {
        if Some(&entry.id()) == withhold {
            continue;
        }
        backend
            .put_verified(entry.clone())
            .await
            .expect("entry should store");
    }
    backend
}

#[tokio::test]
async fn store_at_returns_the_whole_history_when_it_is_whole() {
    let (entries, ids) = linear_history();
    let backend = backend_holding(&entries, None).await;

    let folded = backend
        .store_at(&ids[0], STORE, &Snapshot::from(vec![ids[2].clone()]))
        .await
        .expect("a complete history should traverse");

    let got: Vec<ID> = folded.iter().map(|entry| entry.id()).collect();
    assert_eq!(got, ids, "expected root, a, b in store-height order");
}

#[tokio::test]
async fn store_at_reports_an_ancestor_it_does_not_hold() {
    let (entries, ids) = linear_history();
    // `a` sits between the root and the tip: exactly the entry a partial sync
    // can be missing while still holding the tip that depends on it.
    let backend = backend_holding(&entries, Some(&ids[1])).await;

    let err = backend
        .store_at(&ids[0], STORE, &Snapshot::from(vec![ids[2].clone()]))
        .await
        .expect_err("a truncated history must not fold silently");

    assert!(
        err.is_incomplete_history(),
        "expected IncompleteHistory, got: {err:?}"
    );
    assert!(
        err.to_string().contains(&ids[1].to_string()),
        "the error should name the missing ancestor: {err}"
    );
}

#[tokio::test]
async fn store_at_reports_a_tip_it_does_not_hold() {
    let (entries, ids) = linear_history();
    let backend = backend_holding(&entries, Some(&ids[2])).await;

    let err = backend
        .store_at(&ids[0], STORE, &Snapshot::from(vec![ids[2].clone()]))
        .await
        .expect_err("a tip we do not hold is an unreachable branch, not an empty one");

    assert!(
        err.is_incomplete_history(),
        "expected IncompleteHistory, got: {err:?}"
    );
}

#[tokio::test]
async fn store_at_still_ignores_a_held_tip_that_is_not_in_the_store() {
    let (entries, ids) = linear_history();
    let backend = backend_holding(&entries, None).await;

    // The tip is held; it just has nothing to contribute to this store. That
    // is an empty answer, not a gap in the history.
    let folded = backend
        .store_at(
            &ids[0],
            "no_such_store",
            &Snapshot::from(vec![ids[2].clone()]),
        )
        .await
        .expect("a held tip outside the store should not error");

    assert!(folded.is_empty());
}

#[tokio::test]
async fn find_merge_base_resolves_a_fork_it_holds_whole() {
    let (entries, ids) = forked_history();
    let backend = backend_holding(&entries, None).await;

    let base = backend
        .find_merge_base(&ids[0], STORE, &[ids[2].clone(), ids[3].clone()])
        .await
        .expect("a complete fork should resolve");

    assert_eq!(base, Some(ids[1].clone()), "the fork point is `a`");
}

#[tokio::test]
async fn find_merge_base_reports_an_ancestor_it_does_not_hold() {
    let (entries, ids) = forked_history();
    // Without `a` the two branches look like independent roots, and any merge
    // base derived from them would be too shallow.
    let backend = backend_holding(&entries, Some(&ids[1])).await;

    let err = backend
        .find_merge_base(&ids[0], STORE, &[ids[2].clone(), ids[3].clone()])
        .await
        .expect_err("a truncated fork must not resolve to a shallower base");

    assert!(
        err.is_incomplete_history(),
        "expected IncompleteHistory, got: {err:?}"
    );
}

#[tokio::test]
async fn get_path_from_to_walks_a_history_it_holds_whole() {
    let (entries, ids) = linear_history();
    let backend = backend_holding(&entries, None).await;

    let path = backend
        .get_path_from_to(&ids[0], STORE, Some(&ids[0]), &[ids[2].clone()])
        .await
        .expect("a complete path should walk");

    assert_eq!(path, vec![ids[1].clone(), ids[2].clone()]);
}

#[tokio::test]
async fn get_path_from_to_reports_an_entry_it_does_not_hold() {
    let (entries, ids) = linear_history();
    let backend = backend_holding(&entries, Some(&ids[1])).await;

    let err = backend
        .get_path_from_to(&ids[0], STORE, Some(&ids[0]), &[ids[2].clone()])
        .await
        .expect_err("a path missing a segment must not be merged over");

    assert!(
        err.is_incomplete_history(),
        "expected IncompleteHistory, got: {err:?}"
    );
}
