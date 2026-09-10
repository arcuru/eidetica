//! Integration tests for the Eidetica service (daemon) mode.

#![cfg(all(unix, feature = "service"))]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use eidetica::Entry;
use eidetica::Instance;
use eidetica::auth::crypto::{create_challenge_response, generate_keypair, sign_entry};
use eidetica::backend::database::InMemory;
use eidetica::backend::{ProjectionDescriptor, StoreStateRequest};
use eidetica::crdt::Doc;
use eidetica::service::ServiceServer;
use eidetica::service::protocol::{
    Handshake, HandshakeAck, PROTOCOL_VERSION, ReadScope, ServerFrame, ServiceRequest,
    ServiceResponse, read_frame, write_frame,
};
use eidetica::store::{DocStore, PasswordStore, Table};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::UnixStream;
use tokio::sync::watch;

/// Read the next server frame and unwrap it as a `ServiceResponse`. Tests
/// that drive the server at the raw protocol layer don't subscribe to
/// notifications, so an interleaved `Notification` would be a real bug —
/// hence the panic on the wrong variant.
async fn read_response<R: AsyncRead + Unpin>(reader: &mut R) -> ServiceResponse {
    let frame: ServerFrame = read_frame(reader).await.unwrap().unwrap();
    match frame {
        ServerFrame::Response(resp) => *resp,
        ServerFrame::Notification(n) => panic!("unexpected notification frame: {n:?}"),
    }
}

/// Start a test server with InMemory backend; returns (path, shutdown, server-side
/// Instance, tempdir guard).
///
/// The tempdir is returned so the socket directory is cleaned up when the caller
/// goes out of scope; the server-side Instance is returned so tests can observe
/// state both locally and over the wire.
async fn start_test_server() -> (PathBuf, watch::Sender<()>, Instance, TempDir) {
    start_test_server_with_token_ttl(Duration::from_secs(5 * 60)).await
}

/// Same as [`start_test_server`], with the session-token idle lifetime under
/// the caller's control so expiry is observable without a long wait.
async fn start_test_server_with_token_ttl(
    ttl: Duration,
) -> (PathBuf, watch::Sender<()>, Instance, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("test.sock");
    let (instance, _admin) = Instance::create_backend(
        Box::new(InMemory::new()),
        eidetica::NewUser::passwordless("admin"),
    )
    .await
    .unwrap();
    let (tx, rx) = watch::channel(());
    let server =
        ServiceServer::new(instance.clone(), socket_path.clone()).with_token_idle_ttl_for_test(ttl);
    let server = server.bind().await.unwrap();
    tokio::spawn(server.run(rx));
    (socket_path, tx, instance, dir)
}

/// Helper: log in as the bootstrap admin and create a user server-side.
async fn create_user_via_admin(server: &Instance, username: &str) {
    crate::helpers::create_user(server, username, None)
        .await
        .unwrap();
}

/// Helper: with the admin bootstrapped at instance creation, log in as admin,
/// create a test user, connect and authenticate as that user, create a
/// database, and return (client-instance, root_id, identity).
///
/// The database is created server-side so auth_settings bind the user's
/// key as Admin(0). The client authenticates via the remote connection.
async fn setup_db(
    server: &Instance,
    socket_path: &std::path::Path,
    username: &str,
) -> (Instance, eidetica::entry::ID, eidetica::auth::types::SigKey) {
    // Admin was created by Instance::open_backend bootstrap — use it to create the test user.
    crate::helpers::create_user(server, username, None)
        .await
        .unwrap();

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let user = instance.login_user(username, None).await.unwrap();
    let pubkey = user.get_default_key().unwrap();

    // Create db server-side
    let mut server_user = server.login_user(username, None).await.unwrap();
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", format!("{username}_db"));
    let server_key = server_user.get_default_key().unwrap();
    let db = server_user
        .create_database(settings, &server_key)
        .await
        .unwrap();
    let root_id = db.root_id().clone();

    let sigkeys = eidetica::Database::find_sigkeys(server, &root_id, &pubkey)
        .await
        .unwrap();
    let (identity, _perm) = sigkeys
        .into_iter()
        .next()
        .expect("user must have a resolved SigKey for this database");

    (instance, root_id, identity)
}

#[tokio::test]
async fn test_connect_and_create_instance() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let _instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let users = crate::helpers::list_users(&server).await.unwrap();
    // Admin user bootstrapped at Instance creation
    assert_eq!(users.len(), 1);
    assert_eq!(users[0], "admin");
}

#[tokio::test]
async fn test_user_lifecycle() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    // Admin is bootstrapped — use it to create test user
    crate::helpers::create_user(&server, "alice", None)
        .await
        .unwrap();

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();

    let user = instance.login_user("alice", None).await.unwrap();
    assert_eq!(user.username(), "alice");

    // Create a database server-side
    let mut server_user = server.login_user("alice", None).await.unwrap();
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "test_db");
    let default_key = server_user.get_default_key().unwrap();
    let db = server_user
        .create_database(settings, &default_key)
        .await
        .unwrap();

    // Verify database exists
    let _tracked = user.databases().await.unwrap();
    assert!(!db.root_id().is_empty());
}

#[tokio::test]
async fn test_error_propagation() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "err-test").await;
    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let user = instance.login_user("err-test", None).await.unwrap();

    let pubkey = user.get_default_key().unwrap();
    let root_id = eidetica::ID::from_bytes("nonexistent-db");
    let conn = remote_conn(&instance);
    let identity = eidetica::auth::types::SigKey::from_pubkey(&pubkey);
    let result = conn
        .db_get_entry(
            root_id,
            identity,
            eidetica::entry::ID::from_bytes("nonexistent"),
        )
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().is_not_found());
}

#[tokio::test]
async fn test_unauthenticated_backend_op_rejected() {
    let (socket_path, _tx, _server, _dir) = start_test_server().await;
    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();

    let conn = remote_conn(&instance);
    let result = conn
        .db_get_entry(
            eidetica::entry::ID::default(),
            eidetica::auth::types::SigKey::default(),
            eidetica::entry::ID::from_bytes("nonexistent"),
        )
        .await;
    let err = result.expect_err("server must reject database op on unauthenticated connection");
    assert!(
        !err.is_not_found(),
        "expected auth error, got NotFound — gate not enforced; {err}"
    );
}

#[tokio::test]
async fn test_concurrent_clients() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "bob").await;

    let instance1 = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let instance2 = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();

    let _user1 = instance1.login_user("bob", None).await.unwrap();
    let user2 = instance2.login_user("bob", None).await.unwrap();
    assert_eq!(user2.username(), "bob");
}

#[tokio::test]
async fn test_instance_connect_convenience() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "charlie").await;

    let _instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut users = crate::helpers::list_users(&server).await.unwrap();
    // `list_users` returns users in UUID order (random per run), so sort
    // before comparing — the set is what matters, not iteration order.
    users.sort();
    assert_eq!(users, vec!["admin", "charlie"]);
}

#[tokio::test]
async fn test_instance_identity_round_trip() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let client = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();

    assert_eq!(client.id(), server.id());
}

/// Open a raw connection to the daemon and complete the protocol handshake.
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

    create_user_via_admin(&server, "alice").await;
    let alice = server.login_user("alice", None).await.unwrap();
    let alice_pubkey = alice.get_default_key().unwrap();
    let alice_signing_key = alice.get_signing_key(&alice_pubkey).unwrap();

    let (mut reader, mut writer) = raw_handshake(&socket_path).await;

    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginUser {
            username: "alice".to_string(),
        },
    )
    .await
    .unwrap();
    let resp: ServiceResponse = read_response(&mut reader).await;
    let challenge = match resp {
        ServiceResponse::TrustedLoginChallenge { challenge, .. } => challenge,
        other => panic!("expected TrustedLoginChallenge, got {other:?}"),
    };
    assert_eq!(challenge.len(), 32, "challenge must be 32 random bytes");

    let signature = create_challenge_response(&challenge, &alice_signing_key);
    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginProve { signature },
    )
    .await
    .unwrap();
    let resp: ServiceResponse = read_response(&mut reader).await;
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
    let resp: ServiceResponse = read_response(&mut reader).await;
    match resp {
        ServiceResponse::Error(e) => {
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

    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginProve {
            signature: vec![0u8; 64],
        },
    )
    .await
    .unwrap();
    let resp: ServiceResponse = read_response(&mut reader).await;
    assert!(matches!(resp, ServiceResponse::Error(_)));
}

// === DatabaseOp end-to-end tests ===

/// Get a `RemoteConnection` from a client `Instance` created via `Instance::connect`.
fn remote_conn(instance: &Instance) -> eidetica::service::client::RemoteConnection {
    instance
        .remote_connection()
        .expect("test server always creates Remote backend")
}

/// Exercise `DatabaseOp::BeginTransaction` end-to-end over the wire.
#[tokio::test]
async fn test_database_begin_transaction() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    let mut server_user = server.login_user("alice", None).await.unwrap();
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "test_db");
    let server_key = server_user.get_default_key().unwrap();
    let server_db = server_user
        .create_database(settings, &server_key)
        .await
        .unwrap();
    let root_id = server_db.root_id().clone();

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let user = instance.login_user("alice", None).await.unwrap();

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

    assert!(
        !ctx.main_parents.is_empty(),
        "TransactionContext must have at least one main parent"
    );
    for (_id, height) in &ctx.main_parents {
        assert!(*height < u64::MAX, "height must be a valid value");
    }
    assert!(
        ctx.settings_value.is_object() || ctx.settings_value.is_null(),
        "settings_value must be a JSON value, got: {:?}",
        ctx.settings_value
    );
}

