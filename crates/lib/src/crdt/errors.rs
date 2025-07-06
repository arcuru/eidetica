//! Error types for CRDT operations.
//!
//! This module defines structured error types specific to CRDT (Conflict-free Replicated Data Type)
//! operations, providing detailed context for merge failures, serialization issues,
//! and type mismatches that can occur during CRDT operations.

use thiserror::Error;

/// Structured error types for CRDT operations.
///
/// This enum provides specific error variants for different types of failures
/// that can occur during CRDT operations, including merge conflicts, serialization
/// issues, and type validation errors.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum CRDTError {
    /// A merge operation failed between two CRDT instances
    #[error("CRDT merge failed: {reason}")]
    MergeFailed { reason: String },

    /// Serialization of CRDT data failed
    #[error("CRDT serialization failed: {reason}")]
    SerializationFailed { reason: String },

    /// Deserialization of CRDT data failed
    #[error("CRDT deserialization failed: {reason}")]
    DeserializationFailed { reason: String },

    /// Type mismatch during CRDT operation
    #[error("CRDT type mismatch: expected {expected}, found {actual}")]
    TypeMismatch { expected: String, actual: String },

    /// Invalid value provided for CRDT operation
    #[error("Invalid CRDT value: {reason}")]
    InvalidValue { reason: String },

    /// Array operation failed
    #[error("CRDT array operation failed: {operation} - {reason}")]
    ArrayOperationFailed { operation: String, reason: String },

    /// Map operation failed
    #[error("CRDT map operation failed: {operation} - {reason}")]
    MapOperationFailed { operation: String, reason: String },

    /// Nested structure operation failed
    #[error("CRDT nested operation failed: {path} - {reason}")]
    NestedOperationFailed { path: String, reason: String },

    /// Invalid UUID format in array operations
    #[error("Invalid UUID format: {uuid}")]
    InvalidUuid { uuid: String },

    /// Element not found in CRDT structure
    #[error("CRDT element not found: {key}")]
    ElementNotFound { key: String },

    /// Invalid path for nested operations
    #[error("Invalid CRDT path: {path}")]
    InvalidPath { path: String },
}

impl CRDTError {
    /// Check if this error is related to merge operations
    pub fn is_merge_error(&self) -> bool {
        matches!(self, CRDTError::MergeFailed { .. })
    }

    /// Check if this error is related to serialization
    pub fn is_serialization_error(&self) -> bool {
        matches!(
            self,
            CRDTError::SerializationFailed { .. } | CRDTError::DeserializationFailed { .. }
        )
    }

    /// Check if this error is related to type mismatches
    pub fn is_type_error(&self) -> bool {
        matches!(self, CRDTError::TypeMismatch { .. })
    }

    /// Check if this error is related to array operations
    pub fn is_array_error(&self) -> bool {
        matches!(self, CRDTError::ArrayOperationFailed { .. })
    }

    /// Check if this error is related to map operations
    pub fn is_map_error(&self) -> bool {
        matches!(self, CRDTError::MapOperationFailed { .. })
    }

    /// Check if this error is related to nested operations
    pub fn is_nested_error(&self) -> bool {
        matches!(self, CRDTError::NestedOperationFailed { .. })
    }

    /// Check if this error is related to element lookup
    pub fn is_not_found_error(&self) -> bool {
        matches!(self, CRDTError::ElementNotFound { .. })
    }

    /// Get the operation type if this is an operation-specific error
    pub fn operation(&self) -> Option<&str> {
        match self {
            CRDTError::ArrayOperationFailed { operation, .. }
            | CRDTError::MapOperationFailed { operation, .. } => Some(operation),
            _ => None,
        }
    }

    /// Get the path if this is a path-related error
    pub fn path(&self) -> Option<&str> {
        match self {
            CRDTError::NestedOperationFailed { path, .. } | CRDTError::InvalidPath { path } => {
                Some(path)
            }
            _ => None,
        }
    }

    /// Get the key if this is a key-related error
    pub fn key(&self) -> Option<&str> {
        match self {
            CRDTError::ElementNotFound { key } => Some(key),
            _ => None,
        }
    }
}

// Conversion from CRDTError to the main Error type
impl From<CRDTError> for crate::Error {
    fn from(err: CRDTError) -> Self {
        crate::Error::CRDT(err)
    }
}
