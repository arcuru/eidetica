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
//! Each user has a private database (`user:<username>`) storing:
//! - **keys**: Encrypted Ed25519 keypairs with per-database SigKey mappings
//! - **databases**: Tracked databases with sync preferences
//! - **settings**: User configuration
//!
//! # Key Management
//!
//! Keys are Ed25519 keypairs, encrypted at rest using the user's password (Argon2id).
//! Each key can authenticate with multiple databases via SigKey mappings, which are
//! auto-discovered when adding a database.
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
pub mod system_databases;
pub mod types;
pub mod user_session;

pub use errors::UserError;
pub use key_manager::UserKeyManager;
pub use types::*;
pub use user_session::User;
