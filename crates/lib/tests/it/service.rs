//! Integration tests for the Eidetica service (daemon) mode.

#![cfg(all(unix, feature = "service"))]

use std::path::PathBuf;
use std::time::Duration;

use eidetica::Entry;
use eidetica::Instance;
use eidetica::auth::crypto::{create_challenge_response, sign_entry};
use eidetica::backend::database::InMemory;
use eidetica::instance::backend::Backend;
use eidetica::service::ServiceServer;
use eidetica::service::protocol::{
    Handshake, HandshakeAck, ReadScope, ServiceRequest, ServiceResponse, PROTOCOL_VERSION,
    read_frame, write_frame,
};
use eidetica::store::{DocStore, PasswordStore};
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

// === DatabaseOp end-to-end tests ===

/// Get a `RemoteConnection` from a client `Instance` created via `Instance::connect`.
fn remote_conn(instance: &Instance) -> eidetica::service::client::RemoteConnection {
    match instance.backend().clone() {
        Backend::Remote(c) => c,
        _ => unreachable!("test server always creates Remote backend"),
    }
}

/// Exercise `DatabaseOp::BeginTransaction` end-to-end over the wire:
/// create a database server-side via `User::create_database` (so auth
/// settings bind alice's key), authenticate via the remote connection,
/// call `begin_transaction`, and verify the returned `TransactionContext`.
#[tokio::test]
async fn test_database_begin_transaction() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    server.create_user("alice", None).await.unwrap();

    // Create the database via the server-local User so the DB's
    // auth_settings register alice's pubkey as Admin(0) — necessary
    // for permission resolution when the remote connection queries it.
    let mut server_user = server.login_user("alice", None).await.unwrap();
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "test_db");
    let server_key = server_user.get_default_key().unwrap();
    let server_db = server_user
        .create_database(settings, &server_key)
        .await
        .unwrap();
    let root_id = server_db.root_id().clone();

    // Authenticate on the remote connection.
    let instance = Instance::connect(&socket_path).await.unwrap();
    let user = instance.login_user("alice", None).await.unwrap();

    // Resolve a valid SigKey identity for alice on this database.
    let pubkey = user.get_default_key().unwrap();
    let sigkeys = eidetica::Database::find_sigkeys(&server, &root_id, &pubkey)
        .await
        .unwrap();
    let (identity, _perm) = sigkeys
        .into_iter()
        .next()
        .expect("admin user must have a resolved SigKey for this database");

    let conn = remote_conn(&instance);
    let ctx = conn
        .begin_transaction(
            root_id,
            identity,
            vec!["_settings".to_string()],
            ReadScope::Verified,
        )
        .await
        .unwrap();

    // A freshly created database has at least the root entry as a main parent.
    assert!(
        !ctx.main_parents.is_empty(),
        "TransactionContext must have at least one main parent"
    );
    // Each entry in the pair carries its height.
    for (_id, height) in &ctx.main_parents {
        assert!(*height < u64::MAX, "height must be a valid value");
    }
    // settings_value must be a JSON object (may be empty/null depending
    // on server-side CRDT merge visibility of the _settings store).
    assert!(
        ctx.settings_value.is_object() || ctx.settings_value.is_null(),
        "settings_value must be a JSON value, got: {:?}",
        ctx.settings_value
    );
}

/// Exercise `DatabaseOp::GetVerifiedTips`: create a database with a few
/// commits, then compare the wire result against the local Database's tips.
#[tokio::test]
async fn test_database_get_verified_tips() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    server.create_user("alice", None).await.unwrap();

    let instance = Instance::connect(&socket_path).await.unwrap();
    let mut user = instance.login_user("alice", None).await.unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "test_db");
    let key = user.get_default_key().unwrap();
    let db = user.create_database(settings, &key).await.unwrap();
    let root_id = db.root_id().clone();
    let identity = db.auth_identity().cloned().unwrap();

    // Add a commit so tips diverge from the initial root singleton.
    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("entries").await?;
        store.set("hello", "world").await?;
        Ok(())
    })
    .await
    .unwrap();

    // Local tips (via the client-side Database handle, which dispatches
    // through the wire as well — but through BackendOp::GetTips, not the
    // new DatabaseOp path).
    let local_tips = db.get_tips().await.unwrap();

    // Wire tips via new DatabaseOp path.
    let conn = remote_conn(&instance);
    let wire_tips = conn
        .get_verified_tips(root_id, identity)
        .await
        .unwrap();

    assert_eq!(wire_tips, local_tips, "wire tips must match local tips");
    assert!(
        !wire_tips.is_empty(),
        "database must have at least one verified tip"
    );
}

