//! Error types for the user system
use thiserror::Error;

#[derive(Error, Debug)]
pub enum UserError {
    #[error("User not found: {user_id}")]
    UserNotFound { user_id: String },

    #[error("User already exists: {user_id}")]
    UserAlreadyExists { user_id: String },

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

    #[error("No implicit user available (use multi-user mode)")]
    NoImplicitUser,

    #[error("Operation requires admin permission")]
    InsufficientPermissions,

    #[error("No admin key available for database: {database_id}")]
    NoAdminKey { database_id: String },

    #[error("Database preference not found: {database_id}")]
    DatabasePreferenceNotFound { database_id: String },

    #[error("User account disabled: {user_id}")]
    UserDisabled { user_id: String },

    #[error("Invalid salt length: expected {expected}, got {actual}")]
    InvalidSaltLength { expected: usize, actual: usize },

    #[error("Invalid nonce length: expected {expected}, got {actual}")]
    InvalidNonceLength { expected: usize, actual: usize },
}
