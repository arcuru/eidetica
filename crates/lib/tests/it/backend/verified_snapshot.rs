//! Retained verified prefix/frontier tests.
//!
//! The default read view (`verified_snapshot`) must equal a cold full
//! rebuild after every insertion and promotion order, on every backend.
//! `Database::snapshot()` must serve the settled state without rescanning
//! history; `GetVerifiedTips` inherits that path with no protocol change.

use std::{
    any::Any,
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use eidetica::{
    Instance, NewUser, Snapshot,
    backend::{
        BackendImpl, InstanceMetadata, InstanceSecrets, RecordMutations, RecordPage, RecordRange,
        RecordView, StagingToken, StoreStateRequest, VerificationStatus,
    },
    crdt::Doc,
    entry::{Entry, ID},
    store::DocStore,
};

use super::helpers::test_backend;
use crate::helpers::{TestVerify, create_user};

/// Promote each entry to `Verified`, in order.
async fn promote(backend: &dyn BackendImpl, ids: &[ID]) {
    for id in ids {
        backend
            .update_verification_status(id, VerificationStatus::Verified)
            .await
            .unwrap();
    }
}

/// Assert the retained frontier equals `expected` AND a cold full rebuild —
/// the incremental promotion path must match the oracle after every order.
async fn assert_verified(backend: &dyn BackendImpl, tree: &ID, expected: &[ID]) {
    let retained = backend.verified_snapshot(tree).await.unwrap();
    assert_eq!(
        retained.tips().iter().collect::<HashSet<_>>(),
        expected.iter().collect::<HashSet<_>>(),
        "retained verified frontier mismatch"
    );
    let rebuilt = backend.rebuild_verified_state(tree).await.unwrap();
    assert_eq!(
        retained, rebuilt,
        "incremental verified state must equal a cold full rebuild"
    );
}

/// Assert only the retained frontier (no rebuild): for intermediate steps
/// where the rebuild comparison happens at the end.
async fn assert_retained(backend: &dyn BackendImpl, tree: &ID, expected: &[ID]) {
    let retained = backend.verified_snapshot(tree).await.unwrap();
    assert_eq!(
        retained.tips().iter().collect::<HashSet<_>>(),
        expected.iter().collect::<HashSet<_>>(),
        "retained verified frontier mismatch"
    );
}

fn root_entry() -> Entry {
    Entry::root_builder().build().expect("root builds")
}

/// Build a child entry with distinguishing content and an explicit height.
///
/// `tag` must be unique per entry within a tree: entries are
/// content-addressed, so two children with identical parents and payloads
/// would be the *same* entry. `height` mirrors what the commit path assigns
/// (one above the highest parent).
fn child_entry(tree: &ID, parents: &[ID], tag: &str, height: u64) -> Entry {
    let mut builder = Entry::builder(tree.clone()).set_height(height);
    for p in parents {
        builder = builder.add_parent(p.clone());
    }
    builder
        .set_subtree_data("test", tag.as_bytes())
        .build()
        .expect("child builds")
}

#[tokio::test]
async fn verified_snapshot_empty_until_root_verified() {
    let backend = test_backend().await;
    let root = root_entry();
    let root_id = root.id();
    backend.put(root).await.unwrap();

    assert_retained(&*backend, &root_id, &[]).await;
    backend
        .update_verification_status(&root_id, VerificationStatus::Verified)
        .await
        .unwrap();
    assert_verified(&*backend, &root_id, std::slice::from_ref(&root_id)).await;
}

#[tokio::test]
async fn linear_chain_in_order_promotion() {
    let backend = test_backend().await;
    let root = root_entry();
    let root_id = root.id();
    backend.put(root).await.unwrap();
    let a = child_entry(&root_id, std::slice::from_ref(&root_id), "a", 1);
    let a_id = a.id();
    backend.put(a).await.unwrap();
    let b = child_entry(&root_id, std::slice::from_ref(&a_id), "b", 2);
    let b_id = b.id();
    backend.put(b).await.unwrap();

    promote(&*backend, std::slice::from_ref(&root_id)).await;
    assert_verified(&*backend, &root_id, std::slice::from_ref(&root_id)).await;
    promote(&*backend, std::slice::from_ref(&a_id)).await;
    assert_verified(&*backend, &root_id, std::slice::from_ref(&a_id)).await;
    promote(&*backend, std::slice::from_ref(&b_id)).await;
    assert_verified(&*backend, &root_id, std::slice::from_ref(&b_id)).await;
}

#[tokio::test]
async fn child_before_parent_storage() {
    let backend = test_backend().await;
    let root = root_entry();
    let root_id = root.id();
    backend.put(root).await.unwrap();
    let a = child_entry(&root_id, std::slice::from_ref(&root_id), "a", 1);
    let a_id = a.id();
    let b = child_entry(&root_id, std::slice::from_ref(&a_id), "b", 2);
    let b_id = b.id();
    // Store the child before its parent, as sync ingest routinely does.
    backend.put(b).await.unwrap();
    backend.put(a).await.unwrap();

    promote(&*backend, &[root_id.clone(), a_id.clone(), b_id.clone()]).await;
    assert_verified(&*backend, &root_id, std::slice::from_ref(&b_id)).await;
}

#[tokio::test]
async fn child_before_parent_promotion() {
    let backend = test_backend().await;
    let root = root_entry();
    let root_id = root.id();
    backend.put(root).await.unwrap();
    let a = child_entry(&root_id, std::slice::from_ref(&root_id), "a", 1);
    let a_id = a.id();
    backend.put(a).await.unwrap();
    let b = child_entry(&root_id, std::slice::from_ref(&a_id), "b", 2);
    let b_id = b.id();
    backend.put(b).await.unwrap();

    // A promotion that arrives before its parents simply waits: nothing is
    // admitted until the missing ancestors join the prefix.
    promote(&*backend, std::slice::from_ref(&b_id)).await;
    assert_retained(&*backend, &root_id, &[]).await;
    promote(&*backend, std::slice::from_ref(&a_id)).await;
    assert_retained(&*backend, &root_id, &[]).await;
    promote(&*backend, std::slice::from_ref(&root_id)).await;
    // Promoting the last missing parent recursively activates the waiting
    // descendants — no read-time scan needed.
    assert_verified(&*backend, &root_id, std::slice::from_ref(&b_id)).await;
}

#[tokio::test]
async fn diamond_converges_regardless_of_promotion_order() {
    for order in [0u8, 1u8] {
        let backend = test_backend().await;
        let root = root_entry();
        let root_id = root.id();
        backend.put(root).await.unwrap();
        let left = child_entry(&root_id, std::slice::from_ref(&root_id), "left", 1);
        let left_id = left.id();
        backend.put(left).await.unwrap();
        let right = child_entry(&root_id, std::slice::from_ref(&root_id), "right", 1);
        let right_id = right.id();
        backend.put(right).await.unwrap();
        let merge = child_entry(&root_id, &[left_id.clone(), right_id.clone()], "merge", 2);
        let merge_id = merge.id();
        backend.put(merge).await.unwrap();

        if order == 0 {
            promote(
                &*backend,
                &[
                    root_id.clone(),
                    left_id.clone(),
                    right_id.clone(),
                    merge_id.clone(),
                ],
            )
            .await;
        } else {
            // Merge first: both its parents are missing from the prefix, so
            // it waits until the last one is promoted.
            promote(
                &*backend,
                &[
                    merge_id.clone(),
                    right_id.clone(),
                    left_id.clone(),
                    root_id.clone(),
                ],
            )
            .await;
        }
        assert_verified(&*backend, &root_id, std::slice::from_ref(&merge_id)).await;
    }
}

#[tokio::test]
async fn multi_tip_fork_keeps_both_frontier_tips() {
    let backend = test_backend().await;
    let root = root_entry();
    let root_id = root.id();
    backend.put(root).await.unwrap();
    let a = child_entry(&root_id, std::slice::from_ref(&root_id), "a", 1);
    let a_id = a.id();
    backend.put(a).await.unwrap();
    let b = child_entry(&root_id, std::slice::from_ref(&root_id), "b", 1);
    let b_id = b.id();
    backend.put(b).await.unwrap();

    promote(&*backend, &[root_id.clone(), a_id.clone(), b_id.clone()]).await;
    assert_verified(&*backend, &root_id, &[a_id.clone(), b_id.clone()]).await;
}

#[tokio::test]
async fn failed_ancestor_blocks_verified_descendant() {
    let backend = test_backend().await;
    let root = root_entry();
    let root_id = root.id();
    backend.put(root).await.unwrap();
    let bad = child_entry(&root_id, std::slice::from_ref(&root_id), "bad", 1);
    let bad_id = bad.id();
    backend.put(bad).await.unwrap();
    let grandchild = child_entry(&root_id, std::slice::from_ref(&bad_id), "grandchild", 2);
    let grandchild_id = grandchild.id();
    backend.put(grandchild).await.unwrap();

    promote(&*backend, std::slice::from_ref(&root_id)).await;
    backend
        .update_verification_status(&bad_id, VerificationStatus::Failed)
        .await
        .unwrap();
    // Even promoted, the grandchild can never enter the prefix: its parent
    // is Failed and Failed never enters the prefix.
    promote(&*backend, std::slice::from_ref(&grandchild_id)).await;
    assert_verified(&*backend, &root_id, std::slice::from_ref(&root_id)).await;
}

#[tokio::test]
async fn failed_tip_drops_out_of_frontier() {
    let backend = test_backend().await;
    let root = root_entry();
    let root_id = root.id();
    backend.put_verified(root).await.unwrap();
    let a = child_entry(&root_id, std::slice::from_ref(&root_id), "a", 1);
    let a_id = a.id();
    backend.put_verified(a).await.unwrap();
    assert_verified(&*backend, &root_id, std::slice::from_ref(&a_id)).await;

    let b = child_entry(&root_id, std::slice::from_ref(&a_id), "b", 2);
    let b_id = b.id();
    backend.put(b).await.unwrap();
    backend
        .update_verification_status(&b_id, VerificationStatus::Failed)
        .await
        .unwrap();
    assert_verified(&*backend, &root_id, std::slice::from_ref(&a_id)).await;
}

#[tokio::test]
async fn idempotent_promotion_is_harmless() {
    let backend = test_backend().await;
    let root = root_entry();
    let root_id = root.id();
    backend.put(root).await.unwrap();
    promote(&*backend, std::slice::from_ref(&root_id)).await;
    promote(&*backend, std::slice::from_ref(&root_id)).await;
    assert_verified(&*backend, &root_id, std::slice::from_ref(&root_id)).await;
}

#[tokio::test]
async fn unverified_insertion_advances_raw_only() {
    let backend = test_backend().await;
    let root = root_entry();
    let root_id = root.id();
    backend.put_verified(root).await.unwrap();
    let a = child_entry(&root_id, std::slice::from_ref(&root_id), "a", 1);
    let a_id = a.id();
    backend.put_verified(a).await.unwrap();
    assert_verified(&*backend, &root_id, std::slice::from_ref(&a_id)).await;

    // A new Unverified insertion moves the raw frontier but must not move
    // (or invalidate) the retained verified frontier.
    let b = child_entry(&root_id, std::slice::from_ref(&a_id), "b", 2);
    let b_id = b.id();
    backend.put(b).await.unwrap();
    assert_retained(&*backend, &root_id, std::slice::from_ref(&a_id)).await;
    let raw = backend.snapshot(&root_id).await.unwrap();
    assert_eq!(raw.tips(), &[b_id]);
}

#[tokio::test]
async fn demotion_rebuilds_and_repromotion_recovers() {
    let backend = test_backend().await;
    let root = root_entry();
    let root_id = root.id();
    backend.put_verified(root).await.unwrap();
    let a = child_entry(&root_id, std::slice::from_ref(&root_id), "a", 1);
    let a_id = a.id();
    backend.put_verified(a).await.unwrap();
    let b = child_entry(&root_id, std::slice::from_ref(&a_id), "b", 2);
    let b_id = b.id();
    backend.put_verified(b).await.unwrap();
    assert_verified(&*backend, &root_id, std::slice::from_ref(&b_id)).await;

    // Demoting out of Verified funnels into the expensive per-tree rebuild.
    backend
        .update_verification_status(&a_id, VerificationStatus::Unverified)
        .await
        .unwrap();
    assert_verified(&*backend, &root_id, std::slice::from_ref(&root_id)).await;

    // Re-promoting the missing parent recursively re-admits the still-
    // Verified descendant.
    promote(&*backend, std::slice::from_ref(&a_id)).await;
    assert_verified(&*backend, &root_id, std::slice::from_ref(&b_id)).await;
}

/// Deterministic PRNG (LCG) — no extra dev-dependency for property coverage.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % bound
    }
}