/// Exercise `DatabaseOp::GetVerifiedTips`.
#[tokio::test]
async fn test_database_get_verified_tips() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (instance, root_id, identity) = setup_db(&server, &socket_path, "alice").await;

    // Add a commit server-side so tips diverge.
    let server_user = server.login_user("alice", None).await.unwrap();
    let server_key_pub = server_user.get_default_key().unwrap();
    let server_sk = server_user.get_signing_key(&server_key_pub).unwrap();
    let db = eidetica::Database::open(&server, &root_id)
        .await
        .unwrap()
        .with_key(server_sk);
    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("entries").await?;
        store.set("hello", "world").await?;
        Ok(())
    })
    .await
    .unwrap();

    let conn = remote_conn(&instance);
    let wire_tips = conn.get_verified_tips(root_id, identity).await.unwrap();

    assert!(
        !wire_tips.is_empty(),
        "database must have at least one verified tip"
    );
}

/// Exercise `DatabaseOp::GetStoreState`.
#[tokio::test]
async fn test_database_get_store_state() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

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

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let user = instance.login_user("alice", None).await.unwrap();

    let pubkey = user.get_default_key().unwrap();
    let sigkeys = eidetica::Database::find_sigkeys(&server, &root_id, &pubkey)
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

    assert!(
        state.is_object() || state.is_null(),
        "get_store_state must return a JSON value, got: {:?}",
        state
    );
}

/// Exercise `DatabaseOp::GetStoreEntries`.
#[tokio::test]
async fn test_database_get_store_entries() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (instance, root_id, identity) = setup_db(&server, &socket_path, "alice").await;

    // Write data server-side.
    let server_user = server.login_user("alice", None).await.unwrap();
    let server_key_pub = server_user.get_default_key().unwrap();
    let server_sk = server_user.get_signing_key(&server_key_pub).unwrap();
    let db = eidetica::Database::open(&server, &root_id)
        .await
        .unwrap()
        .with_key(server_sk);
    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("entries").await?;
        store.set("key", "value").await?;
        Ok(())
    })
    .await
    .unwrap();

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
            tips.into_tips(),
            ReadScope::Verified,
        )
        .await
        .unwrap();

    assert!(
        !entries.is_empty(),
        "store entries must include at least one committed entry"
    );
    for w in entries.windows(2) {
        let prev_height = w[0].subtree_height("entries").unwrap_or(0);
        let next_height = w[1].subtree_height("entries").unwrap_or(0);
        assert!(
            prev_height <= next_height,
            "entries must be ordered by subtree height"
        );
    }
}

/// Exercise `DatabaseOp::SubmitSignedEntry`.
#[tokio::test]
async fn test_database_submit_signed_entry() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (instance, root_id, identity) = setup_db(&server, &socket_path, "alice").await;

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

    let user = instance.login_user("alice", None).await.unwrap();
    let key = user.get_default_key().unwrap();
    let signing_key = user.get_signing_key(&key).unwrap();

    let parents: Vec<eidetica::entry::ID> =
        ctx.main_parents.iter().map(|(id, _)| id.clone()).collect();
    let entry = Entry::builder(root_id.clone())
        .set_parents(parents)
        .set_subtree_data("submitted", b"{\"submitted\":true}")
        .build()
        .unwrap();
    let signature = sign_entry(&entry, &signing_key).unwrap();
    let entry = entry.with_auth(|auth| auth.signature = Some(signature));
    let entry_id = entry.id();

    conn.submit_signed_entry(root_id.clone(), identity.clone(), entry)
        .await
        .unwrap();

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

    let fetched = conn
        .db_get_entry(root_id, identity, entry_id)
        .await
        .unwrap();
    assert_eq!(fetched.id(), entries[0].id());
}

/// Exercise encrypted store roundtrip.
#[tokio::test]
async fn test_database_encrypted_store_roundtrip() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (instance, root_id, identity) = setup_db(&server, &socket_path, "alice").await;

    let password = "hunter2";
    let secret_data = "top-secret-value";

    // Write encrypted data server-side.
    let server_user = server.login_user("alice", None).await.unwrap();
    let server_key_pub = server_user.get_default_key().unwrap();
    let server_sk = server_user.get_signing_key(&server_key_pub).unwrap();
    let db = eidetica::Database::open(&server, &root_id)
        .await
        .unwrap()
        .with_key(server_sk);
    db.with_transaction(|tx| async move {
        let mut encrypted = tx.get_store::<PasswordStore<DocStore>>("secrets").await?;
        encrypted
            .initialize(password, eidetica::crdt::Doc::new())
            .await?;
        let inner = encrypted.inner().await?;
        inner.set("secret", secret_data).await?;
        Ok(())
    })
    .await
    .unwrap();

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
            tips.into_tips(),
            ReadScope::Verified,
        )
        .await
        .unwrap();

    assert!(
        !entries.is_empty(),
        "encrypted store entries must be retrievable"
    );

    for entry in &entries {
        let names = entry.subtrees();
        assert!(
            names.contains(&"secrets".to_string()),
            "entry must include the 'secrets' subtree"
        );
    }

    // Verify local decrypt works.
    let tx = db.new_transaction().await.unwrap();
    let mut encrypted = tx
        .get_store::<PasswordStore<DocStore>>("secrets")
        .await
        .unwrap();
    encrypted.open(password).unwrap();
    let inner = encrypted.inner().await.unwrap();
    let decrypted: String = inner.get_as("secret").await.unwrap();
    assert_eq!(decrypted, secret_data, "decrypted data must match original");
}

/// Positive control: owner can read via `get_verified_tips`.
#[tokio::test]
async fn test_backend_snapshot_allowed_for_owner() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (instance, root_id, identity) = setup_db(&server, &socket_path, "alice").await;

    let conn = remote_conn(&instance);
    let tips = conn.get_verified_tips(root_id, identity).await.unwrap();
    assert!(
        !tips.is_empty(),
        "newly created database must have at least one tip"
    );
}

/// Negative control: unauthorised user is rejected.
#[tokio::test]
async fn test_backend_snapshot_denied_for_unauthorised_user() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (_alice_inst, alice_db_id, _alice_identity) =
        setup_db(&server, &socket_path, "alice").await;

    // Create bob and try to read alice's database.
    create_user_via_admin(&server, "bob").await;
    let bob_inst = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let bob_user = bob_inst.login_user("bob", None).await.unwrap();
    let bob_key = bob_user.get_default_key().unwrap();
    let bob_identity = eidetica::auth::types::SigKey::from_pubkey(&bob_key);
    let conn = remote_conn(&bob_inst);

    let err = conn
        .get_verified_tips(alice_db_id, bob_identity)
        .await
        .expect_err("server must reject GetVerifiedTips for unauthorised user");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("permission") || msg.contains("auth"),
        "expected permission/auth error, got: {err}",
    );
}

/// D2: cross-tree read is denied via `db_get_entry`.
#[tokio::test]
async fn test_backend_get_denied_cross_tree() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (alice_inst, alice_root, alice_identity) = setup_db(&server, &socket_path, "alice").await;

    let alice_conn = remote_conn(&alice_inst);

    // Owner can read her own entry (positive control).
    alice_conn
        .db_get_entry(alice_root.clone(), alice_identity, alice_root.clone())
        .await
        .expect("owner must be able to GetEntry in her own database");

    // Bob is logged in but has no access.
    create_user_via_admin(&server, "bob").await;
    let bob_inst = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let bob_user = bob_inst.login_user("bob", None).await.unwrap();
    let bob_key = bob_user.get_default_key().unwrap();
    let bob_identity = eidetica::auth::types::SigKey::from_pubkey(&bob_key);
    let bob_conn = remote_conn(&bob_inst);

    let err = bob_conn
        .db_get_entry(alice_root.clone(), bob_identity, alice_root)
        .await
        .expect_err("GetEntry must be gated on the target database");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("permission") || msg.contains("auth"),
        "expected permission/auth denial, got: {err}",
    );
}

/// `SetInstanceMetadata` allowed for admin.
#[tokio::test]
async fn test_set_instance_metadata_allowed_for_admin() {
    let (socket_path, _tx, _server, _dir) = start_test_server().await;

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
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

/// `SetInstanceMetadata` denied for non-admin.
#[tokio::test]
async fn test_set_instance_metadata_denied_for_non_admin() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "bob").await;

    let bob_inst = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
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
    create_user_via_admin(&server, "bob").await;

    let (mut reader, mut writer) = raw_handshake(&socket_path).await;

    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginUser {
            username: "bob".to_string(),
        },
    )
    .await
    .unwrap();
    let resp: ServiceResponse = read_response(&mut reader).await;
    assert!(matches!(
        resp,
        ServiceResponse::TrustedLoginChallenge { .. }
    ));

    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginProve {
            signature: vec![0xAB; 64],
        },
    )
    .await
    .unwrap();
    let resp: ServiceResponse = read_response(&mut reader).await;
    assert!(matches!(resp, ServiceResponse::Error(_)));

    write_frame(
        &mut writer,
        &ServiceRequest::TrustedLoginProve {
            signature: vec![0xCD; 64],
        },
    )
    .await
    .unwrap();
    let resp: ServiceResponse = read_response(&mut reader).await;
    assert!(matches!(resp, ServiceResponse::Error(_)));
}

