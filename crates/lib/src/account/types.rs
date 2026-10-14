//! Core data types for the account system

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    auth::{
        SigKey,
        crypto::{PrivateKey, PublicKey},
    },
    entry::ID,
};

/// Credentials for unlocking an account's root signing key.
///
/// For password-protected accounts, contains the salt (for Argon2id key derivation)
/// and the encrypted root key (AES-256-GCM). Decryption failure IS password
/// verification — no separate password hash is needed.
/// For passwordless accounts, contains the unencrypted root key directly.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountCredentials {
    /// Public key (available without decryption)
    pub root_key_id: PublicKey,

    /// Encrypted or unencrypted root private key
    pub root_key: KeyStorage,

    /// Salt for Argon2id key derivation (None for passwordless)
    pub password_salt: Option<String>,
}

/// Instance-local login registration stored in the `_users` database.
///
/// The Table UUID and `username` identify a login on this instance, not the
/// Account itself. `account_id` identifies the ordinary private root database.
/// Credentials retain their existing storage layout; this is not a portable
/// export format or a claim of instance-local private-key isolation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountInfo {
    /// Unique username (login identifier)
    pub username: String,

    /// ID of the account's private database
    #[serde(rename = "user_database_id")]
    pub account_id: ID,

    /// Credentials containing the account's root signing key
    pub credentials: AccountCredentials,

    /// Account creation timestamp (Unix timestamp)
    pub created_at: i64,

    /// Account status
    pub status: AccountStatus,
}

/// Account status
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AccountStatus {
    Active,
    Disabled,
    Locked,
}

/// Account profile stored in account's private database
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountProfile {
    /// Username
    pub username: String,

    /// Display name
    pub display_name: Option<String>,

    /// Email or other contact info
    pub contact_info: Option<String>,

    /// Account preferences
    pub preferences: AccountPreferences,
}

/// Account-specific preferences
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct AccountPreferences {
    /// Default sync tracked databases
    pub default_sync_enabled: bool,

    /// Other account-specific settings
    pub properties: HashMap<String, String>,
}

/// How an account's private key is stored
///
/// Encrypted keys store AES-256-GCM ciphertext of a prefixed-string-encoded `PrivateKey`.
/// Unencrypted keys store the `PrivateKey` directly (passwordless accounts only).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum KeyStorage {
    /// Key is encrypted with a password-derived key (AES-256-GCM)
    Encrypted {
        /// Encryption algorithm identifier
        algorithm: String,
        /// Encrypted prefixed-string-encoded PrivateKey
        ciphertext: Vec<u8>,
        /// Encryption nonce/IV (12 bytes for AES-GCM)
        nonce: Vec<u8>,
    },
    /// Key is stored unencrypted (passwordless accounts only)
    Unencrypted {
        /// PrivateKey stored directly — serde carries the signing algorithm tag
        key: PrivateKey,
    },
}

/// Account's private key with database mappings
///
/// Keys can be either encrypted (for password-protected accounts) or
/// unencrypted (for passwordless single-account mode).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountKey {
    /// Key identifier (the public key)
    pub key_id: PublicKey,

    /// Key storage (encrypted ciphertext or plaintext PrivateKey)
    pub storage: KeyStorage,

    /// Display name for this key
    pub display_name: Option<String>,

    /// When this key was created (Unix timestamp)
    pub created_at: i64,

    /// Last time this key was used (Unix timestamp)
    pub last_used: Option<i64>,

    /// Whether this is the account's default key, which has admin access on the account's DB
    /// Only one key should be marked as default at a time
    pub is_default: bool,

    /// Database-specific SigKey mappings
    /// Maps: Database ID → SigKey identity for that database
    ///
    /// `None` = default identity (pubkey derived from this key's `key_id`)
    /// `Some(sigkey)` = non-default identity (global wildcard, name-based, delegation)
    pub database_sigkeys: HashMap<ID, Option<SigKey>>,
}

/// A database tracked by a account.
///
/// Stored in the account's private database "databases" Table.
/// Records which databases the account has added to their list, along with
/// which key to use and sync preferences.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TrackedDatabase {
    /// Database ID
    pub database_id: ID,

    /// Which account key to use for this database
    pub key_id: PublicKey,

    /// Sync preferences for this database
    pub sync_settings: SyncSettings,
}

/// Synchronization settings for a database
///
/// Per-account-per-database sync configuration.
/// Different accounts may have different sync preferences for the same database.
///
/// # Construction
///
/// Use factory methods for common configurations:
/// - `SyncSettings::disabled()` — sync disabled (this is the `Default`)
/// - `SyncSettings::enabled()` — sync enabled, no sync on commit
/// - `SyncSettings::on_commit()` — sync enabled with sync on commit on every transaction
///
/// Chain `.with_interval()` to override the periodic sync interval:
/// ```
/// # use eidetica::account::types::SyncSettings;
/// let settings = SyncSettings::enabled().with_interval(60);
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SyncSettings {
    /// Whether account wants to sync this database
    pub sync_enabled: bool,

    /// Sync on commit
    /// Whether to sync after every commit
    pub sync_on_commit: bool,

    /// Sync interval in seconds (for periodic sync).
    /// `None` uses the background sync default interval.
    pub interval_seconds: Option<u64>,

    /// Additional sync configuration
    pub properties: HashMap<String, String>,
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self::disabled()
    }
}

impl SyncSettings {
    /// Sync disabled. This is the `Default`.
    pub fn disabled() -> Self {
        Self {
            sync_enabled: false,
            sync_on_commit: false,
            interval_seconds: None,
            properties: HashMap::new(),
        }
    }

    /// Sync enabled, no sync on commit.
    pub fn enabled() -> Self {
        Self {
            sync_enabled: true,
            ..Self::disabled()
        }
    }

    /// Sync enabled with sync on commit on every transaction.
    pub fn on_commit() -> Self {
        Self {
            sync_enabled: true,
            sync_on_commit: true,
            ..Self::disabled()
        }
    }

    /// Set periodic sync interval in seconds.
    pub fn with_interval(mut self, seconds: u64) -> Self {
        self.interval_seconds = Some(seconds);
        self
    }
}

/// Database tracking information in _databases table
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DatabaseTracking {
    /// Database ID
    pub database_id: ID,

    /// Cached database name (for quick lookup)
    pub name: Option<String>,

    /// AccountSession UUIDs who have this database in their preferences
    /// (stores internal UUIDs, not usernames)
    pub users: Vec<String>,

    /// Database creation time (Unix timestamp)
    pub created_at: i64,

    /// Last modification time (Unix timestamp)
    pub last_modified: i64,

    /// Additional metadata
    pub metadata: HashMap<String, String>,
}