#[tokio::test]
async fn shuffled_orders_equal_cold_rebuild() {
    let backend = test_backend().await;
    let mut rng = Lcg(0x5EED_C0DE_1234_5678);

    // Random DAG: entry i takes 1-2 distinct parents from earlier entries.
    let root = root_entry();
    let root_id = root.id();
    let mut ids = vec![root_id.clone()];
    let mut entries = vec![root];
    let mut heights = vec![0u64];
    for i in 1..40 {
        // Never ask for more distinct parents than exist yet.
        let parent_count = 1 + rng.below(ids.len().min(2));
        let mut parents = Vec::new();
        let mut parent_heights = Vec::new();
        while parents.len() < parent_count {
            let k = rng.below(ids.len());
            let p = ids[k].clone();
            if !parents.contains(&p) {
                parents.push(p);
                parent_heights.push(heights[k]);
            }
        }
        let height = parent_heights.into_iter().max().unwrap_or(0) + 1;
        let entry = child_entry(&root_id, &parents, &format!("entry-{i}"), height);
        ids.push(entry.id());
        heights.push(height);
        entries.push(entry);
    }

    // Random storage order (child-before-parent included).
    let mut stored: Vec<usize> = (0..entries.len()).collect();
    for i in (1..stored.len()).rev() {
        let j = rng.below(i + 1);
        stored.swap(i, j);
    }
    for &i in &stored {
        backend.put(entries[i].clone()).await.unwrap();
    }

    // Random promotion order; a fixed pseudo-random subset Fails instead.
    let mut order: Vec<usize> = (0..ids.len()).collect();
    for i in (1..order.len()).rev() {
        let j = rng.below(i + 1);
        order.swap(i, j);
    }
    let failed: HashSet<usize> = order.iter().step_by(7).copied().collect();
    for &i in &order {
        let status = if failed.contains(&i) {
            VerificationStatus::Failed
        } else {
            VerificationStatus::Verified
        };
        backend
            .update_verification_status(&ids[i], status)
            .await
            .unwrap();
        let retained = backend.verified_snapshot(&root_id).await.unwrap();
        let rebuilt = backend.rebuild_verified_state(&root_id).await.unwrap();
        assert_eq!(
            retained, rebuilt,
            "incremental diverged from cold rebuild after promoting entry {i}"
        );
    }
}