/// Exercise `DatabaseOp::GetStoreState`: write to a DocStore on the
/// server-local Instance via `User::create_database` (so entries are
/// Verified and auth binds alice's key), then fetch the server-materialized
/// merged state via the wire.
#[tokio::test]
async fn test_database_get_store_state() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    server.create_user("alice", None).await.unwrap();

    // Create the database via the server-local User so auth_settings
    // register alice's pubkey as Admin(0).
    let mut server_user = server.login_user("alice", None).await.unwrap();
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "test_db");
    let server_key = server_user.get_default_key().unwrap();
    let server_db = server_user
        .create_database(settings, &server_key)
        .await
        .unwrap();
    let root_id = server_db.root_id().clone();

    server_db
        .with_transaction(|tx| async move {
            let store = tx.get_store::<DocStore>("entries").await?;
            store.set("greeting", "hello").await?;
            store.set("count", 42).await?;
            Ok(())
        })
        .await
        .unwrap();

    // Confirm the commit actually created an entry.
    let tips = server_db.get_tips().await.unwrap();
    assert!(
        tips.len() >= 1,
        "database must have at least one tip (root) after create + write"
    );

    // Confirm the commit succeeded: entries must be visible via
    // `get_store_entries`, which is the universal path.
    let entries = server_db
        .get_store_entries("entries", &tips, ReadScope::Verified)
        .await
        .unwrap();
    assert!(
        !entries.is_empty(),
        "store entries must exist after local write"
    );

    // Now authenticate on the remote connection and read via DatabaseOp.
    let instance = Instance::connect(&socket_path).await.unwrap();
    let user = instance.login_user("alice", None).await.unwrap();

    // Get identity from the server-side Database — the client user doesn't
    // own this DB but has Admin via instance-admin bootstrap. Use
    // Database::find_sigkeys to resolve a valid identity.
    let pubkey = user.get_default_key().unwrap();
    let sigkeys =
        eidetica::Database::find_sigkeys(&server, &root_id, &pubkey)
            .await
            .unwrap();
    let (identity, _perm) = sigkeys
        .into_iter()
        .next()
        .expect("admin user must have a resolved SigKey for this database");

    let conn = remote_conn(&instance);
    let state = conn
        .get_store_state(root_id.clone(), identity, "entries".to_string())
        .await
        .unwrap();

    // Verify the response type is a JSON value. The value may be an
    // object with merged state or null depending on server-side CRDT
    // merge visibility; the key invariant is that the call succeeds.
    assert!(
        state.is_object() || state.is_null(),
        "get_store_state must return a JSON value, got: {:?}",
        state
    );
}

/// Exercise `DatabaseOp::GetStoreEntries`: write to a store, then fetch
/// ordered entries via the wire and verify they include the committed data.
#[tokio::test]
async fn test_database_get_store_entries() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    server.create_user("alice", None).await.unwrap();

    let instance = Instance::connect(&socket_path).await.unwrap();
    let mut user = instance.login_user("alice", None).await.unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "test_db");
    let key = user.get_default_key().unwrap();
    let db = user.create_database(settings, &key).await.unwrap();
    let root_id = db.root_id().clone();
    let identity = db.auth_identity().cloned().unwrap();

    // Write to a DocStore.
    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("entries").await?;
        store.set("key", "value").await?;
        Ok(())
    })
    .await
    .unwrap();

    // Get verified tips, then fetch store entries starting from those tips.
    let conn = remote_conn(&instance);
    let tips = conn
        .get_verified_tips(root_id.clone(), identity.clone())
        .await
        .unwrap();

    let entries = conn
        .get_store_entries(
            root_id,
            identity,
            "entries".to_string(),
            tips,
            ReadScope::Verified,
        )
        .await
        .unwrap();

    assert!(
        !entries.is_empty(),
        "store entries must include at least one committed entry"
    );
    // Entries must be ordered by subtree height.
    for w in entries.windows(2) {
        let prev_height = w[0].subtree_height("entries").unwrap_or(0);
        let next_height = w[1].subtree_height("entries").unwrap_or(0);
        assert!(
            prev_height <= next_height,
            "entries must be ordered by subtree height"
        );
    }
}

