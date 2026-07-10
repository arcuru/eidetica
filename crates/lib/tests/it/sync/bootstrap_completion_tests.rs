//! Client-side outgoing bootstrap completion tests.
//!
//! When a ticket bootstrap against a manual-approval peer comes back pending,
//! the client records an outgoing bootstrap request in its `_sync` tree. Once
//! access is granted, Sync completes the join locally — pulling the now-authorized
//! tree, applying the recorded sync settings, and marking the request hydrated —
//! WITHOUT the caller re-invoking `request_database_access`.
//!
//! Two wake sources drive completion, both exercised here:
//! - a periodic sweep (`sweep_outgoing_bootstrap_requests`) — correctness /
//!   restart-safety;
//! - a broadcast-woken reaction (the approval entry landing via the remote-write
//!   callback) — latency.

use std::time::Duration;

use eidetica::{
    auth::Permission,
    path,
    store::DocStore,
    sync::{DatabaseTicket, transports::http::HttpTransport},
    user::types::SyncSettings,
};

use super::helpers::*;

/// A pending request records an outgoing bootstrap request capturing the ticket
/// target, addresses, key, permission, and the caller's desired sync settings.
#[tokio::test]
async fn pending_request_records_outgoing_bootstrap_request() {
    let (server_instance, _server_user, _server_key_id, _server_database, server_sync, tree_id) =
        setup_manual_approval_server().await;
    let server_addr = start_sync_server(&server_sync).await;

    let (_client_instance, mut client_user, client_key_id, client_sync) =
        setup_sync_enabled_client("client_user", "client_key").await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    let ticket = DatabaseTicket::with_addresses(tree_id.clone(), vec![server_addr.clone()]);

    // The request is deferred for manual approval; the call errors but the
    // outgoing request is recorded for later completion.
    let result = client_user
        .request_database_access(
            &client_sync,
            &ticket,
            &client_key_id,
            Permission::Write(5),
            None,
        )
        .await;
    assert!(result.is_err(), "manual-approval request should be pending");

    let outgoing = client_sync
        .pending_outgoing_bootstrap_requests()
        .await
        .expect("listing outgoing requests should succeed");
    assert_eq!(outgoing.len(), 1, "exactly one outgoing request recorded");
    let (_id, record) = &outgoing[0];
    assert_eq!(record.tree_id, tree_id);
    assert_eq!(record.requesting_pubkey, client_key_id);
    assert_eq!(record.requested_permission, Permission::Write(5));
    assert_eq!(
        record.addresses,
        vec![server_addr],
        "ticket addresses are carried for later pull"
    );

    server_sync.stop_server().await.unwrap();
    drop(server_instance);
}