#[tokio::test]
async fn snapshot_cache_keys_are_canonical() {
    let backend = test_backend().await;
    let root = root_entry();
    let root_id = root.id();
    backend.put_verified(root).await.unwrap();
    let a = child_entry(&root_id, std::slice::from_ref(&root_id), "a", 1);
    let a_id = a.id();
    backend.put_verified(a).await.unwrap();
    let b = child_entry(&root_id, std::slice::from_ref(&root_id), "b", 1);
    let b_id = b.id();
    backend.put_verified(b).await.unwrap();

    // Construction order must not matter.
    assert_eq!(
        Snapshot::from([a_id.clone(), b_id.clone()]).cache_key_bytes(),
        Snapshot::from([b_id.clone(), a_id.clone()]).cache_key_bytes(),
    );
    // Duplicates collapse to the same key.
    assert_eq!(
        Snapshot::from([a_id.clone(), b_id.clone()]).cache_key_bytes(),
        Snapshot::from([a_id.clone(), a_id.clone(), b_id.clone()]).cache_key_bytes(),
    );
    // One-element snapshots live in the same key space (no separate
    // entry-ID namespace), and distinct states key distinctly.
    assert_ne!(
        Snapshot::from([a_id.clone()]).cache_key_bytes(),
        Snapshot::from([b_id.clone()]).cache_key_bytes(),
    );
    assert_ne!(
        Snapshot::from([a_id.clone()]).cache_key_bytes(),
        Snapshot::from([a_id.clone(), b_id.clone()]).cache_key_bytes(),
    );
    // No synthetic merge IDs anywhere in the encoding.
    for snapshot in [
        Snapshot::from([a_id.clone()]),
        Snapshot::from([a_id.clone(), b_id.clone()]),
    ] {
        assert!(
            !snapshot
                .cache_key_bytes()
                .windows(6)
                .any(|w| w == b"merge:"),
            "cache key must not contain a synthetic merge ID"
        );
    }
}