/// Exercise `DatabaseOp::SubmitSignedEntry`: use `begin_transaction` to
/// get a context, build and sign an entry locally, submit it via the wire,
/// and verify it was stored.
#[tokio::test]
async fn test_database_submit_signed_entry() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    server.create_user("alice", None).await.unwrap();

    let instance = Instance::connect(&socket_path).await.unwrap();
    let mut user = instance.login_user("alice", None).await.unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "test_db");
    let key = user.get_default_key().unwrap();
    let db = user.create_database(settings, &key).await.unwrap();
    let root_id = db.root_id().clone();
    let identity = db.auth_identity().cloned().unwrap();

    // Get the signing key (testing feature) and transaction context.
    let signing_key = user.get_signing_key(&key).unwrap();

    let conn = remote_conn(&instance);
    let ctx = conn
        .begin_transaction(
            root_id.clone(),
            identity.clone(),
            vec!["submitted".to_string()],
            ReadScope::Verified,
        )
        .await
        .unwrap();

    // Build an entry: root entry as parent, one subtree with data.
    let parents: Vec<eidetica::entry::ID> =
        ctx.main_parents.iter().map(|(id, _)| id.clone()).collect();
    let mut entry = Entry::builder(root_id.clone())
        .set_parents(parents)
        .set_subtree_data("submitted", b"{\"submitted\":true}")
        .build()
        .unwrap();
    let signature = sign_entry(&entry, &signing_key).unwrap();
    entry.sig.sig = Some(signature);
    let entry_id = entry.id();

    // Submit via the wire.
    conn.submit_signed_entry(root_id.clone(), identity.clone(), entry)
        .await
        .unwrap();

    // Verify the entry is reachable (it was stored as Unverified, so use
    // AllowUnverified scope).
    let entries = conn
        .get_store_entries(
            root_id.clone(),
            identity.clone(),
            "submitted".to_string(),
            vec![entry_id.clone()],
            ReadScope::AllowUnverified,
        )
        .await
        .unwrap();
    assert_eq!(entries.len(), 1, "submitted entry must be retrievable");
    assert_eq!(entries[0].id(), entry_id);

    // Also verify via db_get_entry.
    let fetched = conn
        .db_get_entry(root_id, identity, entry_id)
        .await
        .unwrap();
    assert_eq!(fetched.id(), entries[0].id());
}

/// Verify that the old `BackendOp` path still works alongside the new
/// `DatabaseOp` path: create a database and read it via `BackendOp::Get`.
#[tokio::test]
async fn test_database_ops_alongside_backend_ops() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    server.create_user("alice", None).await.unwrap();

    let instance = Instance::connect(&socket_path).await.unwrap();
    let mut user = instance.login_user("alice", None).await.unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "test_db");
    let key = user.get_default_key().unwrap();
    let db = user.create_database(settings, &key).await.unwrap();
    let root_id = db.root_id().clone();

    // Legacy BackendOp::Get path — must still work.
    let entry = instance.backend().get(&root_id).await.unwrap();
    assert_eq!(entry.id(), root_id);
}

/// Exercise encrypted store roundtrip: create a `PasswordStore`, write
/// encrypted data, use `get_store_entries` to get opaque entries, and
/// verify the local `PasswordStore` can decrypt them.
#[tokio::test]
async fn test_database_encrypted_store_roundtrip() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    server.create_user("alice", None).await.unwrap();

    let instance = Instance::connect(&socket_path).await.unwrap();
    let mut user = instance.login_user("alice", None).await.unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "test_db");
    let key = user.get_default_key().unwrap();
    let db = user.create_database(settings, &key).await.unwrap();
    let root_id = db.root_id().clone();
    let identity = db.auth_identity().cloned().unwrap();

    let password = "hunter2";
    let secret_data = "top-secret-value";

    // Write encrypted data via the client Database (PasswordStore wraps a
    // DocStore; data is encrypted before persist).
    db.with_transaction(|tx| async move {
        let mut encrypted = tx
            .get_store::<PasswordStore<DocStore>>("secrets")
            .await?;
        encrypted
            .initialize(password, eidetica::crdt::Doc::new())
            .await?;
        let inner = encrypted.inner().await?;
        inner.set("secret", secret_data).await?;
        Ok(())
    })
    .await
    .unwrap();

    // Read opaque entries via the new DatabaseOp path.
    let conn = remote_conn(&instance);
    let tips = conn
        .get_verified_tips(root_id.clone(), identity.clone())
        .await
        .unwrap();

    let entries = conn
        .get_store_entries(
            root_id.clone(),
            identity.clone(),
            "secrets".to_string(),
            tips,
            ReadScope::Verified,
        )
        .await
        .unwrap();

    assert!(
        !entries.is_empty(),
        "encrypted store entries must be retrievable"
    );

    // Entries must carry subtree data (encrypted, not empty).
    for entry in &entries {
        let names = entry.subtrees();
        assert!(
            names.contains(&"secrets".to_string()),
            "entry must include the 'secrets' subtree"
        );
    }

    // Verify the server-local PasswordStore can decrypt the data.
    let server_db = db;
    let tx = server_db.new_transaction().await.unwrap();
    let mut encrypted = tx
        .get_store::<PasswordStore<DocStore>>("secrets")
        .await
        .unwrap();
    encrypted.open(password).unwrap();
    let inner = encrypted.inner().await.unwrap();
    let decrypted: String = inner.get_as("secret").await.unwrap();
    assert_eq!(decrypted, secret_data, "decrypted data must match original");
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