/// End-to-end test for `RemoteDatabaseOps`.
#[tokio::test]
async fn test_remote_database_ops_e2e() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (instance, root_id, identity) = setup_db(&server, &socket_path, "alice").await;

    // Write data server-side.
    let server_user = server.login_user("alice", None).await.unwrap();
    let server_key_pub = server_user.get_default_key().unwrap();
    let server_sk = server_user.get_signing_key(&server_key_pub).unwrap();
    let db = eidetica::Database::open(&server, &root_id)
        .await
        .unwrap()
        .with_key(server_sk);
    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("entries").await?;
        store.set("greeting", "hello from remote ops").await?;
        store.set("count", 42).await?;
        Ok(())
    })
    .await
    .unwrap();

    let conn = remote_conn(&instance);

    // Open a remote Database handle (exercises `open_remote`).
    let _remote_db =
        eidetica::Database::open_remote(&instance, conn.clone(), &root_id, identity.clone())
            .await
            .unwrap();

    let verified_tips = conn
        .get_verified_tips(root_id.clone(), identity.clone())
        .await
        .unwrap();
    assert!(
        !verified_tips.is_empty(),
        "verified tips must include at least the root entry"
    );

    // Store entries reachable via the direct DatabaseOp path.
    let entries = conn
        .get_store_entries(
            root_id.clone(),
            identity,
            "entries".to_string(),
            verified_tips.into_tips(),
            ReadScope::Verified,
        )
        .await
        .unwrap();
    assert!(!entries.is_empty(), "store entries must be reachable");
    for w in entries.windows(2) {
        let prev_height = w[0].subtree_height("entries").unwrap_or(0);
        let next_height = w[1].subtree_height("entries").unwrap_or(0);
        assert!(
            prev_height <= next_height,
            "entries must be ordered by subtree height"
        );
    }
}

/// An authenticated connection must not be able to claim an unregistered
/// identity for a database operation.
#[tokio::test]
async fn test_remote_operation_rejects_pubkey_absent_from_session_keyset() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (instance, root_id, _identity) = setup_db(&server, &socket_path, "alice").await;

    // `setup_db` authenticated this connection as alice. This key has never
    // been registered through the session-key proof-of-possession exchange.
    let (_private_key, absent_pubkey) = generate_keypair();
    let err = remote_conn(&instance)
        .db_get_entry(
            eidetica::entry::ID::default(),
            eidetica::auth::types::SigKey::from_pubkey(&absent_pubkey),
            root_id,
        )
        .await
        .expect_err("server must reject a claimed pubkey absent from the session keyset");

    assert!(
        err.to_string().contains("not in the session keyset"),
        "expected server rejection for a pubkey absent from the session keyset, got: {err}",
    );
}
// === Change A: verification-gated SubmitSignedEntry ===
//
// `SubmitSignedEntry` requires only an *authenticated* connection (gate 1).
// The per-tree session gate and the identity cross-check are skipped for
// submit; the server's own verification pass against the tree's pinned auth
// is the boundary. Pre-Change-A, both tests below would have failed at the
// gate. Post-Change-A, the socket accepts both and the verification pass
// distinguishes the legitimate case from the unauthorized case.

/// An authenticated session may submit an entry signed by a key that's
/// `Admin` on a *different* tree (i.e., not the session's own tree). The
/// server stores it `Unverified`, verifies it against the target tree's
/// pinned auth, and promotes it to `Verified` — it lands in the target
/// tree's Verified frontier even though the submitting session has no
/// permission on that tree.
#[tokio::test]
async fn test_submit_cross_session_signed_by_tree_admin_becomes_verified() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;

    // Bob owns a database (created server-side, bob is Admin(0) on it).
    create_user_via_admin(&server, "bob").await;
    let mut server_bob = server.login_user("bob", None).await.unwrap();
    let bob_pub = server_bob.get_default_key().unwrap();
    let bob_sk = server_bob.get_signing_key(&bob_pub).unwrap();
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "bob_db");
    let bob_db = server_bob
        .create_database(settings, &bob_pub)
        .await
        .unwrap();
    let bob_root = bob_db.root_id().clone();
    let initial_tips = bob_db.snapshot().await.unwrap().into_tips();

    // The bootstrap admin (NOT bob) connects over the wire. Admin holds
    // Admin on `_users`/`_databases` but is *not* a member of bob's tree.
    let admin_inst = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let _admin_user = admin_inst.login_user("admin", None).await.unwrap();
    let conn = remote_conn(&admin_inst);

    // Resolve bob's SigKey in his own tree (the same shape `setup_db`
    // produces). The verifier reads `entry.auth().key` to look up bob's key
    // in the tree's auth_settings; if we left this defaulted, the resolver
    // would find no candidates and verification would fail regardless of
    // signature validity.
    let sigkeys = eidetica::Database::find_sigkeys(&server, &bob_root, &bob_pub)
        .await
        .unwrap();
    let (bob_identity, _perm) = sigkeys
        .into_iter()
        .next()
        .expect("bob must have a resolved SigKey in his own tree");

    // Pull settings_tips server-side so the entry's signed metadata pins
    // the auth state verification must validate against. Without this the
    // verifier returns `Complete(AuthSettings::new())` for a non-`_settings`
    // entry — i.e. "no auth configured" — and rejects any signed entry.
    // `transaction_context` is the same primitive the wire seam uses; here
    // we call it on the local Database since we're constructing an entry
    // that bypasses the begin_transaction wire round-trip.
    let local_bob_db = eidetica::Database::open(&server, &bob_root).await.unwrap();
    let ctx = local_bob_db
        .transaction_context(&["note".to_string()], ReadScope::Verified)
        .await
        .unwrap();
    let max_parent_height = ctx.main_parents.iter().map(|(_, h)| *h).max().unwrap_or(0);
    let parents: Vec<eidetica::entry::ID> =
        ctx.main_parents.iter().map(|(id, _)| id.clone()).collect();
    // `EntryMetadata` is `pub(crate)`; construct its JSON wire form
    // (`{settings_tips, entropy}`) directly so the verifier's deserializer
    // accepts it.
    let metadata_bytes = serde_json::to_vec(&serde_json::json!({
        "settings_tips": ctx.settings_tips,
        "entropy": serde_json::Value::Null,
    }))
    .unwrap();

    // Build the entry with an explicit height = max(parent_heights) + 1 so
    // it sorts after the genesis in the topo-sorted `get_tree` walk that
    // `verified_frontier` relies on (height-ascending, ID-tiebreaking).
    // Without this the new entry could tie at height 0 and the frontier
    // would short-circuit on the child before its parent is in the prefix.
    // Set the identity hint to bob's, then sign — signing must happen
    // after `sig.key`, `metadata`, and `height` are set because the
    // canonical signing bytes include them all.
    let entry = Entry::builder(bob_root.clone())
        .set_parents(parents)
        .set_subtree_data("note", b"{\"cross_session\":true}")
        .set_metadata(metadata_bytes)
        .set_height(max_parent_height + 1)
        .build()
        .unwrap();
    let entry = entry.with_auth(|auth| auth.key = bob_identity.clone());
    let signature = sign_entry(&entry, &bob_sk).unwrap();
    let entry = entry.with_auth(|auth| auth.signature = Some(signature));
    let entry_id = entry.id();

    conn.submit_signed_entry(bob_root.clone(), bob_identity, entry)
        .await
        .expect(
            "verification-gated submit must accept a cross-session entry validly \
             signed by the target tree's admin",
        );

    // The submitted entry is in bob's Verified frontier.
    let tips_after = bob_db.snapshot().await.unwrap().into_tips();
    assert!(
        tips_after.contains(&entry_id),
        "submitted entry must be a Verified tip; tips={tips_after:?}, entry={entry_id}"
    );
    assert!(
        !tips_after.iter().any(|t| initial_tips.contains(t)),
        "old tip must have been superseded by the submitted entry"
    );
}

/// An authenticated session submitting an entry whose signer holds no key
/// on the target tree's auth: accepted at the socket (Change A skips the
/// gate), but the server's verification pass marks it `Failed`/leaves
/// `Unverified`, so it never appears in the Verified frontier or any
/// default read. Correctness is preserved by verification, not the gate.
#[tokio::test]
async fn test_submit_unauthorized_signer_stays_invisible_in_default_reads() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;

    create_user_via_admin(&server, "bob").await;
    let mut server_bob = server.login_user("bob", None).await.unwrap();
    let bob_pub = server_bob.get_default_key().unwrap();
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "bob_db");
    let bob_db = server_bob
        .create_database(settings, &bob_pub)
        .await
        .unwrap();
    let bob_root = bob_db.root_id().clone();
    let initial_tips = bob_db.snapshot().await.unwrap().into_tips();

    // Admin connects and signs an entry with the *admin* key, which has no
    // auth on bob's tree.
    let admin_inst = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let admin_user = admin_inst.login_user("admin", None).await.unwrap();
    let admin_pub = admin_user.get_default_key().unwrap();
    let admin_sk = admin_user.get_signing_key(&admin_pub).unwrap();
    let conn = remote_conn(&admin_inst);

    let entry = Entry::builder(bob_root.clone())
        .set_parents(initial_tips.clone())
        .set_subtree_data("note", b"{\"unauthorized_signer\":true}")
        .build()
        .unwrap();
    let signature = sign_entry(&entry, &admin_sk).unwrap();
    let entry = entry.with_auth(|auth| auth.signature = Some(signature));
    let entry_id = entry.id();
    let admin_identity = eidetica::auth::types::SigKey::from_pubkey(&admin_pub);

    // Accepted at the socket — Change A's relaxation. Verification is the
    // boundary the server still enforces.
    conn.submit_signed_entry(bob_root.clone(), admin_identity, entry)
        .await
        .expect("submit accepted at socket; verification rejects in-handler");

    // Bob's Verified frontier is unchanged: the unauthorized entry never
    // graduated past Unverified/Failed and is excluded from default reads.
    let tips_after = bob_db.snapshot().await.unwrap().into_tips();
    assert!(
        !tips_after.contains(&entry_id),
        "unauthorized-signer entry must NOT appear in Verified frontier; tips={tips_after:?}"
    );
    assert_eq!(
        tips_after, initial_tips,
        "Verified frontier must be unchanged after a rejected submit"
    );
}