/// Shared hot-path counters. Held behind an `Arc` so the test keeps
/// observing them after the `Instance` takes ownership of the engine.
#[derive(Default)]
struct HotPathCounters {
    get_tree_calls: AtomicUsize,
    status_calls: AtomicUsize,
}

impl HotPathCounters {
    fn counts(&self) -> (usize, usize) {
        (
            self.get_tree_calls.load(Ordering::Relaxed),
            self.status_calls.load(Ordering::Relaxed),
        )
    }

    fn reset(&self) {
        self.get_tree_calls.store(0, Ordering::Relaxed);
        self.status_calls.store(0, Ordering::Relaxed);
    }
}

/// Backend wrapper counting full-tree loads and per-entry status reads —
/// the two operations the settled hot path must never perform.
struct Counting {
    inner: Box<dyn BackendImpl>,
    counters: Arc<HotPathCounters>,
}

#[async_trait::async_trait]
impl BackendImpl for Counting {
    async fn get(&self, id: &ID) -> eidetica::Result<Entry> {
        self.inner.get(id).await
    }

    async fn get_verification_status(&self, id: &ID) -> eidetica::Result<VerificationStatus> {
        self.counters.status_calls.fetch_add(1, Ordering::Relaxed);
        self.inner.get_verification_status(id).await
    }

