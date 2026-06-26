//! Order-independence as a property, fuzzed from fixed seeds.
//!
//! The reorder test ([`super::sim_delivery_control_tests`]) proves one hand-picked
//! delivery order converges. This generalises it: a tiny seeded PRNG drives a
//! random interleaving of writes and message deliveries over the controllable
//! [`SimNetwork`] queue, and every seed must land the cluster on an identical,
//! complete, signed state. A real socket transport can't be steered like this —
//! it delivers in wired order — so this property is only testable because the
//! fabric lets the test own the schedule.
//!
//! Two properties live here:
//! - **Reordering alone always converges** — a lossless but arbitrarily-ordered
//!   wire still lands every peer on the same complete state.
//! - **Duplication is idempotent under fuzzing** — reorder *and* redeliver
//!   messages at random, and convergence still holds with no repair. This is the
//!   chaz gateway-redelivery bug class, fuzzed.
//!
//! Not modelled here: a *permanent* in-band drop. This fabric reports an
//! optimistic `Ack` the moment it captures a push, so a dropped message is lost
//! with no resend — but real eidetica auto-sync retries failed sends from its
//! retry queue, so a genuine network drop is redelivered, never permanently
//! lost. Modelling drop as permanent loss would let a peer hold a child without
//! its parent (an orphan tip that tip-based anti-entropy won't backfill) — a
//! state the retry path prevents, not an engine bug. Transient loss-then-resend
//! is already covered by reordering (a delayed message is just a late delivery)
//! and, at the link level, by the partition/heal test.
//!
//! Determinism matters: the PRNG is seeded, so a failing seed reproduces its
//! exact schedule for debugging. Nothing here reads the clock or a real RNG.

use std::sync::Arc;

use eidetica::{
    Cluster, Database,
    entry::ID,
    testing::{SimLoopback, SimNetwork},
};

use super::helpers::{Prng, cluster_get, cluster_put, cluster_shared_database};

/// Wire every peer to every other for `tree` (so each write fans out to all),
/// drain the setup pushes inline, then switch to manual delivery with an empty
/// queue — the clean slate the schedule runs against.
async fn full_mesh_manual(net: &mut Cluster, fabric: &SimNetwork, room: &ID) {
    net.auto_sync_all(room).await.unwrap();
    // Any push auto-sync setup produced delivers inline here; then capture.
    net.flush_all().await.unwrap();
    fabric.set_manual_delivery(true);
    fabric.drop_all();
}

/// Interleave the `writes` with deliveries of pending messages, choosing at each
/// step — by the `seed`-derived schedule — whether to perform the next write
/// (flushing its push onto the wire) or deliver one random in-flight message.
/// Runs until every write is done and the queue is drained. Returns the ids of
/// the entries the writes committed, for an absolute presence check.
async fn run_schedule(
    net: &mut Cluster,
    fabric: &SimNetwork,
    room: &ID,
    dbs: &[Database],
    writes: &[(usize, &str, &str)],
    seed: u64,
) -> Vec<ID> {
    let mut prng = Prng::new(seed);
    let mut written: Vec<ID> = Vec::new();
    let mut next = 0;

    loop {
        let pending = fabric.pending();
        let more_writes = next < writes.len();
        if !more_writes && pending.is_empty() {
            break;
        }

        // Write when there's one to do and either nothing is in flight or the
        // coin says so — this keeps both writes and deliveries interleaving
        // rather than draining one phase fully before the other.
        if more_writes && (pending.is_empty() || prng.coin()) {
            let (peer, key, value) = writes[next];
            cluster_put(&dbs[peer], key, value).await.unwrap();
            net.flush(peer).await.unwrap();
            // Capture the writer's current tips. Usually that's just the entry it
            // committed, but if deliveries have already landed others' writes here
            // the tip is a merge — still a real entry that must survive everywhere,
            // so it belongs in the presence set either way.
            written.extend(net.tips(peer, room).await.unwrap());
            next += 1;
        } else {
            let seq = pending[prng.below(pending.len())];
            fabric.deliver(seq).await;
        }
    }

    written
}

