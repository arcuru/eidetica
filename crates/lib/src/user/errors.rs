//! Error types for the user system
use thiserror::Error;

#[derive(Error, Debug)]
pub enum UserError {
    #[error("User not found: {username}")]
    UserNotFound { username: String },

    #[error("Username already exists: {username}")]
    UsernameAlreadyExists { username: String },

    #[error(
        "Multiple users detected with username '{username}' ({count} found). This indicates a race condition during user creation. Manual intervention required."
    )]
    DuplicateUsersDetected { username: String, count: usize },

    #[error("Invalid password")]
    InvalidPassword,

    #[error("Password verification failed")]
    PasswordVerificationFailed,

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
    NoAdminKey { database_id: String },

    #[error("Database not tracked: {database_id}")]
    DatabaseNotTracked { database_id: String },

    #[error("User account disabled: {username}")]
    UserDisabled { username: String },

    #[error("Invalid salt length: expected {expected}, got {actual}")]
    InvalidSaltLength { expected: usize, actual: usize },

    #[error("Invalid nonce length: expected {expected}, got {actual}")]
    InvalidNonceLength { expected: usize, actual: usize },

    #[error("No key found for database: {database_id}")]
    NoKeyForDatabase { database_id: crate::entry::ID },

    #[error("No SigKey mapping found for key {key_id} in database {database_id}")]
    NoSigKeyMapping {
        key_id: String,
        database_id: crate::entry::ID,
    },

    #[error("No SigKey found for key {key_id} in database {database_id}")]
    NoSigKeyFound {
        key_id: String,
        database_id: crate::entry::ID,
    },

    #[error("Password required for operation: {operation}")]
    PasswordRequired { operation: String },

    #[error("Invalid key format: {reason}")]
    InvalidKeyFormat { reason: String },

    #[error("No keys available for user")]
    NoKeysAvailable,
}

impl UserError {
    /// Check if this error indicates a resource was not found.
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            UserError::UserNotFound { .. }
                | UserError::KeyNotFound { .. }
                | UserError::DatabaseNotTracked { .. }
        )
    }
}
