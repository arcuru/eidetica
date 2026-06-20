//! Deterministic conflict resolution under concurrent writes to one key.
//!
//! Two peers write the same key with different values and no coordination. After
//! convergence both replicas must agree — identical tip set *and* identical
//! materialized value — proving the CRDT merge is deterministic, not
//! order-dependent.

use eidetica::Cluster;

use super::helpers::{cluster_get, cluster_put, cluster_shared_database};

/// Both peers write key `x` concurrently; after convergence they resolve to the
/// same value, which is one of the two writes.
#[tokio::test]
async fn test_concurrent_write_resolves_deterministically() {
    let mut net = Cluster::builder().peers(2).build().await.unwrap();
    let (room, dbs) = cluster_shared_database(&mut net, "determinism")
        .await
        .unwrap();

    // Same key, different values, no coordination.
    cluster_put(&dbs[0], "x", "alpha").await.unwrap();
    cluster_put(&dbs[1], "x", "beta").await.unwrap();

    assert!(
        net.converge(&room).await.unwrap(),
        "the two peers should converge to an identical tip set"
    );

    // Both replicas resolve the conflict the same way...
    let v0 = cluster_get(&dbs[0], "x").await.unwrap();
    let v1 = cluster_get(&dbs[1], "x").await.unwrap();
    assert_eq!(
        v0, v1,
        "both replicas must materialize the same value for the conflicted key"
    );

    // ...and to one of the values actually written.
    assert!(
        v0 == "alpha" || v0 == "beta",
        "resolved value must be one of the concurrent writes, got {v0:?}"
    );
}
