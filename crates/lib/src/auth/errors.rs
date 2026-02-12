//! Authentication error types for the Eidetica library.
//!
//! This module defines structured error types for authentication-related operations,
//! providing better error context and type safety compared to string-based errors.

use thiserror::Error as ThisError;

use crate::Error;
use crate::entry::ID;

/// Errors that can occur during authentication operations.
///
/// # Stability
///
/// - New variants may be added in minor versions (enum is `#[non_exhaustive]`)
/// - Existing variants will not be removed in minor versions
/// - Field additions/changes require a major version bump
/// - Helper methods like `is_*()` provide stable APIs
#[non_exhaustive]
#[derive(Debug, ThisError)]
pub enum AuthError {
    /// A requested authentication key was not found in the configuration.
    #[error("Key not found: {key_name}")]
    KeyNotFound {
        /// The name of the key that was not found
        key_name: String,
    },

    /// Invalid key format or parsing error.
    #[error("Invalid key format: {reason}")]
    InvalidKeyFormat {
        /// Description of why the key format is invalid
        reason: String,
    },

    /// Key parsing failed due to cryptographic library error.
    #[error("Key parsing failed: {reason}")]
    KeyParsingFailed {
        /// Description of the parsing failure
        reason: String,
    },

    /// No authentication configuration was found.
    #[error("No auth configuration found")]
    NoAuthConfiguration,

    /// The authentication configuration is invalid.
    #[error("Invalid auth configuration: {reason}")]
    InvalidAuthConfiguration {
        /// Description of why the configuration is invalid
        reason: String,
    },

    /// Delegation path is empty when it should contain at least one step.
    #[error("Empty delegation path")]
    EmptyDelegationPath,

    /// Maximum delegation depth was exceeded to prevent infinite loops.
    #[error("Maximum delegation depth ({depth}) exceeded")]
    DelegationDepthExceeded {
        /// The maximum depth that was exceeded
        depth: usize,
    },

    /// A delegation step is invalid.
    #[error("Invalid delegation step: {reason}")]
    InvalidDelegationStep {
        /// Description of why the delegation step is invalid
        reason: String,
    },

    /// Failed to load a delegated tree.
    #[error("Failed to load delegated tree {tree_id}")]
    DelegatedTreeLoadFailed {
        /// The ID of the tree that failed to load
        tree_id: String,
        /// The underlying error
        #[source]
        source: Box<Error>,
    },

    /// Delegation tips don't match the actual tree state.
    #[error(
        "Invalid delegation tips for tree {tree_id}: claimed tips {claimed_tips:?} don't match"
    )]
    InvalidDelegationTips {
        /// The ID of the tree with invalid tips
        tree_id: String,
        /// The tips that were claimed but are invalid
        claimed_tips: Vec<ID>,
    },

    /// A delegated tree reference was not found in the configuration.
    #[error("Delegation not found for tree: {tree_id}")]
    DelegationNotFound {
        /// The root tree ID of the delegation that was not found
        tree_id: String,
    },

    /// Attempted to revoke an entry that is not a key.
    #[error("Cannot revoke non-key entry: {key_name}")]
    CannotRevokeNonKey {
        /// The name of the entry that is not a key
        key_name: String,
    },

    /// Entry has malformed signature info (e.g., hint without signature).
    #[error("Malformed entry: {reason}")]
    MalformedEntry {
        /// Description of why the entry is malformed
        reason: &'static str,
    },

    /// Signature verification failed.
    #[error("Invalid signature")]
    InvalidSignature,

    /// Signature verification failed with specific error.
    #[error("Signature verification failed: {reason}")]
    SignatureVerificationFailed {
        /// Description of the verification failure
        reason: String,
    },

    /// Database is required for the operation but not available.
    #[error("Database required for {operation}")]
    DatabaseRequired {
        /// The operation that requires a database
        operation: String,
    },

    /// Invalid permission string format.
    #[error("Invalid permission string: {value}")]
    InvalidPermissionString {
        /// The invalid permission string
        value: String,
    },

    /// Permission type requires a priority value.
    #[error("{permission_type} permission requires priority")]
    PermissionRequiresPriority {
        /// The permission type that requires priority
        permission_type: String,
    },

    /// Invalid priority value.
    #[error("Invalid priority value: {value}")]
    InvalidPriorityValue {
        /// The invalid priority value
        value: String,
    },

    /// Invalid key status string.
    #[error("Invalid key status: {value}")]
    InvalidKeyStatus {
        /// The invalid status value
        value: String,
    },

    /// Permission denied for an operation.
    #[error("Permission denied: {reason}")]
    PermissionDenied {
        /// Description of why permission was denied
        reason: String,
    },

    /// Attempted to add a key that already exists.
    #[error("Key already exists: {key_name}")]
    KeyAlreadyExists {
        /// The name of the key that already exists
        key_name: String,
    },

    /// Key name conflicts with existing key that has different public key.
    #[error(
        "Key name '{key_name}' conflicts: existing key has pubkey '{existing_pubkey}', new key has pubkey '{new_pubkey}'"
    )]
    KeyNameConflict {
        /// The name of the conflicting key
        key_name: String,
        /// The public key of the existing key
        existing_pubkey: String,
        /// The public key of the new key
        new_pubkey: String,
    },

    /// Signing key does not match the claimed identity in a DatabaseKey.
    #[error("Signing key mismatch: {reason}")]
    SigningKeyMismatch {
        /// Description of the mismatch
        reason: String,
    },
}

