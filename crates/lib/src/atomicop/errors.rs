//! Atomic operation specific errors
//!
//! This module contains error types specific to atomic operations, which compose
//! errors from multiple modules into a cohesive error handling system for
//! cross-module operations.

use thiserror::Error;

/// Errors that can occur during atomic operations
///
/// `AtomicOpError` represents failures specific to atomic operations that
/// span multiple modules and require coordinated error handling. These errors
/// typically occur during entry construction, commit operations, and cross-module
/// data staging.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum AtomicOpError {
    /// Operation has already been committed and cannot be used again
    #[error("Operation has already been committed")]
    OperationAlreadyCommitted,

    /// Tips array cannot be empty when creating an operation
    #[error("Empty tips array not allowed for operation")]
    EmptyTipsNotAllowed,

    /// Invalid tip provided to operation
    #[error("Invalid tip for operation: {tip_id}")]
    InvalidTip { tip_id: String },

    /// Entry construction failed during commit
    #[error("Entry construction failed: {reason}")]
    EntryConstructionFailed { reason: String },

    /// Entry signing failed during commit
    #[error("Entry signing failed for key '{key_name}': {reason}")]
    EntrySigningFailed { key_name: String, reason: String },

    /// Required signing key not found
    #[error("Signing key not found: {key_name}")]
    SigningKeyNotFound { key_name: String },

    /// Authentication is required but not configured
    #[error("Authentication required but not configured")]
    AuthenticationRequired,

    /// Authentication configuration is missing
    #[error("No authentication configuration found")]
    NoAuthConfiguration,

    /// Insufficient permissions for the operation
    #[error("Insufficient permissions for operation")]
    InsufficientPermissions,

    /// Entry signature verification failed
    #[error("Entry signature verification failed")]
    SignatureVerificationFailed,

    /// Subtree data serialization failed
    #[error("Subtree data serialization failed for '{subtree}': {reason}")]
    SubtreeSerializationFailed { subtree: String, reason: String },

    /// Subtree data deserialization failed
    #[error("Subtree data deserialization failed for '{subtree}': {reason}")]
    SubtreeDeserializationFailed { subtree: String, reason: String },

    /// Backend operation failed during commit
    #[error("Backend operation failed during commit: {reason}")]
    BackendOperationFailed { reason: String },

    /// Tree state validation failed
    #[error("Tree state validation failed: {reason}")]
    TreeStateValidationFailed { reason: String },

    /// Metadata construction failed
    #[error("Metadata construction failed: {reason}")]
    MetadataConstructionFailed { reason: String },

    /// Parent resolution failed
    #[error("Parent resolution failed: {reason}")]
    ParentResolutionFailed { reason: String },

    /// Concurrent modification detected
    #[error("Concurrent modification detected")]
    ConcurrentModification,

    /// Operation timeout
    #[error("Operation timed out after {duration_ms}ms")]
    OperationTimeout { duration_ms: u64 },

    /// Invalid operation state
    #[error("Invalid operation state: {reason}")]
    InvalidOperationState { reason: String },

    /// Subtree operation failed
    #[error("Subtree operation failed for '{subtree}': {reason}")]
    SubtreeOperationFailed { subtree: String, reason: String },
}

impl AtomicOpError {
    /// Check if this error indicates the operation was already committed
    pub fn is_already_committed(&self) -> bool {
        matches!(self, AtomicOpError::OperationAlreadyCommitted)
    }

    /// Check if this error is authentication-related
    pub fn is_authentication_error(&self) -> bool {
        matches!(
            self,
            AtomicOpError::SigningKeyNotFound { .. }
                | AtomicOpError::AuthenticationRequired
                | AtomicOpError::NoAuthConfiguration
                | AtomicOpError::InsufficientPermissions
                | AtomicOpError::SignatureVerificationFailed
                | AtomicOpError::EntrySigningFailed { .. }
        )
    }

    /// Check if this error is related to entry operations
    pub fn is_entry_error(&self) -> bool {
        matches!(
            self,
            AtomicOpError::EntryConstructionFailed { .. }
                | AtomicOpError::EntrySigningFailed { .. }
                | AtomicOpError::SignatureVerificationFailed
        )
    }

