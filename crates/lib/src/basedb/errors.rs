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
pub enum InstanceError {
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

impl InstanceError {
    /// Check if this error indicates a resource was not found.
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            InstanceError::TreeNotFound { .. }
                | InstanceError::EntryNotFound { .. }
                | InstanceError::SigningKeyNotFound { .. }
        )
    }

    /// Check if this error indicates a resource already exists.
    pub fn is_already_exists(&self) -> bool {
        matches!(self, InstanceError::TreeAlreadyExists { .. })
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
            InstanceError::OperationAlreadyCommitted
                | InstanceError::EmptyTipsNotAllowed
                | InstanceError::InvalidOperation { .. }
        )
    }

    /// Check if this error is validation-related.
    pub fn is_validation_error(&self) -> bool {
        matches!(
            self,
            InstanceError::EntryNotInTree { .. }
                | InstanceError::InvalidTip { .. }
                | InstanceError::InvalidDataType { .. }
                | InstanceError::InvalidTreeConfiguration { .. }
                | InstanceError::SettingsValidationFailed { .. }
                | InstanceError::EntryValidationFailed { .. }
        )
    }

    /// Check if this error indicates corruption or inconsistency.
    pub fn is_corruption_error(&self) -> bool {
        matches!(self, InstanceError::TreeStateCorruption { .. })
    }

    /// Get the entry ID if this error is about a specific entry.
    pub fn entry_id(&self) -> Option<&ID> {
        match self {
            InstanceError::EntryNotFound { entry_id }
            | InstanceError::EntryNotInTree { entry_id, .. }
            | InstanceError::InvalidTip {
                tip_id: entry_id, ..
            } => Some(entry_id),
            _ => None,
        }
    }

    /// Get the tree ID if this error is about a specific tree.
    pub fn tree_id(&self) -> Option<&ID> {
        match self {
            InstanceError::EntryNotInTree { tree_id, .. }
            | InstanceError::InvalidTip { tree_id, .. } => Some(tree_id),
            _ => None,
        }
    }

    /// Get the tree name if this error is about a named tree.
    pub fn tree_name(&self) -> Option<&str> {
        match self {
            InstanceError::TreeNotFound { name } | InstanceError::TreeAlreadyExists { name } => {
                Some(name)
            }
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
        let err = InstanceError::TreeNotFound {
            name: "test-tree".to_string(),
        };
        assert!(err.is_not_found());
        assert_eq!(err.tree_name(), Some("test-tree"));

        let err = InstanceError::TreeAlreadyExists {
            name: "existing-tree".to_string(),
        };
        assert!(err.is_already_exists());
        assert_eq!(err.tree_name(), Some("existing-tree"));

        let err = InstanceError::EntryNotFound {
            entry_id: ID::from("test-entry"),
        };
        assert!(err.is_not_found());
        assert_eq!(err.entry_id(), Some(&ID::from("test-entry")));

        let err = InstanceError::AuthenticationRequired;
        assert!(err.is_authentication_error());

        let err = InstanceError::OperationAlreadyCommitted;
        assert!(err.is_operation_error());

        let err = InstanceError::InvalidDataType {
            expected: "string".to_string(),
            actual: "number".to_string(),
        };
        assert!(err.is_validation_error());

        let err = InstanceError::TreeStateCorruption {
            reason: "test".to_string(),
        };
        assert!(err.is_corruption_error());
    }

    #[test]
    fn test_error_conversion() {
        let base_err = InstanceError::TreeNotFound {
            name: "test".to_string(),
        };
        let err: crate::Error = base_err.into();
        match err {
            crate::Error::Instance(InstanceError::TreeNotFound { name }) => {
                assert_eq!(name, "test")
            }
            _ => panic!("Unexpected error variant"),
        }
    }
}
