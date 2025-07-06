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
    #[error("Entry not found")]
    NotFound,

    #[error("Already exists")]
    AlreadyExists,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialize(#[from] serde_json::Error),

    #[error("Invalid operation: {0}")]
    InvalidOperation(String),

    /// Structured authentication errors from the auth module
    #[error(transparent)]
    Auth(auth::AuthError),

    /// Structured database errors from the backend module
    #[error(transparent)]
    Backend(backend::DatabaseError),

    /// General authentication errors including configuration issues,
    /// key resolution failures, and validation problems
    #[error("Authentication error: {0}")]
    Authentication(String),

    /// Cryptographic signature verification failed
    #[error("Invalid signature")]
    InvalidSignature,

    /// Authentication key ID not found in _settings.auth configuration
    #[error("Key not found: {0}")]
    KeyNotFound(String),

    /// Insufficient permissions for the requested operation
    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    /// Public key parsing or format validation failed
    #[error("Invalid key format: {0}")]
    InvalidKeyFormat(String),
}

impl Error {
    /// Get the originating module for this error.
    pub fn module(&self) -> &'static str {
        match self {
            Error::Auth(_) => "auth",
            Error::Backend(_) => "backend",
            Error::Authentication(_)
            | Error::InvalidSignature
            | Error::KeyNotFound(_)
            | Error::PermissionDenied(_)
            | Error::InvalidKeyFormat(_) => "auth",
            Error::Io(_) => "io",
            Error::Serialize(_) => "serialize",
            Error::NotFound | Error::AlreadyExists => "core",
            Error::InvalidOperation(_) => "core",
        }
    }

    /// Check if this error indicates a resource was not found.
    pub fn is_not_found(&self) -> bool {
        match self {
            Error::NotFound | Error::KeyNotFound(_) => true,
            Error::Auth(auth_err) => auth_err.is_key_not_found(),
            Error::Backend(backend_err) => backend_err.is_not_found(),
            _ => false,
        }
    }

    /// Check if this error indicates permission was denied.
    pub fn is_permission_denied(&self) -> bool {
        match self {
            Error::PermissionDenied(_) => true,
            Error::Auth(auth_err) => auth_err.is_permission_denied(),
            _ => false,
        }
    }

    /// Check if this error indicates a conflict (already exists).
    pub fn is_conflict(&self) -> bool {
        matches!(self, Error::AlreadyExists)
    }

    /// Check if this error is authentication-related.
    pub fn is_authentication_error(&self) -> bool {
        matches!(
            self,
            Error::Auth(_)
                | Error::Authentication(_)
                | Error::InvalidSignature
                | Error::KeyNotFound(_)
                | Error::PermissionDenied(_)
                | Error::InvalidKeyFormat(_)
        )
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
}
