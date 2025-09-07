use crate::{Result, Transaction};

mod errors;
pub use errors::StoreError;

mod docstore;
pub use docstore::DocStore;

mod table;
pub use table::Table;

#[cfg(feature = "y-crdt")]
mod ydoc;
#[cfg(feature = "y-crdt")]
pub use ydoc::{YDoc, YrsBinary};

/// A trait representing a named, CRDT-based data structure within a `Database`.
///
/// `Store` implementations define how data within a specific named partition of a `Database`
/// is structured, accessed, and modified. They work in conjunction with a `Transaction`
/// to stage changes before committing them as a single `Entry`.
///
/// Users typically interact with `Store` implementations obtained either via:
/// 1. `Database::get_store_viewer`: For read-only access to the current merged state.
/// 2. `Transaction::get_store`: For staging modifications within an atomic operation.
pub trait Store: Sized {
    /// Creates a new `Store` handle associated with a specific transaction.
    ///
    /// This constructor is typically called internally by `Transaction::get_store` or
    /// `Database::get_store_viewer`. The resulting `Store` instance provides methods
    /// to interact with the data of the specified `subtree_name`, potentially staging
    /// changes within the provided `op`.
    ///
    /// # Arguments
    /// * `op` - The `Transaction` this `Store` instance will read from and potentially write to.
    /// * `subtree_name` - The name identifying this specific data partition within the `Database`.
    fn new(op: &Transaction, subtree_name: impl Into<String>) -> Result<Self>;

    /// Returns the name of this subtree.
    fn name(&self) -> &str;
}
