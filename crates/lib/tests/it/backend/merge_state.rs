//! Tests for [`BackendImpl::compute_merge_state`] — the merge base and the path
//! leading from it, derived as one pair.
//!
//! The two halves are only meaningful together. Reading them separately lets an
//! ingest land in between, and a backfilled ancestor can open a path around the
//! base that the base computation never saw; the path then carries entries the
//! base state already contains, and replaying them on top can regress newer
//! values. `test_split_calls_straddling_an_ingest_are_inconsistent` builds that
//! situation deterministically.

use eidetica::backend::BackendImpl;
use eidetica::backend::database::InMemory;
use eidetica::entry::{Entry, ID};

use super::helpers::test_backend;
use crate::helpers::TestVerify;

const STORE: &str = "data";

/// Ancestors of `entry` within the store, including `entry` itself: the
/// empty-base walk runs to the store roots.
async fn ancestors(backend: &dyn BackendImpl, tree: &ID, entry: &ID) -> Vec<ID> {
    backend
        .get_path_from_to(tree, STORE, None, std::slice::from_ref(entry))
        .await
        .expect("ancestor collection should succeed")
}

/// The property a merge base and its path must hold together: nothing on the
/// path may be the base or an ancestor of it, or the path replays entries the
/// base state already folded in.
async fn assert_path_below_base(backend: &dyn BackendImpl, tree: &ID, base: &ID, path: &[ID]) {
    let below = ancestors(backend, tree, base).await;
    for id in path {
        assert!(
            !below.contains(id),
            "path entry {id} is the merge base {base} or an ancestor of it"
        );
    }
}

/// Builds the DAG below, storing everything except `X`, which is returned
/// unstored so a test can decide when it "arrives".
///
/// ```text
///     R
///    / \
///   A   X      (X withheld)
///   |   |
///   B   |
///  / \ /
/// E   F
/// ```
///
/// `F` names `X` as a store parent, so while `X` is missing, `F`'s only
/// traversable route to the root runs through `B`.
async fn build_dag(backend: &dyn BackendImpl) -> (ID, ID, ID, ID, Entry) {
    let r = Entry::root_builder()
        .set_subtree_data(STORE, b"r")
        .set_height(0)
        .set_subtree_height(STORE, Some(0))
        .build()
        .expect("root should build");
    let r_id = r.id();
    backend.put_verified(r).await.unwrap();

    let a = Entry::builder(r_id.clone())
        .add_parent(r_id.clone())
        .set_subtree_data(STORE, b"a")
        .add_subtree_parent(STORE, r_id.clone())
        .set_height(1)
        .set_subtree_height(STORE, Some(1))
        .build()
        .expect("A should build");
    let a_id = a.id();
    backend.put_verified(a).await.unwrap();

    let b = Entry::builder(r_id.clone())
        .add_parent(a_id.clone())
        .set_subtree_data(STORE, b"b")
        .add_subtree_parent(STORE, a_id.clone())
        .set_height(2)
        .set_subtree_height(STORE, Some(2))
        .build()
        .expect("B should build");
    let b_id = b.id();
    backend.put_verified(b).await.unwrap();

    let x = Entry::builder(r_id.clone())
        .add_parent(r_id.clone())
        .set_subtree_data(STORE, b"x")
        .add_subtree_parent(STORE, r_id.clone())
        .set_height(1)
        .set_subtree_height(STORE, Some(1))
        .build()
        .expect("X should build");
    let x_id = x.id();

    let e = Entry::builder(r_id.clone())
        .add_parent(b_id.clone())
        .set_subtree_data(STORE, b"e")
        .add_subtree_parent(STORE, b_id.clone())
        .set_height(3)
        .set_subtree_height(STORE, Some(3))
        .build()
        .expect("E should build");
    let e_id = e.id();
    backend.put_verified(e).await.unwrap();

    let f = Entry::builder(r_id.clone())
        .add_parent(b_id.clone())
        .add_parent(x_id.clone())
        .set_subtree_data(STORE, b"f")
        .add_subtree_parent(STORE, b_id.clone())
        .add_subtree_parent(STORE, x_id)
        .set_height(3)
        .set_subtree_height(STORE, Some(3))
        .build()
        .expect("F should build");
    let f_id = f.id();
    backend.put_verified(f).await.unwrap();

    (r_id, b_id, e_id, f_id, x)
}

