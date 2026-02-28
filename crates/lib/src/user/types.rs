//! Core data types for the user system

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    auth::crypto::{PrivateKey, PublicKey},
    entry::ID,
};

/// User information stored in _users database
///
/// Users are stored in a Table with auto-generated UUID primary keys.
/// The username field is used for login and must be unique.
///
/// Passwordless users have None for password fields.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserInfo {
    /// Unique username (login identifier)
    pub username: String,

    /// ID of the user's private database
    pub user_database_id: ID,

    /// Password hash (using Argon2id)
    /// None for passwordless users
    pub password_hash: Option<String>,

    /// Salt for password hashing (base64 encoded string)
    /// None for passwordless users
    pub password_salt: Option<String>,

    /// User account creation timestamp (Unix timestamp)
    pub created_at: i64,

    /// Account status
    pub status: UserStatus,
}

/// User account status
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum UserStatus {
    Active,
    Disabled,
    Locked,
}

/// User profile stored in user's private database
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserProfile {
    /// Username
    pub username: String,

    /// Display name
    pub display_name: Option<String>,

    /// Email or other contact info
    pub contact_info: Option<String>,

    /// User preferences
    pub preferences: UserPreferences,
}

/// User-specific preferences
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct UserPreferences {
    /// Default sync tracked databases
    pub default_sync_enabled: bool,

    /// Other user-specific settings
    pub properties: HashMap<String, String>,
}

/// How a user's private key is stored
///
/// Encrypted keys store AES-256-GCM ciphertext of a prefixed-string-encoded `PrivateKey`.
/// Unencrypted keys store the `PrivateKey` directly (passwordless users only).
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
    /// Key is stored unencrypted (passwordless users only)
    Unencrypted {
        /// PrivateKey stored directly — serde carries the signing algorithm tag
        key: PrivateKey,
    },
}

/// User's private key with database mappings
///
/// Keys can be either encrypted (for password-protected users) or
/// unencrypted (for passwordless single-user mode).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserKey {
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

    /// Whether this is the user's default key, which has admin access on the user's DB
    /// Only one key should be marked as default at a time
    pub is_default: bool,

    /// Database-specific SigKey mappings
    /// Maps: Database ID → SigKey used in that database's auth settings
    pub database_sigkeys: HashMap<ID, String>,
}

/// A database tracked by a user.
///
/// Stored in the user's private database "databases" Table.
/// Records which databases the user has added to their list, along with
/// which key to use and sync preferences.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrackedDatabase {
    /// Database ID
    pub database_id: ID,

    /// Which user key to use for this database
    pub key_id: PublicKey,

    /// Sync preferences for this database
    pub sync_settings: SyncSettings,
}

/// Synchronization settings for a database
///
/// Per-user-per-database sync configuration.
/// Different users may have different sync preferences for the same database.
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
/// # use eidetica::user::types::SyncSettings;
/// let settings = SyncSettings::enabled().with_interval(60);
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncSettings {
    /// Whether user wants to sync this database
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

    /// User UUIDs who have this database in their preferences
    /// (stores internal UUIDs, not usernames)
    pub users: Vec<String>,

    /// Database creation time (Unix timestamp)
    pub created_at: i64,

    /// Last modification time (Unix timestamp)
    pub last_modified: i64,

    /// Additional metadata
    pub metadata: HashMap<String, String>,
}