impl AuthError {
    /// Check if this error indicates a key or delegation was not found.
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            AuthError::KeyNotFound { .. } | AuthError::DelegationNotFound { .. }
        )
    }

    /// Check if this error indicates invalid signature.
    pub fn is_invalid_signature(&self) -> bool {
        matches!(
            self,
            AuthError::InvalidSignature | AuthError::SignatureVerificationFailed { .. }
        )
    }

    /// Check if this error indicates permission was denied.
    pub fn is_permission_denied(&self) -> bool {
        matches!(self, AuthError::PermissionDenied { .. })
    }

    /// Check if this error indicates a key already exists.
    pub fn is_key_already_exists(&self) -> bool {
        matches!(self, AuthError::KeyAlreadyExists { .. })
    }

    /// Check if this error indicates a key name conflict.
    pub fn is_key_name_conflict(&self) -> bool {
        matches!(self, AuthError::KeyNameConflict { .. })
    }

    /// Check if this error indicates a configuration problem.
    pub fn is_configuration_error(&self) -> bool {
        matches!(
            self,
            AuthError::NoAuthConfiguration
                | AuthError::InvalidAuthConfiguration { .. }
                | AuthError::InvalidKeyFormat { .. }
                | AuthError::KeyParsingFailed { .. }
        )
    }

    /// Check if this error is related to delegation.
    pub fn is_delegation_error(&self) -> bool {
        matches!(
            self,
            AuthError::EmptyDelegationPath
                | AuthError::DelegationDepthExceeded { .. }
                | AuthError::InvalidDelegationStep { .. }
                | AuthError::DelegatedTreeLoadFailed { .. }
                | AuthError::InvalidDelegationTips { .. }
                | AuthError::DelegationNotFound { .. }
        )
    }

    /// Get the key name if this error is about a missing key.
    pub fn key_name(&self) -> Option<&str> {
        match self {
            AuthError::KeyNotFound { key_name: id } => Some(id),
            _ => None,
        }
    }
}

// Conversion from AuthError to the main Error type
impl From<AuthError> for Error {
    fn from(err: AuthError) -> Self {
        // Use the new structured Auth variant
        Error::Auth(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_helpers() {
        let err = AuthError::KeyNotFound {
            key_name: "test-key".to_string(),
        };
        assert!(err.is_not_found());
        assert_eq!(err.key_name(), Some("test-key"));

        let err = AuthError::InvalidSignature;
        assert!(err.is_invalid_signature());

        let err = AuthError::PermissionDenied {
            reason: "test".to_string(),
        };
        assert!(err.is_permission_denied());

        let err = AuthError::NoAuthConfiguration;
        assert!(err.is_configuration_error());

        let err = AuthError::EmptyDelegationPath;
        assert!(err.is_delegation_error());
    }

    #[test]
    fn test_error_conversion() {
        let auth_err = AuthError::KeyNotFound {
            key_name: "test".to_string(),
        };
        let err: Error = auth_err.into();
        match err {
            Error::Auth(AuthError::KeyNotFound { key_name: id }) => assert_eq!(id, "test"),
            _ => panic!("Unexpected error variant"),
        }
    }
}
