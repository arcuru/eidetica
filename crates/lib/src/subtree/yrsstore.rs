//! Y-CRDT integration for Eidetica
//!
//! This module provides seamless integration between Eidetica's atomic operation system
//! and Y-CRDT (Yjs) for real-time collaborative editing. The main component is `YrsStore`,
//! which implements differential saving to minimize storage overhead while maintaining
//! full compatibility with Y-CRDT's conflict resolution algorithms.
//!
//! # Key Features
//!
//! - **Differential Saving**: Only stores incremental changes, not full document state
//! - **Efficient Caching**: Caches expensive backend data retrieval operations
//! - **Seamless Integration**: Works with Eidetica's atomic operation and viewer model
//! - **Full Y-CRDT API**: Exposes the complete yrs library functionality
//!
//! # Performance Considerations
//!
//! The implementation caches the expensive `get_full_state()` backend operation and
//! constructs documents and state vectors on-demand from this cached data. This
//! approach minimizes both I/O overhead and memory usage.
//!
//! This module is only available when the "y-crdt" feature is enabled.

use crate::atomicop::AtomicOp;
use crate::data::{CRDT, Data};
use crate::subtree::SubTree;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use yrs::updates::decoder::Decode;
use yrs::{Doc, ReadTxn, Transact, Update};

/// A CRDT wrapper for Y-CRDT binary update data.
///
/// This wrapper implements the required `Data` and `CRDT` traits to allow
/// Y-CRDT binary updates to be stored and merged within the Eidetica system.
///
/// ## Design
///
/// Y-CRDT represents document state as binary updates that can be efficiently
/// merged and applied. This wrapper enables these binary updates to participate
/// in Eidetica's CRDT-based data storage and synchronization system.
///
/// ## Merging Strategy
///
/// When two `YrsBinary` instances are merged, both updates are applied to a new
/// Y-CRDT document, and the resulting merged state is returned as a new binary
/// update. This ensures that Y-CRDT's sophisticated conflict resolution algorithms
/// are preserved within Eidetica's merge operations.
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct YrsBinary {
    data: Vec<u8>,
}

impl Data for YrsBinary {}

impl CRDT for YrsBinary {
    /// Merges two Y-CRDT binary updates by applying both to a new document
    /// and returning the resulting state as a binary update.
    fn merge(&self, other: &Self) -> Result<Self> {
        let doc = Doc::new();

        // Apply self's update if not empty
        if !self.data.is_empty() {
            let update = Update::decode_v1(&self.data).map_err(|e| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Failed to decode Y-CRDT update (self): {e}"),
                ))
            })?;
            let mut txn = doc.transact_mut();
            txn.apply_update(update).map_err(|e| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Failed to apply Y-CRDT update (self): {e}"),
                ))
            })?;
        }

        // Apply other's update if not empty
        if !other.data.is_empty() {
            let other_update = Update::decode_v1(&other.data).map_err(|e| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Failed to decode Y-CRDT update (other): {e}"),
                ))
            })?;
            let mut txn = doc.transact_mut();
            txn.apply_update(other_update).map_err(|e| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Failed to apply Y-CRDT update (other): {e}"),
                ))
            })?;
        }

        // Return the merged state as a binary update
        let txn = doc.transact();
        let merged_update = txn.encode_state_as_update_v1(&yrs::StateVector::default());

        Ok(YrsBinary {
            data: merged_update,
        })
    }
}

