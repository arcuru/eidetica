//!
//! EideticaDB: A decentralized database designed to "Remember Everything".
//! This library provides the core components for building and interacting with EideticaDB instances.
//!
//! ## Core Concepts
//!
//! EideticaDB is built around several key concepts:
//!
//! * **Entries (`entry::Entry`)**: The fundamental, content-addressable unit of data. Entries contain data for a main tree and optional named subtrees.
//! * **Trees (`basedb::Tree`)**: Analogous to tables or branches, representing a history of related entries identified by a root entry ID.
//! * **Backends (`backend::Backend`)**: A pluggable storage layer for persisting entries.
//! * **BaseDB (`basedb::BaseDB`)**: The main database struct that manages trees and interacts with a backend.
//! * **CRDTs (`data::CRDT`)**: Conflict-free Replicated Data Types used for merging data from different entries, particularly for settings and subtree data.
//! * **SubTrees (`subtree::SubTree`)**: Named data structures within a tree that provide specialized data access patterns:
//!     * **KVStore (`subtree::KVStore`)**: A key-value store within a tree.
//!     * **RowStore (`subtree::RowStore`)**: A record-oriented store with automatic primary key generation, similar to a database table.
//! * **Merkle-CRDT**: The underlying principle combining Merkle DAGs (formed by entries and parent links) with CRDTs for efficient, decentralized data synchronization.

pub mod atomicop;
pub mod backend;
pub mod basedb;
pub mod constants;
pub mod data;
pub mod entry;
pub mod subtree;
pub mod tree;

/// Re-export the `Tree` struct for easier access.
pub use tree::Tree;

/// Result type used throughout the EideticaDB library.
pub type Result<T> = std::result::Result<T, Error>;

/// Common error type for the EideticaDB library.
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
}
