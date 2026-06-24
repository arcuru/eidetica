//! Tests that a bootstrap request carries free-form approver metadata.
//!
//! The requester can attach an arbitrary `Doc` which is surfaced verbatim on
//! the stored `BootstrapRequest` for the approver to inspect.

use eidetica::{
    auth::Permission,
    crdt::Doc,
    sync::{DatabaseTicket, transports::http::HttpTransport},
};

use super::helpers::*;

#[tokio::test]
async fn bootstrap_request_carries_metadata_to_approver() {
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

    // Attach free-form context for the approver to inspect.
    let mut metadata = Doc::new();
    metadata.set("settings_db_id", "bafyrtestsettingsdb");
    metadata.set("note", "matrix bridge login");

    let result = client_user
        .request_database_access(
            &client_sync,
            &ticket,
            &client_key_id,
            Permission::Write(5),
            Some(metadata),
        )
        .await;
    assert!(result.is_err(), "manual-approval request should be pending");

    // The approver sees the metadata verbatim on the stored pending request.
    let pending = server_sync.pending_bootstrap_requests().await.unwrap();
    assert_eq!(pending.len(), 1, "exactly one pending request expected");
    let (_request_id, request) = &pending[0];
    let got = request
        .metadata
        .as_ref()
        .expect("the stored request should carry the requester's metadata");
    assert_eq!(
        got.get_as::<String>("settings_db_id").as_deref(),
        Some("bafyrtestsettingsdb")
    );
    assert_eq!(
        got.get_as::<String>("note").as_deref(),
        Some("matrix bridge login")
    );

    server_sync.stop_server().await.unwrap();
    drop(server_instance);
}
