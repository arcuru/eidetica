//! Base database error types for the Eidetica library.
//!
//! This module defines structured error types for tree operations, entry management,
//! and database operations, providing better error context and type safety compared to string-based errors.

use crate::entry::ID;
use thiserror::Error;

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
pub enum BaseError {
    /// Tree not found by name.
    #[error("Tree not found: {name}")]
    TreeNotFound {
        /// The name of the tree that was not found
        name: String,
    },

    /// Tree already exists with the given name.
    #[error("Tree already exists: {name}")]
    TreeAlreadyExists {
        /// The name of the tree that already exists
        name: String,
    },

    /// Entry does not belong to the specified tree.
    #[error("Entry '{entry_id}' does not belong to tree '{tree_id}'")]
    EntryNotInTree {
        /// The ID of the entry
        entry_id: ID,
        /// The ID of the tree
        tree_id: ID,
    },

    /// Entry not found by ID.
    #[error("Entry not found: {entry_id}")]
    EntryNotFound {
        /// The ID of the entry that was not found
        entry_id: ID,
    },

    /// Operation has already been committed and cannot be modified.
    #[error("Operation has already been committed")]
    OperationAlreadyCommitted,

    /// Cannot create operation with empty tips.
    #[error("Cannot create operation with empty tips")]
    EmptyTipsNotAllowed,

    /// Tip entry does not belong to the specified tree.
    #[error("Tip entry '{tip_id}' does not belong to tree '{tree_id}'")]
    InvalidTip {
        /// The ID of the invalid tip entry
        tip_id: ID,
        /// The ID of the tree
        tree_id: ID,
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

    /// Invalid tree configuration.
    #[error("Invalid tree configuration: {reason}")]
    InvalidTreeConfiguration {
        /// Description of why the tree configuration is invalid
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

    /// Tree initialization failed.
    #[error("Tree initialization failed: {reason}")]
    TreeInitializationFailed {
        /// Description of why tree initialization failed
        reason: String,
    },

    /// Entry validation failed.
    #[error("Entry validation failed: {reason}")]
    EntryValidationFailed {
        /// Description of why entry validation failed
        reason: String,
    },

    /// Tree state is corrupted or inconsistent.
    #[error("Tree state corruption detected: {reason}")]
    TreeStateCorruption {
        /// Description of the corruption detected
        reason: String,
    },
}

impl BaseError {
    /// Check if this error indicates a resource was not found.
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            BaseError::TreeNotFound { .. }
                | BaseError::EntryNotFound { .. }
                | BaseError::SigningKeyNotFound { .. }
        )
    }

    /// Check if this error indicates a resource already exists.
    pub fn is_already_exists(&self) -> bool {
        matches!(self, BaseError::TreeAlreadyExists { .. })
    }

    /// Check if this error is authentication-related.
    pub fn is_authentication_error(&self) -> bool {
        matches!(
            self,
            BaseError::AuthenticationRequired
                | BaseError::NoAuthConfiguration
                | BaseError::AuthenticationValidationFailed { .. }
                | BaseError::InsufficientPermissions
                | BaseError::SignatureVerificationFailed
                | BaseError::SigningKeyNotFound { .. }
        )
    }

    /// Check if this error is operation-related.
    pub fn is_operation_error(&self) -> bool {
        matches!(
            self,
            BaseError::OperationAlreadyCommitted
                | BaseError::EmptyTipsNotAllowed
                | BaseError::InvalidOperation { .. }
        )
    }

    /// Check if this error is validation-related.
    pub fn is_validation_error(&self) -> bool {
        matches!(
            self,
            BaseError::EntryNotInTree { .. }
                | BaseError::InvalidTip { .. }
                | BaseError::InvalidDataType { .. }
                | BaseError::InvalidTreeConfiguration { .. }
                | BaseError::SettingsValidationFailed { .. }
                | BaseError::EntryValidationFailed { .. }
        )
    }

    /// Check if this error indicates corruption or inconsistency.
    pub fn is_corruption_error(&self) -> bool {
        matches!(self, BaseError::TreeStateCorruption { .. })
    }

    /// Get the entry ID if this error is about a specific entry.
    pub fn entry_id(&self) -> Option<&ID> {
        match self {
            BaseError::EntryNotFound { entry_id }
            | BaseError::EntryNotInTree { entry_id, .. }
            | BaseError::InvalidTip {
                tip_id: entry_id, ..
            } => Some(entry_id),
            _ => None,
        }
    }

    /// Get the tree ID if this error is about a specific tree.
    pub fn tree_id(&self) -> Option<&ID> {
        match self {
            BaseError::EntryNotInTree { tree_id, .. } | BaseError::InvalidTip { tree_id, .. } => {
                Some(tree_id)
            }
            _ => None,
        }
    }

    /// Get the tree name if this error is about a named tree.
    pub fn tree_name(&self) -> Option<&str> {
        match self {
            BaseError::TreeNotFound { name } | BaseError::TreeAlreadyExists { name } => Some(name),
            _ => None,
        }
    }
}

// Conversion from BaseError to the main Error type
impl From<BaseError> for crate::Error {
    fn from(err: BaseError) -> Self {
        // Use the new structured Base variant
        crate::Error::Base(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_helpers() {
        let err = BaseError::TreeNotFound {
            name: "test-tree".to_string(),
        };
        assert!(err.is_not_found());
        assert_eq!(err.tree_name(), Some("test-tree"));

        let err = BaseError::TreeAlreadyExists {
            name: "existing-tree".to_string(),
        };
        assert!(err.is_already_exists());
        assert_eq!(err.tree_name(), Some("existing-tree"));

        let err = BaseError::EntryNotFound {
            entry_id: ID::from("test-entry"),
        };
        assert!(err.is_not_found());
        assert_eq!(err.entry_id(), Some(&ID::from("test-entry")));

        let err = BaseError::AuthenticationRequired;
        assert!(err.is_authentication_error());

        let err = BaseError::OperationAlreadyCommitted;
        assert!(err.is_operation_error());

        let err = BaseError::InvalidDataType {
            expected: "string".to_string(),
            actual: "number".to_string(),
        };
        assert!(err.is_validation_error());

        let err = BaseError::TreeStateCorruption {
            reason: "test".to_string(),
        };
        assert!(err.is_corruption_error());
    }

    #[test]
    fn test_error_conversion() {
        let base_err = BaseError::TreeNotFound {
            name: "test".to_string(),
        };
        let err: crate::Error = base_err.into();
        match err {
            crate::Error::Base(BaseError::TreeNotFound { name }) => assert_eq!(name, "test"),
            _ => panic!("Unexpected error variant"),
        }
    }
}
