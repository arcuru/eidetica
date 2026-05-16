//! Integration tests for the Eidetica service (daemon) mode.

#![cfg(all(unix, feature = "service"))]

use std::path::PathBuf;
use std::time::Duration;

use eidetica::Instance;
use eidetica::auth::crypto::create_challenge_response;
use eidetica::backend::database::InMemory;
use eidetica::service::ServiceServer;
use eidetica::service::protocol::{
    Handshake, HandshakeAck, PROTOCOL_VERSION, ServiceRequest, ServiceResponse, read_frame,
    write_frame,
};
use tempfile::TempDir;
use tokio::io::{ReadHalf, WriteHalf};
use tokio::net::UnixStream;
use tokio::sync::watch;

/// Start a test server with InMemory backend; returns (path, shutdown, server-side
/// Instance, tempdir guard).
///
/// The tempdir is returned so the socket directory is cleaned up when the caller
/// goes out of scope; the server-side Instance is returned so tests can observe
/// state both locally and over the wire.
async fn start_test_server() -> (PathBuf, watch::Sender<()>, Instance, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("test.sock");
    let instance = Instance::open(Box::new(InMemory::new())).await.unwrap();
    let (tx, rx) = watch::channel(());
    let server = ServiceServer::new(instance.clone(), socket_path.clone());
    tokio::spawn(async move {
        let _ = server.run(rx).await;
    });
    // Wait for the socket to appear (server binds asynchronously).
    for _ in 0..50 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (socket_path, tx, instance, dir)
}

#[tokio::test]
async fn test_connect_and_create_instance() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let _instance = Instance::connect(&socket_path).await.unwrap();
    // The wire surface no longer carries `ListUsers`; user enumeration runs on
    // the server-local Instance (an admin reading `_users`). Smoke-check that
    // the handshake landed and the daemon is responsive: no users exist yet.
    let users = server.list_users().await.unwrap();
    assert!(users.is_empty());
}

#[tokio::test]
async fn test_user_lifecycle() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    // User creation runs against the server-local Instance (admin-only),
    // mirroring the production "admin edits `_users`" flow. The wire client
    // then drives login + per-user operations.
    server.create_user("alice", None).await.unwrap();
    let instance = Instance::connect(&socket_path).await.unwrap();

    // Login
    let mut user = instance.login_user("alice", None).await.unwrap();
    assert_eq!(user.username(), "alice");

    // Create a database
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "test_db");
    let default_key = user.get_default_key().unwrap();
    let db = user.create_database(settings, &default_key).await.unwrap();

    // Verify database exists
    let tracked = user.databases().await.unwrap();
    assert_eq!(tracked.len(), 1);
    assert_eq!(tracked[0].database_id, *db.root_id());
}

#[tokio::test]
async fn test_error_propagation() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    // Backend ops over the wire are gated on a TrustedLogin'd connection, so
    // create + login a user before exercising the error path.
    server.create_user("err-test", None).await.unwrap();
    let instance = Instance::connect(&socket_path).await.unwrap();
    let _user = instance.login_user("err-test", None).await.unwrap();

    // Try to get a nonexistent entry — surfaces the server's NotFound through
    // the wire's `ServiceResponse::Error` round-trip.
    let result = instance
        .backend()
        .get(&eidetica::entry::ID::from_bytes("nonexistent"))
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().is_not_found());
}

#[tokio::test]
async fn test_unauthenticated_backend_op_rejected() {
    let (socket_path, _tx, _server, _dir) = start_test_server().await;
    let instance = Instance::connect(&socket_path).await.unwrap();

    // No login; any Authenticated-wrapped backend op must be rejected.
    let result = instance
        .backend()
        .get(&eidetica::entry::ID::from_bytes("nonexistent"))
        .await;
    let err = result.expect_err("server must reject backend op on unauthenticated connection");
    assert!(
        !err.is_not_found(),
        "expected auth error, got NotFound — gate not enforced; {err}"
    );
}