// === Connection resilience tests ===
//
// One bad connection (abrupt drop, garbage frame, partial write) must not
// take the daemon down or poison state shared with other connections. These
// are the minimum gates documented in the service-foundation review punch
// list — extend them as the wire surface grows.

/// Confirm the daemon survives a client that disconnects without a graceful
/// shutdown after authenticating, and continues serving fresh clients with
/// shared instance state still intact.
#[tokio::test]
async fn test_daemon_survives_abrupt_client_disconnect() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    // Client A connects, logs in, then is dropped without any teardown.
    {
        let instance = Instance::connect(format!("unix://{}", socket_path.display()))
            .await
            .unwrap();
        let _user = instance.login_user("alice", None).await.unwrap();
        // Drop instance — the underlying UnixStream tears down without the
        // server ever seeing an EOF-after-request boundary.
    }

    // Daemon must still answer fresh connections. Anything else (a `connect`
    // hang, an authentication failure, a server crash) means abrupt drops are
    // poisoning shared state.
    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let user = instance.login_user("alice", None).await.unwrap();
    assert_eq!(user.username(), "alice");
}

/// Confirm the daemon rejects a malformed (oversized) length-prefixed frame
/// from a client without dying, and that an unrelated subsequent client can
/// still complete a real RPC.
#[tokio::test]
async fn test_daemon_survives_malformed_frame_from_client() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    // Raw client #1: complete the handshake, then send a length prefix that
    // exceeds `MAX_FRAME_SIZE`. `read_frame` on the server side must reject
    // and close this connection.
    {
        let (mut reader, mut writer) = raw_handshake(&socket_path).await;
        let oversize_len = eidetica::service::protocol::MAX_FRAME_SIZE + 1;
        writer
            .write_all(&oversize_len.to_be_bytes())
            .await
            .expect("write of bogus header should succeed at the socket layer");
        // The server is expected to drop the connection. Reading from the
        // half-closed stream should either return EOF (None) or an I/O error;
        // either way, the daemon must not panic or wedge.
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            read_frame::<_, ServerFrame>(&mut reader),
        )
        .await
        .expect("server must close the bad connection within the timeout");
    }

    // Daemon should still serve a fresh connection.
    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let user = instance.login_user("alice", None).await.unwrap();
    assert_eq!(user.username(), "alice");
}

/// Confirm the daemon survives a client that disconnects partway through a
/// request — specifically, a half-written length prefix followed by EOF.
/// This exercises the `read_exact` failure path inside the server's framing
/// loop.
#[tokio::test]
async fn test_daemon_survives_half_written_request_frame() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    {
        let (_reader, mut writer) = raw_handshake(&socket_path).await;
        // Two bytes of a four-byte length prefix, then close. The server's
        // next `read_exact(&mut [u8; 4])` should hit `UnexpectedEof`.
        writer
            .write_all(&[0x00, 0x00])
            .await
            .expect("partial write should succeed at the socket layer");
        // Dropping `writer` (the WriteHalf) doesn't close the stream — the
        // ReadHalf still holds the other half of the split. Explicit shutdown
        // forces the server-side reader to see EOF promptly.
        writer.shutdown().await.ok();
        drop(writer);
    }

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let user = instance.login_user("alice", None).await.unwrap();
    assert_eq!(user.username(), "alice");
}

// === Store-state records over the wire ===
//
// The daemon owns the record substrate; a client resolves an opaque view and
// reads through it. Every request is rebound to the authenticated session: a
// client addresses its own user scope, and the daemon's own shared
// materializations remain readable as a fallback.

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ServiceTodo {
    title: String,
    done: bool,
}

/// Log in over the socket and return the connected client instance.
async fn login_client(socket_path: &std::path::Path, username: &str) -> Instance {
    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    instance.login_user(username, None).await.unwrap();
    instance
}

/// One derived Store-state request for a connected client.
fn derived_request(root_id: &eidetica::entry::ID, store: &str) -> StoreStateRequest {
    StoreStateRequest {
        database: root_id.clone(),
        store: store.to_string(),
        lifecycle: eidetica::backend::StoreStateLifecycle::Derived,
        scope: eidetica::backend::CacheScope::Shared,
        projection: ProjectionDescriptor {
            name: "eidetica/opaque/test".to_string(),
            version: 0,
        },
        source_key: b"tips".to_vec(),
    }
}

/// Publish one opaque record through the service staging protocol.
async fn publish_remote_state(
    conn: &eidetica::service::client::RemoteConnection,
    identity: &eidetica::auth::types::SigKey,
    request: StoreStateRequest,
    value: Vec<u8>,
) -> String {
    let root_id = request.database.clone();
    let token = conn
        .begin_store_state_staging(identity.clone(), request)
        .await
        .unwrap();
    conn.stage_store_state_records(
        root_id.clone(),
        identity.clone(),
        token.clone(),
        0,
        BTreeMap::from([(vec![0], Some(value))]),
    )
    .await
    .unwrap();
    conn.publish_store_state(root_id, identity.clone(), token)
        .await
        .unwrap()
}

/// A request nothing has published resolves to no view.
#[tokio::test]
async fn test_remote_store_state_miss_returns_none() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (instance, root_id, identity) = setup_db(&server, &socket_path, "alice").await;
    let conn = remote_conn(&instance);

    assert!(
        conn.resolve_store_state(identity, derived_request(&root_id, "never-built"))
            .await
            .unwrap()
            .is_none()
    );
}

/// A published record set is readable through a resolved view, and survives a
/// fresh connection because the daemon owns the records.
#[tokio::test]
async fn test_remote_store_state_round_trip_survives_reconnect() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (instance, root_id, identity) = setup_db(&server, &socket_path, "alice").await;
    let conn = remote_conn(&instance);
    let request = derived_request(&root_id, "entries");

    let view =
        publish_remote_state(&conn, &identity, request.clone(), b"materialized".to_vec()).await;
    assert_eq!(
        conn.store_state_record_get(root_id.clone(), identity.clone(), view, vec![0])
            .await
            .unwrap(),
        Some(b"materialized".to_vec())
    );

    let reconnected = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let _user = reconnected.login_user("alice", None).await.unwrap();
    let conn = remote_conn(&reconnected);
    let view = conn
        .resolve_store_state(identity.clone(), request)
        .await
        .unwrap()
        .expect("published record set must resolve on a new connection");
    assert_eq!(
        conn.store_state_record_get(root_id, identity, view, vec![0])
            .await
            .unwrap(),
        Some(b"materialized".to_vec())
    );
}

/// Published record sets are immutable: republishing the same request keeps the
/// first bytes rather than overwriting them.
#[tokio::test]
async fn test_remote_store_state_publication_is_immutable() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (instance, root_id, identity) = setup_db(&server, &socket_path, "alice").await;
    let conn = remote_conn(&instance);
    let request = derived_request(&root_id, "entries");

    publish_remote_state(&conn, &identity, request.clone(), b"first".to_vec()).await;
    let view = publish_remote_state(&conn, &identity, request, b"second".to_vec()).await;
    assert_eq!(
        conn.store_state_record_get(root_id, identity, view, vec![0])
            .await
            .unwrap(),
        Some(b"first".to_vec())
    );
}

/// A request naming another user's scope is refused rather than served, and a
/// client-computed record set is not visible to a different session user.
#[tokio::test]
async fn test_remote_store_state_scope_is_bound_to_the_session_user() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (alice_instance, root_id, identity) = setup_db(&server, &socket_path, "alice").await;
    let alice_conn = remote_conn(&alice_instance);
    let request = derived_request(&root_id, "entries");
    publish_remote_state(
        &alice_conn,
        &identity,
        request.clone(),
        b"alice-only".to_vec(),
    )
    .await;

    let foreign = StoreStateRequest {
        scope: eidetica::backend::CacheScope::User("someone-else".to_string()),
        ..request.clone()
    };
    assert!(
        alice_conn
            .resolve_store_state(identity.clone(), foreign)
            .await
            .is_err(),
        "a request naming another user's scope must be refused"
    );

    // The client asked with the shared scope; the daemon narrowed it to the
    // session user, so nothing was published into the daemon's own shared cached state.
    assert!(
        server
            .backend()
            .local_engine()
            .expect("test server is always Local")
            .resolve_store_state(&request)
            .await
            .unwrap()
            .is_none(),
        "a client publication must not land in the daemon's shared cached state"
    );
    assert!(
        alice_conn
            .resolve_store_state(identity, request)
            .await
            .unwrap()
            .is_some(),
        "the publishing session must still resolve its own record set"
    );
}

