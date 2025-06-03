use crate::Error;
use crate::Result;
use crate::auth::crypto::sign_entry;
use crate::auth::types::AuthId;
use crate::auth::types::AuthInfo;
use crate::constants::SETTINGS;
use crate::data::CRDT;
use crate::data::KVOverWrite;
use crate::entry::Entry;
use crate::entry::{EntryBuilder, ID};
use crate::subtree::SubTree;
use crate::tree::Tree;
use std::cell::RefCell;
use std::rc::Rc;

/// Represents a single, atomic transaction for modifying a `Tree`.
///
/// An `AtomicOp` encapsulates a mutable `EntryBuilder` being constructed. Users interact with
/// specific `SubTree` instances obtained via `AtomicOp::get_subtree` to stage changes.
/// All staged changes across different subtrees within the operation are recorded
/// in the internal `EntryBuilder`.
///
/// When `commit()` is called, the operation:
/// 1. Finalizes the `EntryBuilder` by building an immutable `Entry`
/// 2. Calculates the entry's content-addressable ID
/// 3. Ensures the correct parent links are set based on the tree's state
/// 4. Removes any empty subtrees that didn't have data staged
/// 5. Signs the entry if authentication is configured
/// 6. Persists the resulting immutable `Entry` to the backend
///
/// `AtomicOp` instances are typically created via `Tree::new_operation()`.
#[derive(Clone)]
pub struct AtomicOp {
    /// The entry builder being modified, wrapped in Option to support consuming on commit
    entry_builder: Rc<RefCell<Option<EntryBuilder>>>,
    /// The tree this operation belongs to
    tree: Tree,
    /// Optional authentication key ID for signing entries
    auth_key_id: Option<String>,
}

impl AtomicOp {
    /// Creates a new atomic operation for a specific `Tree`.
    ///
    /// Initializes an internal `EntryBuilder` with its main parent pointers set to the
    /// current tips of the target `Tree`. This captures the state upon which
    /// the operation builds.
    ///
    /// This is typically called internally by `Tree::new_operation()`.
    ///
    /// # Arguments
    /// * `tree` - The `Tree` this operation will modify.
    pub(crate) fn new(tree: &Tree) -> Result<Self> {
        // Start with a basic entry linked to the tree's root.
        // Data and parents will be filled based on the operation type.
        let mut builder = Entry::builder(tree.root_id().clone(), "".to_string());

        // Get current tree tips
        let tree_tips = {
            let backend_guard = tree.lock_backend()?;
            backend_guard.get_tips(tree.root_id())?
        };
        builder.set_parents_mut(tree_tips);

        Ok(Self {
            entry_builder: Rc::new(RefCell::new(Some(builder))),
            tree: tree.clone(),
            auth_key_id: None,
        })
    }

    /// Set the authentication key ID for signing entries created by this operation.
    ///
    /// If set, the operation will attempt to sign the entry with the specified
    /// private key during commit. The private key must be available in the backend's
    /// local key storage.
    ///
    /// # Arguments
    /// * `key_id` - The identifier of the private key to use for signing
    ///
    /// # Returns
    /// Self for method chaining
    pub fn with_auth(mut self, key_id: &str) -> Self {
        self.auth_key_id = Some(key_id.to_string());
        self
    }

    /// Set the authentication key ID for this operation (mutable version).
    ///
    /// # Arguments
    /// * `key_id` - The identifier of the private key to use for signing
    pub fn set_auth_key(&mut self, key_id: &str) {
        self.auth_key_id = Some(key_id.to_string());
    }

    /// Get the current authentication key ID for this operation.
    pub fn auth_key_id(&self) -> Option<&str> {
        self.auth_key_id.as_deref()
    }

    /// Stages an update for a specific subtree within this atomic operation.
    ///
    /// This method is primarily intended for internal use by `SubTree` implementations
    /// (like `KVStore::set`). It records the serialized `data` for the given `subtree`
    /// name within the operation's internal `EntryBuilder`.
    ///
    /// If this is the first modification to the named subtree within this operation,
    /// it also fetches and records the current tips of that subtree from the backend
    /// to set the correct `subtree_parents` for the new entry.
    ///
    /// # Arguments
    /// * `subtree` - The name of the subtree to update.
    /// * `data` - The serialized CRDT data to stage for the subtree.
    ///
    /// # Returns
    /// A `Result<()>` indicating success or an error.
    pub(crate) fn update_subtree(&self, subtree: &str, data: &str) -> Result<()> {
        let mut builder_ref = self.entry_builder.borrow_mut();
        let builder = builder_ref.as_mut().ok_or_else(|| {
            Error::Io(std::io::Error::other(
                "Operation has already been committed",
            ))
        })?;

        // If we haven't cached the tips for this subtree yet, get them now
        let subtrees = builder.subtrees();
        if !subtrees.contains(&subtree.to_string()) {
            let backend_guard = self.tree.lock_backend()?;
            // FIXME: we should get the subtree tips while still using the parent pointers
            let tips = backend_guard.get_subtree_tips(self.tree.root_id(), subtree)?;
            builder.set_subtree_data_mut(subtree.to_string(), data.to_string());
            builder.set_subtree_parents_mut(subtree, tips);
        } else {
            builder.set_subtree_data_mut(subtree.to_string(), data.to_string());
        }

        Ok(())
    }

