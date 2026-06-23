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
//! Determinism matters: the PRNG is seeded, so a failing seed reproduces its
//! exact schedule for debugging. Nothing here reads the clock or a real RNG.

use std::sync::Arc;

use eidetica::{
    Cluster, Database,
    entry::ID,
    testing::{SimLoopback, SimNetwork},
};

use super::helpers::{cluster_get, cluster_put, cluster_shared_database};

/// Deterministic xorshift64* — a self-contained, dependency-free PRNG so a seed
/// reproduces a schedule exactly. Not cryptographic; just a stable bit source.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Spread the seed and force a non-zero state (xorshift fixes on zero).
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `0..n` (caller guarantees `n > 0`).
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    fn coin(&mut self) -> bool {
        self.next_u64() & 1 == 0
    }
}

/// Wire every peer to every other for `tree` (so each write fans out to all),
/// drain the setup pushes inline, then switch to manual delivery with an empty
/// queue — the clean slate the schedule runs against.
async fn full_mesh_manual(net: &mut Cluster, fabric: &SimNetwork, room: &ID, n: usize) {
    for i in 0..n {
        for j in (i + 1)..n {
            net.auto_sync(i, j, room).await.unwrap();
        }
    }
    // Any push auto-sync setup produced delivers inline here; then capture.
    for p in 0..n {
        net.flush(p).await.unwrap();
    }
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
    let mut rng = Rng::new(seed);
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
        if more_writes && (pending.is_empty() || rng.coin()) {
            let (peer, key, value) = writes[next];
            cluster_put(&dbs[peer], key, value).await.unwrap();
            net.flush(peer).await.unwrap();
            // A single commit leaves exactly one new tip on the writer.
            written.extend(net.tips(peer, room).await.unwrap());
            next += 1;
        } else {
            let seq = pending[rng.below(pending.len())];
            fabric.deliver(seq).await;
        }
    }

    written
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
    let n = 3;

    for seed in 0..16u64 {
        let fabric = SimNetwork::new();
        let mut net = Cluster::builder()
            .peers(n)
            .transport(Arc::new(SimLoopback::new(fabric.clone())))
            .build()
            .await
            .unwrap();
        let (room, dbs) = cluster_shared_database(&mut net, "fuzz").await.unwrap();
        full_mesh_manual(&mut net, &fabric, &room, n).await;

        let written = run_schedule(&mut net, &fabric, &room, &dbs, &writes, seed).await;

        let all: Vec<usize> = (0..n).collect();
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