#[tokio::test]
async fn test_concurrent_clients() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    server.create_user("bob", None).await.unwrap();

    // Two clients connect to the same daemon and complete login concurrently.
    let instance1 = Instance::connect(&socket_path).await.unwrap();
    let instance2 = Instance::connect(&socket_path).await.unwrap();

    let _user1 = instance1.login_user("bob", None).await.unwrap();
    let user2 = instance2.login_user("bob", None).await.unwrap();
    assert_eq!(user2.username(), "bob");
}

#[tokio::test]
async fn test_instance_connect_convenience() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    server.create_user("charlie", None).await.unwrap();

    let _instance = Instance::connect(&socket_path).await.unwrap();
    // ListUsers is no longer on the wire surface; verify via the server-local
    // Instance that the user we just created is visible.
    let users = server.list_users().await.unwrap();
    assert_eq!(users, vec!["charlie"]);
}

#[tokio::test]
async fn test_instance_identity_round_trip() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let client = Instance::connect(&socket_path).await.unwrap();

    // The metadata fetched at Instance::connect() handshake must report the same
    // instance identity (server's device public key) as the local Instance.
    assert_eq!(client.id(), server.id());
}

/// Open a raw connection to the daemon and complete the protocol handshake.
///
/// Returns the read + write halves of the stream so tests can drive the
/// TrustedLogin* flow before the Instance::login_user_remote API lands (chunk 4).
async fn raw_handshake(socket_path: &PathBuf) -> (ReadHalf<UnixStream>, WriteHalf<UnixStream>) {
    let stream = UnixStream::connect(socket_path).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(stream);
    write_frame(
        &mut writer,
        &Handshake {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await
    .unwrap();
    let _ack: HandshakeAck = read_frame(&mut reader).await.unwrap().unwrap();
    (reader, writer)
}

#[tokio::test]
async fn test_trusted_login_challenge_response_round_trip() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;

    // Set up alice on the server's Instance directly so we have the signing key
    // for the test (Instance::login_user_remote lands in chunk 4).
    server.create_user("alice", None).await.unwrap();
    let alice = server.login_user("alice", None).await.unwrap();
    let alice_pubkey = alice.get_default_key().unwrap();
    let alice_signing_key = alice.get_signing_key(&alice_pubkey).unwrap();

    let (mut reader, mut writer) = raw_handshake(&socket_path).await;

    // Step 1: TrustedLoginUser → expect a non-empty challenge.
    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginUser {
            username: "alice".to_string(),
        },
    )
    .await
    .unwrap();
    let resp: ServiceResponse = read_frame(&mut reader).await.unwrap().unwrap();
    let challenge = match resp {
        ServiceResponse::TrustedLoginChallenge { challenge, .. } => challenge,
        other => panic!("expected TrustedLoginChallenge, got {other:?}"),
    };
    assert_eq!(challenge.len(), 32, "challenge must be 32 random bytes");

    // Step 2: sign the challenge with alice's private key and send TrustedLoginProve.
    let signature = create_challenge_response(&challenge, &alice_signing_key);
    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginProve { signature },
    )
    .await
    .unwrap();
    let resp: ServiceResponse = read_frame(&mut reader).await.unwrap().unwrap();
    assert!(matches!(resp, ServiceResponse::TrustedLoginOk));
}