/// The hazard the fused call exists to close: a base read before an ingest and
/// a path read after it do not describe the same DAG.
///
/// Uses `InMemory` directly rather than the backend matrix — the SQL engines
/// treat an entry that is referenced but absent as a root, so the DAG resolves
/// differently there while the ancestor is still missing.
#[tokio::test]
async fn test_split_calls_straddling_an_ingest_are_inconsistent() {
    let backend = InMemory::new();
    let (tree, b_id, e_id, f_id, x) = build_dag(&backend).await;
    let tips = [e_id, f_id];

    // With X missing, B dominates both tips.
    let base = backend
        .find_merge_base(&tree, STORE, &tips)
        .await
        .expect("merge base should resolve")
        .expect("the tips share a common ancestor");
    assert_eq!(base, b_id, "B is the merge base while X is missing");

    // X arrives — a backfilled ancestor, exactly what sync does.
    backend.put_verified(x).await.unwrap();

    // The path is now read against a DAG in which F reaches the root without
    // passing through B, so it runs past the base and picks up the base's own
    // ancestors.
    let path = backend
        .get_path_from_to(&tree, STORE, Some(&base), &tips)
        .await
        .expect("path should resolve");
    let below = ancestors(&backend, &tree, &base).await;
    assert!(
        path.iter().any(|id| below.contains(id)),
        "the split pair should be inconsistent: path {path:?} vs ancestors of {base}: {below:?}"
    );
}

/// The same DAG through the fused call, once the withheld ancestor has landed:
/// the base moves off `B` — `F` now reaches the root around it — and the pair
/// the call returns is self-consistent, because both halves describe the DAG as
/// it stood when the call started.
#[tokio::test]
async fn test_merge_state_is_consistent_after_an_ingest() {
    let backend = InMemory::new();
    let (tree, b_id, e_id, f_id, x) = build_dag(&backend).await;
    let tips = [e_id, f_id];

    backend.put_verified(x).await.unwrap();

    let state = backend
        .compute_merge_state(&tree, STORE, &tips)
        .await
        .expect("merge state should resolve");
    let base = state
        .merge_base
        .clone()
        .expect("the tips share a common ancestor");
    assert_ne!(base, b_id, "with X present, B no longer dominates F");
    assert_path_below_base(&backend, &tree, &base, &state.path).await;
}

/// On a quiescent DAG the fused call answers exactly as the two calls it
/// replaces. Runs across the backend matrix, so it covers the SQL engines'
/// single-transaction implementation as well.
#[tokio::test]
async fn test_merge_state_matches_split_calls() {
    let backend = test_backend().await;
    let (tree, _b_id, e_id, f_id, x) = build_dag(&*backend).await;
    backend.put_verified(x).await.unwrap();
    let tips = [e_id, f_id];

    let state = backend
        .compute_merge_state(&tree, STORE, &tips)
        .await
        .expect("merge state should resolve");

    let base = backend
        .find_merge_base(&tree, STORE, &tips)
        .await
        .unwrap()
        .expect("the tips share a common ancestor");
    let path = backend
        .get_path_from_to(&tree, STORE, Some(&base), &tips)
        .await
        .unwrap();

    assert_eq!(state.merge_base, Some(base.clone()));
    assert_eq!(state.path, path);
    assert_path_below_base(&*backend, &tree, &base, &state.path).await;
}

/// Tips with no common ancestor are a valid result, not an error: the fused
/// call reports no base and leaves the path empty, and the caller folds the
/// full ancestry from a default state.
#[tokio::test]
async fn test_merge_state_reports_disjoint_histories() {
    let backend = test_backend().await;

    // A tree root that carries no store data, so the two store writes below
    // have no store parent in common.
    let root = Entry::root_builder()
        .set_height(0)
        .build()
        .expect("root should build");
    let tree = root.id();
    backend.put_verified(root).await.unwrap();

    let mut tips = Vec::new();
    for payload in [b"left".as_slice(), b"right".as_slice()] {
        let entry = Entry::builder(tree.clone())
            .add_parent(tree.clone())
            .set_subtree_data(STORE, payload)
            .set_height(1)
            .set_subtree_height(STORE, Some(0))
            .build()
            .expect("store root should build");
        tips.push(entry.id());
        backend.put_verified(entry).await.unwrap();
    }

    let state = backend
        .compute_merge_state(&tree, STORE, &tips)
        .await
        .expect("merge state should resolve");

    assert_eq!(
        state.merge_base, None,
        "independently created store roots share no ancestor"
    );
    assert!(
        state.path.is_empty(),
        "the empty-base case folds the full ancestry instead of a path, got {:?}",
        state.path
    );
}
