//! Selected-vertex frontier conformance. Historical materialization and the
//! ancestor-closed Verified frontier are intentionally separate contracts.

use std::collections::HashSet;

use eidetica::{
    Snapshot,
    backend::{BackendImpl, VerificationStatus},
    entry::{Entry, ID},
};

use super::helpers::test_backend;
use crate::helpers::TestVerify;

fn child(tree: &ID, parents: &[ID], tag: &str) -> Entry {
    parents
        .iter()
        .fold(
            Entry::builder(tree.clone()).set_subtree_data("test", tag.as_bytes()),
            |builder, parent| builder.add_parent(parent.clone()),
        )
        .build()
        .unwrap()
}

/// Slow, test-only reconstruction from immutable edges and current labels.
async fn check(backend: &dyn BackendImpl, tree: &ID, expected: &[ID]) {
    let retained = backend.non_failed_snapshot(tree).await.unwrap();
    assert_eq!(retained, Snapshot::new(expected.to_vec()));
    let entries = backend.get_tree(tree).await.unwrap();
    let mut selected = Vec::new();
    for entry in entries {
        if entry.root().unwrap_or_else(|| entry.id()) == *tree
            && backend.get_verification_status(&entry.id()).await.unwrap()
                != VerificationStatus::Failed
        {
            selected.push(entry);
        }
    }
    let mut oracle: HashSet<_> = selected.iter().map(Entry::id).collect();
    for entry in selected {
        for parent in entry.parents().unwrap() {
            oracle.remove(&parent);
        }
    }
    assert_eq!(retained, Snapshot::new(oracle.into_iter().collect()));
}

#[tokio::test]
async fn non_failed_chain_restores_parent_without_raw_or_verified_change() {
    let backend = test_backend().await;
    let root = Entry::root_builder().build().unwrap();
    let tree = root.id();
    backend.put_verified(root).await.unwrap();
    let middle = child(&tree, std::slice::from_ref(&tree), "middle");
    let leaf = child(&tree, &[middle.id()], "leaf");
    backend.put(middle.clone()).await.unwrap();
    backend.put(leaf.clone()).await.unwrap();
    backend
        .update_verification_status(&leaf.id(), VerificationStatus::Failed)
        .await
        .unwrap();
    check(&*backend, &tree, &[middle.id()]).await;
    let raw = backend.snapshot(&tree).await.unwrap();
    let verified = backend.verified_snapshot(&tree).await.unwrap();
    backend
        .update_verification_status(&middle.id(), VerificationStatus::Failed)
        .await
        .unwrap();
    check(&*backend, &tree, std::slice::from_ref(&tree)).await;
    assert_eq!(backend.snapshot(&tree).await.unwrap(), raw);
    assert_eq!(backend.verified_snapshot(&tree).await.unwrap(), verified);
    // Failed remains terminal, including duplicate reception and assignment.
    backend.put(middle.clone()).await.unwrap();
    backend
        .update_verification_status(&middle.id(), VerificationStatus::Failed)
        .await
        .unwrap();
    assert!(
        backend
            .update_verification_status(&middle.id(), VerificationStatus::Unverified)
            .await
            .is_err()
    );
    check(&*backend, &tree, std::slice::from_ref(&tree)).await;
}

