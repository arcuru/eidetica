//! Auto-sync relay: a write propagates along a chain of background links.
//!
//! Auto-sync is wired as a chain — `auto_sync(0,1)` and `auto_sync(1,2)` with no
//! direct `0<->2` link. A write at peer 0 reaches peer 2 only by peer 1 relaying
//! it: peer 1 receives the entry and, because a remotely-synced entry fires the
//! local write callback, re-queues it on to peer 2. `flush` is the barrier.

use eidetica::Cluster;

use super::helpers::{cluster_get, cluster_put, cluster_shared_database};

/// A peer-0 write reaches peer 2 transitively through peer 1, with no direct
/// 0<->2 sync link.
#[tokio::test]
async fn test_auto_sync_relay_chain() {
    let mut net = Cluster::builder().peers(3).build().await.unwrap();
    let (room, dbs) = cluster_shared_database(&mut net, "relay").await.unwrap();

    // Chain topology: 0<->1 and 1<->2 auto-sync, but NOT 0<->2.
    net.auto_sync(0, 1, &room).await.unwrap();
    net.auto_sync(1, 2, &room).await.unwrap();

    // Write at the head of the chain.
    cluster_put(&dbs[0], "relay", "payload").await.unwrap();

    // Push along the chain: 0 -> 1, then 1 (having received it) -> 2.
    net.flush(0).await.unwrap();
    net.flush(1).await.unwrap();

    // Peer 2 received the write despite never syncing directly with peer 0.
    assert_eq!(
        cluster_get(&dbs[2], "relay").await.unwrap(),
        "payload",
        "the write should reach peer 2 by relay through peer 1"
    );
    assert!(
        net.converged_all(&room).await.unwrap(),
        "the whole chain should be converged after the relay"
    );
}
