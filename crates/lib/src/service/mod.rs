//! Local service (daemon) mode for Eidetica.
//!
//! This module enables running Eidetica as a local daemon that serves an Instance
//! to multiple client processes over a Unix domain socket. The primary motivation is
//! shared storage: multiple CLI tools and applications can operate on the same
//! Eidetica data without each process opening its own backend.
//!
//! ## Architecture
//!
//! The RPC boundary sits at the storage operation level. A `RemoteConnection` forwards
//! all operations over a Unix socket to the daemon, wrapped in a `Backend::Remote` variant.
//! `Instance::connect(path)` loads `InstanceMetadata` from the remote backend,
//! then constructs an Instance with no local secrets.
//!
//! ## Security Model
//!
//! The daemon acts as a key server, similar to PostgreSQL over a Unix socket:
//!
//! - **Signing keys stay server-side**: the daemon holds decrypted private keys in
//!   memory for authenticated connections. Clients never receive key material.
//! - **Password login**: clients authenticate by sending a password at connection
//!   start. The server verifies it (Argon2id), decrypts the user's keys, and holds
//!   them in per-connection state for the connection's lifetime.
//! - **Connection = session**: no tokens or handles. Unix socket lifecycle manages
//!   auth state. Connection close drops the decrypted keys.
//! - **Server-side signing**: entry signing happens on the server. The client holds a
//!   `PrivateKey::Remote` variant that proxies async `sign()` calls to the server
//!   via RPC.
//! - **Filesystem permissions**: the socket directory is owner-only (mode 0700) as
//!   an additional access control layer.
//!
//! ## Write Coordination
//!
//! Client writes go through the daemon's backend (via `Put` RPC), then a
//! `NotifyEntryWritten` RPC tells the server to fire its write callbacks (sync triggers,
//! etc.) without re-storing the entry.
//!
//! ## V1 Limitations
//!
//! - **No server-push notifications**: Clients see latest state on each request but
//!   are not notified when the server receives entries from sync peers. Future: evolve
//!   to a bidirectional protocol where the server sends unsolicited `Notification`
//!   frames alongside responses. The frame envelope gains a type tag
//!   (`Request | Response | Notification`). The client needs a background reader task
//!   that routes responses to pending requests and notifications to callbacks.
//!
//! - **`enable_sync()` on remote Instance**: Creates a client-side sync module
//!   (not useful). Future: add an `EnableSync` RPC that delegates to the server's
//!   Instance, and similarly for `sync()`, `flush_sync()`, etc.

pub mod client;
pub mod error;
pub mod protocol;
pub mod server;

pub use client::RemoteConnection;
pub use server::ServiceServer;

use std::path::PathBuf;

/// Default socket path for the Eidetica service.
///
/// Uses `$XDG_RUNTIME_DIR/eidetica/service.sock` if available,
/// falling back to `/tmp/eidetica-$USER/service.sock`.
pub fn default_socket_path() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime_dir)
            .join("eidetica")
            .join("service.sock")
    } else {
        let user = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
        PathBuf::from(format!("/tmp/eidetica-{user}")).join("service.sock")
    }
}