/// Like [`run_schedule`], but a random pending message may be *duplicated*
/// (queued to be delivered a second time) before delivery. Duplicates are
/// budget-capped so the queue still drains and the loop terminates. Every
/// message is still eventually delivered — possibly more than once — so the
/// cluster converges with no repair. Returns the committed entry ids and how
/// many duplicates fired (so the test can confirm it exercised the path).
async fn run_duplicating_schedule(
    net: &mut Cluster,
    fabric: &SimNetwork,
    room: &ID,
    dbs: &[Database],
    writes: &[(usize, &str, &str)],
    seed: u64,
) -> (Vec<ID>, usize) {
    let mut prng = Prng::new(seed);
    let mut written: Vec<ID> = Vec::new();
    let mut next = 0;
    let mut duplicated = 0;
    // Bound duplication so the queue is guaranteed to drain.
    let mut dup_budget = writes.len();

    loop {
        let pending = fabric.pending();
        let more_writes = next < writes.len();
        if !more_writes && pending.is_empty() {
            break;
        }

        if more_writes && (pending.is_empty() || prng.coin()) {
            let (peer, key, value) = writes[next];
            cluster_put(&dbs[peer], key, value).await.unwrap();
            net.flush(peer).await.unwrap();
            written.extend(net.tips(peer, room).await.unwrap());
            next += 1;
            continue;
        }

        let seq = pending[prng.below(pending.len())];
        // 1-in-4 duplicate (budget permitting), else deliver. Duplicate queues a
        // copy without removing the original, so both get delivered later —
        // every arm still drives toward an empty queue.
        if dup_budget > 0 && prng.below(4) == 0 {
            fabric.duplicate(seq).expect("seq came from pending()");
            dup_budget -= 1;
            duplicated += 1;
        } else {
            fabric.deliver(seq).await;
        }
    }

    (written, duplicated)
}

/// Across a spread of seeds: three peers each write two distinct keys in an
/// interleaved order, the network delivers under a random per-seed schedule, and
/// every run must converge onto the same complete, signed state — every key
/// readable on every peer. This is the order-independence guarantee, fuzzed.
#[tokio::test]
async fn test_randomized_delivery_schedules_all_converge() {
    let writes = [
        (0, "a", "0a"),
        (1, "b", "1b"),
        (2, "c", "2c"),
        (0, "d", "0d"),
        (1, "e", "1e"),
        (2, "f", "2f"),
    ];

    for seed in 0..16u64 {
        let fabric = SimNetwork::new();
        let mut net = Cluster::builder()
            .peers(3)
            .transport(Arc::new(SimLoopback::new(fabric.clone())))
            .build()
            .await
            .unwrap();
        let (room, dbs) = cluster_shared_database(&mut net, "fuzz").await.unwrap();
        full_mesh_manual(&mut net, &fabric, &room).await;

        let written = run_schedule(&mut net, &fabric, &room, &dbs, &writes, seed).await;

        let all: Vec<usize> = (0..net.len()).collect();
        assert!(
            net.converged_all(&room).await.unwrap(),
            "seed {seed}: cluster must converge after the schedule drains"
        );
        net.assert_no_lost_entries(&all, &room).await.unwrap();
        net.assert_all_present(&all, &room, &written).await.unwrap();

        for (peer, db) in dbs.iter().enumerate() {
            net.assert_all_signed(peer, &room).await.unwrap();
            // Every write is readable everywhere, regardless of arrival order.
            for (_, key, value) in &writes {
                assert_eq!(
                    cluster_get(db, key).await.unwrap(),
                    *value,
                    "seed {seed}: peer {peer} should read {key}={value}"
                );
            }
        }
    }
}

/// The same fan-out, but the random schedule also *duplicates* messages in
/// flight — some entries arrive twice, in a seed-determined order. Sync must be
/// idempotent: the redeliveries are no-ops and the cluster still converges onto
/// the same complete, signed state with no repair. The duplicate count is
/// tallied across seeds and asserted non-zero, so the test can't pass by never
/// injecting one.
#[tokio::test]
async fn test_randomized_duplicate_delivery_is_idempotent() {
    let writes = [
        (0, "a", "0a"),
        (1, "b", "1b"),
        (2, "c", "2c"),
        (0, "d", "0d"),
        (1, "e", "1e"),
        (2, "f", "2f"),
    ];
    let mut total_duplicated = 0;

    for seed in 0..16u64 {
        let fabric = SimNetwork::new();
        let mut net = Cluster::builder()
            .peers(3)
            .transport(Arc::new(SimLoopback::new(fabric.clone())))
            .build()
            .await
            .unwrap();
        let (room, dbs) = cluster_shared_database(&mut net, "dup").await.unwrap();
        full_mesh_manual(&mut net, &fabric, &room).await;

        let (written, duplicated) =
            run_duplicating_schedule(&mut net, &fabric, &room, &dbs, &writes, seed).await;
        total_duplicated += duplicated;

        let all: Vec<usize> = (0..net.len()).collect();
        assert!(
            net.converged_all(&room).await.unwrap(),
            "seed {seed}: cluster must converge despite duplicate delivery"
        );
        net.assert_no_lost_entries(&all, &room).await.unwrap();
        net.assert_all_present(&all, &room, &written).await.unwrap();
        for (peer, db) in dbs.iter().enumerate() {
            net.assert_all_signed(peer, &room).await.unwrap();
            for (_, key, value) in &writes {
                assert_eq!(
                    cluster_get(db, key).await.unwrap(),
                    *value,
                    "seed {seed}: peer {peer} should read {key}={value}"
                );
            }
        }
    }

    // Guard against a vacuous pass: the schedule must have actually duplicated.
    assert!(
        total_duplicated > 0,
        "fuzzer never duplicated a message across any seed"
    );
}
