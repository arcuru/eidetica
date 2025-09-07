//! Database error types for the Eidetica backend.
//!
//! This module defines structured error types for database operations,
//! providing better error context and type safety compared to string-based errors.

use thiserror::Error;

use crate::entry::ID;

/// Errors that can occur during database operations.
///
/// # Stability
///
/// - New variants may be added in minor versions (enum is `#[non_exhaustive]`)
/// - Existing variants will not be removed in minor versions
/// - Field additions/changes require a major version bump
/// - Helper methods like `is_*()` provide stable APIs
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum BackendError {
    /// Entry not found by ID.
    #[error("Entry not found: {id}")]
    EntryNotFound {
        /// The ID of the entry that was not found
        id: ID,
    },

    /// Verification status not found for entry.
    #[error("Verification status not found for entry: {id}")]
    VerificationStatusNotFound {
        /// The ID of the entry whose verification status was not found
        id: ID,
    },

    /// Entry is not part of the specified tree.
    #[error("Entry {entry_id} is not in tree {tree_id}")]
    EntryNotInTree {
        /// The ID of the entry
        entry_id: ID,
        /// The ID of the tree
        tree_id: ID,
    },

    /// Entry is not part of the specified subtree.
    #[error("Entry {entry_id} is not in subtree {subtree} of tree {tree_id}")]
    EntryNotInSubtree {
        /// The ID of the entry
        entry_id: ID,
        /// The ID of the tree
        tree_id: ID,
        /// The name of the subtree
        subtree: String,
    },

    /// Cycle detected in DAG structure.
    #[error("Cycle detected in DAG while traversing from {entry_id}")]
    CycleDetected {
        /// The entry ID where cycle was detected
        entry_id: ID,
    },

    /// No common ancestor found for given entries.
    #[error("No common ancestor found for entries: {entry_ids:?}")]
    NoCommonAncestor {
        /// The entry IDs that have no common ancestor
        entry_ids: Vec<ID>,
    },

    /// Empty entry list provided where non-empty list required.
    #[error("No entry IDs provided for {operation}")]
    EmptyEntryList {
        /// The operation that required a non-empty list
        operation: String,
    },

    /// Data corruption detected during height calculation.
    #[error("Height calculation corruption: {reason}")]
    HeightCalculationCorruption {
        /// Description of the corruption detected
        reason: String,
    },

    /// Private key not found.
    #[error("Private key not found: {key_name}")]
    PrivateKeyNotFound {
        /// The name of the private key that was not found
        key_name: String,
    },

    /// Serialization failed.
    #[error("Serialization failed")]
    SerializationFailed {
        /// The underlying serialization error
        #[source]
        source: serde_json::Error,
    },

    /// Deserialization failed.
    #[error("Deserialization failed")]
    DeserializationFailed {
        /// The underlying deserialization error
        #[source]
        source: serde_json::Error,
    },

    /// File I/O error.
    #[error("File I/O error")]
    FileIo {
        /// The underlying I/O error
        #[source]
        source: std::io::Error,
    },

    /// CRDT cache operation failed.
    #[error("CRDT cache operation failed: {reason}")]
    CrdtCacheError {
        /// Description of the cache operation failure
        reason: String,
    },

    /// Database integrity violation detected.
    #[error("Database integrity violation: {reason}")]
    TreeIntegrityViolation {
        /// Description of the integrity violation
        reason: String,
    },

    /// Invalid tree reference or tree ID.
    #[error("Invalid tree reference: {tree_id}")]
    InvalidTreeReference {
        /// The invalid tree ID
        tree_id: String,
    },

    /// Database state inconsistency detected.
    #[error("Database state inconsistency: {reason}")]
    StateInconsistency {
        /// Description of the state inconsistency
        reason: String,
    },

    /// Cache miss or cache corruption.
    #[error("Cache operation failed: {reason}")]
    CacheError {
        /// Description of the cache error
        reason: String,
    },
}

impl BackendError {
    /// Check if this error indicates a resource was not found.
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            BackendError::EntryNotFound { .. }
                | BackendError::VerificationStatusNotFound { .. }
                | BackendError::PrivateKeyNotFound { .. }
        )
    }

    /// Check if this error indicates a data integrity issue.
    pub fn is_integrity_error(&self) -> bool {
        matches!(
            self,
            BackendError::CycleDetected { .. }
                | BackendError::HeightCalculationCorruption { .. }
                | BackendError::TreeIntegrityViolation { .. }
                | BackendError::StateInconsistency { .. }
        )
    }

    /// Check if this error is related to I/O operations.
    pub fn is_io_error(&self) -> bool {
        matches!(
            self,
            BackendError::FileIo { .. }
                | BackendError::SerializationFailed { .. }
                | BackendError::DeserializationFailed { .. }
        )
    }

    /// Check if this error is related to cache operations.
    pub fn is_cache_error(&self) -> bool {
        matches!(
            self,
            BackendError::CrdtCacheError { .. } | BackendError::CacheError { .. }
        )
    }

    /// Check if this error indicates a logical inconsistency.
    pub fn is_logical_error(&self) -> bool {
        matches!(
            self,
            BackendError::EntryNotInTree { .. }
                | BackendError::EntryNotInSubtree { .. }
                | BackendError::NoCommonAncestor { .. }
                | BackendError::EmptyEntryList { .. }
        )
    }

    /// Get the entry ID if this error is about a specific entry.
    pub fn entry_id(&self) -> Option<&ID> {
        match self {
            BackendError::EntryNotFound { id }
            | BackendError::VerificationStatusNotFound { id }
            | BackendError::CycleDetected { entry_id: id }
            | BackendError::EntryNotInTree { entry_id: id, .. }
            | BackendError::EntryNotInSubtree { entry_id: id, .. } => Some(id),
            _ => None,
        }
    }

    /// Get the tree ID if this error is about a specific tree.
    pub fn tree_id(&self) -> Option<String> {
        match self {
            BackendError::EntryNotInTree { tree_id, .. }
            | BackendError::EntryNotInSubtree { tree_id, .. } => Some(tree_id.to_string()),
            BackendError::InvalidTreeReference { tree_id } => Some(tree_id.clone()),
            _ => None,
        }
    }
}

// Conversion from DatabaseError to the main Error type
impl From<BackendError> for crate::Error {
    fn from(err: BackendError) -> Self {
        // Use the new structured Backend variant
        crate::Error::Backend(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_helpers() {
        let err = BackendError::EntryNotFound {
            id: ID::from("test-entry"),
        };
        assert!(err.is_not_found());
        assert_eq!(err.entry_id(), Some(&ID::from("test-entry")));

        let err = BackendError::CycleDetected {
            entry_id: ID::from("cycle-entry"),
        };
        assert!(err.is_integrity_error());
        assert_eq!(err.entry_id(), Some(&ID::from("cycle-entry")));

        let err = BackendError::FileIo {
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "test"),
        };
        assert!(err.is_io_error());

        let err = BackendError::CacheError {
            reason: "test".to_string(),
        };
        assert!(err.is_cache_error());

        let err = BackendError::EmptyEntryList {
            operation: "test".to_string(),
        };
        assert!(err.is_logical_error());
    }

    #[test]
    fn test_error_conversion() {
        let db_err = BackendError::EntryNotFound {
            id: ID::from("test"),
        };
        let err: crate::Error = db_err.into();
        match err {
            crate::Error::Backend(BackendError::EntryNotFound { id }) => {
                assert_eq!(id.to_string(), "test")
            }
            _ => panic!("Unexpected error variant"),
        }
    }
}
