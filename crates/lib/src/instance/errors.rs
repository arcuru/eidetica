//! Base database error types for the Eidetica library.
//!
//! This module defines structured error types for tree operations, entry management,
//! and database operations, providing better error context and type safety compared to string-based errors.

use thiserror::Error;

use crate::entry::ID;

/// Errors that can occur during base database operations.
///
/// # Stability
///
/// - New variants may be added in minor versions (enum is `#[non_exhaustive]`)
/// - Existing variants will not be removed in minor versions
/// - Field additions/changes require a major version bump
/// - Helper methods like `is_*()` provide stable APIs
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum InstanceError {
    /// Database not found by name.
    #[error("Database not found: {name}")]
    DatabaseNotFound {
        /// The name of the database that was not found
        name: String,
    },

    /// Database already exists with the given name.
    #[error("Database already exists: {name}")]
    DatabaseAlreadyExists {
        /// The name of the database that already exists
        name: String,
    },

    /// Entry does not belong to the specified database.
    #[error("Entry '{entry_id}' does not belong to database '{database_id}'")]
    EntryNotInDatabase {
        /// The ID of the entry
        entry_id: ID,
        /// The ID of the database
        database_id: ID,
    },

    /// Entry not found by ID.
    #[error("Entry not found: {entry_id}")]
    EntryNotFound {
        /// The ID of the entry that was not found
        entry_id: ID,
    },

    /// Transaction has already been committed and cannot be modified.
    #[error("Transaction has already been committed")]
    TransactionAlreadyCommitted,

    /// Cannot create transaction with empty tips.
    #[error("Cannot create transaction with empty tips")]
    EmptyTipsNotAllowed,

    /// Tip entry does not belong to the specified database.
    #[error("Tip entry '{tip_id}' does not belong to database '{database_id}'")]
    InvalidTip {
        /// The ID of the invalid tip entry
        tip_id: ID,
        /// The ID of the database
        database_id: ID,
    },

    /// Signing key not found in backend storage.
    #[error("Signing key '{key_name}' not found in backend")]
    SigningKeyNotFound {
        /// The name of the signing key that was not found
        key_name: String,
    },

    /// Authentication is required but no key is configured.
    #[error("Authentication required but no key configured")]
    AuthenticationRequired,

    /// Device key (_device_key) not found in backend storage.
    #[error("Device key (_device_key) not found in backend")]
    DeviceKeyNotFound,

    /// No authentication configuration found.
    #[error("No authentication configuration found")]
    NoAuthConfiguration,

    /// Authentication validation failed.
    #[error("Authentication validation failed: {reason}")]
    AuthenticationValidationFailed {
        /// Description of why authentication validation failed
        reason: String,
    },

    /// Insufficient permissions for the requested operation.
    #[error("Insufficient permissions for operation")]
    InsufficientPermissions,

    /// Signature verification failed.
    #[error("Signature verification failed")]
    SignatureVerificationFailed,

    /// Invalid data type encountered.
    #[error("Invalid data type: expected {expected}, got {actual}")]
    InvalidDataType {
        /// The expected data type
        expected: String,
        /// The actual data type found
        actual: String,
    },

    /// Serialization failed.
    #[error("Serialization failed for {context}")]
    SerializationFailed {
        /// The context where serialization failed
        context: String,
    },

    /// Invalid database configuration.
    #[error("Invalid database configuration: {reason}")]
    InvalidDatabaseConfiguration {
        /// Description of why the database configuration is invalid
        reason: String,
    },

    /// Settings validation failed.
    #[error("Settings validation failed: {reason}")]
    SettingsValidationFailed {
        /// Description of why settings validation failed
        reason: String,
    },

    /// Invalid operation attempted.
    #[error("Invalid operation: {reason}")]
    InvalidOperation {
        /// Description of why the operation is invalid
        reason: String,
    },

    /// Database initialization failed.
    #[error("Database initialization failed: {reason}")]
    DatabaseInitializationFailed {
        /// Description of why database initialization failed
        reason: String,
    },

    /// Entry validation failed.
    #[error("Entry validation failed: {reason}")]
    EntryValidationFailed {
        /// Description of why entry validation failed
        reason: String,
    },

    /// Database state is corrupted or inconsistent.
    #[error("Database state corruption detected: {reason}")]
    DatabaseStateCorruption {
        /// Description of the corruption detected
        reason: String,
    },

    /// Operation is not supported in the current mode or not yet implemented.
    #[error("Operation not supported: {operation}")]
    OperationNotSupported {
        /// Description of the unsupported operation
        operation: String,
    },
}

