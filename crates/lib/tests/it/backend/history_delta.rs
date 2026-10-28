//! v2 has a strict independent set oracle; permissive v1 is tested separately.
use crate::helpers::test_backend;
use eidetica::{
    Snapshot,
    backend::{BackendImpl, HistoryPageLimits},
    entry::{Entry, ID},
};
use std::collections::{BTreeSet, HashMap, HashSet};
const STORE: &str = "delta";
fn limits(n: usize) -> HistoryPageLimits {
    HistoryPageLimits {
        max_entries: n,
        max_encoded_bytes: 1 << 20,
        allow_large_singleton: false,
    }
}
fn closure(entries: &HashMap<ID, Entry>, tips: &Snapshot) -> HashSet<ID> {
    let mut seen = HashSet::new();
    let mut pending = tips.tips().to_vec();
    while let Some(id) = pending.pop() {
        if seen.insert(id.clone()) {
            pending.extend(entries[&id].subtree_parents(STORE).unwrap_or_default());
        }
    }
    seen
}
fn frontier(entries: &HashMap<ID, Entry>, set: &HashSet<ID>) -> Snapshot {
    let mut tips: BTreeSet<ID> = set.iter().cloned().collect();
    for id in set {
        for parent in entries[id].subtree_parents(STORE).unwrap_or_default() {
            tips.remove(&parent);
        }
    }
    Snapshot::from(tips.into_iter().collect::<Vec<_>>())
}
async fn check(
    b: &dyn BackendImpl,
    tree: &ID,
    entries: &HashMap<ID, Entry>,
    target: Snapshot,
    mut known: Snapshot,
    n: usize,
) {
    let t = closure(entries, &target);
    let k = closure(entries, &known);
    let mut expected: Vec<_> = t.difference(&k).cloned().collect();
    expected.sort_by_key(|id| (entries[id].subtree_height(STORE).unwrap_or(0), id.clone()));
    let mut covered: HashSet<_> = t.intersection(&k).cloned().collect();
    let mut got = Vec::new();
    for _ in 0..100 {
        let page = b
            .store_history_delta(tree, STORE, &target, &known, limits(n))
            .await
            .unwrap();
        assert!(page.entries.len() <= n);
        for entry in &page.entries {
            // Every prefix is ancestor-complete relative to the supplied known history.
            assert!(
                entry
                    .subtree_parents(STORE)
                    .unwrap_or_default()
                    .iter()
                    .all(|p| covered.contains(p))
            );
            assert!(covered.insert(entry.id()));
            got.push(entry.id());
        }
        assert_eq!(page.covered, frontier(entries, &covered));
        assert_eq!(page.complete, covered == t);
        if page.complete {
            assert_eq!(got, expected);
            return;
        }
        assert!(!page.entries.is_empty());
        known = page.covered;
    }
    panic!("delta failed to terminate");
}
async fn graph(b: &dyn BackendImpl) -> (ID, HashMap<ID, Entry>, Vec<ID>) {
    let root = Entry::root_builder()
        .set_subtree_data(STORE, "root")
        .build()
        .unwrap();
    let tree = root.id();
    let mut map = HashMap::new();
    let mut ids = vec![tree.clone()];
    b.put(root.clone()).await.unwrap();
    map.insert(tree.clone(), root);
    // Fork/merge diamond and divergent branch; main parents deliberately differ.
    for (height, parents) in [(1, vec![0]), (1, vec![0]), (2, vec![1, 2]), (3, vec![2])] {
        let mut builder = Entry::builder(tree.clone())
            .add_parent(tree.clone())
            .set_subtree_data(STORE, ids.len().to_string())
            .set_subtree_height(STORE, Some(height));
        for p in parents {
            builder = builder.add_subtree_parent(STORE, ids[p].clone());
        }
        let e = builder.build().unwrap();
        ids.push(e.id());
        b.put(e.clone()).await.unwrap();
        map.insert(e.id(), e);
    }
    (tree, map, ids)
}
#[tokio::test]
async fn history_delta_forks_redundancy_capture_and_independent_oracle() {
    let b = test_backend().await;
    let (tree, map, ids) = graph(&*b).await;
    for target in [
        Snapshot::EMPTY,
        Snapshot::from([ids[3].clone()]),
        Snapshot::from([ids[0].clone(), ids[3].clone()]),
        Snapshot::from([ids[3].clone(), ids[4].clone()]),
    ] {
        for known in [
            Snapshot::EMPTY,
            Snapshot::from([ids[1].clone()]),
            Snapshot::from([ids[0].clone(), ids[3].clone()]),
            Snapshot::from([ids[4].clone()]),
        ] {
            for n in [1, 2, 10] {
                check(&*b, &tree, &map, target.clone(), known.clone(), n).await;
            }
        }
    }
    // Above transfers fix target despite another branch already present.
}
#[tokio::test]
async fn history_delta_strict_missing_scope_and_store_order() {
    let b = test_backend().await;
    let (tree, _, ids) = graph(&*b).await;
    for (target, known, store) in [
        (
            Snapshot::from([ID::from_bytes("absent")]),
            Snapshot::EMPTY,
            STORE,
        ),
        (
            Snapshot::from([ids[3].clone()]),
            Snapshot::from([ID::from_bytes("absent")]),
            STORE,
        ),
        (Snapshot::from([ids[3].clone()]), Snapshot::EMPTY, "wrong"),
    ] {
        assert!(
            b.store_history_delta(&tree, store, &target, &known, limits(1))
                .await
                .is_err()
        );
    }
    let foreign = Entry::root_builder()
        .set_subtree_data(STORE, "foreign")
        .build()
        .unwrap();
    let foreign_id = foreign.id();
    b.put(foreign).await.unwrap();
    assert!(
        b.store_history_delta(
            &tree,
            STORE,
            &Snapshot::from([foreign_id]),
            &Snapshot::EMPTY,
            limits(1)
        )
        .await
        .is_err()
    );
    for (parent, height) in [
        (ids[3].clone(), 0),
        (ID::from_bytes("missing ancestor"), 10),
    ] {
        let child = Entry::builder(tree.clone())
            .add_parent(tree.clone())
            .set_subtree_data(STORE, "bad")
            .set_subtree_height(STORE, Some(height))
            .add_subtree_parent(STORE, parent)
            .build()
            .unwrap();
        let target = Snapshot::from([child.id()]);
        b.put(child).await.unwrap();
        assert!(
            b.store_history_delta(&tree, STORE, &target, &Snapshot::EMPTY, limits(1))
                .await
                .is_err()
        );
    }
}
#[tokio::test]
async fn history_delta_exact_bytes_singleton_frontier_and_binding() {
    let b = test_backend().await;
    let (tree, map, ids) = graph(&*b).await;
    let target = Snapshot::from([ids[3].clone()]);
    let page = b
        .store_history_delta(&tree, STORE, &target, &Snapshot::EMPTY, limits(2))
        .await
        .unwrap();
    let size = serde_json::to_vec(&page).unwrap().len();
    let mut cap = limits(2);
    cap.max_encoded_bytes = size;
    assert_eq!(
        b.store_history_delta(&tree, STORE, &target, &Snapshot::EMPTY, cap)
            .await
            .unwrap(),
        page
    );
    cap.max_encoded_bytes -= 1;
    let short = b
        .store_history_delta(&tree, STORE, &target, &Snapshot::EMPTY, cap)
        .await
        .unwrap();
    assert_eq!(short.entries.len(), 1);
    assert!(!short.complete);
    let firstsize = serde_json::to_vec(&short).unwrap().len();
    cap.max_encoded_bytes = firstsize - 1;
    assert!(
        b.store_history_delta(&tree, STORE, &target, &Snapshot::EMPTY, cap)
            .await
            .is_err()
    );
    cap.allow_large_singleton = true;
    let large = b
        .store_history_delta(&tree, STORE, &target, &Snapshot::EMPTY, cap)
        .await
        .unwrap();
    assert_eq!(large.entries.len(), 1);
    assert!(serde_json::to_vec(&large).unwrap().len() > cap.max_encoded_bytes);
    let redundant = Snapshot::from([tree.clone(), ids[3].clone()]);
    let other = b
        .store_history_delta(&tree, STORE, &redundant, &target, limits(2))
        .await
        .unwrap();
    assert!(other.complete && other.entries.is_empty());
    assert_eq!(other.covered, frontier(&map, &closure(&map, &target)));
    assert_ne!(
        other.binding, page.binding,
        "binding covers exact target input, not just its closure"
    );
}

