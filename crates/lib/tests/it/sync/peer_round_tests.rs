//! Tests for the on-demand per-peer sync round.
//!
//! The engine's per-peer tree walk — the walk the periodic timer performs for
//! each active peer — was reachable only through that timer, whose default
//! interval is 300 seconds, so no test could observe it. The `testing`-only
//! `Sync::sync_with_peer_now_for_test` hook drives the same round; these tests run it end
//! to end: the walk moving data, and the liveness stamp
//! (`SyncStatus::last_sync`, set only when a round moved a tree).

use eidetica::{
    store::DocStore,
    sync::{PeerId, transports::http::HttpTransport},
};

use super::helpers::*;

/// A ticket sync registers the peer and its tree but is a one-shot pull: it
/// does not stamp liveness. One on-demand round then walks the registered
/// tree, moves the data the client lacks, and stamps `last_sync`.
#[tokio::test]
async fn on_demand_round_walks_registered_trees_and_stamps_liveness() {
    let (server_instance, _server_user, _server_key_id, server_database, tree_id, server_sync) =
        setup_public_sync_enabled_server("server_user", "server_key", "round_database").await;

    // Initial data so the ticket sync bootstraps a non-empty tree.
    {
        let tx = server_database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("messages").await.unwrap();
        store.set("msg1", "before the round").await.unwrap();
        tx.commit().await.unwrap();
    }

    let _server_addr = start_sync_server(&server_sync).await;
    let ticket = server_sync
        .create_ticket(&tree_id)
        .await
        .expect("create_ticket should succeed");

    let (client_instance, _client_user, _client_key_id, client_sync) =
        setup_sync_enabled_client("client_user", "client_key").await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();
    client_sync
        .sync_with_ticket(&ticket)
        .await
        .expect("initial ticket sync should succeed");
    client_sync.flush().await.ok();

    let server_pubkey = server_instance.id();

    // The ticket sync is a one-shot pull, not an engine round, so liveness
    // must still read `None` before any round has run.
    let before = client_sync
        .get_sync_status(&tree_id, &server_pubkey)
        .await
        .unwrap();
    assert_eq!(before.last_sync, None, "no engine round has run yet");

    // Server writes data the client does not have.
    let new_entry_id = {
        let tx = server_database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("messages").await.unwrap();
        store.set("msg2", "after the round").await.unwrap();
        tx.commit().await.unwrap()
    };
    assert!(
        !client_instance.has_entry(&new_entry_id).await,
        "client must not have the new entry before the round"
    );

    // One on-demand round walks every tree registered against the peer.
    client_sync
        .sync_with_peer_now_for_test(&PeerId::from(&server_pubkey))
        .await
        .expect("on-demand round should complete");

    assert!(
        client_instance.has_entry(&new_entry_id).await,
        "the round's tree walk should have moved the new entry"
    );

    // A round that moved a tree is the peer answering — the fact `last_sync`
    // reports.
    let after = client_sync
        .get_sync_status(&tree_id, &server_pubkey)
        .await
        .unwrap();
    assert!(
        after.last_sync.is_some(),
        "a round that moved a tree should stamp last_sync"
    );

    server_sync.stop_server().await.unwrap();
}

/// A round that cannot establish a route surfaces the failure to its caller,
/// moves nothing, and stamps no liveness: a round in which no tree moved is
/// not a successful round.
#[tokio::test]
async fn on_demand_round_to_a_dead_peer_fails_without_stamping_liveness() {
    let (server_instance, _server_user, _server_key_id, server_database, tree_id, server_sync) =
        setup_public_sync_enabled_server("server_user", "server_key", "dead_database").await;

    {
        let tx = server_database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("messages").await.unwrap();
        store.set("msg1", "before the round").await.unwrap();
        tx.commit().await.unwrap();
    }

    let _server_addr = start_sync_server(&server_sync).await;
    let ticket = server_sync
        .create_ticket(&tree_id)
        .await
        .expect("create_ticket should succeed");

    let (client_instance, _client_user, _client_key_id, client_sync) =
        setup_sync_enabled_client("client_user", "client_key").await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();
    client_sync
        .sync_with_ticket(&ticket)
        .await
        .expect("initial ticket sync should succeed");
    client_sync.flush().await.ok();

    let server_pubkey = server_instance.id();

    // New data on the server, then the server goes away.
    let new_entry_id = {
        let tx = server_database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("messages").await.unwrap();
        store.set("msg2", "never arrives").await.unwrap();
        tx.commit().await.unwrap()
    };
    server_sync.stop_server().await.unwrap();

    let result = client_sync
        .sync_with_peer_now_for_test(&PeerId::from(&server_pubkey))
        .await;

    assert!(
        result.is_err(),
        "a round with no usable route must surface the failure"
    );
    assert!(
        !client_instance.has_entry(&new_entry_id).await,
        "a failed round must move nothing"
    );
    let status = client_sync
        .get_sync_status(&tree_id, &server_pubkey)
        .await
        .unwrap();
    assert_eq!(
        status.last_sync, None,
        "a round that moved no tree must not stamp liveness"
    );
}