impl InstanceError {
    /// Check if this error indicates a resource was not found.
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            InstanceError::DatabaseNotFound { .. }
                | InstanceError::EntryNotFound { .. }
                | InstanceError::SigningKeyNotFound { .. }
        )
    }

    /// Check if this error indicates a resource already exists.
    pub fn is_already_exists(&self) -> bool {
        matches!(self, InstanceError::DatabaseAlreadyExists { .. })
    }

    /// Check if this error is authentication-related.
    pub fn is_authentication_error(&self) -> bool {
        matches!(
            self,
            InstanceError::AuthenticationRequired
                | InstanceError::NoAuthConfiguration
                | InstanceError::AuthenticationValidationFailed { .. }
                | InstanceError::InsufficientPermissions
                | InstanceError::SignatureVerificationFailed
                | InstanceError::SigningKeyNotFound { .. }
        )
    }

    /// Check if this error is operation-related.
    pub fn is_operation_error(&self) -> bool {
        matches!(
            self,
            InstanceError::TransactionAlreadyCommitted
                | InstanceError::EmptyTipsNotAllowed
                | InstanceError::InvalidOperation { .. }
        )
    }

    /// Check if this error is validation-related.
    pub fn is_validation_error(&self) -> bool {
        matches!(
            self,
            InstanceError::EntryNotInDatabase { .. }
                | InstanceError::InvalidTip { .. }
                | InstanceError::InvalidDataType { .. }
                | InstanceError::InvalidDatabaseConfiguration { .. }
                | InstanceError::SettingsValidationFailed { .. }
                | InstanceError::EntryValidationFailed { .. }
        )
    }

    /// Check if this error indicates corruption or inconsistency.
    pub fn is_corruption_error(&self) -> bool {
        matches!(self, InstanceError::DatabaseStateCorruption { .. })
    }

    /// Get the entry ID if this error is about a specific entry.
    pub fn entry_id(&self) -> Option<&ID> {
        match self {
            InstanceError::EntryNotFound { entry_id }
            | InstanceError::EntryNotInDatabase { entry_id, .. }
            | InstanceError::InvalidTip {
                tip_id: entry_id, ..
            } => Some(entry_id),
            _ => None,
        }
    }

    /// Get the database ID if this error is about a specific database.
    pub fn database_id(&self) -> Option<&ID> {
        match self {
            InstanceError::EntryNotInDatabase { database_id, .. }
            | InstanceError::InvalidTip { database_id, .. } => Some(database_id),
            _ => None,
        }
    }

    /// Get the database name if this error is about a named database.
    pub fn database_name(&self) -> Option<&str> {
        match self {
            InstanceError::DatabaseNotFound { name }
            | InstanceError::DatabaseAlreadyExists { name } => Some(name),
            _ => None,
        }
    }
}

// Conversion from BaseError to the main Error type
impl From<InstanceError> for crate::Error {
    fn from(err: InstanceError) -> Self {
        // Use the new structured Base variant
        crate::Error::Instance(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_helpers() {
        let err = InstanceError::DatabaseNotFound {
            name: "test-database".to_string(),
        };
        assert!(err.is_not_found());
        assert_eq!(err.database_name(), Some("test-database"));

        let err = InstanceError::DatabaseAlreadyExists {
            name: "existing-database".to_string(),
        };
        assert!(err.is_already_exists());
        assert_eq!(err.database_name(), Some("existing-database"));

        let err = InstanceError::EntryNotFound {
            entry_id: ID::from("test-entry"),
        };
        assert!(err.is_not_found());
        assert_eq!(err.entry_id(), Some(&ID::from("test-entry")));

        let err = InstanceError::AuthenticationRequired;
        assert!(err.is_authentication_error());

        let err = InstanceError::TransactionAlreadyCommitted;
        assert!(err.is_operation_error());

        let err = InstanceError::InvalidDataType {
            expected: "string".to_string(),
            actual: "number".to_string(),
        };
        assert!(err.is_validation_error());

        let err = InstanceError::DatabaseStateCorruption {
            reason: "test".to_string(),
        };
        assert!(err.is_corruption_error());
    }

    #[test]
    fn test_error_conversion() {
        let base_err = InstanceError::DatabaseNotFound {
            name: "test".to_string(),
        };
        let err: crate::Error = base_err.into();
        match err {
            crate::Error::Instance(InstanceError::DatabaseNotFound { name }) => {
                assert_eq!(name, "test")
            }
            _ => panic!("Unexpected error variant"),
        }
    }
}
