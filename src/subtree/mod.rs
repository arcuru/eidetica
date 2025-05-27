use crate::Result;
use crate::atomicop::AtomicOp;

mod kvstore;
pub use kvstore::KVStore;

mod rowstore;
pub use rowstore::RowStore;

#[cfg(feature = "y-crdt")]
mod yrsstore;
#[cfg(feature = "y-crdt")]
pub use yrsstore::{YrsBinary, YrsStore};

/// A trait representing a named, CRDT-based data structure within a `Tree`.
///
/// `SubTree` implementations define how data within a specific named partition of a `Tree`
/// is structured, accessed, and modified. They work in conjunction with an `AtomicOp`
/// to stage changes before committing them as a single `Entry`.
///
/// Users typically interact with `SubTree` implementations obtained either via:
/// 1. `Tree::get_subtree_viewer`: For read-only access to the current merged state.
/// 2. `AtomicOp::get_subtree`: For staging modifications within an atomic operation.
pub trait SubTree: Sized {
    /// Creates a new `SubTree` handle associated with a specific atomic operation.
    ///
    /// This constructor is typically called internally by `AtomicOp::get_subtree` or
    /// `Tree::get_subtree_viewer`. The resulting `SubTree` instance provides methods
    /// to interact with the data of the specified `subtree_name`, potentially staging
    /// changes within the provided `op`.
    ///
    /// # Arguments
    /// * `op` - The `AtomicOp` this `SubTree` instance will read from and potentially write to.
    /// * `subtree_name` - The name identifying this specific data partition within the `Tree`.
    fn new(op: &AtomicOp, subtree_name: &str) -> Result<Self>;

    /// Returns the name of this subtree.
    fn name(&self) -> &str;
}
