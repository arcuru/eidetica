//! Core data types for the user system

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::entry::ID;

/// User information stored in _users database
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserInfo {
    /// Unique user identifier
    pub user_id: String,

    /// ID of the user's private database
    pub user_database_id: ID,

    /// Password hash (using Argon2id)
    pub password_hash: String,

    /// Salt for password hashing (base64 encoded string)
    pub password_salt: String,

    /// User account creation timestamp
    pub created_at: u64,

    /// Last login timestamp
    pub last_login: Option<u64>,

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
    /// User ID
    pub user_id: String,

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

/// User's encrypted private key with database mappings
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserKey {
    /// Local key identifier (public key or special name like "_device_key")
    pub key_id: String,

    /// Encrypted private key (encrypted with user's password-derived key)
    pub encrypted_private_key: Vec<u8>,

    /// Encryption nonce/IV (12 bytes for AES-GCM)
    pub nonce: Vec<u8>,

    /// Display name for this key
    pub display_name: Option<String>,

    /// When this key was created
    pub created_at: u64,

    /// Last time this key was used
    pub last_used: Option<u64>,

    /// Database-specific SigKey mappings
    /// Maps: Database ID → SigKey used in that database's auth settings
    pub database_sigkeys: HashMap<ID, String>,
}

/// User's preferences for a specific database
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserDatabasePreferences {
    /// Database ID
    pub database_id: ID,

    /// Whether user wants to sync this database
    pub sync_enabled: bool,

    /// Sync settings specific to this database
    pub sync_settings: SyncSettings,

    /// User's preferred SigKey for this database
    pub preferred_sigkey: Option<String>,

    /// Custom labels or notes
    pub notes: Option<String>,
}

/// Synchronization settings for a database
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SyncSettings {
    /// Sync interval in seconds
    pub interval_seconds: Option<u64>,

    /// Sync on commit
    /// Whether to sync after every commit
    pub sync_on_commit: bool,

    /// Additional sync configuration
    pub properties: HashMap<String, String>,
}

/// Database tracking information in _databases table
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DatabaseTracking {
    /// Database ID
    pub database_id: ID,

    /// Cached database name (for quick lookup)
    pub name: Option<String>,

    /// Users who have this database in their preferences
    pub users: Vec<String>,

    /// Database creation time
    pub created_at: u64,

    /// Last modification time
    pub last_modified: u64,

    /// Additional metadata
    pub metadata: HashMap<String, String>,
}

/// Update operation for database tracking
#[derive(Clone, Debug)]
pub enum DatabaseTrackingUpdate {
    AddUser(String),
    RemoveUser(String),
    UpdateMetadata(HashMap<String, String>),
}

/// Update operation for database preferences
#[derive(Clone, Debug)]
pub enum DatabasePreferenceUpdate {
    EnableSync(bool),
    SetSyncSettings(SyncSettings),
    SetPreferredSigKey(String),
    UpdateNotes(String),
}
