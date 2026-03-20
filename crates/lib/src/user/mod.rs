//! User system for Eidetica.
//!
//! Provides multi-user account management with per-user key storage, database tracking,
//! and sync preferences.
//!
//! # Architecture
//!
//! - **[`Instance`](crate::Instance)**: Manages infrastructure (user accounts, system databases, backends)
//! - **[`User`]**: Handles contextual operations (database access, key management) via sessions
//!
//! The user's root signing key is stored in `_users` as [`types::UserCredentials`]
//! (encrypted or unencrypted). Each user has a private database (`_user_<username>`)
//! owned by the user's root key (Admin(0)), with the device key granted Read permission.
//! The private database stores:
//! - **keys**: Additional Ed25519 keypairs with per-database SigKey mappings
//! - **databases**: Tracked databases with sync preferences
//! - **settings**: User configuration
//!
//! # Key Management
//!
//! Keys are Ed25519 keypairs. The root key is encrypted at rest using the user's password
//! (Argon2id key derivation + AES-256-GCM). Decryption failure IS password verification —
//! no separate password hash is stored. Each key can authenticate with multiple databases
//! via SigKey mappings, which are auto-discovered when tracking a database.
//!
//! # Sync Settings
//!
//! Per-database sync preferences ([`types::SyncSettings`]):
//! - `sync_enabled`: Master switch
//! - `sync_on_commit`: Trigger sync on each commit
//! - `interval_seconds`: Periodic sync interval
//!
//! When multiple users track the same database, settings are merged to use the most aggressive settings.

pub mod crypto;
pub mod errors;
pub mod key_manager;
pub mod session;
pub mod system_databases;
pub mod types;

pub use errors::UserError;
pub use key_manager::UserKeyManager;
pub use session::User;
pub use types::*;