/// The daemon's own shared materializations stay readable by an authorized
/// user: a user-scope miss falls back to shared cached state.
#[tokio::test]
async fn test_remote_store_state_shared_fallback_for_daemon_materialized_state() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (instance, root_id, identity) = setup_db(&server, &socket_path, "alice").await;
    let request = derived_request(&root_id, "shared-fallback-store");

    // Seed the shared scope directly on the daemon's backend, as the daemon's
    // own materialization of an unencrypted store would.
    let backend = server
        .backend()
        .local_engine()
        .expect("test server is always Local");
    let token = backend
        .begin_store_state_staging(request.clone())
        .await
        .unwrap();
    backend
        .stage_store_state_records(
            &token,
            BTreeMap::from([(vec![0], Some(b"daemon-computed".to_vec()))]),
        )
        .await
        .unwrap();
    backend.publish_store_state(token).await.unwrap();

    let conn = remote_conn(&instance);
    let view = conn
        .resolve_store_state(identity.clone(), request)
        .await
        .unwrap()
        .expect("user-scope miss must fall back to the daemon's shared cached state");
    assert_eq!(
        conn.store_state_record_get(root_id, identity, view, vec![0])
            .await
            .unwrap(),
        Some(b"daemon-computed".to_vec())
    );
}

/// The daemon is byte-blind: ciphertext records round-trip verbatim.
#[tokio::test]
async fn test_remote_store_state_stores_ciphertext_verbatim() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (instance, root_id, identity) = setup_db(&server, &socket_path, "alice").await;
    let conn = remote_conn(&instance);
    let ciphertext = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x42, 0x42, 0x42, 0x42];

    let view = publish_remote_state(
        &conn,
        &identity,
        derived_request(&root_id, "encrypted-store"),
        ciphertext.clone(),
    )
    .await;
    assert_eq!(
        conn.store_state_record_get(root_id, identity, view, vec![0])
            .await
            .unwrap(),
        Some(ciphertext)
    );
}

/// An idle private-build capability is reclaimed: the build it was staging is
/// aborted, so the token stops working and nothing was published.
#[tokio::test]
async fn test_remote_store_state_idle_staging_token_expires() {
    let (socket_path, _tx, server, _dir) =
        start_test_server_with_token_ttl(Duration::from_millis(50)).await;
    let (instance, root_id, identity) = setup_db(&server, &socket_path, "alice").await;
    let conn = remote_conn(&instance);
    let request = derived_request(&root_id, "expiring");

    let token = conn
        .begin_store_state_staging(identity.clone(), request.clone())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(120)).await;

    assert!(
        conn.publish_store_state(root_id, identity.clone(), token)
            .await
            .is_err(),
        "an expired staging token must not publish"
    );
    assert!(
        conn.resolve_store_state(identity, request)
            .await
            .unwrap()
            .is_none(),
        "an expired build must leave no ready record set behind"
    );
}

/// A historical `Table` read over the service publishes an addressable row
/// record set rather than falling back to a whole-`Doc` response.
#[tokio::test]
async fn test_historical_table_service_reads_use_row_record_set() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    let mut server_user = server.login_user("alice", None).await.unwrap();
    let key = server_user.get_default_key().unwrap();
    let database = server_user.create_database(Doc::new(), &key).await.unwrap();
    let root_id = database.root_id().clone();
    database
        .with_transaction(|transaction| async move {
            let table = transaction.get_store::<Table<ServiceTodo>>("rows").await?;
            table
                .set(
                    "a",
                    ServiceTodo {
                        title: "first".into(),
                        done: false,
                    },
                )
                .await?;
            table
                .set(
                    "b",
                    ServiceTodo {
                        title: "second".into(),
                        done: true,
                    },
                )
                .await
        })
        .await
        .unwrap();

    let engine = server.backend().local_engine().unwrap();
    let memory = engine
        .as_any()
        .downcast_ref::<InMemory>()
        .expect("test server is always InMemory");
    assert_eq!(memory.store_state_record_count(&root_id, "rows"), 0);

    let client = login_client(&socket_path, "alice").await;
    let identity = eidetica::Database::find_sigkeys(&server, &root_id, &key)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .0;
    let remote = eidetica::Database::open_remote(&client, remote_conn(&client), &root_id, identity)
        .await
        .unwrap();
    let table = remote
        .get_store_viewer::<Table<ServiceTodo>>("rows")
        .await
        .unwrap();
    assert_eq!(
        memory.store_state_record_count(&root_id, "rows"),
        0,
        "loading a remote Table must not materialize any row"
    );
    assert_eq!(table.get("a").await.unwrap().title, "first");
    assert_eq!(
        memory.store_state_record_count(&root_id, "rows"),
        2,
        "the service read must publish the row record set, not a whole Doc"
    );
    let first = table.scan_page(None, 1).await.unwrap();
    let second = table.scan_page(first.next.as_ref(), 1).await.unwrap();
    assert_eq!(first.rows[0].0, "a");
    assert_eq!(second.rows[0].0, "b");
    assert!(second.next.is_none());
}

/// A connected `Table` keeps reading after its published-view capability
/// expires: the stale cached view resolves transparently to a fresh view onto
/// the same published record set.
///
/// Regression net for the view/staging-token wire split: the daemon must
/// report a missing or expired published view as `InvalidStoreStateView` (not
/// `InvalidStoreStateStagingToken`) so the transaction drops its cached view
/// and re-resolves instead of surfacing the expiry to the caller.
#[tokio::test]
async fn test_connected_table_recovers_after_published_view_expiry() {
    let (socket_path, _tx, server, _dir) =
        start_test_server_with_token_ttl(Duration::from_millis(50)).await;
    create_user_via_admin(&server, "alice").await;

    let mut server_user = server.login_user("alice", None).await.unwrap();
    let key = server_user.get_default_key().unwrap();
    let database = server_user.create_database(Doc::new(), &key).await.unwrap();
    let root_id = database.root_id().clone();
    database
        .with_transaction(|transaction| async move {
            let table = transaction.get_store::<Table<ServiceTodo>>("rows").await?;
            table
                .set(
                    "a",
                    ServiceTodo {
                        title: "first".into(),
                        done: false,
                    },
                )
                .await?;
            table
                .set(
                    "b",
                    ServiceTodo {
                        title: "second".into(),
                        done: true,
                    },
                )
                .await
        })
        .await
        .unwrap();

    let client = login_client(&socket_path, "alice").await;
    let identity = eidetica::Database::find_sigkeys(&server, &root_id, &key)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .0;
    let remote = eidetica::Database::open_remote(&client, remote_conn(&client), &root_id, identity)
        .await
        .unwrap();
    let table = remote
        .get_store_viewer::<Table<ServiceTodo>>("rows")
        .await
        .unwrap();
    assert_eq!(table.get("a").await.unwrap().title, "first");

    // Let the daemon's idle janitor reclaim the published-view capability
    // while the client's cached view still points at it.
    tokio::time::sleep(Duration::from_millis(150)).await;

    assert_eq!(
        table.get("a").await.unwrap().title,
        "first",
        "a stale cached view must re-resolve instead of failing"
    );
    let page = table.scan_page(None, 2).await.unwrap();
    assert_eq!(page.rows.len(), 2);
    assert_eq!(page.rows[0].0, "a");
    assert_eq!(page.rows[1].0, "b");
}

// =============================================================================
// Server-push write notifications (`Notification::DatabaseWrite`).
//
// Exercises the end-to-end path: client `Database::on_write` registration sends
// `DatabaseOp::SubscribeWrites`, daemon's global write callback fans events
// out to subscribed connections, client reader task routes notifications into
// the local callback registry.
//
// Generous timeouts: the notification is asynchronous to `commit().await` on a
// connected instance (by design — see `Database::on_write` docs § Callback
// timing). 2s is enormous for a Unix-socket round-trip but absorbs CI jitter.
// =============================================================================

/// Helper: drain a callback-fed channel until `min` events arrive (or timeout).
async fn collect_events<T>(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<T>,
    min: usize,
    timeout_secs: u64,
) -> Vec<T> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    while out.len() < min {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(v)) => out.push(v),
            Ok(None) => break, // channel closed
            Err(_) => break,   // timeout
        }
    }
    out
}

#[tokio::test]
async fn test_on_write_fires_for_local_commit_on_connected_instance() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user = instance.login_user("alice", None).await.unwrap();
    let pubkey = user.get_default_key().unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "callback_test");
    let db = user.create_database(settings, &pubkey).await.unwrap();

    // Single fire per submit under the fire-on-Verified model — no
    // need to filter on verification state, every notification we
    // receive here is settled.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<usize>();
    let _cb = db
        .on_write(move |event, db| {
            let prev = event.previous_tips().clone();
            let post = event.post_tips().clone();
            let db = db.clone();
            let tx = event_tx.clone();
            async move {
                let count = db.ids_added(&prev, &post).await?.len();
                let _ = tx.send(count);
                Ok(())
            }
        })
        .await
        .unwrap();

    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v").await?;
        Ok(())
    })
    .await
    .unwrap();

    let events = collect_events(&mut event_rx, 1, 2).await;
    assert_eq!(events.len(), 1, "exactly one Verified notification");
    assert_eq!(events[0], 1, "exactly one entry in the event");
}