    async fn put(&self, entry: Entry) -> eidetica::Result<()> {
        self.inner.put(entry).await
    }

    async fn update_verification_status(
        &self,
        id: &ID,
        verification_status: VerificationStatus,
    ) -> eidetica::Result<()> {
        self.inner
            .update_verification_status(id, verification_status)
            .await
    }

    async fn get_entries_by_verification_status(
        &self,
        status: VerificationStatus,
    ) -> eidetica::Result<Vec<ID>> {
        self.inner.get_entries_by_verification_status(status).await
    }

    async fn snapshot(&self, tree: &ID) -> eidetica::Result<Snapshot> {
        self.inner.snapshot(tree).await
    }

    async fn verified_snapshot(&self, tree: &ID) -> eidetica::Result<Snapshot> {
        self.inner.verified_snapshot(tree).await
    }

    async fn rebuild_verified_state(&self, tree: &ID) -> eidetica::Result<Snapshot> {
        self.inner.rebuild_verified_state(tree).await
    }

    async fn store_snapshot(&self, tree: &ID, store: &str) -> eidetica::Result<Snapshot> {
        self.inner.store_snapshot(tree, store).await
    }

    async fn store_snapshot_at(
        &self,
        tree: &ID,
        store: &str,
        main_snapshot: &Snapshot,
    ) -> eidetica::Result<Snapshot> {
        self.inner
            .store_snapshot_at(tree, store, main_snapshot)
            .await
    }

    async fn all_roots(&self) -> eidetica::Result<Vec<ID>> {
        self.inner.all_roots().await
    }

