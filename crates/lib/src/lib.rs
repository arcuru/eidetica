//!
//! Eidetica: A decentralized database designed to "Remember Everything".
//! This library provides the core components for building and interacting with Eidetica instances.
//!
//! ## Core Concepts
//!
//! Eidetica is built around several key concepts:
//!
//! * **Entries (`entry::Entry`)**: The fundamental, content-addressable unit of data. Entries contain data for a main tree and optional named subtrees.
//! * **Trees (`basedb::Tree`)**: Analogous to tables or branches, representing a history of related entries identified by a root entry ID.
//! * **Backends (`backend::Backend`)**: A pluggable storage layer for persisting entries.
//! * **BaseDB (`basedb::BaseDB`)**: The main database struct that manages trees and interacts with a backend.
//! * **CRDTs (`data::CRDT`)**: Conflict-free Replicated Data Types used for merging data from different entries, particularly for settings and subtree data.
//! * **SubTrees (`subtree::SubTree`)**: Named data structures within a tree that provide specialized data access patterns:
//!     * **KVStore (`subtree::KVStore`)**: A key-value store within a tree.
//!     * **RowStore (`subtree::RowStore`)**: A record-oriented store with automatic primary key generation, similar to a database table.
//!     * **YrsStore (`subtree::YrsStore`)**: A Y-CRDT based store for collaborative data structures (requires the "y-crdt" feature).
//! * **Merkle-CRDT**: The underlying principle combining Merkle DAGs (formed by entries and parent links) with CRDTs for efficient, decentralized data synchronization.

pub mod atomicop;
pub mod auth;
pub mod backend;
pub mod basedb;
pub mod constants;
pub mod crdt;
pub mod data;
pub mod entry;
pub mod subtree;
pub mod tree;

/// Re-export the `Tree` struct for easier access.
pub use tree::Tree;

/// Y-CRDT types re-exported for convenience when the "y-crdt" feature is enabled.
///
/// This module re-exports commonly used types from the `yrs` crate so that client code
/// doesn't need to add `yrs` as a separate dependency when using `YrsStore`.
#[cfg(feature = "y-crdt")]
pub mod y_crdt {
    pub use yrs::*;
}

/// Result type used throughout the Eidetica library.
pub type Result<T> = std::result::Result<T, Error>;

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
    Backend(backend::DatabaseError),

    /// Structured base database errors from the basedb module
    #[error(transparent)]
    Base(basedb::BaseError),

    /// Structured CRDT errors from the crdt module
    #[error(transparent)]
    CRDT(crdt::CRDTError),

    /// Structured subtree errors from the subtree module
    #[error(transparent)]
    Subtree(subtree::SubtreeError),

    /// Structured atomic operation errors from the atomicop module
    #[error(transparent)]
    AtomicOp(atomicop::AtomicOpError),
}

impl Error {
    /// Get the originating module for this error.
    pub fn module(&self) -> &'static str {
        match self {
            Error::Auth(_) => "auth",
            Error::Backend(_) => "backend",
            Error::Base(_) => "basedb",
            Error::CRDT(_) => "crdt",
            Error::Subtree(_) => "subtree",
            Error::AtomicOp(_) => "atomicop",
            Error::Io(_) => "io",
            Error::Serialize(_) => "serialize",
        }
    }

    /// Check if this error indicates a resource was not found.
    pub fn is_not_found(&self) -> bool {
        match self {
            Error::Auth(auth_err) => auth_err.is_key_not_found(),
            Error::Backend(backend_err) => backend_err.is_not_found(),
            Error::Base(base_err) => base_err.is_not_found(),
            Error::CRDT(crdt_err) => crdt_err.is_not_found_error(),
            Error::Subtree(subtree_err) => subtree_err.is_not_found(),
            _ => false,
        }
    }

    /// Check if this error indicates permission was denied.
    pub fn is_permission_denied(&self) -> bool {
        match self {
            Error::Auth(auth_err) => auth_err.is_permission_denied(),
            Error::AtomicOp(atomicop_err) => atomicop_err.is_authentication_error(),
            _ => false,
        }
    }

    /// Check if this error indicates a conflict (already exists).
    pub fn is_conflict(&self) -> bool {
        match self {
            Error::Base(base_err) => base_err.is_already_exists(),
            _ => false,
        }
    }

    /// Check if this error is authentication-related.
    pub fn is_authentication_error(&self) -> bool {
        match self {
            Error::Auth(_) => true,
            Error::Base(base_err) => base_err.is_authentication_error(),
            Error::AtomicOp(atomicop_err) => atomicop_err.is_authentication_error(),
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
        matches!(self, Error::Base(_))
    }

    /// Check if this error is validation-related.
    pub fn is_validation_error(&self) -> bool {
        match self {
            Error::Base(base_err) => base_err.is_validation_error(),
            Error::Backend(backend_err) => backend_err.is_logical_error(),
            Error::AtomicOp(atomicop_err) => atomicop_err.is_validation_error(),
            _ => false,
        }
    }

    /// Check if this error is operation-related.
    pub fn is_operation_error(&self) -> bool {
        match self {
            Error::Base(base_err) => base_err.is_operation_error(),
            Error::Subtree(subtree_err) => subtree_err.is_operation_error(),
            Error::AtomicOp(atomicop_err) => atomicop_err.is_validation_error(),
            _ => false,
        }
    }

    /// Check if this error is type-related.
    pub fn is_type_error(&self) -> bool {
        match self {
            Error::Subtree(subtree_err) => subtree_err.is_type_error(),
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

    /// Check if this error is subtree-related.
    pub fn is_subtree_error(&self) -> bool {
        matches!(self, Error::Subtree(_))
    }

    /// Check if this error is a subtree serialization failure.
    pub fn is_subtree_serialization_error(&self) -> bool {
        match self {
            Error::Subtree(subtree_err) => subtree_err.is_serialization_error(),
            _ => false,
        }
    }

    /// Check if this error is a subtree type mismatch.
    pub fn is_subtree_type_error(&self) -> bool {
        match self {
            Error::Subtree(subtree_err) => subtree_err.is_type_error(),
            _ => false,
        }
    }

    /// Check if this error indicates an operation was already committed.
    pub fn is_already_committed(&self) -> bool {
        match self {
            Error::AtomicOp(atomicop_err) => atomicop_err.is_already_committed(),
            _ => false,
        }
    }

    /// Check if this error is related to entry operations.
    pub fn is_entry_error(&self) -> bool {
        match self {
            Error::AtomicOp(atomicop_err) => atomicop_err.is_entry_error(),
            _ => false,
        }
    }

    /// Check if this error is related to concurrency issues.
    pub fn is_concurrency_error(&self) -> bool {
        match self {
            Error::AtomicOp(atomicop_err) => atomicop_err.is_concurrency_error(),
            _ => false,
        }
    }

    /// Check if this error indicates a timeout.
    pub fn is_timeout_error(&self) -> bool {
        match self {
            Error::AtomicOp(atomicop_err) => atomicop_err.is_timeout_error(),
            _ => false,
        }
    }
}
