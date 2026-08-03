//! Authorization tests for the incremental sync path.
//!
//! Bootstrap (empty tips) enforces the database's auth policy. Incremental
//! (non-empty tips) is the branch the *caller* selects by sending any tip at
//! all, so it must enforce the same policy — otherwise a fabricated tip is a
//! complete bypass of manual approval.

use eidetica::{
    Clock, Entry, FixedClock,
    auth::{Permission, crypto::PrivateKey, generate_keypair, types::AuthKey},
    entry::ID,
    store::DocStore,
    sync::{
        handler::SyncHandler,
        protocol::{RequestContext, SyncRequest, SyncRequestAuth, SyncResponse, SyncTreeRequest},
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
        auth: None,
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
async fn unauthenticated_incremental_pull_is_refused() {
    let (_instance, _user, _key_id, database, sync, tree_id) =
        helpers::setup_manual_approval_server().await;
    write_secret(&database).await;

    let handler = helpers::create_test_sync_handler(&sync);
    let response = handler
        .handle_request(
            &fabricated_tip_request(&tree_id),
            &RequestContext::default(),
        )
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
async fn unauthenticated_incremental_pull_does_not_leak_auth_settings() {
    let (_instance, _user, _key_id, _database, sync, tree_id) =
        helpers::setup_manual_approval_server().await;

    let handler = helpers::create_test_sync_handler(&sync);
    let response = handler
        .handle_request(
            &fabricated_tip_request(&tree_id),
            &RequestContext::default(),
        )
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
        .handle_request(
            &fabricated_tip_request(&tree_id),
            &RequestContext::default(),
        )
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

/// The current time on the same clock the test instances run on.
///
/// Test instances are built with [`FixedClock::default`], not the system clock,
/// so a real epoch timestamp would land years outside the freshness window.
fn now_ms() -> u64 {
    FixedClock::default().now_millis()
}

/// A request signed by `key`, addressed to `server_pubkey`.
fn signed_request(
    key: &PrivateKey,
    server_pubkey: &eidetica::auth::crypto::PublicKey,
    tree_id: &ID,
    timestamp_ms: u64,
) -> SyncRequest {
    let our_tips: eidetica::Snapshot = vec![ID::from_bytes("nonexistent_tip")].into();
    let auth = SyncRequestAuth::sign(key, server_pubkey, tree_id, &our_tips, timestamp_ms);
    SyncRequest::SyncTree(SyncTreeRequest {
        tree_id: tree_id.clone(),
        our_tips,
        peer_pubkey: None,
        requesting_key: None,
        requesting_key_name: None,
        requested_permission: None,
        metadata: None,
        auth: Some(auth),
    })
}

/// Grant `pubkey` read access on the database.
async fn grant_read(database: &eidetica::Database, pubkey: &eidetica::auth::crypto::PublicKey) {
    let txn = database.new_transaction().await.unwrap();
    let settings = txn.get_settings().unwrap();
    settings
        .set_auth_key(pubkey, AuthKey::active(Some("client"), Permission::Read))
        .await
        .unwrap();
    txn.commit().await.unwrap();
}

/// A caller holding an authorized key still syncs normally.
#[tokio::test]
async fn authorized_signed_incremental_pull_is_served() {
    let (instance, _user, _key_id, database, sync, tree_id) =
        helpers::setup_manual_approval_server().await;
    write_secret(&database).await;

    let (client_key, client_pubkey) = generate_keypair();
    grant_read(&database, &client_pubkey).await;

    let handler = helpers::create_test_sync_handler(&sync);
    let response = handler
        .handle_request(
            &signed_request(&client_key, &instance.id(), &tree_id, now_ms()),
            &RequestContext::default(),
        )
        .await;

    assert!(
        discloses_secret(&response),
        "authorized peer was not served: {response:?}"
    );
}

/// A valid signature from a key with no grant on this tree proves identity,
/// not authority.
#[tokio::test]
async fn signed_pull_from_unauthorized_key_is_refused() {
    let (instance, _user, _key_id, database, sync, tree_id) =
        helpers::setup_manual_approval_server().await;
    write_secret(&database).await;

    let (stranger_key, _stranger_pubkey) = generate_keypair();

    let handler = helpers::create_test_sync_handler(&sync);
    let response = handler
        .handle_request(
            &signed_request(&stranger_key, &instance.id(), &tree_id, now_ms()),
            &RequestContext::default(),
        )
        .await;

    assert!(
        served_entries(&response).is_empty(),
        "a key with no grant was served: {response:?}"
    );
}

/// A captured request must not work a second time: without this, anyone who
/// observes one legitimate signed request on a plaintext transport can pull the
/// database at will.
#[tokio::test]
async fn replayed_signed_request_is_refused() {
    let (instance, _user, _key_id, database, sync, tree_id) =
        helpers::setup_manual_approval_server().await;
    write_secret(&database).await;

    let (client_key, client_pubkey) = generate_keypair();
    grant_read(&database, &client_pubkey).await;

    let handler = helpers::create_test_sync_handler(&sync);
    let request = signed_request(&client_key, &instance.id(), &tree_id, now_ms());

    let first = handler
        .handle_request(&request, &RequestContext::default())
        .await;
    assert!(
        discloses_secret(&first),
        "the original request should have been served: {first:?}"
    );

    let replay = handler
        .handle_request(&request, &RequestContext::default())
        .await;
    assert!(
        served_entries(&replay).is_empty(),
        "a replayed request was served: {replay:?}"
    );
}

/// Signatures age out, so a capture kept past the window is worthless even
/// against a server that has since restarted and forgotten the nonce.
#[tokio::test]
async fn stale_signed_request_is_refused() {
    let (instance, _user, _key_id, database, sync, tree_id) =
        helpers::setup_manual_approval_server().await;
    write_secret(&database).await;

    let (client_key, client_pubkey) = generate_keypair();
    grant_read(&database, &client_pubkey).await;

    let handler = helpers::create_test_sync_handler(&sync);
    let stale = now_ms() - 10 * 60 * 1000;
    let response = handler
        .handle_request(
            &signed_request(&client_key, &instance.id(), &tree_id, stale),
            &RequestContext::default(),
        )
        .await;

    assert!(
        served_entries(&response).is_empty(),
        "a stale request was served: {response:?}"
    );
}

/// The signature names the server it was made for, so a request captured en
/// route to one peer cannot be relayed to another that holds the same tree.
#[tokio::test]
async fn request_signed_for_another_server_is_refused() {
    let (instance, _user, _key_id, database, sync, tree_id) =
        helpers::setup_manual_approval_server().await;
    write_secret(&database).await;

    let (client_key, client_pubkey) = generate_keypair();
    grant_read(&database, &client_pubkey).await;
    let (_other_server_key, other_server_pubkey) = generate_keypair();

    let handler = helpers::create_test_sync_handler(&sync);
    let response = handler
        .handle_request(
            &signed_request(&client_key, &other_server_pubkey, &tree_id, now_ms()),
            &RequestContext::default(),
        )
        .await;

    assert!(
        served_entries(&response).is_empty(),
        "a request addressed to a different server was served: {response:?}"
    );
    assert_ne!(instance.id(), other_server_pubkey);
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
        .handle_request(
            &fabricated_tip_request(&tree_id),
            &RequestContext::default(),
        )
        .await;

    assert!(
        discloses_secret(&response),
        "probe cannot detect a positive"
    );
    assert!(
        served_entries(&response)
            .iter()
            .any(|entry| entry.data("_settings").is_ok()),
        "probe cannot detect _settings disclosure"
    );
}
