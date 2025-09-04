//!
//! Eidetica: A decentralized database designed to "Remember Everything".
//! This library provides the core components for building and interacting with Eidetica instances.
//!
//! ## Core Concepts
//!
//! Eidetica is built around several key concepts:
//!
//! * **Entries (`Entry`)**: The fundamental, content-addressable unit of data. Entries contain data for a main database and optional named stores.
//! * **Databases (`Database`)**: Like a traditional database or branch, representing a versioned collection of related entries identified by a root entry ID.
//! * **Backends (`backend::Backend`)**: A pluggable storage layer for persisting entries.
//! * **Instance (`Instance`)**: The main database struct that manages multiple databases and interacts with a backend.
//! * **CRDTs (`crdt::CRDT`)**: Conflict-free Replicated Data Types used for merging data from different entries, particularly for settings and store data.
//! * **Stores (`Store`)**: Named data structures within a database that provide specialized data access patterns, analogous to tables:
//!     * **DocStore (`store::DocStore`)**: A document-oriented store for structured data with path-based operations.
//!     * **Table (`store::Table`)**: A record-oriented store with automatic primary key generation, similar to a database table.
//!     * **YDoc (`store::YDoc`)**: A Y-CRDT based store for collaborative data structures (requires the "y-crdt" feature).
//! * **Merkle-CRDT**: The underlying principle combining Merkle DAGs (formed by entries and parent links) with CRDTs for efficient, decentralized data synchronization.

pub mod transaction;
pub mod auth;
pub mod backend;
pub mod instance;
pub mod constants;
pub mod crdt;
pub mod entry;
pub mod store;
pub mod sync;
pub mod database;

/// Re-export fundamental types for easier access.
pub use transaction::Transaction;
pub use instance::Instance;
pub use entry::Entry;
pub use store::Store;
pub use database::Database;

/// Y-CRDT types re-exported for convenience when the "y-crdt" feature is enabled.
///
/// This module re-exports commonly used types from the `yrs` crate so that client code
/// doesn't need to add `yrs` as a separate dependency when using `YDoc`.
#[cfg(feature = "y-crdt")]
pub mod y_crdt {
    pub use yrs::*;
}

/// Result type used throughout the Eidetica library.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Common error type for the Eidetica library.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialize(#[from] serde_json::Error),

    /// Structured authentication errors from the auth module
    #[error(transparent)]
    Auth(auth::AuthError),

    /// Structured database errors from the backend module
    #[error(transparent)]
    Backend(backend::BackendError),

    /// Structured base database errors from the instance module
    #[error(transparent)]
    Instance(instance::InstanceError),

    /// Structured CRDT errors from the crdt module
    #[error(transparent)]
    CRDT(crdt::CRDTError),

    /// Structured subtree errors from the store module
    #[error(transparent)]
    Store(store::StoreError),

    /// Structured atomic operation errors from the transaction module
    #[error(transparent)]
    Transaction(transaction::TransactionError),

    /// Structured synchronization errors from the sync module
    #[error(transparent)]
    Sync(sync::SyncError),
}

impl From<sync::SyncError> for Error {
    fn from(err: sync::SyncError) -> Self {
        Error::Sync(err)
    }
}

