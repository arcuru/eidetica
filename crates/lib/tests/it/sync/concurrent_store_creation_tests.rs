//! Concurrent first-write to a store created *after* fork.
//!
//! When two peers each create the *same named store* independently — the store
//! did not exist at the point they forked, so neither side's first write to it
//! has a `store_parent` in common — the resulting two store roots share no
//! ancestor. They merge from the **empty base**: the union of both histories,
//! folded from a default state.
//!
//! This mirrors the main tree, which already merges two independent roots into
//! a diamond during sync. Convergence must not depend on which peer created the
//! store first, since that is a race: if one peer creates the store and syncs it
//! before the other writes, a common ancestor exists and the merge is ordinary.

use eidetica::{
    auth::{Permission, types::AuthKey},
    crdt::Doc,
    store::DocStore,
    testing::{Cluster, set_global_auth_key},
};

/// Two peers create the `data` store independently then sync; the two roots
/// merge from the empty base and both writes survive.
#[tokio::test]
async fn concurrent_store_creation_after_fork_merges() {
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

    assert!(
        net.converge(&room).await.unwrap(),
        "the two peers should converge to an identical tip set"
    );

    // Both peers hold both store roots. The two histories share no
    // `store_parent`, so materializing folds them from the empty base and
    // neither peer's write is lost.
    for (label, db) in [("peer 0", &db0), ("peer 1", &db1)] {
        let tx = db.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("data").await.unwrap();
        assert_eq!(
            store.get_string("x").await.unwrap(),
            "from-0",
            "{label} lost peer 0's write"
        );
        assert_eq!(
            store.get_string("y").await.unwrap(),
            "from-1",
            "{label} lost peer 1's write"
        );
    }
}
