use crate::Result;
use crate::atomicop::AtomicOp;
use crate::crdt::map::{List, Node, Value};
use crate::crdt::{Doc, traits::CRDT};
use crate::subtree::SubTree;
use crate::subtree::errors::SubtreeError;

/// A document-oriented SubTree providing ergonomic access to Map CRDT data.
///
/// It assumes that the SubTree data is a Map CRDT, which allows for nested map structures.
/// This implementation supports string values, as well as deletions via tombstones.
/// For more complex data structures, consider using the nested capabilities of Map directly.
pub struct DocStore {
    name: String,
    atomic_op: AtomicOp,
}

impl SubTree for DocStore {
    fn new(op: &AtomicOp, subtree_name: impl Into<String>) -> Result<Self> {
        Ok(Self {
            name: subtree_name.into(),
            atomic_op: op.clone(),
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

impl DocStore {
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
    /// A `Result` containing the MapValue if found, or `Error::NotFound`.
    pub fn get(&self, key: impl AsRef<str>) -> Result<Value> {
        let key = key.as_ref();
        // First check if there's any data in the atomic op itself
        let local_data: Result<Doc> = self.atomic_op.get_local_data(&self.name);

        // If there's data in the operation and it contains the key, return that
        if let Ok(data) = local_data
            && let Some(value) = data.get(key)
        {
            return Ok(value.clone());
        }

        // Otherwise, get the full state from the backend
        let data: Doc = self.atomic_op.get_full_state(&self.name)?;

        // Get the value
        match data.get(key) {
            Some(value) => Ok(value.clone()),
            None => Err(SubtreeError::KeyNotFound {
                subtree: self.name.clone(),
                key: key.to_string(),
            }
            .into()),
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
    pub fn get_string(&self, key: impl AsRef<str>) -> Result<String> {
        let key_ref = key.as_ref();
        match self.get(key_ref)? {
            Value::Text(value) => Ok(value),
            Value::Node(_) => Err(SubtreeError::TypeMismatch {
                subtree: self.name.clone(),
                expected: "String".to_string(),
                actual: "Map".to_string(),
            }
            .into()),
            Value::List(_) => Err(SubtreeError::TypeMismatch {
                subtree: self.name.clone(),
                expected: "String".to_string(),
                actual: "list".to_string(),
            }
            .into()),
            Value::Deleted => Err(SubtreeError::KeyNotFound {
                subtree: self.name.clone(),
                key: key_ref.to_string(),
            }
            .into()),
            _ => Err(SubtreeError::TypeMismatch {
                subtree: self.name.clone(),
                expected: "String".to_string(),
                actual: "Other".to_string(),
            }
            .into()),
        }
    }

    /// Stages the setting of a key-value pair within the associated `AtomicOp`.
    ///
    /// This method updates the `Map` data held within the `AtomicOp` for this
    /// `Doc` instance's subtree name. The change is **not** persisted to the backend
    /// until the `AtomicOp::commit()` method is called.
    ///
    /// Calling this method on a `Doc` obtained via `Tree::get_subtree_viewer` is possible
    /// but the changes will be ephemeral and discarded, as the viewer's underlying `AtomicOp`
    /// is not intended to be committed.
    ///
    /// # Arguments
    /// * `key` - The key to set.
    /// * `value` - The value to associate with the key (can be &str, String, MapValue, etc.)
    ///
    /// # Returns
    /// A `Result<()>` indicating success or an error during serialization or staging.
    pub fn set(&self, key: impl Into<String>, value: impl Into<Value>) -> Result<()> {
        // Get current data from the atomic op, or create new if not existing
        let mut data = self
            .atomic_op
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        // Update the data
        data.as_hashmap_mut().insert(key.into(), value.into());

        // Serialize and update the atomic op
        let serialized = serde_json::to_string(&data)?;
        self.atomic_op.update_subtree(&self.name, &serialized)
    }

    /// Convenience method to set a string value.
    pub fn set_string(&self, key: impl Into<String>, value: impl Into<String>) -> Result<()> {
        self.set(key, Value::Text(value.into()))
    }

    /// Stages the setting of a nested value within the associated `AtomicOp`.
    ///
    /// This method allows setting any valid Value type (String, Map, or Deleted).
    ///
    /// # Arguments
    /// * `key` - The key to set.
    /// * `value` - The Value to associate with the key.
    ///
    /// # Returns
    /// A `Result<()>` indicating success or an error during serialization or staging.
    /// Convenience method to get a List value.
    pub fn get_list(&self, key: impl AsRef<str>) -> Result<List> {
        match self.get(key)? {
            Value::List(list) => Ok(list),
            _ => Err(SubtreeError::TypeMismatch {
                subtree: self.name.clone(),
                expected: "list".to_string(),
                actual: "Other".to_string(),
            }
            .into()),
        }
    }

    /// Convenience method to get a Map value.
    pub fn get_node(&self, key: impl AsRef<str>) -> Result<Doc> {
        match self.get(key)? {
            Value::Node(node) => Ok(node.into()),
            _ => Err(SubtreeError::TypeMismatch {
                subtree: self.name.clone(),
                expected: "Map".to_string(),
                actual: "Other".to_string(),
            }
            .into()),
        }
    }

    /// Convenience method to set a list value.
    pub fn set_list(&self, key: impl Into<String>, list: impl Into<List>) -> Result<()> {
        self.set(key, Value::List(list.into()))
    }

    /// Convenience method to set a node value.
    pub fn set_node(&self, key: impl Into<String>, node: impl Into<Doc>) -> Result<()> {
        self.set(key, Value::Node(node.into().into()))
    }

    /// Legacy method for backward compatibility - now just an alias to set
    pub fn set_value(&self, key: impl Into<String>, value: impl Into<Value>) -> Result<()> {
        self.set(key, value.into())
    }

    /// Legacy method for backward compatibility - now just an alias to get
    pub fn get_value(&self, key: impl AsRef<str>) -> Result<Value> {
        self.get(key)
    }

    /// Stages the deletion of a key within the associated `AtomicOp`.
    ///
    /// This method removes the key-value pair from the `Map` data held within
    /// the `AtomicOp` for this `Doc` instance's subtree name. A tombstone is created,
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
    /// # use eidetica::subtree::DocStore;
    /// # let tree: Tree = unimplemented!();
    /// let op = tree.new_operation().unwrap();
    /// let store = op.get_subtree::<DocStore>("my_data").unwrap();
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
    pub fn delete(&self, key: impl AsRef<str>) -> Result<()> {
        // Get current data from the atomic op, or create new if not existing
        let mut data = self
            .atomic_op
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        // Remove the key (creates a tombstone)
        data.remove(key.as_ref());

        // Serialize and update the atomic op
        let serialized = serde_json::to_string(&data)?;
        self.atomic_op.update_subtree(&self.name, &serialized)
    }

    /// Retrieves all key-value pairs, merging staged data with historical state.
    ///
    /// This method combines the data staged within the current `AtomicOp` with the
    /// fully merged historical state from the backend, providing a complete view
    /// of the document as it would appear if the operation were committed.
    /// The staged data takes precedence in case of conflicts (overwrites).
    ///
    /// # Returns
    /// A `Result` containing the merged `Map` data structure.
    pub fn get_all(&self) -> Result<Doc> {
        // First get the local data directly from the atomic op
        let local_data = self.atomic_op.get_local_data::<Doc>(&self.name);

        // Get the full state from the backend
        let mut data = self.atomic_op.get_full_state::<Doc>(&self.name)?;

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
    /// and must be committed via `AtomicOp::commit()` to be persisted to the `Doc`'s backend.
    pub fn get_value_mut(&self, key: impl Into<String>) -> ValueEditor<'_> {
        ValueEditor::new(self, vec![key.into()])
    }

    /// Gets a mutable editor for the root of this Doc's subtree.
    ///
    /// Changes made via the `ValueEditor` are staged in the `AtomicOp` by its `set` method
    /// and must be committed via `AtomicOp::commit()` to be persisted to the `Doc`'s backend.
    pub fn get_root_mut(&self) -> ValueEditor<'_> {
        ValueEditor::new(self, Vec::new())
    }

    /// Retrieves a `Value` from the Doc using a specified path.
    ///
    /// The path is a slice of strings, where each string is a key in the
    /// nested map structure. If the path is empty, it retrieves the entire
    /// content of this Doc's named subtree as a `Value::Node`.
    ///
    /// This method operates on the fully merged view of the Doc's data,
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
    ///   or if the final value or an intermediate value is a `Value::Deleted` (tombstone).
    /// * `Error::Io` with `ErrorKind::InvalidData` if a non-map value is
    ///   encountered during path traversal where a map was expected.
    pub fn get_at_path<S, P>(&self, path: P) -> Result<Value>
    where
        S: AsRef<str>,
        P: AsRef<[S]>,
    {
        let path_slice = path.as_ref();
        if path_slice.is_empty() {
            // Requesting the root of this Doc's named subtree
            return Ok(Value::Node(self.get_all()?.into()));
        }

        let mut current_value_view = Value::Node(self.get_all()?.into());

        for key_segment_s in path_slice.iter() {
            match current_value_view {
                Value::Node(node) => match node.get(key_segment_s.as_ref()) {
                    Some(next_value) => {
                        current_value_view = next_value.clone();
                    }
                    None => {
                        return Err(SubtreeError::KeyNotFound {
                            subtree: self.name.clone(),
                            key: path_slice
                                .iter()
                                .map(|s| s.as_ref())
                                .collect::<Vec<_>>()
                                .join("."),
                        }
                        .into());
                    }
                },
                Value::Deleted => {
                    // A tombstone encountered in the path means the path doesn't lead to a value.
                    return Err(SubtreeError::KeyNotFound {
                        subtree: self.name.clone(),
                        key: path_slice
                            .iter()
                            .map(|s| s.as_ref())
                            .collect::<Vec<_>>()
                            .join("."),
                    }
                    .into());
                }
                _ => {
                    // Expected a map to continue traversal, but found something else.
                    return Err(SubtreeError::TypeMismatch {
                        subtree: self.name.clone(),
                        expected: "Map".to_string(),
                        actual: "non-map value".to_string(),
                    }
                    .into());
                }
            }
        }

        // Check if the final resolved value is a tombstone.
        match current_value_view {
            Value::Deleted => Err(SubtreeError::KeyNotFound {
                subtree: self.name.clone(),
                key: path_slice
                    .iter()
                    .map(|s| s.as_ref())
                    .collect::<Vec<_>>()
                    .join("."),
            }
            .into()),
            _ => Ok(current_value_view),
        }
    }

    /// Sets a `Value` at a specified path within the `Doc`'s `AtomicOp`.
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
    /// * `value`: The `Value` to set at the specified path.
    ///
    /// # Errors
    ///
    /// * `Error::InvalidOperation` if the `path` is empty and `value` is not a `Value::Node`.
    /// * `Error::Serialize` if the updated subtree data cannot be serialized to JSON.
    /// * Potentially other errors from `AtomicOp::update_subtree`.
    pub fn set_at_path<S, P>(&self, path: P, value: Value) -> Result<()>
    where
        S: Into<String> + Clone,
        P: AsRef<[S]>,
    {
        let path_slice = path.as_ref();
        if path_slice.is_empty() {
            // Setting the root of this Doc's named subtree.
            // The value must be a node.
            if let Value::Node(node) = value {
                let serialized_data = serde_json::to_string(&node)?;
                return self.atomic_op.update_subtree(&self.name, &serialized_data);
            } else {
                return Err(SubtreeError::TypeMismatch {
                    subtree: self.name.clone(),
                    expected: "Map".to_string(),
                    actual: "non-map value".to_string(),
                }
                .into());
            }
        }

        let mut subtree_data = self
            .atomic_op
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        let mut current_map_mut = subtree_data.as_map_mut();

        // Traverse or create path segments up to the parent of the target key.
        for key_segment_s in path_slice.iter().take(path_slice.len() - 1) {
            let key_segment_string = key_segment_s.clone().into();
            let entry = current_map_mut.as_hashmap_mut().entry(key_segment_string);
            current_map_mut = match entry.or_insert_with(|| Value::Node(Node::default())) {
                Value::Node(node) => node,
                non_map_val => {
                    // If a non-map value exists at an intermediate path segment,
                    // overwrite it with a map to continue.
                    *non_map_val = Value::Node(Node::default());
                    match non_map_val {
                        Value::Node(node) => node,
                        _ => unreachable!("Just assigned a map"),
                    }
                }
            };
        }

        // Set the value at the final key in the path.
        if let Some(last_key_s) = path_slice.last() {
            let map_value = value;
            current_map_mut
                .as_hashmap_mut()
                .insert(last_key_s.clone().into(), map_value);
        } else {
            // This case should be prevented by the initial path.is_empty() check.
            // Given the check, this is technically unreachable if path is not empty.
            return Err(SubtreeError::InvalidOperation {
                subtree: self.name.clone(),
                operation: "set_at_path".to_string(),
                reason: "Path became empty unexpectedly".to_string(),
            }
            .into());
        }

        let serialized_data = serde_json::to_string(&subtree_data)?;
        self.atomic_op.update_subtree(&self.name, &serialized_data)
    }
}

/// An editor for a `Value` obtained from a `DocStore`.
///
/// This provides a mutable lens into a value, allowing modifications
/// to be staged and then saved back to the DocStore.
pub struct ValueEditor<'a> {
    kv_store: &'a DocStore,
    keys: Vec<String>,
}

impl<'a> ValueEditor<'a> {
    pub fn new<K>(kv_store: &'a DocStore, keys: K) -> Self
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
    /// This method starts from the fully merged view of the DocStore's subtree (local
    /// AtomicOp changes layered on top of backend state) and navigates using the path
    /// specified by `self.keys`. If `self.keys` is empty, it retrieves the root
    /// of the DocStore's subtree.
    ///
    /// Returns `Error::NotFound` if any part of the path does not exist, or if the
    /// final value is a tombstone (`Value::Deleted`).
    /// Returns `Error::Io` with `ErrorKind::InvalidData` if a non-map value is encountered
    /// during path traversal where a map was expected.
    pub fn get(&self) -> Result<Value> {
        self.kv_store.get_at_path(&self.keys)
    }

    /// Sets a `Value` at the path specified by `self.keys` within the `DocStore`'s `AtomicOp`.
    ///
    /// This method modifies the local data associated with the `AtomicOp`. The changes
    /// are not persisted to the backend until `AtomicOp::commit()` is called.
    /// If the path specified by `self.keys` does not exist, it will be created.
    /// Intermediate non-map values in the path will be overwritten by maps as needed.
    /// If `self.keys` is empty (editor points to root), the provided `value` must
    /// be a `Value::Node`.
    ///
    /// Returns `Error::InvalidOperation` if setting the root and `value` is not a node.
    pub fn set(&self, value: Value) -> Result<()> {
        self.kv_store.set_at_path(&self.keys, value)
    }

    /// Returns a nested value by appending `key` to the current editor's path.
    ///
    /// This is a convenience method that uses `self.get()` to find the map at the current
    /// editor's path, and then retrieves `key` from that map.
    pub fn get_value(&self, key: impl AsRef<str>) -> Result<Value> {
        let key = key.as_ref();
        if self.keys.is_empty() {
            // If the base path is empty, trying to get a sub-key implies trying to get a top-level key.
            return self.kv_store.get_at_path([key]);
        }

        let mut path_to_value = self.keys.clone();
        path_to_value.push(key.to_string());
        self.kv_store.get_at_path(&path_to_value)
    }

    /// Constructs a new `ValueEditor` for a path one level deeper.
    ///
    /// The new editor's path will be `self.keys` with `key` appended.
    pub fn get_value_mut(&self, key: impl Into<String>) -> ValueEditor<'a> {
        let mut new_keys = self.keys.clone();
        new_keys.push(key.into());
        ValueEditor::new(self.kv_store, new_keys)
    }

    /// Marks the value at the editor's current path as deleted.
    /// This is achieved by setting its value to `Value::Deleted`.
    /// The change is staged in the `AtomicOp` and needs to be committed.
    pub fn delete_self(&self) -> Result<()> {
        self.set(Value::Deleted)
    }

    /// Marks the value at the specified child `key` (relative to the editor's current path) as deleted.
    /// This is achieved by setting its value to `Value::Deleted`.
    /// The change is staged in the `AtomicOp` and needs to be committed.
    ///
    /// If the editor points to the root (empty path), this will delete the top-level `key`.
    pub fn delete_child(&self, key: impl Into<String>) -> Result<()> {
        let mut path_to_delete = self.keys.clone();
        path_to_delete.push(key.into());
        self.kv_store.set_at_path(&path_to_delete, Value::Deleted)
    }
}