impl YrsBinary {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

/// A Y-CRDT based SubTree implementation with efficient differential saving.
///
/// `YrsStore` provides a CRDT-based storage abstraction using the yrs library,
/// which is a Rust port of Yjs. This allows for real-time collaborative editing
/// and automatic conflict resolution through the Y-CRDT algorithms.
///
/// ## Architecture
///
/// The `YrsStore` integrates with Eidetica's atomic operation system to provide:
/// - **Differential Updates**: Only saves incremental changes, not full document state
/// - **Efficient Caching**: Caches expensive backend data retrieval operations
/// - **Operation/Viewer Model**: Compatible with Eidetica's transaction patterns
/// - **Full Y-CRDT API**: Direct access to the complete yrs library functionality
///
/// ## Caching Strategy
///
/// To optimize performance, `YrsStore` caches the expensive `get_full_state()` operation
/// from the backend and constructs documents and state vectors on-demand from this
/// cached data. This approach minimizes I/O operations while keeping memory usage low.
///
/// ## Differential Saving
///
/// When saving documents, `YrsStore` calculates diffs relative to the current backend
/// state rather than saving full document snapshots. This significantly reduces storage
/// overhead for large documents with incremental changes.
///
/// ## Usage
///
/// The `YrsStore` exposes the underlying Y-CRDT document directly, allowing users
/// to work with the full yrs API. Changes are automatically captured and stored
/// when the atomic operation is committed.
///
/// ```rust,no_run
/// use eidetica::subtree::YrsStore;
/// use yrs::{Map, Text, Transact};
/// # use eidetica::Result;
/// # fn example(store: &YrsStore) -> Result<()> {
/// // Work directly with the yrs document
/// store.with_doc_mut(|doc| {
///     let map = doc.get_or_insert_map("root");
///     let text = doc.get_or_insert_text("document");
///
///     let mut txn = doc.transact_mut();
///     map.insert(&mut txn, "key", "value");
///     text.insert(&mut txn, 0, "Hello, World!");
///
///     Ok(())
/// })?;
/// # Ok(())
/// # }
/// ```
pub struct YrsStore {
    /// The name identifier for this subtree within the atomic operation
    name: String,
    /// Reference to the atomic operation for backend data access
    atomic_op: AtomicOp,
    /// Cached backend data to avoid expensive get_full_state() calls
    /// This contains the merged historical state as Y-CRDT binary data
    cached_backend_data: RefCell<Option<YrsBinary>>,
}

impl SubTree for YrsStore {
    fn new(op: &AtomicOp, subtree_name: &str) -> Result<Self> {
        Ok(Self {
            name: subtree_name.to_string(),
            atomic_op: op.clone(),
            cached_backend_data: RefCell::new(None),
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

impl YrsStore {
    /// Gets the current Y-CRDT document, merging all historical state.
    ///
    /// This method reconstructs the current state of the Y-CRDT document by:
    /// 1. Loading the full historical state from the backend (cached)
    /// 2. Applying any local changes from the current atomic operation
    /// 3. Returning a Y-Doc that can be used for reading and further modifications
    ///
    /// ## Performance
    ///
    /// The expensive backend data retrieval is cached, so subsequent calls are fast.
    /// Documents are constructed fresh each time to ensure isolation between operations.
    ///
    /// ## Returns
    /// A `Result` containing the merged `Doc` (Y-CRDT document).
    ///
    /// ## Errors
    /// Returns an error if there are issues deserializing the Y-CRDT updates.
    pub fn doc(&self) -> Result<Doc> {
        let doc = self.get_initial_doc()?;

        // Apply local changes if they exist
        let local_data = self
            .atomic_op
            .get_local_data::<YrsBinary>(&self.name)
            .unwrap_or_default();

        if !local_data.is_empty() {
            let local_update = Update::decode_v1(local_data.as_bytes()).map_err(|e| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Failed to decode local Y-CRDT update: {e}"),
                ))
            })?;

            let mut txn = doc.transact_mut();
            txn.apply_update(local_update).map_err(|e| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Failed to apply local Y-CRDT update: {e}"),
                ))
            })?;
        }

