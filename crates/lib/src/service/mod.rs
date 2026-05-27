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
//! all operations over a Unix socket to the daemon, backing the `RemoteBackend` seam impl.
//! `Instance::connect(path)` loads `InstanceMetadata` from the remote backend,
//! then constructs an Instance with no local secrets.
//!
//! ## Security Model
//!
//! Client-side signing. The daemon stores and serves encrypted key material and
//! signed entries but never holds plaintext user signing keys or passwords.
//!
//! - **User keys stay client-side**: clients fetch encrypted `UserCredentials` from
//!   the daemon, derive the key-encryption-key locally (Argon2id), decrypt the user's
//!   signing key in-process, and sign entries before sending them to the daemon for
//!   storage. The signing key never crosses the socket.
//! - **Authentication via challenge-response**: when the daemon needs to prove a
//!   connecting client controls a user account, the daemon issues a fresh random
//!   challenge per session and the client signs it with the user's root key. The
//!   daemon verifies against the user's public key from its auth tables. No password
//!   is sent over the wire; successful decryption of the user's signing key on the
//!   client *is* password verification.
//! - **Encrypted stores remain opaque to the daemon**: per-database encrypted CRDTs
//!   (e.g. `PasswordStore`) merge as `Vec<EncryptedBlob>` — the daemon participates
//!   in storage and sync without ever holding a content encryption key. Clients
//!   decrypt and merge in-process and may write the result back as an encrypted
//!   cache entry.
//! - **Filesystem permissions**: the socket directory is owner-only (mode 0700) and
//!   the socket itself is mode 0600 as an additional access-control layer.
//!
//! See the brain note "Service Architecture" § Security Model for the design rationale,
//! including why daemon-side signing (the earlier draft) was rejected and the
//! deferred work that grew out of that decision (hardware-backed `PrivateKey::Remote`,
//! async `sign()`, OS-keyring caching of derived encryption keys).
//!
//! ## Write Coordination
//!
//! Client writes travel as `DatabaseOp::SubmitSignedEntry` — the daemon stores
//! the entry `Unverified`, then runs its own verification pass before the
//! entry is exposed on any default read.
//!
//! On a connected setup the daemon is also the **sole publisher** of write
//! events for [`Database::on_write`](crate::Database::on_write) callbacks:
//! a connected client's `Instance::put_entry` deliberately *does not* fire
//! its local callback registry, because the daemon will round-trip a
//! `Notification::DatabaseWrite` (carried in a `ServerFrame::Notification`
//! envelope) back to every subscribed connection. A client subscribes to
//! a tree lazily on the first `Database::on_write` registration via
//! `DatabaseOp::SubscribeWrites`. Subscriptions live for the connection's
//! lifetime; disconnecting implicitly unsubscribes everything. This single
//! ordering means every subscriber — including the originating client —
//! observes callbacks in the daemon's canonical order, with full
//! `previous_tips` (no client-side placeholder).
//!
//! ## V1 Limitations
//!
//! - **`enable_sync()` on remote Instance**: A silent no-op (returns `Ok(())`)
//!   rather than building a client-side sync module that would race the
//!   daemon's own sync. The daemon either already runs sync or it does not,
//!   and the client cannot change that over the current wire surface. Future:
//!   add an admin-gated `EnableSync` RPC that delegates to the server's
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
/// Resolution order:
/// 1. `EIDETICA_SOCKET` environment variable, if set.
/// 2. `$XDG_RUNTIME_DIR/eidetica/service.sock`, if `XDG_RUNTIME_DIR` is set
///    (the standard Linux convention).
/// 3. `/tmp/eidetica-$USER/service.sock` as a last-resort fallback.
///
/// Used by the daemon CLI to choose where to bind and by
/// [`default_socket_url`] to construct the equivalent `unix://` URL for
/// `Instance::connect`.
pub fn default_socket_path() -> PathBuf {
    if let Ok(socket) = std::env::var("EIDETICA_SOCKET") {
        return PathBuf::from(socket);
    }
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime_dir)
            .join("eidetica")
            .join("service.sock")
    } else {
        let user = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
        PathBuf::from(format!("/tmp/eidetica-{user}")).join("service.sock")
    }
}

/// Default `unix://` URL for `Instance::connect`, derived from
/// [`default_socket_path`].
///
/// Convenience for apps that want to connect to the local daemon's socket
/// without writing the env / `$XDG_RUNTIME_DIR` resolution themselves:
///
/// ```ignore
/// let instance = Instance::connect(eidetica::service::default_socket_url()).await?;
/// ```
pub fn default_socket_url() -> String {
    format!("unix://{}", default_socket_path().display())
}
