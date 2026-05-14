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

pub(crate) mod cache;
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
