use crate::atomicop::AtomicOp;
use crate::data::{KVNested, NestedValue, CRDT};
use crate::subtree::SubTree;
use crate::{Error, Result};

/// A simple key-value store SubTree
///
/// It assumes that the SubTree data is a KVNested CRDT, which allows for nested map structures.
/// This implementation supports string values, as well as deletions via tombstones.
/// For more complex data structures, consider using the nested capabilities of KVNested directly.
pub struct KVStore {
    name: String,
    atomic_op: AtomicOp,
}

impl SubTree for KVStore {
    fn new(op: &AtomicOp, subtree_name: &str) -> Result<Self> {
        Ok(Self {
            name: subtree_name.to_string(),
            atomic_op: op.clone(),
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

impl KVStore {
    /// Gets a value associated with a key from the SubTree.
    ///
    /// This method prioritizes returning data staged within the current `AtomicOp`.
    /// If the key is not found in the staged data it retrieves the fully merged historical
    /// state from the backend up to the point defined by the `AtomicOp`'s parents and
    /// returns the value from there.
    ///
    /// # Arguments
    /// * `key` - The key to retrieve the value for.
    ///
    /// # Returns
    /// A `Result` containing the NestedValue if found, or `Error::NotFound`.
    pub fn get<K>(&self, key: K) -> Result<NestedValue>
    where
        K: Into<String>,
    {
        let key_s = key.into();
        // First check if there's any data in the atomic op itself
        let local_data: Result<KVNested> = self.atomic_op.get_local_data(&self.name);

        // If there's data in the operation and it contains the key, return that
        if let Ok(data) = local_data {
            if let Some(value) = data.get(&key_s) {
                return Ok(value.clone());
            }
        }

        // Otherwise, get the full state from the backend
        let data: KVNested = self.atomic_op.get_full_state(&self.name)?;

        // Get the value
        match data.get(&key_s) {
            Some(value) => Ok(value.clone()),
            None => Err(Error::NotFound),
        }
    }

    /// Gets a string value associated with a key from the SubTree.
    ///
    /// This is a convenience method that calls `get()` and expects the value to be a string.
    ///
    /// # Arguments
    /// * `key` - The key to retrieve the value for.
    ///
    /// # Returns
    /// A `Result` containing the string value if found, or an error if the key is not found
    /// or if the value is not a string.
    pub fn get_string<K>(&self, key: K) -> Result<String>
    where
        K: Into<String>,
    {
        match self.get(key)? {
            NestedValue::String(value) => Ok(value),
            NestedValue::Map(_) => Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Expected string value, found a nested map",
            ))),
            NestedValue::Deleted => Err(Error::NotFound),
        }
    }

    /// Stages the setting of a key-value pair within the associated `AtomicOp`.
    ///
    /// This method updates the `KVNested` data held within the `AtomicOp` for this
    /// `KVStore` instance's subtree name. The change is **not** persisted to the backend
    /// until the `AtomicOp::commit()` method is called.
    ///
    /// Calling this method on a `KVStore` obtained via `Tree::get_subtree_viewer` is possible
    /// but the changes will be ephemeral and discarded, as the viewer's underlying `AtomicOp`
    /// is not intended to be committed.
    ///
    /// # Arguments
    /// * `key` - The key to set.
    /// * `value` - The value to associate with the key.
    ///
    /// # Returns
    /// A `Result<()>` indicating success or an error during serialization or staging.
    pub fn set<K, V>(&self, key: K, value: V) -> Result<()>
    where
        K: Into<String>,
        V: Into<String>,
    {
        // Get current data from the atomic op, or create new if not existing
        let mut data = self
            .atomic_op
            .get_local_data::<KVNested>(&self.name)
            .unwrap_or_default();

        // Update the data
        data.set_string(key.into(), value.into());

        // Serialize and update the atomic op
        let serialized = serde_json::to_string(&data)?;
        self.atomic_op.update_subtree(&self.name, &serialized)
    }

    /// Stages the setting of a nested value within the associated `AtomicOp`.
    ///
    /// This method allows setting any valid NestedValue type (String, Map, or Deleted).
    ///
    /// # Arguments
    /// * `key` - The key to set.
    /// * `value` - The NestedValue to associate with the key.
    ///
    /// # Returns
    /// A `Result<()>` indicating success or an error during serialization or staging.
    pub fn set_value<K>(&self, key: K, value: NestedValue) -> Result<()>
    where
        K: Into<String>,
    {
        // Get current data from the atomic op, or create new if not existing
        let mut data = self
            .atomic_op
            .get_local_data::<KVNested>(&self.name)
            .unwrap_or_default();

        // Update the data
        data.set(key.into(), value);

        // Serialize and update the atomic op
        let serialized = serde_json::to_string(&data)?;
        self.atomic_op.update_subtree(&self.name, &serialized)
    }

    /// Stages the deletion of a key within the associated `AtomicOp`.
    ///
    /// This method removes the key-value pair from the `KVNested` data held within
    /// the `AtomicOp` for this `KVStore` instance's subtree name. A tombstone is created,
    /// which will propagate the deletion when merged with other data. The change is **not**
    /// persisted to the backend until the `AtomicOp::commit()` method is called.
    ///
    /// When using the `get` method, deleted keys will return `Error::NotFound`. However,
    /// the deletion is still tracked internally as a tombstone, which ensures that the
    /// deletion propagates correctly when merging with other versions of the data.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # use eidetica::Tree;
    /// # use eidetica::subtree::KVStore;
    /// # let tree: Tree = unimplemented!();
    /// let op = tree.new_operation().unwrap();
    /// let store = op.get_subtree::<KVStore>("my_data").unwrap();
    ///
    /// // First set a value
    /// store.set("user1", "Alice").unwrap();
    ///
    /// // Later delete the value
    /// store.delete("user1").unwrap();
    ///
    /// // Attempting to get the deleted key will return NotFound
    /// assert!(store.get("user1").is_err());
    ///
    /// // You can verify the tombstone exists by checking the full state
    /// let all_data = store.get_all().unwrap();
    /// assert!(all_data.as_hashmap().contains_key("user1"));
    /// ```
    ///
    /// # Arguments
    /// * `key` - The key to delete.
    ///
    /// # Returns
    /// A `Result<()>` indicating success or an error during serialization or staging.
    pub fn delete<K>(&self, key: K) -> Result<()>
    where
        K: Into<String>,
    {
        // Get current data from the atomic op, or create new if not existing
        let mut data = self
            .atomic_op
            .get_local_data::<KVNested>(&self.name)
            .unwrap_or_default();

        // Remove the key (creates a tombstone)
        data.remove(&key.into());

        // Serialize and update the atomic op
        let serialized = serde_json::to_string(&data)?;
        self.atomic_op.update_subtree(&self.name, &serialized)
    }

    /// Retrieves all key-value pairs, merging staged data with historical state.
    ///
    /// This method combines the data staged within the current `AtomicOp` with the
    /// fully merged historical state from the backend, providing a complete view
    /// of the key-value store as it would appear if the operation were committed.
    /// The staged data takes precedence in case of conflicts (overwrites).
    ///
    /// # Returns
    /// A `Result` containing the merged `KVNested` data structure.
    pub fn get_all(&self) -> Result<KVNested> {
        // First get the local data directly from the atomic op
        let local_data = self.atomic_op.get_local_data::<KVNested>(&self.name);

        // Get the full state from the backend
        let mut data = self.atomic_op.get_full_state::<KVNested>(&self.name)?;

        // If there's also local data, merge it with the full state
        if let Ok(local) = local_data {
            data = data.merge(&local)?;
        }

        Ok(data)
    }

    /// Gets a mutable editor for a value associated with the given key.
    ///
    /// If the key does not exist, the editor will be initialized with an empty map,
    /// allowing immediate use of map-modifying methods. The type can be changed
    /// later using `ValueEditor::set()`.
    ///
    /// Changes made via the `ValueEditor` are staged in the `AtomicOp` by its `set` method
    /// and must be committed via `AtomicOp::commit()` to be persisted to the `KVStore`'s backend.
    pub fn get_value_mut<K>(&self, key: K) -> ValueEditor<'_>
    where
        K: Into<String>,
    {
        ValueEditor::new(self, vec![key.into()])
    }

    /// Gets a mutable editor for the root of this KVStore's subtree.
    ///
    /// Changes made via the `ValueEditor` are staged in the `AtomicOp` by its `set` method
    /// and must be committed via `AtomicOp::commit()` to be persisted to the `KVStore`'s backend.
    pub fn get_root_mut(&self) -> ValueEditor<'_> {
        ValueEditor::new(self, Vec::new())
    }