#[tokio::test]
async fn non_failed_diamond_failure_orders_match_oracle() {
    for reverse in [false, true] {
        let backend = test_backend().await;
        let root = Entry::root_builder().build().unwrap();
        let tree = root.id();
        let left = child(&tree, std::slice::from_ref(&tree), "left");
        let right = child(&tree, std::slice::from_ref(&tree), "right");
        let join = child(&tree, &[left.id(), right.id()], "join");
        let mut entries = vec![root, left.clone(), right.clone(), join.clone()];
        if reverse {
            entries.reverse();
        }
        for entry in &entries {
            backend.put(entry.clone()).await.unwrap();
            backend.put(entry.clone()).await.unwrap();
        }
        check(&*backend, &tree, &[join.id()]).await;
        backend
            .update_verification_status(&tree, VerificationStatus::Verified)
            .await
            .unwrap();
        backend
            .update_verification_status(&left.id(), VerificationStatus::Verified)
            .await
            .unwrap();
        backend
            .update_verification_status(&left.id(), VerificationStatus::Unverified)
            .await
            .unwrap();
        check(&*backend, &tree, &[join.id()]).await;
        backend
            .update_verification_status(&join.id(), VerificationStatus::Failed)
            .await
            .unwrap();
        check(&*backend, &tree, &[left.id(), right.id()]).await;
        backend
            .update_verification_status(&left.id(), VerificationStatus::Failed)
            .await
            .unwrap();
        check(&*backend, &tree, &[right.id()]).await;
        backend
            .update_verification_status(&right.id(), VerificationStatus::Failed)
            .await
            .unwrap();
        check(&*backend, &tree, std::slice::from_ref(&tree)).await;
        backend
            .update_verification_status(&tree, VerificationStatus::Failed)
            .await
            .unwrap();
        check(&*backend, &tree, &[]).await;
        backend.reset_local_verification().await.unwrap();
        check(&*backend, &tree, &[join.id()]).await;
        assert!(backend.verified_snapshot(&tree).await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn non_failed_missing_parent_arrival_and_cross_tree_isolation() {
    let backend = test_backend().await;
    let root = Entry::root_builder().build().unwrap();
    let tree = root.id();
    let parent = child(&tree, std::slice::from_ref(&tree), "parent");
    let leaf = child(&tree, &[parent.id()], "leaf");
    backend.put(leaf.clone()).await.unwrap();
    check(&*backend, &tree, &[leaf.id()]).await;
    backend
        .update_verification_status(&leaf.id(), VerificationStatus::Failed)
        .await
        .unwrap();
    check(&*backend, &tree, &[]).await;
    backend.put(parent.clone()).await.unwrap();
    check(&*backend, &tree, &[parent.id()]).await;
    backend.put(root).await.unwrap();
    check(&*backend, &tree, &[parent.id()]).await;

    let foreign = Entry::root_builder()
        .set_subtree_data("test", b"foreign")
        .build()
        .unwrap();
    let foreign_tree = foreign.id();
    let cross = child(&foreign_tree, &[parent.id()], "cross-tree reference");
    backend.put(foreign).await.unwrap();
    backend.put(cross.clone()).await.unwrap();
    check(&*backend, &tree, &[parent.id()]).await;
    check(
        &*backend,
        &foreign_tree,
        &[foreign_tree.clone(), cross.id()],
    )
    .await;
    // A non-root ID cannot become an alias namespace for its own Entry.
    check(&*backend, &parent.id(), &[]).await;
    backend
        .update_verification_status(&cross.id(), VerificationStatus::Failed)
        .await
        .unwrap();
    check(&*backend, &tree, &[parent.id()]).await;
    check(
        &*backend,
        &foreign_tree,
        std::slice::from_ref(&foreign_tree),
    )
    .await;
}

#[tokio::test]
async fn non_failed_concurrent_insertion_and_failure() {
    let backend = test_backend().await;
    let root = Entry::root_builder().build().unwrap();
    let tree = root.id();
    backend.put_verified(root).await.unwrap();
    let left = child(&tree, std::slice::from_ref(&tree), "left");
    let right = child(&tree, std::slice::from_ref(&tree), "right");
    backend.put(left.clone()).await.unwrap();
    let left_id = left.id();
    let (put, fail) = tokio::join!(
        backend.put(right.clone()),
        backend.update_verification_status(&left_id, VerificationStatus::Failed)
    );
    put.unwrap();
    fail.unwrap();
    check(&*backend, &tree, &[right.id()]).await;
    // Duplicate puts race safely as well (PostgreSQL used to race its existence read).
    let (one, two) = tokio::join!(backend.put(right.clone()), backend.put(right.clone()));
    one.unwrap();
    two.unwrap();
    check(&*backend, &tree, &[right.id()]).await;
}

#[tokio::test]
async fn non_failed_seeded_dag_matches_reconstruction_after_every_mutation() {
    let backend = test_backend().await;
    let root = Entry::root_builder().build().unwrap();
    let tree = root.id();
    let mut entries = vec![root];
    for i in 1..24 {
        let parents = [entries[i - 1].id(), entries[i / 2].id()];
        entries.push(child(&tree, &parents, &format!("{i}")));
    }
    // Child-before-parent puts interleaved with failures, including references
    // to missing parents. Check independently after every mutation.
    for i in (0..entries.len()).rev() {
        backend.put(entries[i].clone()).await.unwrap();
        if i % 3 == 0 {
            backend
                .update_verification_status(&entries[i].id(), VerificationStatus::Failed)
                .await
                .unwrap();
        }
        let retained = backend.non_failed_snapshot(&tree).await.unwrap();
        check(&*backend, &tree, retained.tips()).await;
    }
    backend.repair_verification(&tree).await.unwrap();
    let retained = backend.non_failed_snapshot(&tree).await.unwrap();
    check(&*backend, &tree, retained.tips()).await;
}

#[tokio::test]
async fn non_failed_interior_rejection_preserves_descendant_bytes_and_selection() {
    let backend = test_backend().await;
    let root = Entry::root_builder().build().unwrap();
    let tree = root.id();
    let middle = child(&tree, std::slice::from_ref(&tree), "middle");
    let leaf = child(&tree, &[middle.id()], "leaf");
    for entry in [&root, &middle, &leaf] {
        backend.put_verified(entry.clone()).await.unwrap();
    }
    let historical = backend
        .get_tree_from_tips(&tree, &[leaf.id()])
        .await
        .unwrap();
    backend
        .update_verification_status(&middle.id(), VerificationStatus::Failed)
        .await
        .unwrap();
    assert_eq!(
        backend.get_verification_status(&leaf.id()).await.unwrap(),
        VerificationStatus::Unverified
    );
    check(&*backend, &tree, &[tree.clone(), leaf.id()]).await;
    assert_eq!(
        backend.verified_snapshot(&tree).await.unwrap(),
        Snapshot::from([tree.clone()])
    );
    assert_eq!(
        backend
            .get_tree_from_tips(&tree, &[leaf.id()])
            .await
            .unwrap(),
        historical,
        "selection must not filter or rewrite immutable Snapshot ancestry"
    );
    backend
        .update_verification_status(&leaf.id(), VerificationStatus::Failed)
        .await
        .unwrap();
    check(&*backend, &tree, std::slice::from_ref(&tree)).await;
}