#[tokio::test]
async fn history_delta_exhaustive_four_vertex_set_oracle() {
    use eidetica::backend::database::InMemory;
    let mut cases = 0;
    for mask in 0u8..64 {
        let b = InMemory::new();
        let root = Entry::root_builder()
            .set_subtree_data(STORE, "root")
            .build()
            .unwrap();
        let tree = root.id();
        let mut entries = HashMap::from([(tree.clone(), root.clone())]);
        b.put(root).await.unwrap();
        let mut ids = vec![tree.clone()];
        let mut edge = 0;
        for child in 1..4 {
            let mut builder = Entry::builder(tree.clone())
                .add_parent(tree.clone())
                .set_subtree_data(STORE, child.to_string())
                .set_subtree_height(STORE, Some(child as u64));
            for parent in ids.iter().take(child) {
                if mask & (1 << edge) != 0 {
                    builder = builder.add_subtree_parent(STORE, parent.clone());
                }
                edge += 1;
            }
            let entry = builder.build().unwrap();
            ids.push(entry.id());
            b.put(entry.clone()).await.unwrap();
            entries.insert(entry.id(), entry);
        }
        let snapshot = |mask: u8| {
            Snapshot::from(
                ids.iter()
                    .enumerate()
                    .filter(|(n, _)| mask & (1 << n) != 0)
                    .map(|(_, id)| id.clone())
                    .collect::<Vec<_>>(),
            )
        };
        for target in 0..16 {
            for known in 0..16 {
                for count in [1, 3] {
                    check(
                        &b,
                        &tree,
                        &entries,
                        snapshot(target),
                        snapshot(known),
                        count,
                    )
                    .await;
                    cases += 1;
                }
            }
        }
    }
    eprintln!(
        "DELTA ORACLE: {cases} actual Rust continuation partitions across 64 ordered four-vertex DAGs; 0 failures"
    );
}

