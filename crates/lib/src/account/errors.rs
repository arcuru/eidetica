//! Error types for the account system
use thiserror::Error;

use crate::entry::ID;

#[derive(Error, Debug)]
pub enum AccountError {
    #[error("Account not found: {username}")]
    AccountNotFound { username: String },

    #[error("Username already exists: {username}")]
    LoginNameAlreadyExists { username: String },

    #[error(
        "Multiple accounts detected with username '{username}' ({count} found). This indicates a race condition during account creation. Manual intervention required."
    )]
    DuplicateAccountsDetected { username: String, count: usize },

    #[error("Invalid password")]
    InvalidPassword,

    #[error("Key not found: {key_id}")]
    KeyNotFound { key_id: String },

    #[error("Key already exists: {key_id}")]
    KeyAlreadyExists { key_id: String },

    #[error("Encryption failed: {reason}")]
    EncryptionFailed { reason: String },

    #[error("Decryption failed: {reason}")]
    DecryptionFailed { reason: String },

    #[error("Operation requires admin permission")]
    InsufficientPermissions,

    #[error("No admin key available for database: {database_id}")]
    NoAdminKey { database_id: ID },

    #[error("Database not tracked: {database_id}")]
    DatabaseNotTracked { database_id: ID },

    #[error("This account is not sharing database: {database_id}")]
    DatabaseNotShared { database_id: ID },

    #[error(
        "Database handle lacks the account capability required to manage sharing: {database_id}"
    )]
    MissingDatabaseCapability { database_id: ID },

    #[error("Database not found by name: {name}")]
    DatabaseNotFoundByName { name: String },

    #[error("Account disabled: {username}")]
    AccountDisabled { username: String },

    #[error("Invalid salt length: expected {expected}, got {actual}")]
    InvalidSaltLength { expected: usize, actual: usize },

    #[error("Invalid nonce length: expected {expected}, got {actual}")]
    InvalidNonceLength { expected: usize, actual: usize },

    #[error("No key found for database: {database_id}")]
    NoKeyForDatabase { database_id: ID },

    #[error("No SigKey mapping found for key {key_id} in database {database_id}")]
    NoSigKeyMapping { key_id: String, database_id: ID },

    #[error("No SigKey found for key {key_id} in database {database_id}")]
    NoSigKeyFound { key_id: String, database_id: ID },

    #[error(
        "Database access pending for {database_id}: a key mapping exists but the database has no local data yet \
         (the bootstrap request may be awaiting approval, or the database has not synced)"
    )]
    DatabaseAccessPending { database_id: ID },

    #[error("Password required for operation: {operation}")]
    PasswordRequired { operation: String },

    #[error("Invalid key format: {reason}")]
    InvalidKeyFormat { reason: String },

    #[error("No keys available for account")]
    NoKeysAvailable,

    #[error("Duplicate store name in DatabaseBuilder: {name}")]
    DuplicateBuilderStore { name: String },
}

impl AccountError {
    /// Check if this error indicates a resource was not found.
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            AccountError::AccountNotFound { .. }
                | AccountError::KeyNotFound { .. }
                | AccountError::DatabaseNotTracked { .. }
                | AccountError::DatabaseNotFoundByName { .. }
        )
    }
}
