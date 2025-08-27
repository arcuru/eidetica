//! Error types for CRDT operations.
//!
//! This module defines structured error types specific to CRDT (Conflict-free Replicated Data Type)
//! operations, providing detailed context for merge failures, serialization issues,
//! and type mismatches that can occur during CRDT operations.

use crate::crdt::doc::PathError;
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

    /// List operation failed
    #[error("CRDT list operation failed: {operation} - {reason}")]
    ListOperationFailed { operation: String, reason: String },

    /// Map operation failed
    #[error("CRDT map operation failed: {operation} - {reason}")]
    MapOperationFailed { operation: String, reason: String },

    /// Nested structure operation failed
    #[error("CRDT nested operation failed: {path} - {reason}")]
    NestedOperationFailed { path: String, reason: String },

    /// Invalid UUID format in list operations
    #[error("Invalid UUID format: {uuid}")]
    InvalidUuid { uuid: String },

    /// Element not found in CRDT structure
    #[error("CRDT element not found: {key}")]
    ElementNotFound { key: String },

    /// Invalid path for nested operations
    #[error("Invalid CRDT path: {path}")]
    InvalidPath { path: String },

    /// List index out of bounds
    #[error("List index out of bounds: index {index}, length {len}")]
    ListIndexOutOfBounds { index: usize, len: usize },
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

    /// Check if this error is related to list operations (operation-level)
    pub fn is_list_operation_error(&self) -> bool {
        matches!(self, CRDTError::ListOperationFailed { .. })
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

    /// Check if this error is related to list operations
    pub fn is_list_error(&self) -> bool {
        matches!(self, CRDTError::ListIndexOutOfBounds { .. })
    }

    /// Get the operation type if this is an operation-specific error
    pub fn operation(&self) -> Option<&str> {
        match self {
            CRDTError::ListOperationFailed { operation, .. }
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

// Conversion from PathError to CRDTError
impl From<PathError> for CRDTError {
    fn from(err: PathError) -> Self {
        CRDTError::InvalidPath {
            path: err.to_string(),
        }
    }
}

// Conversion from CRDTError to the main Error type
impl From<CRDTError> for crate::Error {
    fn from(err: CRDTError) -> Self {
        crate::Error::CRDT(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crdt_error_list_index_out_of_bounds() {
        let error = CRDTError::ListIndexOutOfBounds { index: 5, len: 3 };

        assert!(error.is_list_error());
        assert!(!error.is_merge_error());
        assert!(!error.is_serialization_error());
        assert!(!error.is_type_error());
        assert!(!error.is_list_operation_error());
        assert!(!error.is_map_error());
        assert!(!error.is_nested_error());
        assert!(!error.is_not_found_error());

        assert_eq!(error.operation(), None);
        assert_eq!(error.path(), None);
        assert_eq!(error.key(), None);

        let display = format!("{error}");
        assert!(display.contains("List index out of bounds"));
        assert!(display.contains("index 5"));
        assert!(display.contains("length 3"));
    }

    #[test]
    fn test_crdt_error_classification() {
        let merge_error = CRDTError::MergeFailed {
            reason: "test".to_string(),
        };
        assert!(merge_error.is_merge_error());

        let serialization_error = CRDTError::SerializationFailed {
            reason: "test".to_string(),
        };
        assert!(serialization_error.is_serialization_error());

        let type_error = CRDTError::TypeMismatch {
            expected: "string".to_string(),
            actual: "int".to_string(),
        };
        assert!(type_error.is_type_error());

        let list_error = CRDTError::ListOperationFailed {
            operation: "insert".to_string(),
            reason: "test".to_string(),
        };
        assert!(list_error.is_list_operation_error());
        assert_eq!(list_error.operation(), Some("insert"));

        let map_error = CRDTError::MapOperationFailed {
            operation: "set".to_string(),
            reason: "test".to_string(),
        };
        assert!(map_error.is_map_error());
        assert_eq!(map_error.operation(), Some("set"));

        let nested_error = CRDTError::NestedOperationFailed {
            path: "user.profile".to_string(),
            reason: "test".to_string(),
        };
        assert!(nested_error.is_nested_error());
        assert_eq!(nested_error.path(), Some("user.profile"));

        let not_found_error = CRDTError::ElementNotFound {
            key: "missing".to_string(),
        };
        assert!(not_found_error.is_not_found_error());
        assert_eq!(not_found_error.key(), Some("missing"));
    }

    #[test]
    fn test_crdt_error_conversion_to_main_error() {
        let crdt_error = CRDTError::ListIndexOutOfBounds { index: 1, len: 0 };
        let main_error: crate::Error = crdt_error.into();

        assert_eq!(main_error.module(), "crdt");

        if let crate::Error::CRDT(inner) = main_error {
            assert!(inner.is_list_error());
        } else {
            panic!("Expected CRDT error variant");
        }
    }

    #[test]
    fn test_crdt_error_display_messages() {
        let errors = vec![
            CRDTError::ListIndexOutOfBounds { index: 10, len: 5 },
            CRDTError::MergeFailed {
                reason: "conflict".to_string(),
            },
            CRDTError::TypeMismatch {
                expected: "Map".to_string(),
                actual: "Text".to_string(),
            },
            CRDTError::InvalidPath {
                path: "invalid..path".to_string(),
            },
            CRDTError::ElementNotFound {
                key: "nonexistent".to_string(),
            },
        ];

        for error in errors {
            let display = format!("{error}");
            assert!(!display.is_empty());
            assert!(display.len() > 10); // Should have meaningful error messages
        }
    }

    #[test]
    fn test_path_error_conversion() {
        let path_error = PathError::EmptyComponent { position: 1 };
        let crdt_error: CRDTError = path_error.into();

        match crdt_error {
            CRDTError::InvalidPath { path } => {
                assert!(path.contains("Path component at position 1 is empty"));
            }
            _ => panic!("Expected InvalidPath variant"),
        }

        // Test conversion through main Error type
        let path_error = PathError::LeadingDot;
        let main_error: crate::Error = CRDTError::from(path_error).into();
        assert_eq!(main_error.module(), "crdt");
    }
}