        Ok(doc)
    }

    /// Executes a function with read-only access to the Y-Doc.
    ///
    /// This method provides access to the current state of the document
    /// for read-only operations. No changes are persisted.
    ///
    /// ## Arguments
    /// * `f` - A function that receives the Y-Doc for reading
    ///
    /// ## Returns
    /// A `Result` containing the return value of the function.
    ///
    /// ## Example
    /// ```rust,no_run
    /// # use eidetica::Result;
    /// # use yrs::{Transact, GetString};
    /// # fn example(store: &eidetica::subtree::YrsStore) -> Result<()> {
    /// let content = store.with_doc(|doc| {
    ///     let text = doc.get_or_insert_text("document");
    ///     let txn = doc.transact();
    ///     Ok(text.get_string(&txn))
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_doc<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&Doc) -> Result<R>,
    {
        let doc = self.doc()?;
        f(&doc)
    }

    /// Executes a function with access to the Y-Doc and automatically saves changes.
    ///
    /// This is the preferred way to make changes to the document as it
    /// ensures all changes are captured using differential saving and staged
    /// in the atomic operation for later commit.
    ///
    /// ## Differential Saving
    ///
    /// Changes are saved as diffs relative to the current backend state, which
    /// significantly reduces storage overhead compared to saving full document
    /// snapshots.
    ///
    /// ## Arguments
    /// * `f` - A function that receives the Y-Doc and can make modifications
    ///
    /// ## Returns
    /// A `Result` containing the return value of the function.
    ///
    /// ## Example
    /// ```rust,no_run
    /// # use eidetica::Result;
    /// # use yrs::{Transact, Text};
    /// # fn example(store: &eidetica::subtree::YrsStore) -> Result<()> {
    /// store.with_doc_mut(|doc| {
    ///     let text = doc.get_or_insert_text("document");
    ///     let mut txn = doc.transact_mut();
    ///     text.insert(&mut txn, 0, "Hello, World!");
    ///     Ok(())
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_doc_mut<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&Doc) -> Result<R>,
    {
        let doc = self.doc()?;
        let result = f(&doc)?;
        self.save_doc(&doc)?;
        Ok(result)
    }

    /// Applies a Y-CRDT update to the document.
    ///
    /// This method is useful for receiving updates from other collaborators or
    /// applying updates received through a network provider. The update is applied
    /// to the current document state and saved using differential saving.
    ///
    /// ## Use Cases
    /// - Applying updates from remote collaborators
    /// - Synchronizing with external Y-CRDT instances
    /// - Replaying historical updates
    ///
    /// ## Arguments
    /// * `update_data` - The binary Y-CRDT update data
    ///
    /// ## Returns
    /// A `Result<()>` indicating success or failure.
    ///
    /// ## Errors
    /// Returns an error if the update data is malformed or cannot be applied.
    pub fn apply_update(&self, update_data: &[u8]) -> Result<()> {
        let doc = self.doc()?;
        let update = Update::decode_v1(update_data).map_err(|e| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Failed to decode Y-CRDT update: {e}"),
            ))
        })?;

        {
            let mut txn = doc.transact_mut();
            txn.apply_update(update).map_err(|e| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Failed to apply Y-CRDT update: {e}"),
                ))
            })?;
        }

        self.save_doc(&doc)
    }

    /// Gets the current state of the document as a binary update.
    ///
    /// This method encodes the complete current document state as a Y-CRDT binary
    /// update that can be used to synchronize with other instances or persist
    /// the current state.
    ///
    /// ## Use Cases
    /// - Synchronizing document state with other instances
    /// - Creating snapshots of the current state
    /// - Sharing the complete document with new collaborators
    ///
    /// ## Returns
    /// A `Result` containing the binary update data representing the full document state.
    ///
    /// ## Performance
    /// This method constructs the full document state, so it may be expensive for
    /// large documents. For incremental synchronization, consider using the
    /// differential updates automatically saved by `with_doc_mut()`.
    pub fn get_update(&self) -> Result<Vec<u8>> {
        let doc = self.doc()?;
        let txn = doc.transact();
        let update = txn.encode_state_as_update_v1(&yrs::StateVector::default());
        Ok(update)
    }

    /// Saves the complete document state to the atomic operation.
    ///
    /// This method captures the entire current state of the document and stages it
    /// in the atomic operation. Unlike `save_doc()`, this saves the full document
    /// state rather than just the incremental changes.
    ///
    /// ## When to Use
    /// - When you need to ensure the complete state is captured
    /// - For creating clean snapshots without incremental history
    /// - When differential saving is not suitable for your use case
    ///
    /// ## Performance Impact
    /// This method is less storage-efficient than `save_doc()` as it saves the
    /// complete document state regardless of what changes were made.
    ///
    /// ## Arguments
    /// * `doc` - The Y-CRDT document to save
    ///
    /// ## Returns
    /// A `Result<()>` indicating success or failure.
    pub fn save_doc_full(&self, doc: &Doc) -> Result<()> {
        let txn = doc.transact();
        let update = txn.encode_state_as_update_v1(&yrs::StateVector::default());

        let yrs_binary = YrsBinary::new(update);
        let serialized = serde_json::to_string(&yrs_binary)?;
        self.atomic_op.update_subtree(&self.name, &serialized)
    }

    /// Saves the document state using efficient differential encoding.
    ///
    /// This method captures only the changes since the current backend state and
    /// stages them in the atomic operation. This is the preferred saving method
    /// as it significantly reduces storage overhead for incremental changes.
    ///
    /// ## Differential Encoding
    ///
    /// The method works by:
    /// 1. Getting the current backend state vector (cached for efficiency)
    /// 2. Encoding only the changes since that state
    /// 3. Saving only the incremental diff, not the full document
    ///
    /// ## Storage Efficiency
    ///
    /// For a document with small incremental changes, this can reduce storage
    /// requirements by orders of magnitude compared to saving full snapshots.
    ///
    /// ## Arguments
    /// * `doc` - The Y-CRDT document to save differentially
    ///
    /// ## Returns
    /// A `Result<()>` indicating success or failure.
    ///
    /// ## Performance
    /// This method is optimized for performance - the expensive backend state
    /// retrieval is cached, and only minimal diff calculation is performed.
    pub fn save_doc(&self, doc: &Doc) -> Result<()> {
        let txn = doc.transact();

        // Get the backend state vector efficiently
        let backend_state_vector = self.get_initial_state_vector()?;

        // Encode only the changes since the backend state
        let diff_update = txn.encode_state_as_update_v1(&backend_state_vector);

        // Only save if there are actual changes
        if !diff_update.is_empty() {
            let yrs_binary = YrsBinary::new(diff_update);
            let serialized = serde_json::to_string(&yrs_binary)?;
            self.atomic_op.update_subtree(&self.name, &serialized)?;
        }

        Ok(())
    }

    /// Gets the state vector of the backend data efficiently without constructing the full document.
    ///
    /// This method extracts just the state vector from the cached backend data,
    /// which is used for efficient differential encoding. The state vector represents
    /// the "version" information for each client that has contributed to the document.
    ///
    /// ## Implementation
    ///
    /// Rather than constructing the full document just to get the state vector,
    /// this method creates a minimal temporary document, applies the backend data,
    /// and extracts only the state vector information.
    ///
    /// ## Caching
    ///
    /// This method leverages the cached backend data, so the expensive `get_full_state()`
    /// operation is only performed once per `YrsStore` instance.
    ///
    /// ## Returns
    /// A `Result` containing the state vector representing the backend document state.
    ///
    /// ## Errors
    /// Returns an error if the cached backend data cannot be decoded or applied.
    fn get_initial_state_vector(&self) -> Result<yrs::StateVector> {
        // Get the cached backend data
        let backend_data = self.get_cached_backend_data()?;

        if backend_data.is_empty() {
            return Ok(yrs::StateVector::default());
        }

        // Construct a temporary document to extract the state vector
        let temp_doc = Doc::new();
        let backend_update = Update::decode_v1(backend_data.as_bytes()).map_err(|e| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Failed to decode backend Y-CRDT update: {e}"),
            ))
        })?;
        let mut temp_txn = temp_doc.transact_mut();
        temp_txn.apply_update(backend_update).map_err(|e| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Failed to apply backend Y-CRDT update: {e}"),
            ))
        })?;
        drop(temp_txn);
        let temp_txn = temp_doc.transact();
        Ok(temp_txn.state_vector())
    }

    /// Constructs a Y-CRDT document from the cached backend data.
    ///
    /// This method creates a fresh document instance and applies the historical
    /// backend state to it. Each call returns a new document instance to ensure
    /// proper isolation between different operations and viewers.
    ///
    /// ## Caching Strategy
    ///
    /// The expensive `get_full_state()` operation is cached, but documents are
    /// constructed fresh each time. This balances performance (avoiding expensive
    /// I/O) with safety (ensuring document isolation).
    ///
    /// ## Returns
    /// A `Result` containing a new `Doc` instance with the backend state applied.
    ///
    /// ## Errors
    /// Returns an error if the cached backend data cannot be decoded or applied.
    fn get_initial_doc(&self) -> Result<Doc> {
        // Get the cached backend data
        let backend_data = self.get_cached_backend_data()?;

        // Create a new doc and apply backend data if it exists
        let doc = Doc::new();
        if !backend_data.is_empty() {
            let update = Update::decode_v1(backend_data.as_bytes()).map_err(|e| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Failed to decode Y-CRDT update: {e}"),
                ))
            })?;

            let mut txn = doc.transact_mut();
            txn.apply_update(update).map_err(|e| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Failed to apply Y-CRDT update from backend: {e}"),
                ))
            })?;
        }

        Ok(doc)
    }

    /// Retrieves backend data with caching to avoid expensive repeated `get_full_state()` calls.
    ///
    /// This is the core caching mechanism for `YrsStore`. The first call performs the
    /// expensive `atomic_op.get_full_state()` operation and caches the result. All
    /// subsequent calls return the cached data immediately.
    ///
    /// ## Performance Impact
    ///
    /// The `get_full_state()` operation can be expensive as it involves reading and
    /// merging potentially large amounts of historical data from the backend storage.
    /// By caching this data, we avoid repeating this expensive operation multiple times
    /// within the same atomic operation scope.
    ///
    /// ## Cache Lifetime
    ///
    /// The cache is tied to the lifetime of the `YrsStore` instance, which typically
    /// corresponds to a single atomic operation. This ensures that:
    /// - Data is cached for the duration of the operation
    /// - Fresh data is loaded for each new operation
    /// - Memory usage is bounded to the operation scope
    ///
    /// ## Returns
    /// A `Result` containing the cached `YrsBinary` backend data.
    ///
    /// ## Errors
    /// Returns an error if the backend data cannot be retrieved or deserialized.
    fn get_cached_backend_data(&self) -> Result<YrsBinary> {
        // Check if we already have the backend data cached
        if let Some(backend_data) = self.cached_backend_data.borrow().as_ref() {
            return Ok(backend_data.clone());
        }

        // Perform the expensive operation once
        let backend_data = self.atomic_op.get_full_state::<YrsBinary>(&self.name)?;

        // Cache it for future use
        *self.cached_backend_data.borrow_mut() = Some(backend_data.clone());

        Ok(backend_data)
    }
}
