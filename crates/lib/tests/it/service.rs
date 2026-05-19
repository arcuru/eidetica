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

/// Helper: log in as the bootstrap admin and create a user server-side.
async fn create_user_via_admin(server: &Instance, username: &str) {
    let mut admin = server.login_user("admin", Some("admin")).await.unwrap();
    admin.create_user(username, None).await.unwrap();
}

/// Helper: with the admin bootstrapped at instance creation, log in as admin,
/// create a test user, connect and authenticate as that user, create a
/// database, and return (client-instance, root_id, identity).
///
/// The database is created server-side so auth_settings bind the user's
/// key as Admin(0). The client authenticates via the remote connection.
async fn setup_db(
    server: &Instance,
    socket_path: &PathBuf,
    username: &str,
) -> (Instance, eidetica::entry::ID, eidetica::auth::types::SigKey) {
    // Admin was created by Instance::open bootstrap — use it to create the test user.
    let mut admin = server.login_user("admin", Some("admin")).await.unwrap();
    admin.create_user(username, None).await.unwrap();

    let instance = Instance::connect(socket_path).await.unwrap();
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

    let sigkeys =
        eidetica::Database::find_sigkeys(server, &root_id, &pubkey)
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
    let _instance = Instance::connect(&socket_path).await.unwrap();
    let users = server.list_users().await.unwrap();
    // Admin user bootstrapped at Instance creation
    assert_eq!(users.len(), 1);
    assert_eq!(users[0], "admin");
}

#[tokio::test]
async fn test_user_lifecycle() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    // Admin is bootstrapped — use it to create test user
    let mut admin = server.login_user("admin", Some("admin")).await.unwrap();
    admin.create_user("alice", None).await.unwrap();

    let instance = Instance::connect(&socket_path).await.unwrap();

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
    let instance = Instance::connect(&socket_path).await.unwrap();
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
    let instance = Instance::connect(&socket_path).await.unwrap();

    let conn = remote_conn(&instance);
    let result = conn
        .db_get_entry(
            eidetica::entry::ID::default(),
            eidetica::auth::types::SigKey::default(),
            eidetica::entry::ID::from_bytes("nonexistent"),
        )
        .await;
    let err =
        result.expect_err("server must reject database op on unauthenticated connection");
    assert!(
        !err.is_not_found(),
        "expected auth error, got NotFound — gate not enforced; {err}"
    );
}

#[tokio::test]
async fn test_concurrent_clients() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "bob").await;

    let instance1 = Instance::connect(&socket_path).await.unwrap();
    let instance2 = Instance::connect(&socket_path).await.unwrap();

    let _user1 = instance1.login_user("bob", None).await.unwrap();
    let user2 = instance2.login_user("bob", None).await.unwrap();
    assert_eq!(user2.username(), "bob");
}

#[tokio::test]
async fn test_instance_connect_convenience() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    create_user_via_admin(&server, "charlie").await;

    let _instance = Instance::connect(&socket_path).await.unwrap();
    let users = server.list_users().await.unwrap();
    assert_eq!(users, vec!["admin", "charlie"]);
}

#[tokio::test]
async fn test_instance_identity_round_trip() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let client = Instance::connect(&socket_path).await.unwrap();

    assert_eq!(client.id(), server.id());
}

/// Open a raw connection to the daemon and complete the protocol handshake.
async fn raw_handshake(
    socket_path: &PathBuf,
) -> (ReadHalf<UnixStream>, WriteHalf<UnixStream>) {
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
    let resp: ServiceResponse = read_frame(&mut reader).await.unwrap().unwrap();
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

    let instance = Instance::connect(&socket_path).await.unwrap();
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
    let (instance, root_id, identity) =
        setup_db(&server, &socket_path, "alice").await;

    // Add a commit server-side so tips diverge.
    let mut server_user = server.login_user("alice", None).await.unwrap();
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
    let wire_tips = conn
        .get_verified_tips(root_id, identity)
        .await
        .unwrap();

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

    let instance = Instance::connect(&socket_path).await.unwrap();
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
    let (instance, root_id, identity) =
        setup_db(&server, &socket_path, "alice").await;

    // Write data server-side.
    let mut server_user = server.login_user("alice", None).await.unwrap();
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
            tips,
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
    let (instance, root_id, identity) =
        setup_db(&server, &socket_path, "alice").await;

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
    let mut entry = Entry::builder(root_id.clone())
        .set_parents(parents)
        .set_subtree_data("submitted", b"{\"submitted\":true}")
        .build()
        .unwrap();
    let signature = sign_entry(&entry, &signing_key).unwrap();
    entry.sig.sig = Some(signature);
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
    let (instance, root_id, identity) =
        setup_db(&server, &socket_path, "alice").await;

    let password = "hunter2";
    let secret_data = "top-secret-value";

    // Write encrypted data server-side.
    let mut server_user = server.login_user("alice", None).await.unwrap();
    let server_key_pub = server_user.get_default_key().unwrap();
    let server_sk = server_user.get_signing_key(&server_key_pub).unwrap();
    let db = eidetica::Database::open(&server, &root_id)
        .await
        .unwrap()
        .with_key(server_sk);
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
async fn test_backend_get_tips_allowed_for_owner() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (instance, root_id, identity) =
        setup_db(&server, &socket_path, "alice").await;

    let conn = remote_conn(&instance);
    let tips = conn.get_verified_tips(root_id, identity).await.unwrap();
    assert!(
        !tips.is_empty(),
        "newly created database must have at least one tip"
    );
}

/// Negative control: unauthorised user is rejected.
#[tokio::test]
async fn test_backend_get_tips_denied_for_unauthorised_user() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (_alice_inst, alice_db_id, _alice_identity) =
        setup_db(&server, &socket_path, "alice").await;

    // Create bob and try to read alice's database.
    create_user_via_admin(&server, "bob").await;
    let bob_inst = Instance::connect(&socket_path).await.unwrap();
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
    let (alice_inst, alice_root, alice_identity) =
        setup_db(&server, &socket_path, "alice").await;

    let alice_conn = remote_conn(&alice_inst);

    // Owner can read her own entry (positive control).
    alice_conn
        .db_get_entry(alice_root.clone(), alice_identity, alice_root.clone())
        .await
        .expect("owner must be able to GetEntry in her own database");

    // Bob is logged in but has no access.
    create_user_via_admin(&server, "bob").await;
    let bob_inst = Instance::connect(&socket_path).await.unwrap();
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

    let instance = Instance::connect(&socket_path).await.unwrap();
    let _admin = instance.login_user("admin", Some("admin")).await.unwrap();

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
    let resp: ServiceResponse = read_frame(&mut reader).await.unwrap().unwrap();
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
    let resp: ServiceResponse = read_frame(&mut reader).await.unwrap().unwrap();
    assert!(matches!(resp, ServiceResponse::Error(_)));

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

/// End-to-end test for `RemoteDatabaseOps`.
#[tokio::test]
async fn test_remote_database_ops_e2e() {
    let (socket_path, _tx, server, _dir) = start_test_server().await;
    let (instance, root_id, identity) =
        setup_db(&server, &socket_path, "alice").await;

    // Write data server-side.
    let mut server_user = server.login_user("alice", None).await.unwrap();
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
            verified_tips,
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
