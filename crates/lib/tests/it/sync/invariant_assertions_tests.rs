//! Convergence invariants beyond tip-set equality.
//!
//! [`Cluster::converged`] proves peers *agree* on a tip set, but agreement is a
//! weak guarantee: it says nothing about *what* was agreed on. A merge could
//! converge onto a state that silently dropped a signed entry while still landing
//! on the same tips. This drives an ordinary N-peer convergence and then asserts
//! the stronger properties — no lost entries, the test's own writes present
//! everywhere, every entry signed — that the existing convergence tests don't
//! check.
//!
//! Not asserted here: `Cluster::assert_all_verified`. On this build sync
//! ingestion records a placeholder verification status rather than running a
//! per-entry signature check (see the TODO on `VerificationStatus` and
//! `docs/src/design/verification.md`), so a bootstrapped peer legitimately holds
//! entries marked `Failed`. That assertion becomes meaningful once
//! verification-on-ingest lands.

use eidetica::Cluster;

use super::helpers::{cluster_put, cluster_shared_database};

/// Three peers each write a distinct key, then converge. Beyond tip equality,
/// assert the full-history invariants: the union of entries survives on every
/// peer, the test's own writes are present everywhere, and every entry each peer
/// holds is signed.
#[tokio::test]
async fn test_convergence_preserves_all_signed_entries() {
    let mut net = Cluster::builder().peers(3).build().await.unwrap();
    let (room, dbs) = cluster_shared_database(&mut net, "invariants")
        .await
        .unwrap();

    // One uncoordinated write per peer. Capture each write's resulting tip so we
    // can demand those exact entries survive the merge on every peer.
    let mut written: Vec<eidetica::entry::ID> = Vec::new();
    for (i, db) in dbs.iter().enumerate() {
        cluster_put(db, &format!("k{i}"), &format!("v{i}"))
            .await
            .unwrap();
        // The write is now this peer's tip.
        written.extend(net.tips(i, &room).await.unwrap());
    }

    assert!(
        net.converge(&room).await.unwrap(),
        "cluster should reach an identical tip set"
    );

    let all = [0, 1, 2];

    // No peer is missing an entry another holds...
    net.assert_no_lost_entries(&all, &room).await.unwrap();
    // ...and specifically, every write this test made survives everywhere.
    net.assert_all_present(&all, &room, &written).await.unwrap();

    // Every entry each peer holds is signed — a merge must not admit an unsigned
    // entry into a converged state.
    for &p in &all {
        net.assert_all_signed(p, &room).await.unwrap();
    }
}
