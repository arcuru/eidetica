//! Generic error types for subtree operations.
//!
//! This module defines generic error types that can be used by any subtree implementation.
//! Specific subtree types should define their own error types for implementation-specific errors.

use thiserror::Error;

/// Generic error types for subtree operations.
///
/// This enum provides fundamental error variants that apply to any subtree implementation.
/// Specific subtree types (DocStore, Table, etc.) should define their own error types
/// for implementation-specific errors and convert them to SubtreeError when needed.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum StoreError {
    /// Key or record not found in subtree
    #[error("Key not found in subtree '{subtree}': {key}")]
    KeyNotFound { subtree: String, key: String },

    /// Serialization failed for subtree data
    #[error("Serialization failed in subtree '{subtree}': {reason}")]
    SerializationFailed { subtree: String, reason: String },

    /// Deserialization failed for subtree data
    #[error("Deserialization failed in subtree '{subtree}': {reason}")]
    DeserializationFailed { subtree: String, reason: String },

    /// Type mismatch in subtree operation
    #[error("Type mismatch in subtree '{subtree}': expected {expected}, found {actual}")]
    TypeMismatch {
        subtree: String,
        expected: String,
        actual: String,
    },

    /// Invalid operation for the subtree type
    #[error("Invalid operation '{operation}' for subtree '{subtree}': {reason}")]
    InvalidOperation {
        subtree: String,
        operation: String,
        reason: String,
    },

    /// Subtree operation requires atomic operation context
    #[error("Operation requires atomic operation context for subtree '{subtree}'")]
    RequiresAtomicOperation { subtree: String },

    /// Data corruption detected in subtree
    #[error("Data corruption detected in subtree '{subtree}': {reason}")]
    DataCorruption { subtree: String, reason: String },

    /// Implementation-specific error from a subtree type
    #[error("Subtree implementation error in '{subtree}': {reason}")]
    ImplementationError { subtree: String, reason: String },
}

impl StoreError {
    /// Check if this error indicates a resource was not found
    pub fn is_not_found(&self) -> bool {
        matches!(self, StoreError::KeyNotFound { .. })
    }

    /// Check if this error is related to serialization
    pub fn is_serialization_error(&self) -> bool {
        matches!(
            self,
            StoreError::SerializationFailed { .. } | StoreError::DeserializationFailed { .. }
        )
    }

    /// Check if this error is related to type mismatches
    pub fn is_type_error(&self) -> bool {
        matches!(self, StoreError::TypeMismatch { .. })
    }

    /// Check if this error is related to data integrity
    pub fn is_integrity_error(&self) -> bool {
        matches!(self, StoreError::DataCorruption { .. })
    }

    /// Check if this error is related to invalid operations
    pub fn is_operation_error(&self) -> bool {
        matches!(
            self,
            StoreError::InvalidOperation { .. } | StoreError::RequiresAtomicOperation { .. }
        )
    }

    /// Check if this error is implementation-specific
    pub fn is_implementation_error(&self) -> bool {
        matches!(self, StoreError::ImplementationError { .. })
    }

    /// Get the subtree name associated with this error
    pub fn subtree_name(&self) -> &str {
        match self {
            StoreError::KeyNotFound { subtree, .. }
            | StoreError::SerializationFailed { subtree, .. }
            | StoreError::DeserializationFailed { subtree, .. }
            | StoreError::TypeMismatch { subtree, .. }
            | StoreError::InvalidOperation { subtree, .. }
            | StoreError::RequiresAtomicOperation { subtree, .. }
            | StoreError::DataCorruption { subtree, .. }
            | StoreError::ImplementationError { subtree, .. } => subtree,
        }
    }

    /// Get the operation name if this is an operation-specific error
    pub fn operation(&self) -> Option<&str> {
        match self {
            StoreError::InvalidOperation { operation, .. } => Some(operation),
            _ => None,
        }
    }

    /// Get the key if this is a key-related error
    pub fn key(&self) -> Option<&str> {
        match self {
            StoreError::KeyNotFound { key, .. } => Some(key),
            _ => None,
        }
    }
}

// Conversion from SubtreeError to the main Error type
impl From<StoreError> for crate::Error {
    fn from(err: StoreError) -> Self {
        crate::Error::Store(err)
    }
}
