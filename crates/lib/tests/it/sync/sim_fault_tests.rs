//! Link faults as a property: a lossy, flapping network still converges via the
//! auto-sync retry queue — no reconciling `exchange` required.
//!
//! [`super::sim_schedule_tests`] fuzzes *delivery order* over captured pushes
//! (`set_manual_delivery`), and deliberately does **not** model a real drop: with
//! manual delivery the fabric optimistic-Acks, so a dropped message is gone with
//! no resend. This module models the drop *faithfully* instead — through the same
//! path production uses. A [`SimNetwork`] partition makes a send fail like a
//! connection error, so eidetica's auto-sync engine parks the entry in its retry
//! queue ([`flush`] → `flush_retry_queue`); when the link heals, the next flush
//! redelivers it. Nothing is lost, because the sender never got a false Ack.
//!
//! The fuzzer drives a full mesh of `auto_sync` peers and, from a seed, randomly
//! interleaves writes with link partition/heal toggles. A write whose owner has a
//! cut link leaves entries stranded in the retry queue. After the chaos it heals
//! every link and flushes every peer — retry-queue redelivery alone, no
//! `exchange` — and asserts the cluster lands on one complete, signed state with
//! every write readable everywhere.
//!
//! Why retry-only drain converges despite cross-peer orphans: a push carries just
//! the committed entries, not their ancestors, so a peer can receive a child
//! before its parent. The receiver stores it regardless (as `Failed`), and tip
//! reconciliation completes once the parent arrives from its own peer's flush.
//! Order-independence (proven in `sim_schedule_tests`) is what lets retry-driven,
//! arbitrarily-late delivery still converge.
//!
//! Partitions are kept short relative to flush frequency so no entry exhausts the
//! engine's 10-attempt retry budget mid-chaos; the final heal-and-drain always
//! sends on attempt < 10. The PRNG is seeded, so a failing seed replays exactly.

use std::sync::Arc;

use eidetica::{
    Cluster, Database,
    entry::ID,
    testing::{SimLoopback, SimNetwork},
};

use super::helpers::{Rng, cluster_get, cluster_put, cluster_shared_database};

/// Wire every peer to every other for `room` (full mesh, so each write fans out
/// directly to all peers — no relaying), then flush once with every link up to
/// drain the setup pushes inline. Leaves the fabric in plain inline-delivery mode
/// (no manual capture): real delivery, steered only by partitions.
async fn full_mesh_auto(net: &mut Cluster, room: &ID, n: usize) {
    for i in 0..n {
        for j in (i + 1)..n {
            net.auto_sync(i, j, room).await.unwrap();
        }
    }
    for p in 0..n {
        net.flush(p).await.unwrap();
    }
}

/// Symmetric link-state matrix over `n` peers, all links initially up. Tracks
/// which pairs the schedule has partitioned so a toggle knows whether to cut or
/// heal, and a write can tell whether its owner is currently stranded.
struct Links {
    up: Vec<Vec<bool>>,
}

impl Links {
    fn new(n: usize) -> Self {
        Self {
            up: vec![vec![true; n]; n],
        }
    }

    fn is_up(&self, a: usize, b: usize) -> bool {
        self.up[a][b]
    }

    fn set(&mut self, a: usize, b: usize, up: bool) {
        self.up[a][b] = up;
        self.up[b][a] = up;
    }

    /// True if `peer` has at least one cut link — a write it makes now will
    /// strand entries in the retry queue.
    fn peer_isolated_from_any(&self, peer: usize, n: usize) -> bool {
        (0..n).any(|other| other != peer && !self.up[peer][other])
    }
}

