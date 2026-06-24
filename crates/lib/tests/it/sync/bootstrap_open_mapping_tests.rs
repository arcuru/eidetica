//! Regression tests for bootstrap establishing the User-layer SigKey mapping.
//!
//! A successful bootstrap grants access at the sync layer (auth + entries) but
//! historically left no User-layer SigKey mapping, so `User::open_database`
//! failed with "No key found" unless the caller manually invoked
//! `track_database`. `User::request_database_access` now establishes the mapping
//! itself, and opening a not-yet-approved database surfaces a clear
//! `UserError::DatabaseAccessPending` instead of a cryptic backend error.

use eidetica::{
    Error,
    auth::Permission,
    store::DocStore,
    sync::{DatabaseTicket, transports::http::HttpTransport},
    user::UserError,
};

use super::helpers::*;

/// Auto-approve server -> client `request_database_access` -> the database is
/// immediately openable and usable WITHOUT a manual `track_database` call.
#[tokio::test]
async fn request_access_makes_database_openable_without_manual_track() {
    let (_server_instance, _server_user, _server_key_id, server_database, tree_id, server_sync) =
        setup_sync_enabled_server_with_auto_approve("server_user", "server_key", "test_db").await;

    // Seed some data on the server.
    {
        let tx = server_database.new_transaction().await.unwrap();
        let store = tx.get_store::<DocStore>("messages").await.unwrap();
        store.set("msg1", "hello").await.unwrap();
        tx.commit().await.unwrap();
    }

    let _server_addr = start_sync_server(&server_sync).await;
    let ticket = server_sync
        .create_ticket(&tree_id)
        .await
        .expect("create_ticket should succeed");

    let (_client_instance, mut client_user, client_key_id, client_sync) =
        setup_sync_enabled_client("client_user", "client_key").await;
    client_sync
        .register_transport("http", HttpTransport::builder())
        .await
        .unwrap();

    client_user
        .request_database_access(
            &client_sync,
            &ticket,
            &client_key_id,
            Permission::Write(5),
            None,
        )
        .await
        .expect("request_database_access should succeed");

    // No manual `track_database` call here — that omission is exactly the
    // regression being guarded against.
    let db = client_user
        .open_database(&tree_id)
        .await
        .expect("database should be openable immediately after request_database_access");

    // The resolved mapping must yield a usable, authorized key: a signed write
    // commits successfully.
    let tx = db.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("messages").await.unwrap();
    store.set("from_client", "world").await.unwrap();
    tx.commit()
        .await
        .expect("client write should commit with the resolved key");

    server_sync.stop_server().await.unwrap();
}

/// Manual-approval server -> a pending request records a provisional mapping ->
/// opening reports a clear `DatabaseAccessPending` -> after approval + sync the
/// database opens and the by-pubkey mapping is authorized for writes.
#[tokio::test]
async fn pending_access_reports_clear_error_then_opens_after_approval() {
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

    // The request is stored pending and returns an error, but the provisional
    // key mapping is still recorded.
    let result = client_user
        .request_database_access(
            &client_sync,
            &ticket,
            &client_key_id,
            Permission::Write(5),
            None,
        )
        .await;
    assert!(
        result.is_err(),
        "request against a manual-approval server should be pending"
    );

    // find_key now resolves the provisional mapping, so opening reaches the data
    // layer — and surfaces a clear "pending" error instead of a cryptic backend
    // not-found.
    let open_err = client_user
        .open_database(&tree_id)
        .await
        .expect_err("opening a not-yet-approved database should fail");
    assert!(
        matches!(&open_err, Error::User(e) if matches!(**e, UserError::DatabaseAccessPending { .. })),
        "expected DatabaseAccessPending, got: {open_err:?}"
    );

    // Approve and let the client sync.
    let pending = server_sync.pending_bootstrap_requests().await.unwrap();
    assert_eq!(pending.len(), 1, "exactly one pending request expected");
    let (request_id, _) = &pending[0];
    server_user
        .approve_bootstrap_request(&server_sync, request_id, &server_key_id)
        .await
        .expect("approval should succeed");
    server_sync.flush().await.ok();

    // The client re-requests access now that it is authorized. This takes the
    // "already authorized" path: entries sync and the real SigKey is discovered
    // and recorded.
    client_user
        .request_database_access(
            &client_sync,
            &ticket,
            &client_key_id,
            Permission::Write(5),
            None,
        )
        .await
        .expect("access should be granted after approval");
    client_sync.flush().await.ok();

    // The pubkey-identity mapping matches the by-pubkey grant created on
    // approval, so the database now opens and the key is authorized for writes.
    let db = client_user
        .open_database(&tree_id)
        .await
        .expect("database should open after approval and sync");
    let tx = db.new_transaction().await.unwrap();
    let store = tx.get_store::<DocStore>("data").await.unwrap();
    store.set("client_key", "client_val").await.unwrap();
    tx.commit()
        .await
        .expect("client write should commit with the approved by-pubkey key");

    server_sync.stop_server().await.unwrap();
    drop(server_instance);
}
