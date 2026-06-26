//! Convergence under a scrambled delivery schedule, checked with full invariants.
//!
//! This is the payoff of the controllable [`SimTransport`]: a real socket
//! transport delivers in wired order, so it can never answer "does the cluster
//! still converge if the network reorders entry pushes?". Here the test owns the
//! wire — it captures every push and releases them child-before-parent — then
//! asserts not just tip-set equality but the stronger full-history invariants
//! ([`Cluster::assert_no_lost_entries`], [`assert_all_present`], `assert_all_signed`).
//!
//! [`assert_all_present`]: eidetica::Cluster::assert_all_present

use std::sync::Arc;

use eidetica::{
    Cluster,
    testing::{SimLoopback, SimNetwork},
};

use super::helpers::{cluster_get, cluster_put, cluster_shared_database};

/// Peer 0 makes two causally-ordered writes that fan out to peers 1 and 2 over
/// auto-sync. Every push is captured and then delivered in reverse send order,
/// so each receiver sees the second write before the first. The cluster must
/// still converge, lose no entries, and hold every write everywhere — signed.
#[tokio::test]
async fn test_reordered_delivery_converges_with_invariants() {
    let fabric = SimNetwork::new();
    let mut net = Cluster::builder()
        .peers(3)
        .transport(Arc::new(SimLoopback::new(fabric.clone())))
        .build()
        .await
        .unwrap();
    let (room, dbs) = cluster_shared_database(&mut net, "reorder").await.unwrap();

    // Peer 0 pushes to both other peers automatically.
    net.auto_sync(0, 1, &room).await.unwrap();
    net.auto_sync(0, 2, &room).await.unwrap();

    // Capture from here: every flushed push is held on the wire, not delivered.
    fabric.set_manual_delivery(true);

    // Two causally-ordered writes, each its own flush so each is a distinct push
    // (per destination). The second write descends from the first.
    let mut written: Vec<eidetica::entry::ID> = Vec::new();
    for (k, v) in [("first", "1"), ("second", "2")] {
        cluster_put(&dbs[0], k, v).await.unwrap();
        net.flush(0).await.unwrap();
        written.extend(net.snapshot(0, &room).await.unwrap().into_tips());
    }

    // Two writes × two destinations = four messages still in flight.
    let pending = fabric.pending();
    assert_eq!(
        pending.len(),
        4,
        "two writes fanned out to two peers should capture four pushes"
    );

    // Release them back-to-front: every receiver gets a child before its parent.
    for &seq in pending.iter().rev() {
        assert!(fabric.deliver(seq).await, "message {seq} should deliver");
    }

    let all = [0, 1, 2];

    // Tip equality first — the baseline the old tests stop at...
    assert!(
        net.converged(&all, &room).await.unwrap(),
        "cluster must converge despite child-before-parent delivery"
    );

    // ...then the stronger invariants the harness adds.
    net.assert_no_lost_entries(&all, &room).await.unwrap();
    net.assert_all_present(&all, &room, &written).await.unwrap();
    for &p in &all {
        net.assert_all_signed(p, &room).await.unwrap();
    }

    // And the data is actually readable on the peers that received it reordered.
    for db in &dbs[1..] {
        assert_eq!(cluster_get(db, "first").await.unwrap(), "1");
        assert_eq!(cluster_get(db, "second").await.unwrap(), "2");
    }
}