#[tokio::test]
async fn test_on_write_fires_across_clients_on_same_daemon() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    // Set up a database server-side so both clients have the same root_id.
    let mut server_user = server.login_user("alice", None).await.unwrap();
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "cross_client_test");
    let server_key = server_user.get_default_key().unwrap();
    let server_db = server_user
        .create_database(settings, &server_key)
        .await
        .unwrap();
    let root_id = server_db.root_id().clone();

    // Observer client: registers on_write, waits.
    let observer = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let observer_user = observer.login_user("alice", None).await.unwrap();
    let observer_db = observer_user.open_database(&root_id).await.unwrap();

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let _cb = observer_db
        .on_write(move |_event, _db| {
            let tx = event_tx.clone();
            async move {
                let _ = tx.send(());
                Ok(())
            }
        })
        .await
        .unwrap();

    // Writer client: connects, commits an entry. The observer must see it.
    let writer = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let writer_user = writer.login_user("alice", None).await.unwrap();
    let writer_db = writer_user.open_database(&root_id).await.unwrap();
    writer_db
        .with_transaction(|tx| async move {
            let store = tx.get_store::<DocStore>("data").await?;
            store.set("from", "writer").await?;
            Ok(())
        })
        .await
        .unwrap();

    let events = collect_events(&mut event_rx, 1, 2).await;
    assert_eq!(
        events.len(),
        1,
        "observer must see writer's commit via daemon push"
    );
}

#[tokio::test]
async fn test_on_write_previous_tips_populated_on_connected_instance() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user = instance.login_user("alice", None).await.unwrap();
    let pubkey = user.get_default_key().unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "prev_tips_test");
    let db = user.create_database(settings, &pubkey).await.unwrap();

    // Single fire per commit under fire-on-Verified — no filter needed.
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::unbounded_channel::<(Vec<eidetica::entry::ID>, eidetica::Snapshot)>();
    let _cb = db
        .on_write(move |event, db| {
            let prev_tips = event.previous_tips().clone();
            let post_tips = event.post_tips().clone();
            let db = db.clone();
            let tx = event_tx.clone();
            async move {
                let entries = db.ids_added(&prev_tips, &post_tips).await?;
                let _ = tx.send((entries, prev_tips));
                Ok(())
            }
        })
        .await
        .unwrap();

    // Two sequential commits; the second's previous_tips must include the
    // first entry's id (and must not be empty — the V1 behavior before this
    // change).
    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v1").await?;
        Ok(())
    })
    .await
    .unwrap();
    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v2").await?;
        Ok(())
    })
    .await
    .unwrap();

    let events = collect_events(&mut event_rx, 2, 2).await;
    assert_eq!(events.len(), 2, "both commits must surface as callbacks");
    let (entries_1, _prev_1) = &events[0];
    let (_entries_2, prev_2) = &events[1];
    assert!(
        !prev_2.is_empty(),
        "second callback's previous_tips must be populated (got empty — daemon push not delivering canonical tips)"
    );
    assert!(
        prev_2.tips().contains(&entries_1[0]),
        "second callback's previous_tips must include the first commit's entry id; got prev={prev_2:?}, first_entry={:?}",
        entries_1[0]
    );
}

#[tokio::test]
async fn test_on_write_dropped_handle_stops_callback() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user = instance.login_user("alice", None).await.unwrap();
    let pubkey = user.get_default_key().unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "drop_test");
    let db = user.create_database(settings, &pubkey).await.unwrap();

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let cb = db
        .on_write(move |_event, _db| {
            let tx = event_tx.clone();
            async move {
                let _ = tx.send(());
                Ok(())
            }
        })
        .await
        .unwrap();

    // First commit: callback fires.
    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v1").await?;
        Ok(())
    })
    .await
    .unwrap();
    let first = collect_events(&mut event_rx, 1, 2).await;
    assert_eq!(first.len(), 1, "first commit must fire callback");

    // Drop the WriteCallback handle — unregisters from the local registry.
    // The daemon may still push (subscription persists on the connection),
    // but with no callback registered the dispatch is a no-op.
    drop(cb);

    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v2").await?;
        Ok(())
    })
    .await
    .unwrap();

    // Give the daemon round-trip a moment to complete; if a callback were
    // going to fire (regression), it would do so well within 500ms on a
    // local Unix socket.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after_drop = collect_events(&mut event_rx, 1, 0).await;
    assert!(
        after_drop.is_empty(),
        "dropped WriteCallback must stop firing user code; got {after_drop:?}"
    );
}

#[tokio::test]
async fn test_on_write_only_fires_for_subscribed_tree() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user = instance.login_user("alice", None).await.unwrap();
    let pubkey = user.get_default_key().unwrap();

    let mut settings_a = eidetica::crdt::Doc::new();
    settings_a.set("name", "tree_a");
    let db_a = user.create_database(settings_a, &pubkey).await.unwrap();

    let mut settings_b = eidetica::crdt::Doc::new();
    settings_b.set("name", "tree_b");
    let db_b = user.create_database(settings_b, &pubkey).await.unwrap();

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let _cb = db_a
        .on_write(move |_event, _db| {
            let tx = event_tx.clone();
            async move {
                let _ = tx.send(());
                Ok(())
            }
        })
        .await
        .unwrap();

    // Commit to tree B; tree A's callback must not fire.
    db_b.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v").await?;
        Ok(())
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let cross_tree = collect_events(&mut event_rx, 1, 0).await;
    assert!(
        cross_tree.is_empty(),
        "callback on tree A must not fire for writes to tree B; got {cross_tree:?}"
    );

    // Now commit to tree A — the callback must fire.
    db_a.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v").await?;
        Ok(())
    })
    .await
    .unwrap();
    let same_tree = collect_events(&mut event_rx, 1, 2).await;
    assert_eq!(
        same_tree.len(),
        1,
        "callback on tree A must fire for tree A's own commit"
    );
}

/// Race two `on_write` registrations on the same tree from separate tasks,
/// then commit immediately after both await-points return. Both callbacks
/// must observe the commit — i.e. the second-to-arrive registration must
/// have waited for the in-flight `SubscribeWrites` to land before returning,
/// not raced ahead with the daemon still unsubscribed.
#[tokio::test]
async fn test_on_write_concurrent_registrations_both_observe_commit() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user = instance.login_user("alice", None).await.unwrap();
    let pubkey = user.get_default_key().unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "race_test");
    let db = user.create_database(settings, &pubkey).await.unwrap();

    let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel::<()>();
    let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel::<()>();

    let db_for_a = db.clone();
    let db_for_b = db.clone();
    let reg_a = tokio::spawn(async move {
        db_for_a
            .on_write(move |_event, _db| {
                let tx = tx_a.clone();
                async move {
                    let _ = tx.send(());
                    Ok(())
                }
            })
            .await
            .unwrap()
    });
    let reg_b = tokio::spawn(async move {
        db_for_b
            .on_write(move |_event, _db| {
                let tx = tx_b.clone();
                async move {
                    let _ = tx.send(());
                    Ok(())
                }
            })
            .await
            .unwrap()
    });
    let (_cb_a, _cb_b) = tokio::try_join!(reg_a, reg_b).unwrap();

    // Both `on_write().await` calls have returned. A commit issued now must
    // be visible to both callbacks — if the follower returned without
    // awaiting the leader's subscribe, this commit could land before the
    // daemon recognized the subscription and one callback (or both) would
    // miss it.
    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v").await?;
        Ok(())
    })
    .await
    .unwrap();

    let events_a = collect_events(&mut rx_a, 1, 2).await;
    let events_b = collect_events(&mut rx_b, 1, 2).await;
    assert_eq!(
        events_a.len(),
        1,
        "first registration must observe the commit; got {events_a:?}"
    );
    assert_eq!(
        events_b.len(),
        1,
        "second (concurrent) registration must observe the commit; got {events_b:?}"
    );
}

