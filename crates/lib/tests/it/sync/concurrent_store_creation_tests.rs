//! Regression: concurrent first-write to a store created *after* fork.
//!
//! KNOWN BUG (tripwire test). When two peers each create the *same named store*
//! independently — i.e. the store did not exist at the point they forked, so
//! neither side's first write to it has a `subtree_parent` in common — merging
//! the resulting two subtree roots fails with `Backend(NoCommonAncestor)`. The
//! whole store becomes unreadable on the peer that holds both tips.
//!
//! This is a real correctness gap, not a usage error: a conflict-free database
//! whose entire premise is offline-first convergence must merge two histories
//! that share no ancestor by merging from the *empty* base (their union). The
//! main-tree merge already tolerates a missing common ancestor (two roots merge
//! into a diamond during sync); only the **subtree** merge path
//! (`find_merge_base` over `subtree_parents`, `backend/database/*/traversal.rs`)
//! rejects it.
//!
//! It is order-sensitive (a race): if one peer creates the store and syncs it
//! *before* the other writes, the store exists at the other's fork point, a
//! common ancestor exists, and the merge succeeds. The harness's
//! `cluster_shared_database` deliberately seeds the store pre-bootstrap to dodge
//! this; this test reproduces the unseeded race directly.
//!
//! The test is written as a tripwire: it asserts the *current, buggy* behavior
//! (a `NoCommonAncestor` panic) via `should_panic`. When the subtree merge is
//! fixed to merge from an empty base, this test will start failing — at which
//! point it should be flipped to assert convergence (both values present, see
//! the commented block at the end).

use eidetica::{
    auth::{Permission, types::AuthKey},
    crdt::Doc,
    store::DocStore,
    testing::{Cluster, set_global_auth_key},
};

/// Two peers create the `data` store independently then sync; reading the merged
/// store currently panics with `NoCommonAncestor`.
///
/// Tracks a known engine bug — see this module's docs. Remove `should_panic` and
/// restore the convergence assertions once the subtree merge handles a missing
/// common ancestor.
#[tokio::test]
#[should_panic(expected = "NoCommonAncestor")]
async fn concurrent_store_creation_after_fork_currently_fails_to_merge() {
    let mut net = Cluster::builder().peers(2).build().await.unwrap();

    // Peer 0 creates the database but never touches the `data` store, so the
    // store does not exist at the fork point.
    let key0 = net.peer(0).key_id().clone();
    let mut settings = Doc::new();
    settings.set("name", "concurrent-store");
    let db0 = net
        .peer_mut(0)
        .user_mut()
        .create_database(settings, &key0)
        .await
        .unwrap();
    let room = db0.root_id().clone();
    set_global_auth_key(&db0, AuthKey::active(None, Permission::Admin(10)))
        .await
        .unwrap();
    net.peer_mut(0).serve(&room).await.unwrap();

    net.bootstrap(0, 1, &room, Permission::Write(10))
        .await
        .unwrap();
    net.peer_mut(1).serve(&room).await.unwrap();
    let db1 = net
        .peer_mut(1)
        .user_mut()
        .open_database(&room)
        .await
        .unwrap();

    // Each peer's first write to `data` roots an independent subtree DAG.
    {
        let tx = db0.new_transaction().await.unwrap();
        tx.get_store::<DocStore>("data")
            .await
            .unwrap()
            .set_string("x", "from-0")
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    {
        let tx = db1.new_transaction().await.unwrap();
        tx.get_store::<DocStore>("data")
            .await
            .unwrap()
            .set_string("y", "from-1")
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    net.exchange(0, 1, &room).await.unwrap();

    // Peer 0 now holds both subtree roots. Materializing the store walks
    // `find_merge_base` over the two roots, which share no `subtree_parent`, and
    // panics here with `NoCommonAncestor`.
    let tx = db0.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("data").await.unwrap();
    store.get_string("x").await.unwrap();

    // Desired behavior once fixed (drop `should_panic` and assert this instead):
    //     assert!(net.converge(&room).await.unwrap());
    //     assert_eq!(store.get_string("x").await.unwrap(), "from-0");
    //     assert_eq!(store.get_string("y").await.unwrap(), "from-1");
}
