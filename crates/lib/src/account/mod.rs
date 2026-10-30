//! Account system for Eidetica.
//!
//! Provides AccountSession access with per-account key storage, database tracking,
//! and sync preferences.
//!
//! An [`Account`] is the private root database of a person, group or app, reached
//! through [`AccountSession::account`]. Its root ID is distinct from the
//! instance-local login UUID. Applications store and open direct database
//! references through the session ([`AccountSession::set_reference`],
//! [`AccountSession::open_reference`]); there is no mandatory catalog.
//!
//! Credentials, key mappings and activation preferences are installation-local;
//! Account Entries carry references and public Auth. Enrollment remains separate.
//!
//! # Architecture
//!
//! - **[`Instance`](crate::Instance)**: Manages infrastructure (local login records, system databases, backends)
//! - **[`AccountSession`]**: Handles contextual operations (database access, key management) via sessions
//!
//! Native local records hold [`types::AccountCredentials`], additional keys and
//! per-installation tracking. They are not Entries or inputs to Account sync.
//! The Account's private database stores application references and public Auth;
//! the existing device-Read grant is retained without redesigning device authority.
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
mod credentials;
pub mod crypto;
mod signer;
pub use credentials::{CredentialBundle, UnlockedCredentials};
pub use signer::AccountSigner;
pub mod errors;
pub mod key_manager;
pub(crate) mod local_logins;
pub(crate) mod local_state;
mod root;
pub mod session;
mod sharing;
pub mod system_databases;
pub mod types;

pub use admin::InstanceAdmin;
pub use errors::AccountError;
pub use key_manager::AccountKeyManager;
pub use root::Account;
pub use session::{AccountSession, DatabaseBuilder};
pub(crate) use sharing::ticket_locator;
pub use types::*;
