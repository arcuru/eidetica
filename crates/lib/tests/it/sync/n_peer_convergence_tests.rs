//! N-peer convergence and multi-writer merge.
//!
//! The rest of the sync suite is two-peer throughout. With [`Cluster`] making
//! N>2 cheap, this exercises DAG merge under N-way concurrency: three peers each
//! write independently, then the whole cluster is driven to a single tip set.

use eidetica::Cluster;

use super::helpers::{cluster_get, cluster_put, cluster_shared_database};

/// Three peers each write a distinct key with no coordination, then converge.
/// Every peer must end on an identical tip set and see all three writes.
#[tokio::test]
async fn test_three_peer_convergence() {
    let mut net = Cluster::builder().peers(3).build().await.unwrap();
    let (room, dbs) = cluster_shared_database(&mut net, "convergence")
        .await
        .unwrap();

    // Concurrent, uncoordinated writes — one per peer.
    cluster_put(&dbs[0], "k0", "v0").await.unwrap();
    cluster_put(&dbs[1], "k1", "v1").await.unwrap();
    cluster_put(&dbs[2], "k2", "v2").await.unwrap();

    // Before convergence the peers disagree.
    assert!(
        !net.converged_all(&room).await.unwrap(),
        "peers should diverge after independent writes, before sync"
    );

    // Drive bidirectional exchange across all pairs to a fixpoint.
    assert!(
        net.converge(&room).await.unwrap(),
        "cluster should reach an identical tip set"
    );

    // Every peer sees every write.
    for (i, db) in dbs.iter().enumerate() {
        for (k, v) in [("k0", "v0"), ("k1", "v1"), ("k2", "v2")] {
            assert_eq!(
                cluster_get(db, k).await.unwrap(),
                v,
                "peer {i} missing {k} after convergence"
            );
        }
    }
}