#[tokio::test]
async fn history_delta_equal_height_uses_full_cid_order_and_fixed_target() {
    let b = test_backend().await;
    let root = Entry::root_builder()
        .set_subtree_data(STORE, "root")
        .build()
        .unwrap();
    let tree = root.id();
    b.put(root.clone()).await.unwrap();
    // Search deterministic bodies for both directions at the same Store height.
    let mut good = None;
    let mut bad = None;
    for n in 0..100 {
        let e = Entry::builder(tree.clone())
            .add_parent(tree.clone())
            .set_subtree_data(STORE, n.to_string())
            .set_subtree_height(STORE, Some(0))
            .add_subtree_parent(STORE, tree.clone())
            .build()
            .unwrap();
        if e.id() > tree {
            good = Some(e);
        } else {
            bad = Some(e);
        }
        if good.is_some() && bad.is_some() {
            break;
        }
    }
    let good = good.unwrap();
    let bad = bad.unwrap();
    b.put(good.clone()).await.unwrap();
    b.put(bad.clone()).await.unwrap();
    let target = Snapshot::from([good.id()]);
    let first = b
        .store_history_delta(&tree, STORE, &target, &Snapshot::EMPTY, limits(1))
        .await
        .unwrap();
    assert_eq!(first.entries, vec![root]);
    // New writes cannot change membership of the fixed target.
    let new = Entry::builder(tree.clone())
        .add_parent(good.id())
        .set_subtree_data(STORE, "new")
        .set_subtree_height(STORE, Some(1))
        .add_subtree_parent(STORE, good.id())
        .build()
        .unwrap();
    b.put(new).await.unwrap();
    let last = b
        .store_history_delta(&tree, STORE, &target, &first.covered, limits(1))
        .await
        .unwrap();
    assert_eq!(last.entries, vec![good]);
    assert!(last.complete);
    assert!(
        b.store_history_delta(
            &tree,
            STORE,
            &Snapshot::from([bad.id()]),
            &Snapshot::EMPTY,
            limits(1)
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn history_delta_wide_intermediate_frontier_is_never_truncated() {
    let b = test_backend().await;
    let root = Entry::root_builder()
        .set_subtree_data(STORE, "root")
        .build()
        .unwrap();
    let tree = root.id();
    b.put(root).await.unwrap();
    let mut tips = Vec::new();
    for n in 0..150 {
        let e = Entry::builder(tree.clone())
            .add_parent(tree.clone())
            .set_subtree_data(STORE, n.to_string())
            .set_subtree_height(STORE, Some(1))
            .add_subtree_parent(STORE, tree.clone())
            .build()
            .unwrap();
        tips.push(e.id());
        b.put(e).await.unwrap();
    }
    let mut merge = Entry::builder(tree.clone())
        .add_parent(tree.clone())
        .set_subtree_data(STORE, "merge")
        .set_subtree_height(STORE, Some(2));
    for tip in &tips {
        merge = merge.add_subtree_parent(STORE, tip.clone());
    }
    let merge = merge.build().unwrap();
    let target = Snapshot::from([merge.id()]);
    b.put(merge).await.unwrap();
    let mut known = Snapshot::EMPTY;
    let mut emitted = 0;
    let mut cap = limits(1);
    cap.max_encoded_bytes = 4096;
    for _ in 0..200 {
        match b
            .store_history_delta(&tree, STORE, &target, &known, cap)
            .await
        {
            Ok(p) => {
                assert!(
                    !p.complete,
                    "small budget cannot represent wide intermediate frontier"
                );
                emitted += p.entries.len();
                assert_eq!(p.covered.len(), emitted.saturating_sub(1).max(1));
                known = p.covered;
            }
            Err(_) => {
                assert!(
                    emitted > 5 && emitted < 151,
                    "fixture did not reach a wide progress frontier"
                );
                return;
            }
        }
    }
    panic!("unrepresentable frontier did not fail");
}

#[tokio::test]
async fn history_delta_shared_snapshot_vocabulary_does_not_relabel_p2p_progress() {
    use eidetica::sync::protocol::{IncrementalResponse, SyncTreeRequest};
    let b = test_backend().await;
    let (tree, map, ids) = graph(&*b).await;
    let req = SyncTreeRequest {
        tree_id: tree.clone(),
        our_tips: Snapshot::from([ids[4].clone()]),
        peer_pubkey: None,
        requesting_key: None,
        requesting_key_name: None,
        requested_permission: None,
        metadata: None,
        auth: None,
    };
    let advertised = IncrementalResponse {
        tree_id: tree.clone(),
        their_tips: vec![ids[3].clone()],
        missing_entries: Vec::new(),
    };
    // Vocabulary mapping only: current P2P wire has no covered/completion fields
    // and its main-tree helper is deliberately not the Store delta oracle.
    let target = Snapshot::from(advertised.their_tips);
    let page = b
        .store_history_delta(&tree, STORE, &target, &req.our_tips, limits(1))
        .await
        .unwrap();
    let mut expected = closure(&map, &target);
    let k = closure(&map, &req.our_tips);
    expected.retain(|id| k.contains(id));
    expected.extend(page.entries.iter().map(Entry::id));
    assert_eq!(page.covered, frontier(&map, &expected));
    assert_ne!(page.covered, target);
    check(&*b, &tree, &map, target, req.our_tips, 1).await;
}