#[tokio::test]
async fn test_trusted_login_unknown_user_errors() {
    let (socket_path, _tx, _server, _dir) = start_test_server().await;
    let (mut reader, mut writer) = raw_handshake(&socket_path).await;

    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginUser {
            username: "ghost".to_string(),
        },
    )
    .await
    .unwrap();
    let resp: ServiceResponse = read_frame(&mut reader).await.unwrap().unwrap();
    match resp {
        ServiceResponse::Error(e) => {
            // The error originates from UserError::UserNotFound; we don't assert
            // the exact kind string to avoid coupling to wire-format details.
            assert!(
                e.message.contains("ghost") || e.kind.contains("NotFound"),
                "expected user-not-found-ish error, got {e:?}"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn test_trusted_login_prove_without_user_errors() {
    let (socket_path, _tx, _server, _dir) = start_test_server().await;
    let (mut reader, mut writer) = raw_handshake(&socket_path).await;

    // No prior TrustedLoginUser — server should reject.
    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginProve {
            signature: vec![0u8; 64],
        },
    )
    .await
    .unwrap();
    let resp: ServiceResponse = read_frame(&mut reader).await.unwrap().unwrap();
    assert!(matches!(resp, ServiceResponse::Error(_)));
}

/// Chunk-6 per-user cache isolation, end-to-end over the wire.
///
/// Two authenticated connections (alice and bob) on the same daemon, hitting
/// the same `(entry_id, store)` slot. The service-layer cache namespaces by
/// session `user_uuid`, so:
///   - bob's read sees nothing of alice's write
///   - bob's write doesn't poison alice's slot
///   - each user's `ClearCrdtCache` only affects their own slice
#[tokio::test]
async fn test_crdt_cache_is_per_user() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    server.create_user("alice", None).await.unwrap();
    server.create_user("bob", None).await.unwrap();

    let alice_inst = Instance::connect(&socket_path).await.unwrap();
    let _alice = alice_inst.login_user("alice", None).await.unwrap();

    let bob_inst = Instance::connect(&socket_path).await.unwrap();
    let _bob = bob_inst.login_user("bob", None).await.unwrap();

    let entry_id = eidetica::entry::ID::from_bytes("cache-isolation-entry");
    let store = "test_store";

    // Alice writes; bob's slot for the same key must still be empty.
    alice_inst
        .backend()
        .cache_crdt_state(&entry_id, store, b"alice-bytes".to_vec())
        .await
        .unwrap();
    assert_eq!(
        bob_inst
            .backend()
            .get_cached_crdt_state(&entry_id, store)
            .await
            .unwrap(),
        None,
        "bob must not see alice's cache slot",
    );

    // Bob writes different bytes; alice's slot must be unchanged.
    bob_inst
        .backend()
        .cache_crdt_state(&entry_id, store, b"bob-bytes".to_vec())
        .await
        .unwrap();
    assert_eq!(
        alice_inst
            .backend()
            .get_cached_crdt_state(&entry_id, store)
            .await
            .unwrap(),
        Some(b"alice-bytes".to_vec()),
        "bob's write must not poison alice's cache slot",
    );
    assert_eq!(
        bob_inst
            .backend()
            .get_cached_crdt_state(&entry_id, store)
            .await
            .unwrap(),
        Some(b"bob-bytes".to_vec()),
    );
    // Note: per-user cache *clearing* (`clear_user`) is covered by the
    // `cache::tests` unit tests; `ClearCrdtCache` is intentionally no longer
    // a wire op, so it isn't exercised here.
}

/// Positive control for the chunk-5b per-tree gate: a logged-in user can
/// read tree-scoped data on a database where they hold Admin/Write/Read.
///
/// `Database::create` registers the user's pubkey as Admin(0), so the gate
/// resolves to Admin and the read passes. A regression that broke the gate
/// for legitimate access would surface here as a `PermissionDenied`.
#[tokio::test]
async fn test_backend_get_tips_allowed_for_owner() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    server.create_user("alice", None).await.unwrap();
    let instance = Instance::connect(&socket_path).await.unwrap();
    let mut alice = instance.login_user("alice", None).await.unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "alice_db");
    let key = alice.get_default_key().unwrap();
    let db = alice.create_database(settings, &key).await.unwrap();

    let tips = instance.backend().get_tips(db.root_id()).await.unwrap();
    assert!(
        !tips.is_empty(),
        "newly created database must have at least one tip"
    );
}