    /// Gets a handle to a specific `SubTree` for modification within this operation.
    ///
    /// This method creates and returns an instance of the specified `SubTree` type `T`,
    /// associated with this `AtomicOp`. The returned `SubTree` handle can be used to
    /// stage changes (e.g., using `KVStore::set`) for the `subtree_name`.
    /// These changes are recorded within this `AtomicOp`.
    ///
    /// If this is the first time this `subtree_name` is accessed within the operation,
    /// its parent tips will be fetched and stored.
    ///
    /// # Type Parameters
    /// * `T` - The concrete `SubTree` implementation type to create.
    ///
    /// # Arguments
    /// * `subtree_name` - The name of the subtree to get a modification handle for.
    ///
    /// # Returns
    /// A `Result<T>` containing the `SubTree` handle.
    pub fn get_subtree<T>(&self, subtree_name: &str) -> Result<T>
    where
        T: SubTree,
    {
        {
            let mut builder_ref = self.entry_builder.borrow_mut();
            let builder = builder_ref.as_mut().ok_or_else(|| {
                Error::Io(std::io::Error::other(
                    "Operation has already been committed",
                ))
            })?;

            // If we haven't cached the tips for this subtree yet, get them now
            let subtrees = builder.subtrees();
            if !subtrees.contains(&subtree_name.to_string()) {
                let backend_guard = self.tree.lock_backend()?;
                // FIXME: we should get the subtree tips while still using the parent pointers
                let tips = backend_guard.get_subtree_tips(self.tree.root_id(), subtree_name)?;
                builder.set_subtree_data_mut(subtree_name.to_string(), "".to_string());
                builder.set_subtree_parents_mut(subtree_name, tips);
            }
        }

        // Now create the SubTree with the atomic operation
        T::new(self, subtree_name)
    }

    /// Gets the currently staged data for a specific subtree within this operation.
    ///
    /// This is intended for use by `SubTree` implementations to retrieve the data
    /// they have staged locally within the `AtomicOp` before potentially merging
    /// it with historical data.
    ///
    /// # Type Parameters
    /// * `T` - The data type (expected to be a CRDT) to deserialize the staged data into.
    ///
    /// # Arguments
    /// * `subtree_name` - The name of the subtree whose staged data is needed.
    ///
    /// # Returns
    /// A `Result<T>` containing the deserialized staged data. Returns `Ok(T::default())`
    /// if no data has been staged for this subtree in this operation yet.
    pub fn get_local_data<T>(&self, subtree_name: &str) -> Result<T>
    where
        T: serde::de::DeserializeOwned + Default,
    {
        let builder_ref = self.entry_builder.borrow();
        let builder = builder_ref.as_ref().ok_or_else(|| {
            Error::Io(std::io::Error::other(
                "Operation has already been committed",
            ))
        })?;

        if let Ok(data) = builder.data(subtree_name) {
            serde_json::from_str(data).map_err(Error::from)
        } else {
            // If subtree doesn't exist or has no data, return default
            Ok(T::default())
        }
    }

