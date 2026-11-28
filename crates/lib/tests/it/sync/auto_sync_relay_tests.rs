//! Convergence across a chain of configured links, without a direct push from
//! the origin to the final peer.

use eidetica::Cluster;

use super::helpers::{cluster_get, cluster_put, cluster_shared_database};

/// The intermediate peer serves a verified write it received by auto-sync.
#[tokio::test]
async fn test_sync_converges_across_configured_chain() {
    let mut net = Cluster::builder().peers(3).build().await.unwrap();
    let (room, dbs) = cluster_shared_database(&mut net, "relay").await.unwrap();

    // Discard the final peer's outbound bootstrap relationship to the origin.
    let origin = net.peer(0).instance().id();
    let final_peer = net.peer(2).instance().id();
    net.peer(2)
        .sync()
        .remove_tree_sync(&origin, &room)
        .await
        .unwrap();
    net.auto_sync(0, 1, &room).await.unwrap();
    net.auto_sync(1, 2, &room).await.unwrap();
    assert!(
        !net.peer(0)
            .sync()
            .is_tree_synced_with_peer(&final_peer, &room)
            .await
            .unwrap(),
        "bootstrap must not have installed a shortcut from the origin"
    );

    // Write at the head of the chain.
    cluster_put(&dbs[0], "relay", "payload").await.unwrap();

    net.flush(0).await.unwrap();
    assert_eq!(cluster_get(&dbs[1], "relay").await.unwrap(), "payload");

    // The write hook queues local commits only. Catch up from the next hop
    // with the usual sync exchange, not another flush of the empty queue.
    net.exchange(2, 1, &room).await.unwrap();

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