/// Regression: dropping the last per-tree callback transitions the wire
/// subscription to `Idle`; a re-registration inside the grace window
/// avoids a wire round-trip; once the grace window elapses the sweep
/// sends `UnsubscribeWrites` to the daemon.
///
/// Uses `set_idle_grace_window_for_test` and
/// `set_sweep_interval_for_test` (gated behind the `testing` feature)
/// to shrink the windows so the test runs in ~250ms instead of waiting
/// the production-default 60s grace.
#[tokio::test]
async fn test_lazy_unsubscribe_after_grace_window() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    // Shrink the grace + sweep windows for this test. The setters are
    // process-global OnceLocks gated behind the `testing` feature;
    // nextest runs each integration-test binary in its own process so
    // no other test sees the override.
    eidetica::service::client::set_idle_grace_window_for_test(std::time::Duration::from_millis(
        100,
    ));
    eidetica::service::client::set_sweep_interval_for_test(std::time::Duration::from_millis(50));

    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    // Count daemon-side per-tree callback registrations as a proxy
    // for "the daemon is subscribed for this tree." Each
    // `SubscribeWrites` adds one; `UnsubscribeWrites` removes one.
    // Track via the daemon's internal `Instance::register_write_callback`
    // detected indirectly via a global callback that observes writes
    // — but that's a side measurement. Simpler: just count
    // notifications received on the client. Before sweep:
    // commit fires a notification. After sweep: commit fires nothing.
    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user = instance.login_user("alice", None).await.unwrap();
    let pubkey = user.get_default_key().unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "lazy_unsub_test");
    let db = user.create_database(settings, &pubkey).await.unwrap();

    // Register a callback, then drop it, then commit. The commit
    // should still fire (subscription is Idle, daemon still pushing,
    // but no local callback to invoke — so we measure differently).
    //
    // Instead, after the grace window elapses, register a *new*
    // callback. By that time the sweep should have unsubscribed —
    // but `on_write_at_tips` then re-subscribes via the leader path.
    // Verify a subsequent commit still fires that new callback (proves
    // the subscribe round-trip happens correctly post-unsubscribe).
    let received = Arc::new(AtomicUsize::new(0));

    {
        let received_clone = received.clone();
        let _cb = db
            .on_write(move |_event, _db| {
                let n = received_clone.clone();
                async move {
                    n.fetch_add(1, AtomicOrdering::Relaxed);
                    Ok(())
                }
            })
            .await
            .unwrap();

        // Fire one commit while callback is alive.
        db.with_transaction(|tx| async move {
            let store = tx.get_store::<DocStore>("data").await?;
            store.set("k", "v1").await?;
            Ok(())
        })
        .await
        .unwrap();

        // Give the notification time to round-trip.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            received.load(AtomicOrdering::Relaxed) >= 1,
            "callback should fire while subscribed"
        );

        // `cb` drops here → transition_to_idle.
    }

    // Wait past the grace window so the sweep runs UnsubscribeWrites.
    // Grace is 100ms, sweep interval is 50ms, so by 300ms we're
    // guaranteed to have swept.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Now register a fresh callback. This should re-subscribe via
    // the leader path (entry was removed by the sweep). Then commit
    // and assert the new callback fires.
    let received2 = Arc::new(AtomicUsize::new(0));
    let received2_clone = received2.clone();
    let _cb2 = db
        .on_write(move |_event, _db| {
            let n = received2_clone.clone();
            async move {
                n.fetch_add(1, AtomicOrdering::Relaxed);
                Ok(())
            }
        })
        .await
        .unwrap();

    db.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v2").await?;
        Ok(())
    })
    .await
    .unwrap();

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        received2.load(AtomicOrdering::Relaxed) >= 1,
        "re-registered callback after sweep must fire on subsequent commit"
    );
}

/// Regression: per-tree dispatch on the client means a slow callback on
/// tree A does not stall callbacks on tree B. The single-drain-task
/// shape this replaced would have serialised them.
#[tokio::test]
async fn test_on_write_per_tree_concurrent_dispatch() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user = instance.login_user("alice", None).await.unwrap();
    let pubkey = user.get_default_key().unwrap();

    let mut settings_a = eidetica::crdt::Doc::new();
    settings_a.set("name", "tree_a_slow");
    let db_a = user.create_database(settings_a, &pubkey).await.unwrap();
    let mut settings_b = eidetica::crdt::Doc::new();
    settings_b.set("name", "tree_b_fast");
    let db_b = user.create_database(settings_b, &pubkey).await.unwrap();

    // A's callback sleeps for 300ms before flipping `a_done`. B's
    // callback runs immediately and records whether `a_done` is set at
    // the moment it observes the write. If dispatch were serial across
    // trees, B's callback would only run *after* A's sleep completes
    // (a_done == true). Per-tree dispatch means B's callback runs
    // independently and should see a_done == false.
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    let a_done = std::sync::Arc::new(AtomicBool::new(false));
    let b_observed: std::sync::Arc<std::sync::Mutex<Option<bool>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));

    let a_done_for_a = a_done.clone();
    let _cb_a = db_a
        .on_write(move |_event, _db| {
            let a_done = a_done_for_a.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                a_done.store(true, AtomicOrdering::Relaxed);
                Ok(())
            }
        })
        .await
        .unwrap();
    let a_done_for_b = a_done.clone();
    let b_observed_for_b = b_observed.clone();
    let _cb_b = db_b
        .on_write(move |_event, _db| {
            let a_done = a_done_for_b.clone();
            let observed = b_observed_for_b.clone();
            async move {
                let snapshot = a_done.load(AtomicOrdering::Relaxed);
                *observed.lock().unwrap() = Some(snapshot);
                Ok(())
            }
        })
        .await
        .unwrap();

    // Commit to both trees. Order doesn't matter; the daemon's per-tree
    // callback fires push notifications independently.
    let commit_a = db_a.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v_a").await?;
        Ok(())
    });
    let commit_b = db_b.with_transaction(|tx| async move {
        let store = tx.get_store::<DocStore>("data").await?;
        store.set("k", "v_b").await?;
        Ok(())
    });
    tokio::try_join!(commit_a, commit_b).unwrap();

    // Wait long enough for B's notification to round-trip and its
    // callback to record, but well before A's 300ms sleep finishes.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let observation = *b_observed.lock().unwrap();
    assert_eq!(
        observation,
        Some(false),
        "B's callback must run before A's slow callback finishes; per-tree dispatch broken if Some(true) or None: {observation:?}",
    );
}

/// Regression: notifications dispatched to user callbacks on a connected
/// client must arrive in daemon-canonical order, even under burst load.
///
/// Pre-fix: every notification spawned its own dispatch task, so the
/// callback for notification N+1 could start before notification N's
/// callback finished — and could even run on a different worker thread.
/// A user counting `event.entries()[0]` height (or similar) would see
/// arrivals interleaved by the scheduler, not by the daemon's send
/// order.
///
/// Post-fix: a single per-connection drain task `await`s each callback
/// before the next, so the order observed by the user matches the order
/// the daemon pushed.
#[tokio::test]
async fn test_on_write_preserves_daemon_canonical_order_under_burst() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user = instance.login_user("alice", None).await.unwrap();
    let pubkey = user.get_default_key().unwrap();

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "ordering_test");
    let db = user.create_database(settings, &pubkey).await.unwrap();

    // Each callback yields *before* it records its arrival index, then
    // sleeps for a tiny stagger to make it easy for a per-notification
    // `tokio::spawn` to land out of order. The serial dispatcher must
    // serialise them so the recorded order is monotonic.
    // One fire per commit under fire-on-Verified; no need to filter on
    // verification status. Per-fire ordering is what this test asserts.
    let arrivals: std::sync::Arc<std::sync::Mutex<Vec<u64>>> = Default::default();
    let next_seq = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let arrivals_for_cb = arrivals.clone();
    let next_seq_for_cb = next_seq.clone();
    let _cb = db
        .on_write(move |_event, _db| {
            let arrivals = arrivals_for_cb.clone();
            let next_seq = next_seq_for_cb.clone();
            async move {
                // Yield + sleep stagger: maximise the window for an
                // out-of-order dispatcher to reorder if it could.
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(2)).await;
                let seq = next_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                arrivals.lock().unwrap().push(seq);
                Ok(())
            }
        })
        .await
        .unwrap();

    // Burst commits. Each one is its own daemon-side write, so each
    // becomes a separate notification frame, pushed in order.
    let n: u64 = 12;
    for i in 0..n {
        db.with_transaction(move |tx| async move {
            let store = tx.get_store::<DocStore>("data").await?;
            store.set("k", format!("v{i}")).await?;
            Ok(())
        })
        .await
        .unwrap();
    }

    // Wait for all callbacks to land.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if arrivals.lock().unwrap().len() >= n as usize {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let recorded = arrivals.lock().unwrap().clone();
    assert_eq!(
        recorded.len(),
        n as usize,
        "every commit must surface as a callback (serialiser must not drop events)"
    );
    // The serial dispatcher guarantees the i'th callback to fire records
    // sequence `i`, monotonically. Pre-fix this assertion would fail
    // intermittently under the yield+sleep stagger.
    for (expected, got) in recorded.iter().enumerate() {
        assert_eq!(
            *got as usize, expected,
            "callbacks must fire in daemon-canonical order; got {recorded:?}"
        );
    }
}