    /// Retrieves a `NestedValue` from the KVStore using a specified path.
    ///
    /// The path is a slice of strings, where each string is a key in the
    /// nested map structure. If the path is empty, it retrieves the entire
    /// content of this KVStore's named subtree as a `NestedValue::Map`.
    ///
    /// This method operates on the fully merged view of the KVStore's data,
    /// including any local changes from the current `AtomicOp` layered on top
    /// of the backend state.
    ///
    /// # Arguments
    ///
    /// * `path`: A slice of `String` representing the path to the desired value.
    ///
    /// # Errors
    ///
    /// * `Error::NotFound` if any segment of the path does not exist (for non-empty paths),
    ///   or if the final value or an intermediate value is a `NestedValue::Deleted` (tombstone).
    /// * `Error::Io` with `ErrorKind::InvalidData` if a non-map value is
    ///   encountered during path traversal where a map was expected.
    pub fn get_at_path<S, P>(&self, path: P) -> Result<NestedValue>
    where
        S: AsRef<str>,
        P: AsRef<[S]>,
    {
        let path_slice = path.as_ref();
        if path_slice.is_empty() {
            // Requesting the root of this KVStore's named subtree
            return Ok(NestedValue::Map(self.get_all()?));
        }

        let mut current_value_view = NestedValue::Map(self.get_all()?);

        for key_segment_s in path_slice.iter() {
            match current_value_view {
                NestedValue::Map(map_data) => match map_data.get(key_segment_s.as_ref()) {
                    Some(next_value) => {
                        current_value_view = next_value.clone();
                    }
                    None => return Err(Error::NotFound),
                },
                NestedValue::Deleted => {
                    // A tombstone encountered in the path means the path doesn't lead to a value.
                    return Err(Error::NotFound);
                }
                _ => {
                    // Expected a map to continue traversal, but found something else.
                    return Err(Error::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "Path traversal failed: expected a map at segment before '{}', but found a non-map value.",
                            key_segment_s.as_ref()
                        ),
                    )));
                }
            }
        }

        // Check if the final resolved value is a tombstone.
        match current_value_view {
            NestedValue::Deleted => Err(Error::NotFound),
            _ => Ok(current_value_view),
        }
    }

    /// Sets a `NestedValue` at a specified path within the `KVStore`'s `AtomicOp`.
    ///
    /// The path is a slice of strings, where each string is a key in the
    /// nested map structure.
    ///
    /// This method modifies the local data associated with the `AtomicOp`. The changes
    /// are not persisted to the backend until `AtomicOp::commit()` is called.
    /// If the path does not exist, it will be created. Intermediate non-map values
    /// in the path will be overwritten by maps as needed to complete the path.
    ///
    /// # Arguments
    ///
    /// * `path`: A slice of `String` representing the path where the value should be set.
    /// * `value`: The `NestedValue` to set at the specified path.
    ///
    /// # Errors
    ///
    /// * `Error::InvalidOperation` if the `path` is empty and `value` is not a `NestedValue::Map`.
    /// * `Error::Serialize` if the updated subtree data cannot be serialized to JSON.
    /// * Potentially other errors from `AtomicOp::update_subtree`.
    pub fn set_at_path<S, P>(&self, path: P, value: NestedValue) -> Result<()>
    where
        S: AsRef<str>,
        P: AsRef<[S]>,
    {
        let path_slice = path.as_ref();
        if path_slice.is_empty() {
            // Setting the root of this KVStore's named subtree.
            // The value must be a map.
            if let NestedValue::Map(map_data) = value {
                let serialized_data = serde_json::to_string(&map_data)?;
                return self.atomic_op.update_subtree(&self.name, &serialized_data);
            } else {
                return Err(Error::InvalidOperation(
                    "Cannot set root of KVStore subtree: value must be a NestedValue::Map"
                        .to_string(),
                ));
            }
        }

        let mut subtree_data = self
            .atomic_op
            .get_local_data::<KVNested>(&self.name)
            .unwrap_or_default();

        let mut current_map_mut = &mut subtree_data;

        // Traverse or create path segments up to the parent of the target key.
        for key_segment_s in path_slice.iter().take(path_slice.len() - 1) {
            let key_segment_string = key_segment_s.as_ref().to_string();
            let entry = current_map_mut.as_hashmap_mut().entry(key_segment_string);
            current_map_mut = match entry.or_insert_with(|| NestedValue::Map(KVNested::default())) {
                NestedValue::Map(ref mut map) => map,
                non_map_val => {
                    // If a non-map value exists at an intermediate path segment,
                    // overwrite it with a map to continue.
                    *non_map_val = NestedValue::Map(KVNested::default());
                    if let NestedValue::Map(ref mut map) = non_map_val {
                        map
                    } else {
                        // This unreachable!() is safe because we just assigned NestedValue::Map.
                        unreachable!("Just assigned a map, should be a map variant");
                    }
                }
            };
        }

        // Set the value at the final key in the path.
        if let Some(last_key_s) = path_slice.last() {
            current_map_mut.set(last_key_s.as_ref().to_string(), value);
        } else {
            // This case should be prevented by the initial path.is_empty() check.
            // Given the check, this is technically unreachable if path is not empty.
            return Err(Error::InvalidOperation(
                "Path became empty unexpectedly during set_at_path".to_string(),
            ));
        }

        let serialized_data = serde_json::to_string(&subtree_data)?;
        self.atomic_op.update_subtree(&self.name, &serialized_data)
    }
}

