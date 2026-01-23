//! Transaction specific errors
//!
//! This module contains error types specific to transactions, which compose
//! errors from multiple modules into a cohesive error handling system for
//! cross-module operations.

use thiserror::Error;

/// Errors that can occur during transactions
///
/// `TransactionError` represents failures specific to transactions that
/// span multiple modules and require coordinated error handling. These errors
/// typically occur during entry construction, commit operations, and cross-module
/// data staging.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum TransactionError {
    /// Transaction has already been committed and cannot be used again
    #[error("Transaction has already been committed")]
    TransactionAlreadyCommitted,

    /// Tips array cannot be empty when creating a transaction
    #[error("Empty tips array not allowed for transaction")]
    EmptyTipsNotAllowed,

    /// Invalid tip provided to transaction
    #[error("Invalid tip for transaction: {tip_id}")]
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

    /// Authentication configuration exists but is corrupted or malformed
    #[error("Authentication configuration is corrupted or malformed")]
    CorruptedAuthConfiguration,

    /// Insufficient permissions for the operation
    #[error("Insufficient permissions for operation")]
    InsufficientPermissions,

    /// Entry signature verification failed
    #[error("Entry signature verification failed")]
    SignatureVerificationFailed,

    /// Entry validation failed (signature, permissions, or configuration)
    #[error("Entry validation failed")]
    EntryValidationFailed,

    /// Store data deserialization failed
    #[error("Store data deserialization failed for '{store}': {reason}")]
    StoreDeserializationFailed { store: String, reason: String },

    /// Backend operation failed during commit
    #[error("Backend operation failed during commit: {reason}")]
    BackendOperationFailed { reason: String },
}

impl TransactionError {
    /// Check if this error indicates the operation was already committed
    pub fn is_already_committed(&self) -> bool {
        matches!(self, TransactionError::TransactionAlreadyCommitted)
    }

    /// Check if this error is authentication-related
    pub fn is_authentication_error(&self) -> bool {
        matches!(
            self,
            TransactionError::SigningKeyNotFound { .. }
                | TransactionError::AuthenticationRequired
                | TransactionError::NoAuthConfiguration
                | TransactionError::CorruptedAuthConfiguration
                | TransactionError::InsufficientPermissions
                | TransactionError::SignatureVerificationFailed
                | TransactionError::EntrySigningFailed { .. }
                | TransactionError::EntryValidationFailed
        )
    }

    /// Check if this error is related to entry operations
    pub fn is_entry_error(&self) -> bool {
        matches!(
            self,
            TransactionError::EntryConstructionFailed { .. }
                | TransactionError::EntrySigningFailed { .. }
                | TransactionError::SignatureVerificationFailed
                | TransactionError::EntryValidationFailed
        )
    }

    /// Check if this error is related to store operations
    pub fn is_store_error(&self) -> bool {
        matches!(self, TransactionError::StoreDeserializationFailed { .. })
    }

    /// Check if this error is related to backend operations
    pub fn is_backend_error(&self) -> bool {
        matches!(self, TransactionError::BackendOperationFailed { .. })
    }

    /// Check if this error is related to validation
    pub fn is_validation_error(&self) -> bool {
        matches!(
            self,
            TransactionError::InvalidTip { .. } | TransactionError::EmptyTipsNotAllowed
        )
    }

    /// Get the store name if this is a store-related error
    pub fn store_name(&self) -> Option<&str> {
        match self {
            TransactionError::StoreDeserializationFailed { store, .. } => Some(store),
            _ => None,
        }
    }

    /// Get the key name if this is an authentication-related error
    pub fn key_name(&self) -> Option<&str> {
        match self {
            TransactionError::SigningKeyNotFound { key_name }
            | TransactionError::EntrySigningFailed { key_name, .. } => Some(key_name),
            _ => None,
        }
    }
}

// Conversion from TransactionError to the main Error type
impl From<TransactionError> for crate::Error {
    fn from(err: TransactionError) -> Self {
        crate::Error::Transaction(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_classification() {
        // Test authentication errors
        let auth_err = TransactionError::AuthenticationRequired;
        assert!(auth_err.is_authentication_error());
        assert!(!auth_err.is_entry_error());

        // Test entry errors
        let entry_err = TransactionError::EntryConstructionFailed {
            reason: "test".to_owned(),
        };
        assert!(entry_err.is_entry_error());
        assert!(!entry_err.is_authentication_error());

        // Test store errors
        let store_err = TransactionError::StoreDeserializationFailed {
            store: "test_store".to_owned(),
            reason: "test".to_owned(),
        };
        assert!(store_err.is_store_error());
        assert_eq!(store_err.store_name(), Some("test_store"));

        // Test validation errors
        let validation_err = TransactionError::EmptyTipsNotAllowed;
        assert!(validation_err.is_validation_error());
        assert!(!validation_err.is_backend_error());
    }

    #[test]
    fn test_already_committed() {
        let err = TransactionError::TransactionAlreadyCommitted;
        assert!(err.is_already_committed());
    }

    #[test]
    fn test_key_name_extraction() {
        let err = TransactionError::SigningKeyNotFound {
            key_name: "test_key".to_owned(),
        };
        assert_eq!(err.key_name(), Some("test_key"));

        let other_err = TransactionError::AuthenticationRequired;
        assert_eq!(other_err.key_name(), None);
    }
}
