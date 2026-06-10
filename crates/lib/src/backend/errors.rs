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

    /// Entry failed structural validation.
    #[error("Entry {entry_id} failed validation: {reason}")]
    EntryValidationFailed {
        /// The ID of the entry that failed validation
        entry_id: ID,
        /// The reason for validation failure
        reason: String,
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
        tree_id: ID,
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

    /// A blob's bytes did not hash to the content address they were stored under.
    ///
    /// Content addressing is self-verifying: a blob's address is the BLAKE3 CID
    /// of its bytes. This is raised when `cid != ID::from_bytes(&data)`, which
    /// means either a programming error (wrong CID passed to `put_blob`) or
    /// corrupted/tampered bytes.
    #[error("Blob content address mismatch: stored under {claimed} but bytes hash to {computed}")]
    BlobHashMismatch {
        /// The content address the blob was claimed to have.
        claimed: ID,
        /// The content address actually computed from the bytes.
        computed: ID,
    },

    /// A blob exceeded the configured maximum size.
    ///
    /// Phase 1 handles small/bounded blobs only; a hard cap guards against
    /// memory-DoS from an address that does not bound its payload. GB-scale
    /// payloads are a later (verified-streaming) capability.
    #[error("Blob size {size} bytes exceeds maximum {max} bytes")]
    BlobTooLarge {
        /// The size of the offending blob in bytes.
        size: usize,
        /// The configured maximum blob size in bytes.
        max: usize,
    },

    /// A blob address used a codec this backend does not store as a raw blob.
    ///
    /// Phase 1 stores only raw-codec (`0x55`) whole blobs. A DAG-CBOR (`0x71`)
    /// address — an `Entry` or a future blob manifest — is not a raw blob and
    /// is rejected here rather than silently missing.
    #[error("Not a raw-codec blob address: {cid}")]
    BlobInvalidCodec {
        /// The non-raw address that was supplied.
        cid: ID,
    },

    /// A bao verified-streaming blob transfer failed to decode or verify.
    ///
    /// Raised when a received range stream is malformed, does not cover the
    /// requested range, or fails verification against the requested CID (a
    /// tampered or wrong-blob stream). Resolution is self-verifying, so this is
    /// how an untrusted peer's bad bytes are rejected.
    #[error("Blob stream did not verify against {cid}")]
    BlobStreamInvalid {
        /// The content address the stream was supposed to deliver.
        cid: ID,
    },

    /// A blob's delivered length did not match the size declared by a
    /// [`BlobRef`](crate::blob::BlobRef).
    ///
    /// A content address pins a blob's *identity* (its bytes hash to the CID)
    /// but not its declared length; a reference may carry a `size` that lets a
    /// reader budget the transfer and decide fetch-or-not before the bytes
    /// arrive (§5.4). Resolving such a reference checks the delivered length
    /// against the declared one and rejects a mismatch here.
    #[error("Blob {cid} length {actual} bytes does not match declared size {declared} bytes")]
    BlobSizeMismatch {
        /// The content address of the blob.
        cid: ID,
        /// The size declared by the reference.
        declared: u64,
        /// The actual byte length resolved.
        actual: u64,
    },

    /// SQL database error (sqlx).
    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    #[error("SQL error: {reason}")]
    SqlxError {
        /// Description of the SQL error
        reason: String,
        /// The underlying sqlx error, if available
        #[source]
        source: Option<sqlx::Error>,
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
            BackendError::EntryValidationFailed { .. }
                | BackendError::CycleDetected { .. }
                | BackendError::HeightCalculationCorruption { .. }
                | BackendError::TreeIntegrityViolation { .. }
                | BackendError::StateInconsistency { .. }
        )
    }

    /// Check if this error is related to I/O operations.
    pub fn is_io_error(&self) -> bool {
        #[cfg(any(feature = "sqlite", feature = "postgres"))]
        if matches!(self, BackendError::SqlxError { .. }) {
            return true;
        }
        matches!(
            self,
            BackendError::FileIo { .. }
                | BackendError::SerializationFailed { .. }
                | BackendError::DeserializationFailed { .. }
        )
    }

    /// Check if this error is related to SQL database operations.
    #[cfg(any(feature = "sqlite", feature = "postgres"))]
    pub fn is_sql_error(&self) -> bool {
        matches!(self, BackendError::SqlxError { .. })
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
            | BackendError::EntryValidationFailed { entry_id: id, .. }
            | BackendError::CycleDetected { entry_id: id }
            | BackendError::EntryNotInTree { entry_id: id, .. }
            | BackendError::EntryNotInSubtree { entry_id: id, .. } => Some(id),
            _ => None,
        }
    }

    /// Get the tree ID if this error is about a specific tree.
    pub fn tree_id(&self) -> Option<&ID> {
        match self {
            BackendError::EntryNotInTree { tree_id, .. }
            | BackendError::EntryNotInSubtree { tree_id, .. }
            | BackendError::InvalidTreeReference { tree_id } => Some(tree_id),
            _ => None,
        }
    }
}

// Conversion from DatabaseError to the main Error type
impl From<BackendError> for crate::Error {
    fn from(err: BackendError) -> Self {
        // Use the new structured Backend variant
        crate::Error::Backend(Box::new(err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_helpers() {
        let err = BackendError::EntryNotFound {
            id: ID::from_bytes("test-entry"),
        };
        assert!(err.is_not_found());
        assert_eq!(err.entry_id(), Some(&ID::from_bytes("test-entry")));

        let err = BackendError::CycleDetected {
            entry_id: ID::from_bytes("cycle-entry"),
        };
        assert!(err.is_integrity_error());
        assert_eq!(err.entry_id(), Some(&ID::from_bytes("cycle-entry")));

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
            id: ID::from_bytes("test"),
        };
        let err: crate::Error = db_err.into();
        match err {
            crate::Error::Backend(e) => match *e {
                BackendError::EntryNotFound { id } => {
                    assert_eq!(id, ID::from_bytes("test"))
                }
                _ => panic!("Unexpected error variant"),
            },
            _ => panic!("Unexpected error variant"),
        }
    }
}