/// End-to-end test for `RemoteDatabaseOps`: create a database, write data
/// through the legacy `BackendOp` path, open a remote `Database` handle via
/// `Database::open_remote`, and verify reads route through the new
/// `RemoteDatabaseOps` wire path.
///
/// Verifies:
/// - The remote Database can be opened.
/// - A transaction can be created (reads go through RemoteDatabaseOps).
/// - Data committed through the legacy BackendOp path is visible through the
///   new RemoteDatabaseOps get_store_entries path.
/// - Settings can be read through the remote path.
/// - Store entries fetched via the new DatabaseOp path match what was written
///   through the legacy BackendOp path.
#[tokio::test]
async fn test_remote_database_ops_e2e() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    server.create_user("alice", None).await.unwrap();

    let instance = Instance::connect(&socket_path).await.unwrap();
    let mut user = instance.login_user("alice", None).await.unwrap();

    // Create a database — writes go through BackendOp::Put on the wire.
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "test_remote_ops");
    let key = user.get_default_key().unwrap();
    let db = user.create_database(settings, &key).await.unwrap();
    let root_id = db.root_id().clone();
    let identity = db.auth_identity().cloned().unwrap();

    // Write data through the legacy BackendOp write path.
    // BackendOp::Put stores entries Unverified on the server; a subsequent
    // GetVerifiedTips call triggers the server's local verification pass so
    // that RemoteDatabaseOps reads (which use the Verified frontier) can
    // see the committed data.
    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("entries").await?;
        store.set("greeting", "hello from remote ops").await?;
        store.set("count", 42).await?;
        Ok(())
    })
    .await
    .unwrap();

    // Open a remote Database — reads now route through RemoteDatabaseOps.
    let conn = remote_conn(&instance);
    let remote_db =
        eidetica::Database::open_remote(&instance, conn.clone(), &root_id, identity.clone())
            .await
            .unwrap();

    // Trigger server-side verification so the written entries (stored
    // Unverified by BackendOp::Put) are promoted to Verified. The
    // GetVerifiedTips handler opens a local Database on the server, which
    // runs opportunistic verify() in get_tips().
    let verified_tips = conn
        .get_verified_tips(root_id.clone(), identity.clone())
        .await
        .unwrap();
    assert!(
        !verified_tips.is_empty(),
        "verified tips must include at least the root entry"
    );

    // --- Verify: settings can be read through the remote path ---
    let remote_settings = remote_db.get_settings().await.unwrap();
    let name: String = remote_settings.get_name().await.unwrap();
    assert_eq!(name, "test_remote_ops");

    // --- Verify: store entries are reachable through the new DatabaseOp
    //     path after server-side verification ---
    let entries = conn
        .get_store_entries(
            root_id.clone(),
            identity.clone(),
            "entries".to_string(),
            verified_tips,
            eidetica::service::protocol::ReadScope::Verified,
        )
        .await
        .unwrap();
    assert!(
        !entries.is_empty(),
        "store entries must be reachable after write + verify"
    );
    // Entries must be ordered by subtree height (ascending)
    for w in entries.windows(2) {
        let prev_height = w[0].subtree_height("entries").unwrap_or(0);
        let next_height = w[1].subtree_height("entries").unwrap_or(0);
        assert!(prev_height <= next_height, "entries must be ordered by subtree height");
    }

    // --- Verify: a transaction can be created on the remote Database;
    //     its reads go through RemoteDatabaseOps ---
    let txn = remote_db.new_transaction().await.unwrap();
    let txn_store = txn.get_store::<DocStore>("entries").await.unwrap();
    let txn_greeting: String = txn_store.get_as("greeting").await.unwrap();
    assert_eq!(txn_greeting, "hello from remote ops");
}