/// Negative control for the chunk-5b per-tree gate: a logged-in user without
/// any key registered in a tree's auth_settings is rejected with a
/// permission-denied-shaped error rather than getting the data.
///
/// This is the load-bearing behaviour for a shared-daemon deployment where
/// multiple users authenticate against the same socket.
#[tokio::test]
async fn test_backend_get_tips_denied_for_unauthorised_user() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    server.create_user("alice", None).await.unwrap();
    server.create_user("bob", None).await.unwrap();

    // Alice creates a database via her own authenticated connection.
    let alice_inst = Instance::connect(&socket_path).await.unwrap();
    let mut alice = alice_inst.login_user("alice", None).await.unwrap();
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "alice_db");
    let alice_key = alice.get_default_key().unwrap();
    let alice_db = alice.create_database(settings, &alice_key).await.unwrap();
    let alice_db_id = alice_db.root_id().clone();

    // Bob authenticates on a separate connection.
    let bob_inst = Instance::connect(&socket_path).await.unwrap();
    let _bob = bob_inst.login_user("bob", None).await.unwrap();

    // Bob asks for tips on alice's database. The chunk-5b gate must reject —
    // bob's pubkey is not in alice_db's auth_settings, so
    // `resolve_identity_permission` fails and the gate normalises the failure
    // to PermissionDenied.
    let err = bob_inst
        .backend()
        .get_tips(&alice_db_id)
        .await
        .expect_err("server must reject GetTips for a user not in the tree's auth_settings");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("permission") || msg.contains("auth"),
        "expected permission/auth error, got: {err}",
    );
}

/// D2: `BackendOp::Get` carries no inline tree id, so the pre-dispatch
/// per-tree gate never runs. The post-fetch `gate_entry_read` must resolve
/// the entry's real owning tree and reject a logged-in caller with no
/// permission on it — otherwise any user can pull any entry on the daemon
/// by id (model B: system DBs are gate-protected, not encrypted).
#[tokio::test]
async fn test_backend_get_denied_cross_tree() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    server.create_user("alice", None).await.unwrap();
    server.create_user("bob", None).await.unwrap();

    let alice_inst = Instance::connect(&socket_path).await.unwrap();
    let mut alice = alice_inst.login_user("alice", None).await.unwrap();
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "alice_db");
    let alice_key = alice.get_default_key().unwrap();
    let alice_db = alice.create_database(settings, &alice_key).await.unwrap();
    let alice_root = alice_db.root_id().clone();

    // Owner can Get an entry in her own tree (positive control).
    alice_inst
        .backend()
        .get(&alice_root)
        .await
        .expect("owner must be able to Get an entry in her own tree");

    // Bob is logged in but holds no key in alice_db's auth_settings.
    let bob_inst = Instance::connect(&socket_path).await.unwrap();
    let _bob = bob_inst.login_user("bob", None).await.unwrap();

    let err = bob_inst
        .backend()
        .get(&alice_root)
        .await
        .expect_err("Get must be gated on the fetched entry's owning tree");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("permission") || msg.contains("auth"),
        "expected permission/auth denial, got: {err}",
    );
}

/// D5: `GetPathFromTo`'s `tree_id` is gated, but `to_ids` are caller-chosen
/// and a foreign target is echoed back verbatim in the path result. The
/// `ensure_entries_in_tree` check must reject a `to_id` not in the gated
/// tree while still accepting an in-tree target.
#[tokio::test]
async fn test_get_path_from_to_rejects_foreign_target() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    server.create_user("alice", None).await.unwrap();
    server.create_user("bob", None).await.unwrap();

    let alice_inst = Instance::connect(&socket_path).await.unwrap();
    let mut alice = alice_inst.login_user("alice", None).await.unwrap();
    let mut a_settings = eidetica::crdt::Doc::new();
    a_settings.set("name", "alice_db");
    let a_key = alice.get_default_key().unwrap();
    let alice_db = alice.create_database(a_settings, &a_key).await.unwrap();
    let alice_root = alice_db.root_id().clone();

    // Bob is Admin on his own db, so the per-tree gate on it passes.
    let bob_inst = Instance::connect(&socket_path).await.unwrap();
    let mut bob = bob_inst.login_user("bob", None).await.unwrap();
    let mut b_settings = eidetica::crdt::Doc::new();
    b_settings.set("name", "bob_db");
    let b_key = bob.get_default_key().unwrap();
    let bob_db = bob.create_database(b_settings, &b_key).await.unwrap();
    let bob_root = bob_db.root_id().clone();

    // An in-tree target on his own tree is accepted (positive control).
    bob_inst
        .backend()
        .get_path_from_to(
            &bob_root,
            "_settings",
            &eidetica::entry::ID::default(),
            std::slice::from_ref(&bob_root),
        )
        .await
        .expect("an in-tree to_id must be accepted");

    // Alice's root as a target on Bob's tree must be rejected (not echoed).
    let err = bob_inst
        .backend()
        .get_path_from_to(
            &bob_root,
            "_settings",
            &eidetica::entry::ID::default(),
            std::slice::from_ref(&alice_root),
        )
        .await
        .expect_err("a foreign to_id must not be echoed back");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("permission") || msg.contains("auth"),
        "expected permission/auth denial, got: {err}",
    );
}