/// Run one seeded fault schedule and return `(written_ids, writes_under_fault)`:
/// the entry ids the writes committed (for an absolute presence check) and how
/// many writes happened while their owner had a cut link (so the test can prove
/// it actually exercised the retry path, not just a quiet network).
async fn run_fault_schedule(
    net: &mut Cluster,
    fabric: &SimNetwork,
    addrs: &[eidetica::sync::peer_types::Address],
    room: &ID,
    dbs: &[Database],
    writes: &[(usize, &str, &str)],
    seed: u64,
) -> (Vec<ID>, usize) {
    let n = dbs.len();
    let mut rng = Rng::new(seed);
    let mut links = Links::new(n);
    let mut written: Vec<ID> = Vec::new();
    let mut next = 0;
    let mut writes_under_fault = 0;

    // Interleave writes with link toggles. ~1 in 3 steps toggles a random link;
    // the rest make progress on the write list so the loop always terminates.
    while next < writes.len() {
        if rng.below(3) == 0 {
            // Toggle a random distinct pair (i < j).
            let i = rng.below(n);
            let mut j = rng.below(n);
            if i == j {
                j = (j + 1) % n;
            }
            let (a, b) = (i.min(j), i.max(j));
            if links.is_up(a, b) {
                fabric.partition(&addrs[a], &addrs[b]);
                links.set(a, b, false);
            } else {
                fabric.heal(&addrs[a], &addrs[b]);
                links.set(a, b, true);
            }
        } else {
            let (peer, key, value) = writes[next];
            if links.peer_isolated_from_any(peer, n) {
                writes_under_fault += 1;
            }
            cluster_put(&dbs[peer], key, value).await.unwrap();
            // Best-effort: a push across a cut link fails and parks in the retry
            // queue (that is the behaviour under test), so the error is expected.
            let _ = net.flush(peer).await;
            written.extend(net.tips(peer, room).await.unwrap());
            next += 1;
        }
    }

    // Heal everything and let the retry queues redeliver. No `exchange`: this is
    // the assertion that auto-sync's own retry path repairs a flapping network.
    // `n + 1` rounds is slack — one full round drains a full mesh, since every
    // entry was originally addressed directly to every peer.
    fabric.heal_all();
    for _ in 0..(n + 1) {
        for p in 0..n {
            let _ = net.flush(p).await;
        }
    }

    (written, writes_under_fault)
}

/// Across a spread of seeds: three full-mesh peers write while links partition
/// and heal at random, and after a heal-and-drain the cluster must converge onto
/// the same complete, signed state with every key readable everywhere — driven
/// purely by retry-queue redelivery. Faults are tallied across seeds and asserted
/// non-zero so the test can't pass on a network that never actually dropped.
#[tokio::test]
async fn test_randomized_link_faults_recover_via_retry() {
    let writes = [
        (0, "a", "0a"),
        (1, "b", "1b"),
        (2, "c", "2c"),
        (0, "d", "0d"),
        (1, "e", "1e"),
        (2, "f", "2f"),
    ];
    let n = 3;
    let mut total_under_fault = 0;

    for seed in 0..16u64 {
        let fabric = SimNetwork::new();
        let mut net = Cluster::builder()
            .peers(n)
            .transport(Arc::new(SimLoopback::new(fabric.clone())))
            .build()
            .await
            .unwrap();
        let (room, dbs) = cluster_shared_database(&mut net, "faults").await.unwrap();
        let addrs: Vec<_> = (0..n).map(|i| net.peer(i).address().clone()).collect();
        full_mesh_auto(&mut net, &room, n).await;

        let (written, under_fault) =
            run_fault_schedule(&mut net, &fabric, &addrs, &room, &dbs, &writes, seed).await;
        total_under_fault += under_fault;

        let all: Vec<usize> = (0..n).collect();
        assert!(
            net.converged_all(&room).await.unwrap(),
            "seed {seed}: cluster must converge after links heal and queues drain"
        );
        net.assert_no_lost_entries(&all, &room).await.unwrap();
        net.assert_all_present(&all, &room, &written).await.unwrap();
        for (peer, db) in dbs.iter().enumerate() {
            net.assert_all_signed(peer, &room).await.unwrap();
            for (_, key, value) in &writes {
                assert_eq!(
                    cluster_get(db, key).await.unwrap(),
                    *value,
                    "seed {seed}: peer {peer} should read {key}={value} after heal"
                );
            }
        }
    }

    // Guard against a vacuous pass: some write must have happened while its owner
    // was partitioned, or the retry path was never exercised.
    assert!(
        total_under_fault > 0,
        "fuzzer never wrote under a partition across any seed"
    );
}