    /// Gets the fully merged historical state of a subtree up to the point this operation began.
    ///
    /// This retrieves all relevant historical entries for the `subtree_name` from the backend,
    /// considering the parent tips recorded when this `AtomicOp` was created (or when the
    /// subtree was first accessed within the operation). It deserializes the data from each
    /// relevant entry into the CRDT type `T` and merges them according to `T`'s `CRDT::merge`
    /// implementation.
    ///
    /// This is intended for use by `SubTree` implementations (e.g., in their `get` or `get_all` methods)
    /// to provide the historical context against which staged changes might be applied or compared.
    ///
    /// # Type Parameters
    /// * `T` - The CRDT type to deserialize and merge the historical subtree data into.
    ///
    /// # Arguments
    /// * `subtree_name` - The name of the subtree.
    ///
    /// # Returns
    /// A `Result<T>` containing the merged historical data of type `T`. Returns `Ok(T::default())`
    /// if the subtree has no history prior to this operation.
    pub(crate) fn get_full_state<T>(&self, subtree_name: &str) -> Result<T>
    where
        T: CRDT,
    {
        // Get the entry builder to get parent pointers
        let mut builder_ref = self.entry_builder.borrow_mut();
        let builder = builder_ref.as_mut().ok_or_else(|| {
            Error::Io(std::io::Error::other(
                "Operation has already been committed",
            ))
        })?;

        // If we haven't cached the tips for this subtree yet, get them now
        let subtrees = builder.subtrees();
        if !subtrees.contains(&subtree_name.to_string()) {
            let backend_guard = self.tree.lock_backend()?;
            // FIXME: we should get the subtree tips while still using the parent pointers
            let tips = backend_guard.get_subtree_tips(self.tree.root_id(), subtree_name)?;
            builder.set_subtree_data_mut(subtree_name.to_string(), "".to_string());
            builder.set_subtree_parents_mut(subtree_name, tips);
        }

        // Get the parent pointers for this subtree
        let parents = builder.subtree_parents(subtree_name).unwrap_or_default();

        // If there are no parents, return a default
        if parents.is_empty() {
            return Ok(T::default());
        }

        // Get the entries from the backend up to these parent pointers
        let backend_guard = self.tree.lock_backend()?;
        let entries =
            backend_guard.get_subtree_from_tips(self.tree.root_id(), subtree_name, &parents)?;

        // Merge all the entries
        let mut result = T::default();
        for entry in entries {
            if let Ok(data) = entry.data(subtree_name) {
                let parsed: T = serde_json::from_str(data)?;
                result = result.merge(&parsed)?;
            }
        }

        Ok(result)
    }

    /// Commits the operation, finalizing and persisting the entry to the backend.
    ///
    /// This method:
    /// 1. Takes ownership of the `EntryBuilder` from the internal `Option`
    /// 2. Removes any empty subtrees
    /// 3. Adds metadata if appropriate
    /// 4. Sets authentication if configured
    /// 5. Builds the immutable `Entry` using `EntryBuilder::build()`
    /// 6. Signs the entry if authentication is configured
    /// 7. Calculates the entry's content-addressable ID
    /// 8. Persists the entry to the backend
    /// 9. Returns the ID of the newly created entry
    ///
    /// After commit, the operation cannot be used again, as the internal
    /// `EntryBuilder` has been consumed.
    ///
    /// # Returns
    /// A `Result<ID>` containing the ID of the committed entry.
    pub fn commit(self) -> Result<ID> {
        // Get the entry out of the RefCell, consuming self in the process
        let builder_cell = self.entry_builder.borrow_mut();
        let builder = builder_cell.as_ref().ok_or_else(|| {
            Error::Io(std::io::Error::other(
                "Operation has already been committed",
            ))
        })?;

        // Clone the builder since we can't easily take ownership
        let mut builder = builder.clone();

        // Check if this is a settings subtree update
        let has_settings_update = builder.subtrees().contains(&SETTINGS.to_string());

        // If this is not a settings update, add metadata with settings tips
        if !has_settings_update {
            // Get the backend to access settings tips
            // FIXME: We should get the subtree tips relative to the parent pointers of this entry
            // rather than the current tips of the tree. This ensures the metadata accurately reflects
            // the settings at the point this entry was created, even in concurrent modification scenarios.
            let backend_guard = self.tree.lock_backend()?;
            let settings_tips = backend_guard.get_subtree_tips(self.tree.root_id(), SETTINGS)?;

            if !settings_tips.is_empty() {
                // Create a KVOverWrite with settings tips
                let mut metadata = KVOverWrite::new();

                // Convert the tips vector to a JSON string
                let tips_json = serde_json::to_string(&settings_tips)?;
                metadata.set(SETTINGS.to_string(), tips_json);

                // Serialize the metadata
                let metadata_json = serde_json::to_string(&metadata)?;

                // Add metadata to the entry builder
                builder.set_metadata_mut(metadata_json);
            }
        }

        // Handle authentication if configured
        let signing_key = if let Some(key_id) = &self.auth_key_id {
            // Set auth ID on the entry builder (without signature initially)
            builder.set_auth_mut(AuthInfo {
                id: AuthId::Direct(key_id.clone()),
                signature: None,
            });

            // Get the private key from backend for signing
            let backend_guard = self.tree.lock_backend()?;
            let signing_key = backend_guard.get_private_key(key_id)?;

            if signing_key.is_none() {
                return Err(Error::Io(std::io::Error::other(format!(
                    "Authentication key '{key_id}' not found in local storage"
                ))));
            }

            signing_key
        } else {
            None
        };

        // Remove empty subtrees and build the final immutable Entry
        let mut entry = builder.remove_empty_subtrees().build();

        // Sign the entry if we have a signing key
        if let Some(signing_key) = signing_key {
            let signature = sign_entry(&entry, &signing_key)?;
            entry.auth.signature = Some(signature);
        }

        // Get the entry's ID
        let id = entry.id();

        // Store in the backend
        let mut backend_guard = self.tree.lock_backend()?;
        backend_guard.put(entry)?;

        Ok(id)
    }
}
