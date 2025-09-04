//! Generic error types for store operations.
//!
//! This module defines generic error types that can be used by any store implementation.
//! Specific store types should define their own error types for implementation-specific errors.

use thiserror::Error;

/// Generic error types for store operations.
///
/// This enum provides fundamental error variants that apply to any store implementation.
/// Specific store types (DocStore, Table, etc.) should define their own error types
/// for implementation-specific errors and convert them to SubtreeError when needed.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum StoreError {
    /// Key or record not found in store
    #[error("Key not found in store '{subtree}': {key}")]
    KeyNotFound { subtree: String, key: String },

    /// Serialization failed for store data
    #[error("Serialization failed in store '{subtree}': {reason}")]
    SerializationFailed { subtree: String, reason: String },

    /// Deserialization failed for store data
    #[error("Deserialization failed in store '{subtree}': {reason}")]
    DeserializationFailed { subtree: String, reason: String },

    /// Type mismatch in store operation
    #[error("Type mismatch in store '{subtree}': expected {expected}, found {actual}")]
    TypeMismatch {
        subtree: String,
        expected: String,
        actual: String,
    },

    /// Invalid operation for the store type
    #[error("Invalid operation '{operation}' for store '{subtree}': {reason}")]
    InvalidOperation {
        subtree: String,
        operation: String,
        reason: String,
    },

    /// Store operation requires transaction context
    #[error("Operation requires transaction context for store '{subtree}'")]
    RequiresTransaction { subtree: String },

    /// Data corruption detected in store
    #[error("Data corruption detected in store '{subtree}': {reason}")]
    DataCorruption { subtree: String, reason: String },

    /// Implementation-specific error from a store type
    #[error("Store implementation error in '{subtree}': {reason}")]
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
            StoreError::InvalidOperation { .. } | StoreError::RequiresTransaction { .. }
        )
    }

    /// Check if this error is implementation-specific
    pub fn is_implementation_error(&self) -> bool {
        matches!(self, StoreError::ImplementationError { .. })
    }

    /// Get the store name associated with this error
    pub fn subtree_name(&self) -> &str {
        match self {
            StoreError::KeyNotFound { subtree, .. }
            | StoreError::SerializationFailed { subtree, .. }
            | StoreError::DeserializationFailed { subtree, .. }
            | StoreError::TypeMismatch { subtree, .. }
            | StoreError::InvalidOperation { subtree, .. }
            | StoreError::RequiresTransaction { subtree, .. }
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