    /// Check if this error is related to subtree operations
    pub fn is_subtree_error(&self) -> bool {
        matches!(
            self,
            AtomicOpError::SubtreeSerializationFailed { .. }
                | AtomicOpError::SubtreeDeserializationFailed { .. }
                | AtomicOpError::SubtreeOperationFailed { .. }
        )
    }

    /// Check if this error is related to backend operations
    pub fn is_backend_error(&self) -> bool {
        matches!(self, AtomicOpError::BackendOperationFailed { .. })
    }

    /// Check if this error is related to validation
    pub fn is_validation_error(&self) -> bool {
        matches!(
            self,
            AtomicOpError::TreeStateValidationFailed { .. }
                | AtomicOpError::InvalidTip { .. }
                | AtomicOpError::EmptyTipsNotAllowed
                | AtomicOpError::InvalidOperationState { .. }
        )
    }

    /// Check if this error indicates a concurrency issue
    pub fn is_concurrency_error(&self) -> bool {
        matches!(self, AtomicOpError::ConcurrentModification)
    }

    /// Check if this error indicates a timeout
    pub fn is_timeout_error(&self) -> bool {
        matches!(self, AtomicOpError::OperationTimeout { .. })
    }

    /// Get the subtree name if this is a subtree-related error
    pub fn subtree_name(&self) -> Option<&str> {
        match self {
            AtomicOpError::SubtreeSerializationFailed { subtree, .. }
            | AtomicOpError::SubtreeDeserializationFailed { subtree, .. }
            | AtomicOpError::SubtreeOperationFailed { subtree, .. } => Some(subtree),
            _ => None,
        }
    }

    /// Get the key name if this is an authentication-related error
    pub fn key_name(&self) -> Option<&str> {
        match self {
            AtomicOpError::SigningKeyNotFound { key_name }
            | AtomicOpError::EntrySigningFailed { key_name, .. } => Some(key_name),
            _ => None,
        }
    }
}

// Conversion from AtomicOpError to the main Error type
impl From<AtomicOpError> for crate::Error {
    fn from(err: AtomicOpError) -> Self {
        crate::Error::AtomicOp(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_classification() {
        // Test authentication errors
        let auth_err = AtomicOpError::AuthenticationRequired;
        assert!(auth_err.is_authentication_error());
        assert!(!auth_err.is_entry_error());

        // Test entry errors
        let entry_err = AtomicOpError::EntryConstructionFailed {
            reason: "test".to_owned(),
        };
        assert!(entry_err.is_entry_error());
        assert!(!entry_err.is_authentication_error());

        // Test subtree errors
        let subtree_err = AtomicOpError::SubtreeSerializationFailed {
            subtree: "test_subtree".to_owned(),
            reason: "test".to_owned(),
        };
        assert!(subtree_err.is_subtree_error());
        assert_eq!(subtree_err.subtree_name(), Some("test_subtree"));

        // Test validation errors
        let validation_err = AtomicOpError::EmptyTipsNotAllowed;
        assert!(validation_err.is_validation_error());
        assert!(!validation_err.is_backend_error());
    }

    #[test]
    fn test_already_committed() {
        let err = AtomicOpError::OperationAlreadyCommitted;
        assert!(err.is_already_committed());
    }

    #[test]
    fn test_key_name_extraction() {
        let err = AtomicOpError::SigningKeyNotFound {
            key_name: "test_key".to_owned(),
        };
        assert_eq!(err.key_name(), Some("test_key"));

        let other_err = AtomicOpError::AuthenticationRequired;
        assert_eq!(other_err.key_name(), None);
    }

    #[test]
    fn test_timeout_error() {
        let err = AtomicOpError::OperationTimeout { duration_ms: 5000 };
        assert!(err.is_timeout_error());
        assert!(!err.is_authentication_error());
    }

    #[test]
    fn test_concurrency_error() {
        let err = AtomicOpError::ConcurrentModification;
        assert!(err.is_concurrency_error());
        assert!(!err.is_validation_error());
    }
}
