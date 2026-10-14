//! Account system for Eidetica.
//!
//! Provides AccountSession access with per-account key storage, database tracking,
//! and sync preferences.
//!
//! An Account is an ordinary private [`Database`](crate::Database), reached through
//! [`AccountSession::account`]. Its root ID is distinct from the instance-local
//! login UUID. It can represent a person, group or app. No extra Database wrapper
//! or mandatory application catalog is needed.
//!
//! This API retains the current key/tracking layout. It does not implement
//! portable secret-free state, cross-instance account enrollment or new Auth.
//!
//! # Architecture
//!
//! - **[`Instance`](crate::Instance)**: Manages infrastructure (local login records, system databases, backends)
//! - **[`AccountSession`]**: Handles contextual operations (database access, key management) via sessions
//!
//! The account's root signing key is stored in `_users` as [`types::AccountCredentials`]
//! (encrypted or unencrypted). Each account has a private database (`_user_<username>`)
//! owned by the account's root key (Admin(0)), with the device key granted Read permission.
//! The private database stores:
//! - **keys**: Additional Ed25519 keypairs with per-database SigKey mappings
//! - **databases**: Tracked databases with sync preferences
//! - **settings**: Account configuration
//!
//! # Key Management
//!
//! Keys are Ed25519 keypairs. The root key is encrypted at rest using the account's password
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
//! When multiple accounts track the same database, settings are merged to use the most aggressive settings.

pub mod admin;
pub mod crypto;
pub mod errors;
pub mod key_manager;
pub mod session;
mod sharing;
pub mod system_databases;
pub mod types;

pub use admin::InstanceAdmin;
pub use errors::AccountError;
pub use key_manager::AccountKeyManager;
pub use session::{AccountSession, DatabaseBuilder};
pub(crate) use sharing::ticket_locator;
pub use types::*;
