//! Entry-specific error types for the Eidetica library.
//!
//! This module defines structured error types for entry operations and validation,
//! providing better error context and type safety.

use thiserror::Error;

/// Errors that can occur during entry operations.
///
/// # Stability
///
/// - New variants may be added in minor versions (enum is `#[non_exhaustive]`)
/// - Existing variants will not be removed in minor versions
/// - Field additions/changes require a major version bump
/// - Helper methods like `is_*()` provide stable APIs
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum EntryError {
    /// Entry validation failed during creation or processing
    #[error("Entry validation failed: {reason}")]
    ValidationFailed {
        /// Reason why entry validation failed
        reason: String,
    },

    /// Entry serialization failed
    #[error("Entry serialization failed: {context}")]
    SerializationFailed {
        /// Context where serialization failed
        context: String,
    },

    /// Entry structure is invalid
    #[error("Invalid entry structure: {reason}")]
    InvalidStructure {
        /// Reason why the entry structure is invalid
        reason: String,
    },
}

impl EntryError {
    /// Check if this error is validation-related.
    pub fn is_validation_error(&self) -> bool {
        matches!(
            self,
            EntryError::ValidationFailed { .. } | EntryError::InvalidStructure { .. }
        )
    }

    /// Check if this error is serialization-related.
    pub fn is_serialization_error(&self) -> bool {
        matches!(self, EntryError::SerializationFailed { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::id::IdError;

    #[test]
    fn test_id_error_direct_conversion() {
        let id_err = IdError::InvalidFormat("not hex".to_string());
        let main_err: crate::Error = id_err.into();

        assert!(main_err.is_id_error());
        assert!(main_err.is_validation_error());
        assert_eq!(main_err.module(), "id");
    }

    #[test]
    fn test_unknown_algorithm_error() {
        let id_err = IdError::UnknownAlgorithm("md5".to_string());
        let main_err: crate::Error = id_err.into();

        assert!(main_err.is_id_error());
        assert!(main_err.is_validation_error());
        assert_eq!(main_err.module(), "id");
    }

    #[test]
    fn test_length_error() {
        let id_err = IdError::InvalidLength {
            expected: 64,
            got: 32,
        };
        let main_err: crate::Error = id_err.into();

        assert!(main_err.is_id_error());
        assert!(main_err.is_validation_error());
        assert_eq!(main_err.module(), "id");
    }

    #[test]
    fn test_error_helpers() {
        let err = EntryError::ValidationFailed {
            reason: "test".to_string(),
        };
        assert!(err.is_validation_error());
        assert!(!err.is_serialization_error());

        let err = EntryError::SerializationFailed {
            context: "test".to_string(),
        };
        assert!(!err.is_validation_error());
        assert!(err.is_serialization_error());

        let err = EntryError::InvalidStructure {
            reason: "test".to_string(),
        };
        assert!(err.is_validation_error());
        assert!(!err.is_serialization_error());
    }

    #[test]
    fn test_error_conversion_to_main() {
        let entry_err = EntryError::ValidationFailed {
            reason: "test".to_string(),
        };
        let main_err: crate::Error = entry_err.into();

        // Should convert to Entry error
        assert!(main_err.is_entry_error());
        assert!(!main_err.is_id_error());
        assert!(main_err.is_validation_error());
    }

    #[test]
    fn test_id_error_integration() {
        use crate::entry::id::IdError;

        // Test direct ID error conversion
        let id_err = IdError::InvalidLength {
            expected: 64,
            got: 32,
        };
        let main_err: crate::Error = id_err.into();

        assert!(!main_err.is_entry_error());
        assert!(main_err.is_id_error());
        assert!(main_err.is_validation_error());
        assert_eq!(main_err.module(), "id");

        // Test Blake3 algorithm error
        let id_err = IdError::UnknownAlgorithm("md5".to_string());
        let main_err: crate::Error = id_err.into();

        assert!(!main_err.is_entry_error());
        assert!(main_err.is_id_error());
        assert!(main_err.is_validation_error());

        // Verify the error message contains the expected information
        let error_string = main_err.to_string();
        assert!(error_string.contains("md5"));
    }

    #[test]
    fn test_error_categorization_comprehensive() {
        // Test that different types of errors are properly categorized
        let id_errors = vec![
            IdError::InvalidFormat("not hex".to_string()),
            IdError::InvalidLength {
                expected: 64,
                got: 32,
            },
            IdError::UnknownAlgorithm("sha1".to_string()),
            IdError::InvalidHex("contains G".to_string()),
        ];

        for id_err in id_errors {
            let main_err: crate::Error = id_err.into();

            // All ID errors should be validation errors
            assert!(main_err.is_id_error());
            assert!(main_err.is_validation_error());
            assert!(!main_err.is_entry_error());
            assert!(!main_err.is_entry_serialization_error());
            assert!(!main_err.is_io_error());
            assert_eq!(main_err.module(), "id");
        }

        // Test non-ID entry errors
        let validation_err = EntryError::ValidationFailed {
            reason: "test".to_string(),
        };
        let main_err: crate::Error = validation_err.into();
        assert!(main_err.is_validation_error());
        assert!(main_err.is_entry_error());
        assert!(!main_err.is_id_error());

        let serialization_err = EntryError::SerializationFailed {
            context: "test".to_string(),
        };
        let main_err: crate::Error = serialization_err.into();
        assert!(main_err.is_entry_serialization_error());
        assert!(main_err.is_entry_error());
        assert!(!main_err.is_validation_error());
        assert!(!main_err.is_id_error());
    }
}
