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
    let (socket_path, _tx, _server, _dir) = start_test_server().await;
    let instance = Instance::connect(&socket_path).await.unwrap();
    // Should be able to list users (system databases loaded)
    let users = instance.list_users().await.unwrap();
    assert!(users.is_empty());
}

#[tokio::test]
#[ignore = "login_user over RPC requires the TrustedLogin client flow (Branch C chunk 4)"]
async fn test_user_lifecycle() {
    let (socket_path, _tx, _server, _dir) = start_test_server().await;
    let instance = Instance::connect(&socket_path).await.unwrap();

    // Create user
    instance.create_user("alice", None).await.unwrap();

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
    let (socket_path, _tx, _server, _dir) = start_test_server().await;
    let instance = Instance::connect(&socket_path).await.unwrap();

    // Try to get a nonexistent entry
    let result = instance
        .backend()
        .get(&eidetica::entry::ID::from_bytes("nonexistent"))
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().is_not_found());
}

#[tokio::test]
#[ignore = "login_user over RPC requires the TrustedLogin client flow (Branch C chunk 4)"]
async fn test_concurrent_clients() {
    let (socket_path, _tx, _server, _dir) = start_test_server().await;

    // Connect two clients
    let instance1 = Instance::connect(&socket_path).await.unwrap();
    let instance2 = Instance::connect(&socket_path).await.unwrap();

    // Create user from client 1
    instance1.create_user("bob", None).await.unwrap();

    // Login from client 2
    let user = instance2.login_user("bob", None).await.unwrap();
    assert_eq!(user.username(), "bob");
}

#[tokio::test]
async fn test_instance_connect_convenience() {
    let (socket_path, _tx, _server, _dir) = start_test_server().await;

    // Use the convenience API
    let instance = Instance::connect(&socket_path).await.unwrap();

    // Verify basic operations work
    instance.create_user("charlie", None).await.unwrap();
    let users = instance.list_users().await.unwrap();
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
        ServiceResponse::TrustedLoginChallenge { challenge } => challenge,
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
