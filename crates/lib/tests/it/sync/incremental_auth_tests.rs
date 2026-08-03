//! Authorization tests for the incremental sync path.
//!
//! Bootstrap (empty tips) enforces the database's auth policy. Incremental
//! (non-empty tips) is the branch the *caller* selects by sending any tip at
//! all, so it must enforce the same policy — otherwise a fabricated tip is a
//! complete bypass of manual approval.

use eidetica::{
    Entry,
    crdt::Doc,
    entry::ID,
    store::DocStore,
    sync::{
        handler::SyncHandler,
        protocol::{RequestContext, SyncRequest, SyncResponse, SyncTreeRequest},
    },
};

use super::helpers;

const SECRET: &str = "TOP_SECRET_VALUE";

/// Write a recognizable secret into the database so disclosure is detectable.
async fn write_secret(database: &eidetica::Database) {
    let txn = database.new_transaction().await.unwrap();
    let store = txn.get_store::<DocStore>("data").await.unwrap();
    store.set("secret", SECRET).await.unwrap();
    txn.commit().await.unwrap();
}

/// An incremental request carrying a tip we have never seen.
///
/// `our_tips` is non-empty, so the handler takes the incremental branch; the
/// tip matches nothing in our DAG, so it never stops the ancestor walk.
fn fabricated_tip_request(tree_id: &ID) -> SyncRequest {
    SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips: vec![ID::from_bytes("nonexistent_tip")].into(),
        peer_pubkey: None,
        requesting_key: None,
        requesting_key_name: None,
        requested_permission: None,
        metadata: None,
    })
}

/// Entries served by this response, or an empty slice if it served none.
fn served_entries(response: &SyncResponse) -> &[Entry] {
    match response {
        SyncResponse::Incremental(incremental) => &incremental.missing_entries,
        _ => &[],
    }
}

/// Whether any served entry carries the secret in any of its subtrees.
///
/// Checks the decoded subtree payloads. `Entry`'s `Debug` renders payloads as
/// byte arrays, so string-matching a `{:?}` dump reports "clean" while the
/// secret is plainly present — do not simplify this into a Debug match.
fn discloses_secret(response: &SyncResponse) -> bool {
    served_entries(response).iter().any(|entry| {
        entry.subtrees().iter().any(|subtree| {
            entry
                .data(subtree)
                .is_ok_and(|data| String::from_utf8_lossy(data).contains(SECRET))
        })
    })
}

/// An unauthorized caller must not be served by the incremental path.
///
/// The caller presents no credentials at all and never touches bootstrap:
/// no handshake, no `requesting_key`, never approved.
#[tokio::test]
// Expected-failure marker: the incremental path enforces no authorization yet.
#[should_panic = "served database contents to an unauthorized caller"]
async fn unauthenticated_incremental_pull_is_refused() {
    let (_instance, _user, _key_id, database, sync, tree_id) =
        helpers::setup_manual_approval_server().await;
    write_secret(&database).await;

    let handler = helpers::create_test_sync_handler(&sync);
    let response = handler
        .handle_request(&fabricated_tip_request(&tree_id), &RequestContext::default())
        .await;

    assert!(
        !discloses_secret(&response),
        "incremental sync served database contents to an unauthorized caller: {response:?}"
    );
    assert!(
        served_entries(&response).is_empty(),
        "incremental sync served {} entries to an unauthorized caller",
        served_entries(&response).len()
    );
}

/// The auth key list is as sensitive as the data: it discloses the member set
/// and each member's permission level, which is targeting information.
#[tokio::test]
// Expected-failure marker: the incremental path enforces no authorization yet.
#[should_panic = "disclosed _settings"]
async fn unauthenticated_incremental_pull_does_not_leak_auth_settings() {
    let (_instance, _user, _key_id, _database, sync, tree_id) =
        helpers::setup_manual_approval_server().await;

    let handler = helpers::create_test_sync_handler(&sync);
    let response = handler
        .handle_request(&fabricated_tip_request(&tree_id), &RequestContext::default())
        .await;

    assert!(
        !served_entries(&response)
            .iter()
            .any(|entry| entry.data("_settings").is_ok()),
        "incremental sync disclosed _settings (auth key list) to an unauthorized caller"
    );
}

/// A database with a global wildcard grant is legitimately world-readable and
/// documented as a supported mode. Gating the incremental path must not break it.
#[tokio::test]
async fn public_database_still_serves_incremental() {
    let (_instance, _user, _key_id, database, sync, tree_id) =
        helpers::setup_global_wildcard_server().await;
    write_secret(&database).await;

    let handler = helpers::create_test_sync_handler(&sync);
    let response = handler
        .handle_request(&fabricated_tip_request(&tree_id), &RequestContext::default())
        .await;

    assert!(
        matches!(response, SyncResponse::Incremental(_)),
        "public database refused an incremental request: {response:?}"
    );
    assert!(
        discloses_secret(&response),
        "public database served no data: {response:?}"
    );
}

/// Positive control for the two refusal tests above: the probe must be able to
/// observe disclosure when it happens, or a passing refusal test proves nothing.
///
/// Uses the same fabricated tip against a public database — same code path,
/// same assertions, authorization deliberately satisfied.
#[tokio::test]
async fn probe_detects_disclosure_when_it_occurs() {
    let (_instance, _user, _key_id, database, sync, tree_id) =
        helpers::setup_global_wildcard_server().await;
    write_secret(&database).await;

    let handler = helpers::create_test_sync_handler(&sync);
    let response = handler
        .handle_request(&fabricated_tip_request(&tree_id), &RequestContext::default())
        .await;

    assert!(discloses_secret(&response), "probe cannot detect a positive");
    assert!(
        served_entries(&response)
            .iter()
            .any(|entry| entry.data("_settings").is_ok()),
        "probe cannot detect _settings disclosure"
    );
}