/// An editor for a `NestedValue` obtained from a `KVStore`.
///
/// This provides a mutable lens into a value, allowing modifications
/// to be staged and then saved back to the KVStore.
pub struct ValueEditor<'a> {
    kv_store: &'a KVStore,
    keys: Vec<String>,
}

impl<'a> ValueEditor<'a> {
    pub fn new<K>(kv_store: &'a KVStore, keys: K) -> Self
    where
        K: Into<Vec<String>>,
    {
        Self {
            kv_store,
            keys: keys.into(),
        }
    }

    /// Uses the stored keys to traverse the nested data structure and retrieve the value.
    ///
    /// This method starts from the fully merged view of the KVStore's subtree (local
    /// AtomicOp changes layered on top of backend state) and navigates using the path
    /// specified by `self.keys`. If `self.keys` is empty, it retrieves the root
    /// of the KVStore's subtree.
    ///
    /// Returns `Error::NotFound` if any part of the path does not exist, or if the
    /// final value is a tombstone (`NestedValue::Deleted`).
    /// Returns `Error::Io` with `ErrorKind::InvalidData` if a non-map value is encountered
    /// during path traversal where a map was expected.
    pub fn get(&self) -> Result<NestedValue> {
        self.kv_store.get_at_path(&self.keys)
    }

    /// Sets a `NestedValue` at the path specified by `self.keys` within the `KVStore`'s `AtomicOp`.
    ///
    /// This method modifies the local data associated with the `AtomicOp`. The changes
    /// are not persisted to the backend until `AtomicOp::commit()` is called.
    /// If the path specified by `self.keys` does not exist, it will be created.
    /// Intermediate non-map values in the path will be overwritten by maps as needed.
    /// If `self.keys` is empty (editor points to root), the provided `value` must
    /// be a `NestedValue::Map`.
    ///
    /// Returns `Error::InvalidOperation` if setting the root and `value` is not a map.
    pub fn set(&self, value: NestedValue) -> Result<()> {
        self.kv_store.set_at_path(&self.keys, value)
    }

    /// Returns a nested value by appending `key` to the current editor's path.
    ///
    /// This is a convenience method that uses `self.get()` to find the map at the current
    /// editor's path, and then retrieves `key` from that map.
    pub fn get_value<K>(&self, key: K) -> Result<NestedValue>
    where
        K: Into<String>,
    {
        let key_s = key.into();
        if self.keys.is_empty() {
            // If the base path is empty, trying to get a sub-key implies trying to get a top-level key.
            return self.kv_store.get_at_path(&[key_s]);
        }

        let mut path_to_value = self.keys.clone();
        path_to_value.push(key_s);
        self.kv_store.get_at_path(&path_to_value)
    }

    /// Constructs a new `ValueEditor` for a path one level deeper.
    ///
    /// The new editor's path will be `self.keys` with `key` appended.
    pub fn get_value_mut<K>(&self, key: K) -> ValueEditor<'a>
    where
        K: Into<String>,
    {
        let mut new_keys = self.keys.clone();
        new_keys.push(key.into());
        ValueEditor::new(self.kv_store, new_keys)
    }

    /// Marks the value at the editor's current path as deleted.
    /// This is achieved by setting its value to `NestedValue::Deleted`.
    /// The change is staged in the `AtomicOp` and needs to be committed.
    pub fn delete_self(&self) -> Result<()> {
        self.set(NestedValue::Deleted)
    }

    /// Marks the value at the specified child `key` (relative to the editor's current path) as deleted.
    /// This is achieved by setting its value to `NestedValue::Deleted`.
    /// The change is staged in the `AtomicOp` and needs to be committed.
    ///
    /// If the editor points to the root (empty path), this will delete the top-level `key`.
    pub fn delete_child<K>(&self, key: K) -> Result<()>
    where
        K: Into<String>,
    {
        let mut path_to_delete = self.keys.clone();
        path_to_delete.push(key.into());
        self.kv_store
            .set_at_path(&path_to_delete, NestedValue::Deleted)
    }
}