/// Regression (#2): notifications for a single tree must reach a subscriber
/// in daemon-canonical order even when **multiple** connections write that
/// tree concurrently — not just under a single writer's burst.
///
/// The daemon serialises the writes themselves under the tree lock, but the
/// subscription's frame send used to run inside a detached `tokio::spawn`
/// dispatched *after* the lock dropped. Two concurrent writers' send tasks
/// could then race, delivering `post_tips` out of order — the client would
/// observe a `post_tips` that regresses behind the one before it.
///
/// Post-fix: the subscription callback's synchronous `frame_tx.send` runs
/// under the tree lock in cursor-advance order, so the observed `post_tips`
/// sequence for the subscribed callback is monotonically forward. This test
/// asserts exactly that invariant via ancestry (`ids_added`), which is
/// timing-independent once both writers have drained.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_on_write_canonical_order_under_concurrent_writers() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    // Two independent client connections, same user, same tree — each is a
    // distinct daemon-side connection, so their writes race at the fan-out.
    let writer1 = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mut user1 = writer1.login_user("alice", None).await.unwrap();
    let pubkey = user1.get_default_key().unwrap();
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "concurrent_writers");
    let db1 = user1.create_database(settings, &pubkey).await.unwrap();
    let root_id = db1.root_id().clone();

    let writer2 = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let user2 = writer2.login_user("alice", None).await.unwrap();
    let db2 = user2.open_database(&root_id).await.unwrap();

    // Subscriber records each event's post_tips in fire order.
    let observed: std::sync::Arc<std::sync::Mutex<Vec<eidetica::snapshot::Snapshot>>> =
        Default::default();
    let observed_for_cb = observed.clone();
    let _cb = db1
        .on_write(move |event, _db| {
            let post = event.post_tips().clone();
            let observed = observed_for_cb.clone();
            async move {
                observed.lock().unwrap().push(post);
                Ok(())
            }
        })
        .await
        .unwrap();

    // Both connections commit to the tree concurrently, interleaved.
    let n: usize = 8;
    let c1 = async {
        for i in 0..n {
            db1.with_transaction(move |tx| async move {
                let store = tx.get_store::<DocStore>("data").await?;
                store.set("w1", format!("v{i}")).await?;
                Ok(())
            })
            .await
            .unwrap();
        }
    };
    let c2 = async {
        for i in 0..n {
            db2.with_transaction(move |tx| async move {
                let store = tx.get_store::<DocStore>("data").await?;
                store.set("w2", format!("v{i}")).await?;
                Ok(())
            })
            .await
            .unwrap();
        }
    };
    tokio::join!(c1, c2);

    // Let the notification stream quiesce. We deliberately do NOT assert an
    // exact fire count: fire-on-Verified batches, so two entries promoted in a
    // single verify pass surface as one notification whose `post_tips` covers
    // both. The number of fires for `2n` concurrent commits is therefore
    // `<= 2n` and load-dependent — not a stable invariant. What IS stable is
    // the *order* of whatever fires do arrive, which is what this test pins.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let posts = observed.lock().unwrap().clone();
    assert!(
        posts.len() >= 2,
        "concurrent writers should surface several notifications; got {}",
        posts.len()
    );

    // Canonical order ⇔ the total number of entries reachable from each observed
    // `post_tips` strictly increases every fire. Measured via
    // `ids_added(EMPTY, post)` — the full ancestor closure, a valid ancestry
    // count in either topology (fork or linear). Each fire advances the daemon's
    // subscription cursor forward by at least one entry, so a correctly-ordered
    // stream is strictly increasing; a reordered send delivers an older
    // `post_tips` after a newer one, which would regress the count. This is
    // timing-independent: it holds no matter how the two writers interleave, as
    // long as delivery is in canonical order.
    //
    // Note: this pins the invariant but is not a red-before/green-after repro of
    // the reorder — the race only manifests on a multi-thread runtime and even
    // then only intermittently, so it can't be relied on to fail pre-fix. The
    // fix makes ordering hold *by construction* (synchronous send under the tree
    // lock in cursor order); this guards against a future regression of it.
    let mut prev_count = 0usize;
    for post in &posts {
        let count = db1
            .ids_added(&eidetica::snapshot::Snapshot::EMPTY, post)
            .await
            .unwrap()
            .len();
        assert!(
            count > prev_count,
            "reachable-entry count must strictly increase each fire (reorder regresses it); \
             got {count} after {prev_count} for {post:?} in sequence {posts:?}"
        );
        prev_count = count;
    }
}

/// Regression: a client request issued *after* the reader task has exited
/// (because the daemon shut down its side) must surface
/// `ConnectionAborted` promptly instead of hanging forever on a oneshot
/// no one will pop.
///
/// Pre-fix: the client checked nothing — it took the writer lock, pushed
/// a sender into the pending FIFO, wrote the frame (which could still
/// succeed against an OS write buffer briefly after peer close), and then
/// awaited a response from a dead connection. The dispatcher's `clear()`
/// had already run by then, so no one would ever fulfil the sender.
#[tokio::test]
async fn test_request_after_reader_exit_returns_connection_aborted() {
    let (socket_path, tx_shutdown, _server, _dir) = start_test_server().await;
    create_user_via_admin(&_server, "alice").await;

    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let _user = instance.login_user("alice", None).await.unwrap();
    let conn = remote_conn(&instance);

    // Shut the daemon down and wait for the reader task to actually drain
    // the closed socket. 250ms is enormous on a local socket but absorbs
    // CI jitter and the scheduler hop into `run_reader_task`'s EOF arm.
    drop(tx_shutdown);
    tokio::time::sleep(Duration::from_millis(250)).await;

    // Any new request must bail with `ConnectionAborted` within the
    // timeout — emphatically *not* hang waiting for a response.
    let result = tokio::time::timeout(Duration::from_secs(2), conn.get_instance_metadata())
        .await
        .expect("request after reader exit must not hang");
    let err = result.expect_err("request after reader exit must return Err");
    assert!(
        matches!(&err, eidetica::Error::Io(e) if e.kind() == std::io::ErrorKind::ConnectionAborted),
        "expected ConnectionAborted, got: {err:?}",
    );
}

/// Regression: dropping the last client handle must close the socket so the
/// daemon tears the connection down.
///
/// The reader task holds a strong `Arc<RemoteConnectionInner>` and `inner`
/// owns the socket's `WriteHalf`, so without an explicit teardown token the
/// two halves pin each other: the reader parks in `read_frame` until the
/// daemon EOFs, and the daemon only EOFs once the client's socket closes.
/// The daemon-side subscription — a per-database write callback on its
/// `Instance` — would then survive forever, with the daemon fanning
/// notifications into a channel nobody drains.
#[tokio::test]
async fn test_client_drop_releases_daemon_subscription() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    {
        let instance = Instance::connect(format!("unix://{}", socket_path.display()))
            .await
            .unwrap();
        let mut user = instance.login_user("alice", None).await.unwrap();
        let pubkey = user.get_default_key().unwrap();

        let mut settings = eidetica::crdt::Doc::new();
        settings.set("name", "drop_release_test");
        let db = user.create_database(settings, &pubkey).await.unwrap();

        // Detach so the subscription outlives the callback handle: this
        // asserts teardown comes from the connection closing, not from
        // `WriteCallback::drop` unregistering.
        db.on_write(|_event, _db| async { Ok(()) })
            .await
            .unwrap()
            .detach();

        assert!(
            format!("{server:?}").contains("<1 per-db callbacks>"),
            "daemon should hold the subscription while the client is alive: {server:?}"
        );
    }

    // The daemon scrubs via its `ConnectionGuard` once it sees EOF.
    for _ in 0..100 {
        if format!("{server:?}").contains("<0 per-db callbacks>") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("daemon still holds the subscription after the client dropped: {server:?}");
}

/// A user's non-default, per-database key must be usable to open that database
/// over a service connection.
///
/// A `User` holds many keys, and `User::add_private_key` + `create_database`
/// is the ordinary way to give one database its own signing identity. The
/// resulting tree's `auth_settings` grant that per-database key and **not**
/// the user's default key, which is also the pubkey the connection logged in
/// with. Opening such a database from a connected instance must therefore
/// travel as the per-database identity end to end — including the root-entry
/// existence probe — or the daemon's per-tree gate denies a database the user
/// legitimately holds a key for.
#[tokio::test]
async fn test_open_database_with_non_login_per_database_key() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;

    // Server-side: give the database its own key, distinct from alice's
    // default (login) key, and create it under that key so the tree's auth
    // settings bind only the per-database key.
    let mut server_user = server.login_user("alice", None).await.unwrap();
    let login_key = server_user.get_default_key().unwrap();
    let per_db_key = server_user.add_private_key(Some("per-db")).await.unwrap();
    assert_ne!(
        per_db_key, login_key,
        "the per-database key must differ from the login key for this to be a real test"
    );

    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "per_db_key_database");
    let root_id = server_user
        .create_database(settings, &per_db_key)
        .await
        .unwrap()
        .root_id()
        .clone();

    // Client: log in over the socket (session identity = the login key) and
    // open the database under the per-database key.
    let instance = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let user = instance.login_user("alice", None).await.unwrap();
    assert_eq!(
        user.get_default_key().unwrap(),
        login_key,
        "the connection's session identity is the login key"
    );
    assert_eq!(
        user.find_key(&root_id).unwrap(),
        Some(per_db_key.clone()),
        "the user tracks the database under its per-database key"
    );

    let database = user
        .open_database_with_key(&root_id, &per_db_key)
        .await
        .expect("opening a database under the user's own per-database key must be permitted");

    assert_eq!(database.root_id(), &root_id);
    // The handle must be usable, not merely constructible: a read travels as
    // the per-database identity through the same gate.
    let name = database.get_name().await.unwrap();
    assert_eq!(name, "per_db_key_database");
}

/// Negative control for the test above: a key the user does not hold cannot be
/// used to open the database, so the fix widens nothing beyond the user's own
/// keys.
#[tokio::test]
async fn test_open_database_with_unheld_key_is_rejected() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "alice").await;
    create_user_via_admin(&server, "mallory").await;

    let mut alice_server = server.login_user("alice", None).await.unwrap();
    let alice_db_key = alice_server.add_private_key(Some("per-db")).await.unwrap();
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "alices_database");
    let root_id = alice_server
        .create_database(settings, &alice_db_key)
        .await
        .unwrap()
        .root_id()
        .clone();

    let mallory_inst = Instance::connect(format!("unix://{}", socket_path.display()))
        .await
        .unwrap();
    let mallory = mallory_inst.login_user("mallory", None).await.unwrap();

    let err = mallory
        .open_database_with_key(&root_id, &alice_db_key)
        .await
        .expect_err("a user must not open a database under a key they do not hold");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("key") || msg.contains("permission") || msg.contains("auth"),
        "expected a key/permission error, got: {err}",
    );
}
