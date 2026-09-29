//! Database-style backend implementations
//!
//! These backends provide persistent, queryable storage similar to traditional databases.

pub(crate) mod completeness;
mod in_memory;
mod sorting;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub mod sql;

pub use in_memory::InMemory;
#[cfg(feature = "postgres")]
pub use sql::Postgres;
#[cfg(feature = "sqlite")]
pub use sql::Sqlite;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub use sql::{DbKind, SqlxBackend};
