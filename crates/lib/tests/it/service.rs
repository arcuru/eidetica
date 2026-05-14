//! End-to-end smoke test for the Service module.
//!
//! Verifies a real client↔daemon round-trip via the high-level `Instance` API.
//! Lower-level wire protocol tests live next to the implementation in
//! `crates/lib/src/service/server.rs`; this file exercises the public surface
//! a `cargo add eidetica` consumer would actually use.

#![cfg(all(unix, feature = "service"))]

use std::path::PathBuf;
use std::time::Duration;

use eidetica::Instance;
use eidetica::backend::database::InMemory;
use eidetica::service::ServiceServer;
use tempfile::TempDir;
use tokio::sync::watch;

/// Start a service server bound to a temp-dir socket, return (path, shutdown, server-side Instance, dir).
async fn start_server() -> (PathBuf, watch::Sender<()>, Instance, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("smoke.sock");
    let server_instance = Instance::open(Box::new(InMemory::new())).await.unwrap();
    let (tx, rx) = watch::channel(());
    let server = ServiceServer::new(server_instance.clone(), socket_path.clone());
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
    (socket_path, tx, server_instance, dir)
}

/// High-level smoke test: a remote client mutates Instance state and both views agree.
///
/// This exercises the full stack — `Instance::connect()` → `Backend::Remote` →
/// length-prefixed JSON frame over Unix socket → server dispatch → backend
/// → reverse path on response — for two real Instance operations.
#[tokio::test]
async fn smoke_create_and_list_users_over_wire() {
    let (socket_path, _shutdown, server_instance, _dir) = start_server().await;

    let client_instance = Instance::connect(&socket_path).await.unwrap();

    // Pre-state: no users on either side.
    assert!(server_instance.list_users().await.unwrap().is_empty());
    assert!(client_instance.list_users().await.unwrap().is_empty());

    // Mutate via the client — the call routes through the wire protocol.
    let _uuid = client_instance
        .create_user("smoke_user", None)
        .await
        .unwrap();

    // Server view (direct backend) and client view (over the wire) must agree.
    let server_users = server_instance.list_users().await.unwrap();
    let client_users = client_instance.list_users().await.unwrap();
    assert_eq!(server_users, vec!["smoke_user".to_string()]);
    assert_eq!(client_users, vec!["smoke_user".to_string()]);
}

/// Instance metadata is loaded by `Instance::connect()` at handshake time —
/// confirm the client sees the same instance identity as the server.
#[tokio::test]
async fn smoke_instance_identity_round_trip() {
    let (socket_path, _shutdown, server_instance, _dir) = start_server().await;

    let client_instance = Instance::connect(&socket_path).await.unwrap();

    // Both views must report the same instance identity (server's device public key).
    assert_eq!(client_instance.id(), server_instance.id());
}