impl Error {
    /// Get the originating module for this error.
    pub fn module(&self) -> &'static str {
        match self {
            Error::Auth(_) => "auth",
            Error::Backend(_) => "backend",
            Error::Instance(_) => "instance",
            Error::CRDT(_) => "crdt",
            Error::Store(_) => "store",
            Error::Transaction(_) => "transaction",
            Error::Sync(_) => "sync",
            Error::Io(_) => "io",
            Error::Serialize(_) => "serialize",
        }
    }

    /// Check if this error indicates a resource was not found.
    pub fn is_not_found(&self) -> bool {
        match self {
            Error::Auth(auth_err) => auth_err.is_key_not_found(),
            Error::Backend(backend_err) => backend_err.is_not_found(),
            Error::Instance(base_err) => base_err.is_not_found(),
            Error::CRDT(crdt_err) => crdt_err.is_not_found_error(),
            Error::Store(store_err) => store_err.is_not_found(),
            Error::Sync(sync_err) => sync_err.is_not_found(),
            _ => false,
        }
    }

    /// Check if this error indicates permission was denied.
    pub fn is_permission_denied(&self) -> bool {
        match self {
            Error::Auth(auth_err) => auth_err.is_permission_denied(),
            Error::Transaction(transaction_err) => transaction_err.is_authentication_error(),
            _ => false,
        }
    }

    /// Check if this error indicates a conflict (already exists).
    pub fn is_conflict(&self) -> bool {
        match self {
            Error::Instance(base_err) => base_err.is_already_exists(),
            _ => false,
        }
    }

    /// Check if this error is authentication-related.
    pub fn is_authentication_error(&self) -> bool {
        match self {
            Error::Auth(_) => true,
            Error::Instance(base_err) => base_err.is_authentication_error(),
            Error::Transaction(transaction_err) => transaction_err.is_authentication_error(),
            _ => false,
        }
    }

    /// Check if this error is database/backend-related.
    pub fn is_database_error(&self) -> bool {
        matches!(self, Error::Backend(_))
    }

    /// Check if this error indicates a data integrity issue.
    pub fn is_integrity_error(&self) -> bool {
        match self {
            Error::Backend(backend_err) => backend_err.is_integrity_error(),
            _ => false,
        }
    }

    /// Check if this error is I/O related.
    pub fn is_io_error(&self) -> bool {
        match self {
            Error::Io(_) => true,
            Error::Backend(backend_err) => backend_err.is_io_error(),
            _ => false,
        }
    }

    /// Check if this error is base database-related.
    pub fn is_base_database_error(&self) -> bool {
        matches!(self, Error::Instance(_))
    }

    /// Check if this error is validation-related.
    pub fn is_validation_error(&self) -> bool {
        match self {
            Error::Instance(base_err) => base_err.is_validation_error(),
            Error::Backend(backend_err) => backend_err.is_logical_error(),
            Error::Transaction(transaction_err) => transaction_err.is_validation_error(),
            _ => false,
        }
    }

    /// Check if this error is operation-related.
    pub fn is_operation_error(&self) -> bool {
        match self {
            Error::Instance(base_err) => base_err.is_operation_error(),
            Error::Store(store_err) => store_err.is_operation_error(),
            Error::Transaction(transaction_err) => transaction_err.is_validation_error(),
            _ => false,
        }
    }

    /// Check if this error is type-related.
    pub fn is_type_error(&self) -> bool {
        match self {
            Error::Store(store_err) => store_err.is_type_error(),
            _ => false,
        }
    }

    /// Check if this error is CRDT-related.
    pub fn is_crdt_error(&self) -> bool {
        matches!(self, Error::CRDT(_))
    }

    /// Check if this error is a CRDT merge failure.
    pub fn is_crdt_merge_error(&self) -> bool {
        match self {
            Error::CRDT(crdt_err) => crdt_err.is_merge_error(),
            _ => false,
        }
    }

    /// Check if this error is a CRDT serialization failure.
    pub fn is_crdt_serialization_error(&self) -> bool {
        match self {
            Error::CRDT(crdt_err) => crdt_err.is_serialization_error(),
            _ => false,
        }
    }

    /// Check if this error is a CRDT type mismatch.
    pub fn is_crdt_type_error(&self) -> bool {
        match self {
            Error::CRDT(crdt_err) => crdt_err.is_type_error(),
            _ => false,
        }
    }

    /// Check if this error is store-related.
    pub fn is_store_error(&self) -> bool {
        matches!(self, Error::Store(_))
    }

    /// Check if this error is a store serialization failure.
    pub fn is_store_serialization_error(&self) -> bool {
        match self {
            Error::Store(store_err) => store_err.is_serialization_error(),
            _ => false,
        }
    }

    /// Check if this error is a store type mismatch.
    pub fn is_store_type_error(&self) -> bool {
        match self {
            Error::Store(store_err) => store_err.is_type_error(),
            _ => false,
        }
    }

    /// Check if this error indicates an operation was already committed.
    pub fn is_already_committed(&self) -> bool {
        match self {
            Error::Transaction(transaction_err) => transaction_err.is_already_committed(),
            _ => false,
        }
    }

    /// Check if this error is related to entry operations.
    pub fn is_entry_error(&self) -> bool {
        match self {
            Error::Transaction(transaction_err) => transaction_err.is_entry_error(),
            _ => false,
        }
    }

}