/// Sweep-driven completion: after approval, the periodic sweep pulls the tree,
/// applies the settings, marks the request hydrated, and the database opens —
/// with no manual re-request.
#[tokio::test]
async fn sweep_completes_outgoing_bootstrap_after_approval() {
    let (server_instance, server_user, server_key_id, _server_database, server_sync, tree_id) =
        setup_manual_approval_server().await;
    let server_addr = start_sync_server(&server_sync).await;

    let (_client_instance, mut client_user, client_key_id, client_sync) =
        setup_sync_enabled_client("client_user", "client_key").await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    let ticket = DatabaseTicket::with_addresses(tree_id.clone(), vec![server_addr.clone()]);

    // Pending: outgoing request recorded, provisional mapping front-loaded.
    let result = client_user
        .request_database_access(
            &client_sync,
            &ticket,
            &client_key_id,
            Permission::Write(5),
            None,
        )
        .await;
    assert!(result.is_err(), "request should be pending");
    assert_eq!(
        client_sync
            .pending_outgoing_bootstrap_requests()
            .await
            .unwrap()
            .len(),
        1
    );

    // Approve on the server.
    let pending = server_sync.pending_bootstrap_requests().await.unwrap();
    assert_eq!(pending.len(), 1);
    let (request_id, _) = &pending[0];
    server_user
        .approve_bootstrap_request(&server_sync, request_id, &server_key_id)
        .await
        .expect("approval should succeed");
    server_sync.flush().await.ok();

    // The client never re-requests: the sweep drives completion. Retry the sweep
    // a few times to absorb any transport timing.
    let mut hydrated = false;
    for _ in 0..20 {
        client_sync
            .sweep_outgoing_bootstrap_requests()
            .await
            .expect("sweep should not error");
        if client_sync
            .pending_outgoing_bootstrap_requests()
            .await
            .unwrap()
            .is_empty()
        {
            hydrated = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        hydrated,
        "outgoing request should be hydrated after approval + sweep"
    );

    // The database now opens (mapping front-loaded, entries pulled) and is
    // writable with the approved by-pubkey key.
    let db = client_user
        .open_database(&tree_id)
        .await
        .expect("database should open after sweep-driven completion");
    let tx = db.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("data").await.unwrap();
    store.set("swept", "value").await.unwrap();
    tx.commit()
        .await
        .expect("write should commit with the approved key");

    server_sync.stop_server().await.unwrap();
    drop(server_instance);
}

/// Broadcast-woken completion: the client keeps its transport server up so the
/// approval broadcast reaches it. The approval entry landing via the remote-write
/// callback kicks completion — no sweep, no manual re-request.
#[tokio::test]
async fn broadcast_completes_outgoing_bootstrap_after_approval() {
    let (server_instance, server_user, server_key_id, _server_database, server_sync, tree_id) =
        setup_manual_approval_server().await;
    let server_addr = start_sync_server(&server_sync).await;

    // The client runs a server too, so the server's approval broadcast can be
    // pushed to it (it is registered as a tree peer during the initial request).
    let (_client_instance, mut client_user, client_key_id, client_sync) =
        setup_sync_enabled_client("client_user", "client_key").await;
    let _client_addr = start_sync_server(&client_sync).await;

    let ticket = DatabaseTicket::with_addresses(tree_id.clone(), vec![server_addr.clone()]);

    let result = client_user
        .request_database_access(
            &client_sync,
            &ticket,
            &client_key_id,
            Permission::Write(5),
            None,
        )
        .await;
    assert!(result.is_err(), "request should be pending");
    assert_eq!(
        client_sync
            .pending_outgoing_bootstrap_requests()
            .await
            .unwrap()
            .len(),
        1
    );

    // Approve: this broadcasts the approval entry to the database's peers,
    // including the requesting client, which reacts via its remote-write callback.
    let pending = server_sync.pending_bootstrap_requests().await.unwrap();
    let (request_id, _) = &pending[0];
    server_user
        .approve_bootstrap_request(&server_sync, request_id, &server_key_id)
        .await
        .expect("approval should succeed");
    server_sync.flush().await.ok();

    // Wait for the broadcast to arrive and drive completion. No sweep call here —
    // completion must be triggered by the broadcast landing.
    let mut hydrated = false;
    for _ in 0..40 {
        if client_sync
            .pending_outgoing_bootstrap_requests()
            .await
            .unwrap()
            .is_empty()
        {
            hydrated = true;
            break;
        }
        // Nudge the server to redeliver in case the first push raced the client's
        // server coming up; this is delivery retry, not sweep-driven completion.
        server_sync.flush().await.ok();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        hydrated,
        "outgoing request should be hydrated by the approval broadcast"
    );

    // Database opens and is writable.
    let db = client_user
        .open_database(&tree_id)
        .await
        .expect("database should open after broadcast-driven completion");
    let tx = db.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("data").await.unwrap();
    store.set("broadcast", "value").await.unwrap();
    tx.commit()
        .await
        .expect("write should commit with the approved key");

    client_sync.stop_server().await.ok();
    server_sync.stop_server().await.unwrap();
    drop(server_instance);
}

/// The recorded outgoing request carries the caller's desired sync settings,
/// which completion applies to the sync tree's combined settings.
#[tokio::test]
async fn completion_applies_recorded_sync_settings() {
    let (server_instance, server_user, server_key_id, _server_database, server_sync, tree_id) =
        setup_manual_approval_server().await;
    let server_addr = start_sync_server(&server_sync).await;

    let (_client_instance, mut client_user, client_key_id, client_sync) =
        setup_sync_enabled_client("client_user", "client_key").await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    let ticket = DatabaseTicket::with_addresses(tree_id.clone(), vec![server_addr.clone()]);
    let result = client_user
        .request_database_access(
            &client_sync,
            &ticket,
            &client_key_id,
            Permission::Write(5),
            None,
        )
        .await;
    assert!(result.is_err(), "request should be pending");

    // The desired sync settings are threaded from the User entry point into the
    // recorded outgoing request as plain data. Capture what was recorded so we
    // can assert completion applies exactly those settings.
    let outgoing = client_sync
        .pending_outgoing_bootstrap_requests()
        .await
        .unwrap();
    assert_eq!(outgoing.len(), 1);
    let recorded_settings = outgoing[0].1.sync_settings.clone();

    // Approve and complete via sweep.
    let pending = server_sync.pending_bootstrap_requests().await.unwrap();
    let (request_id, _) = &pending[0];
    server_user
        .approve_bootstrap_request(&server_sync, request_id, &server_key_id)
        .await
        .expect("approval should succeed");
    server_sync.flush().await.ok();

    for _ in 0..20 {
        client_sync
            .sweep_outgoing_bootstrap_requests()
            .await
            .unwrap();
        if client_sync
            .pending_outgoing_bootstrap_requests()
            .await
            .unwrap()
            .is_empty()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Completion applied the enabled combined settings for the tree. Read them
    // back directly from the sync tree's `database_users` store (the same store
    // `UserSyncManager::set_combined_settings` writes to).
    let sync_tree = client_sync.sync_tree();
    let db_users = sync_tree
        .get_store_viewer::<DocStore>("database_users")
        .await
        .expect("database_users store should open");
    let settings_json = db_users
        .get_path_as::<String>(path!(&tree_id.to_string(), "combined_settings"))
        .await
        .expect("combined settings should be present after completion");
    let applied: SyncSettings =
        serde_json::from_str(&settings_json).expect("combined settings should deserialize");
    assert_eq!(
        applied.sync_enabled, recorded_settings.sync_enabled,
        "completion applies the recorded sync settings"
    );
    assert_eq!(
        applied.sync_on_commit, recorded_settings.sync_on_commit,
        "completion applies the recorded sync settings"
    );
    assert_eq!(
        applied.interval_seconds, recorded_settings.interval_seconds,
        "completion applies the recorded sync settings"
    );

    server_sync.stop_server().await.unwrap();
    drop(server_instance);
}