/// `SetInstanceMetadata` rewrites the daemon's pointers to its own system DBs,
/// so the daemon now requires `Admin` on `_databases`. The first user on a
/// device is auto-promoted to that role by the instance-admin bootstrap, so
/// they can drive metadata writes through the wire.
#[tokio::test]
async fn test_set_instance_metadata_allowed_for_admin() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    server.create_user("admin", None).await.unwrap();

    let instance = Instance::connect(&socket_path).await.unwrap();
    let _admin = instance.login_user("admin", None).await.unwrap();

    let current = instance
        .backend()
        .get_instance_metadata()
        .await
        .unwrap()
        .expect("daemon must already have an InstanceMetadata record");
    instance
        .backend()
        .set_instance_metadata(&current)
        .await
        .expect("admin must be able to write back the existing metadata");
}

/// Negative control for the `SetInstanceMetadata` admin gate: a second user
/// (no `Admin` on `_databases`) is rejected with a permission error rather
/// than silently rewriting the daemon's system-DB pointers.
#[tokio::test]
async fn test_set_instance_metadata_denied_for_non_admin() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    // First user is auto-promoted to instance admin; second user is not.
    server.create_user("admin", None).await.unwrap();
    server.create_user("bob", None).await.unwrap();

    let bob_inst = Instance::connect(&socket_path).await.unwrap();
    let _bob = bob_inst.login_user("bob", None).await.unwrap();

    let current = bob_inst
        .backend()
        .get_instance_metadata()
        .await
        .unwrap()
        .expect("daemon must already have an InstanceMetadata record");
    let err = bob_inst
        .backend()
        .set_instance_metadata(&current)
        .await
        .expect_err("non-admin must be rejected");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("permission") || msg.contains("auth"),
        "expected permission/auth error, got: {err}",
    );
}

#[tokio::test]
async fn test_trusted_login_bad_signature_errors_and_resets() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    server.create_user("bob", None).await.unwrap();

    let (mut reader, mut writer) = raw_handshake(&socket_path).await;

    // Get a challenge.
    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginUser {
            username: "bob".to_string(),
        },
    )
    .await
    .unwrap();
    let resp: ServiceResponse = read_frame(&mut reader).await.unwrap().unwrap();
    assert!(matches!(
        resp,
        ServiceResponse::TrustedLoginChallenge { .. }
    ));

    // Send a junk signature — server must reject and reset to PreAuth.
    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginProve {
            signature: vec![0xAB; 64],
        },
    )
    .await
    .unwrap();
    let resp: ServiceResponse = read_frame(&mut reader).await.unwrap().unwrap();
    assert!(matches!(resp, ServiceResponse::Error(_)));

    // Confirm reset: a second TrustedLoginProve without a fresh TrustedLoginUser must error
    // (not silently succeed against the previous challenge).
    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginProve {
            signature: vec![0xCD; 64],
        },
    )
    .await
    .unwrap();
    let resp: ServiceResponse = read_frame(&mut reader).await.unwrap().unwrap();
    assert!(matches!(resp, ServiceResponse::Error(_)));
}
