//! Integration tests for the Eidetica service (daemon) mode.

#![cfg(all(unix, feature = "service"))]

use std::path::PathBuf;
use std::time::Duration;

use eidetica::Instance;
use eidetica::backend::database::InMemory;
use eidetica::service::ServiceServer;
use tempfile::TempDir;
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
#[ignore = "login_user over RPC requires the challenge-response auth flow (Branch C)"]
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
#[ignore = "login_user over RPC requires the challenge-response auth flow (Branch C)"]
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
