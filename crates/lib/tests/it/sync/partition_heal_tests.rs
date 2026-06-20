//! Partition and heal: a split cluster reconnects and converges.
//!
//! The partition is *coarse* — modeled by withholding connectivity (only the
//! connected pair exchanges) rather than dropping individual messages.
//! Fine-grained drop/reorder belongs to the Tier 1 `SimTransport`.

use eidetica::Cluster;

use super::helpers::{cluster_get, cluster_put, cluster_shared_database};

/// Peers 0 and 1 stay connected while peer 2 is withheld; the pair converges and
/// peer 2 diverges. After a full heal every peer agrees and holds all writes.
#[tokio::test]
async fn test_partition_then_heal() {
    let mut net = Cluster::builder().peers(3).build().await.unwrap();
    let (room, dbs) = cluster_shared_database(&mut net, "partition")
        .await
        .unwrap();

    // Each peer writes in isolation.
    cluster_put(&dbs[0], "a", "from-0").await.unwrap();
    cluster_put(&dbs[1], "b", "from-1").await.unwrap();
    cluster_put(&dbs[2], "c", "from-2").await.unwrap();

    // Partition: only peers 0 and 1 are connected. Peer 2 is withheld.
    net.exchange(0, 1, &room).await.unwrap();

    // The connected pair converges; the cluster as a whole does not.
    assert!(
        net.converged(&[0, 1], &room).await.unwrap(),
        "the connected pair should converge"
    );
    assert!(
        !net.converged_all(&room).await.unwrap(),
        "peer 2 is partitioned, so the whole cluster must not be converged"
    );

    // Peer 0 has peer 1's write but not the withheld peer 2's.
    assert_eq!(cluster_get(&dbs[0], "b").await.unwrap(), "from-1");
    assert!(
        cluster_get(&dbs[0], "c").await.is_err(),
        "peer 0 must not have peer 2's write while peer 2 is partitioned"
    );

    // Heal: reconnect everyone and drive to a fixpoint.
    assert!(
        net.converge(&room).await.unwrap(),
        "the cluster should converge after healing"
    );

    // Every peer now holds every write.
    for (i, db) in dbs.iter().enumerate() {
        for (k, v) in [("a", "from-0"), ("b", "from-1"), ("c", "from-2")] {
            assert_eq!(
                cluster_get(db, k).await.unwrap(),
                v,
                "peer {i} missing {k} after heal"
            );
        }
    }
}