    async fn find_merge_base(
        &self,
        tree: &ID,
        store: &str,
        entry_ids: &[ID],
    ) -> eidetica::Result<Option<ID>> {
        self.inner.find_merge_base(tree, store, entry_ids).await
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    async fn get_tree(&self, tree: &ID) -> eidetica::Result<Vec<Entry>> {
        self.counters.get_tree_calls.fetch_add(1, Ordering::Relaxed);
        self.inner.get_tree(tree).await
    }

    async fn get_store(&self, tree: &ID, store: &str) -> eidetica::Result<Vec<Entry>> {
        self.inner.get_store(tree, store).await
    }

    async fn get_tree_from_tips(&self, tree: &ID, tips: &[ID]) -> eidetica::Result<Vec<Entry>> {
        self.inner.get_tree_from_tips(tree, tips).await
    }

    async fn store_at(
        &self,
        tree: &ID,
        store: &str,
        snapshot: &Snapshot,
    ) -> eidetica::Result<Vec<Entry>> {
        self.inner.store_at(tree, store, snapshot).await
    }

    async fn get_sorted_store_parents(
        &self,
        tree_id: &ID,
        entry_id: &ID,
        store: &str,
    ) -> eidetica::Result<Vec<ID>> {
        self.inner
            .get_sorted_store_parents(tree_id, entry_id, store)
            .await
    }

    async fn get_path_from_to(
        &self,
        tree_id: &ID,
        store: &str,
        from_id: Option<&ID>,
        to_ids: &[ID],
    ) -> eidetica::Result<Vec<ID>> {
        self.inner
            .get_path_from_to(tree_id, store, from_id, to_ids)
            .await
    }

    async fn get_instance_metadata(&self) -> eidetica::Result<Option<InstanceMetadata>> {
        self.inner.get_instance_metadata().await
    }

    async fn set_instance_metadata(&self, metadata: &InstanceMetadata) -> eidetica::Result<()> {
        self.inner.set_instance_metadata(metadata).await
    }

    async fn get_instance_secrets(&self) -> eidetica::Result<Option<InstanceSecrets>> {
        self.inner.get_instance_secrets().await
    }

    async fn set_instance_secrets(&self, secrets: &InstanceSecrets) -> eidetica::Result<()> {
        self.inner.set_instance_secrets(secrets).await
    }

    async fn resolve_store_state(
        &self,
        request: &StoreStateRequest,
    ) -> eidetica::Result<Option<RecordView>> {
        self.inner.resolve_store_state(request).await
    }

    async fn begin_store_state_staging(
        &self,
        request: StoreStateRequest,
    ) -> eidetica::Result<StagingToken> {
        self.inner.begin_store_state_staging(request).await
    }

    async fn stage_store_state_records(
        &self,
        token: &StagingToken,
        records: RecordMutations,
    ) -> eidetica::Result<()> {
        self.inner.stage_store_state_records(token, records).await
    }

    async fn publish_store_state(&self, token: StagingToken) -> eidetica::Result<RecordView> {
        self.inner.publish_store_state(token).await
    }

    async fn abort_store_state(&self, token: StagingToken) -> eidetica::Result<()> {
        self.inner.abort_store_state(token).await
    }

    async fn store_state_record_get(
        &self,
        view: &RecordView,
        key: &[u8],
    ) -> eidetica::Result<Option<Vec<u8>>> {
        self.inner.store_state_record_get(view, key).await
    }

    async fn store_state_record_scan(
        &self,
        view: &RecordView,
        range: &RecordRange,
        after: Option<&[u8]>,
        limit: usize,
    ) -> eidetica::Result<RecordPage> {
        self.inner
            .store_state_record_scan(view, range, after, limit)
            .await
    }

    async fn clear_derived_store_state(&self) -> eidetica::Result<()> {
        self.inner.clear_derived_store_state().await
    }
}

/// The settled hot path through the real `Database::snapshot()` entry
/// point: after creating a user, database, and committed transaction, a
/// final `verify()` settles every entry, and repeated `snapshot()` calls
/// must then perform zero full-tree loads and zero per-entry status reads.
#[tokio::test]
async fn settled_snapshot_reads_are_scan_free() {
    let counters = Arc::new(HotPathCounters::default());
    let (instance, _admin) = Instance::create_backend(
        Box::new(Counting {
            inner: test_backend().await,
            counters: counters.clone(),
        }),
        NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    create_user(&instance, "reader", None).await.unwrap();
    let mut user = instance.login_user("reader", None).await.unwrap();
    let key = user.get_default_key().expect("default key");

    let mut settings = Doc::new();
    settings.set("name", "scan_free");
    let database = user.create_database(settings, &key).await.unwrap();

    let tx = database.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("data").await.unwrap();
    store.set("k", "v").await.unwrap();
    tx.commit().await.unwrap();
    database.verify().await.unwrap();

    // Settle: one read through whatever path applies, then observe.
    let first = database.snapshot().await.unwrap();
    assert!(!first.is_empty(), "settled database must expose tips");
    counters.reset();

    let second = database.snapshot().await.unwrap();
    let third = database.snapshot().await.unwrap();
    assert_eq!(first, second);
    assert_eq!(second, third);
    assert_eq!(
        counters.counts(),
        (0, 0),
        "settled snapshots must perform no full-tree load and no per-entry status read"
    );
}
