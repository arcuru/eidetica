//! ValueEditor for mutable access to DocStore values.

use crate::{
    Result,
    crdt::{Doc, doc::Value},
    store::errors::StoreError,
};

use super::DocStore;

/// An editor for a `Value` obtained from a `DocStore`.
///
/// This provides a mutable lens into a value, allowing modifications
/// to be staged and then saved back to the DocStore.
pub struct ValueEditor<'a> {
    pub(super) kv_store: &'a DocStore,
    pub(super) keys: Vec<String>,
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
    /// Transaction changes layered on top of backend state) and navigates using the path
    /// specified by `self.keys`. If `self.keys` is empty, it retrieves the root
    /// of the DocStore's subtree.
    ///
    /// Returns `Error::NotFound` if any part of the path does not exist, or if the
    /// final value is a tombstone (`Value::Deleted`).
    /// Returns `Error::Io` with `ErrorKind::InvalidData` if a non-map value is encountered
    /// during path traversal where a map was expected.
    pub async fn get(&self) -> Result<Value> {
        self.kv_store.get_at_path(&self.keys).await
    }

    /// Sets a `Value` at the path specified by `self.keys` within the `DocStore`'s `Transaction`.
    ///
    /// This method modifies the local data associated with the `Transaction`. The changes
    /// are not persisted to the backend until `Transaction::commit()` is called.
    /// If the path specified by `self.keys` does not exist, it will be created.
    /// Intermediate non-map values in the path will be overwritten by maps as needed.
    /// If `self.keys` is empty (editor points to root), the provided `value` must
    /// be a `Value::Doc`.
    ///
    /// Returns `Error::InvalidOperation` if setting the root and `value` is not a node.
    pub async fn set(&self, value: Value) -> Result<()> {
        self.kv_store.set_at_path(&self.keys, value).await
    }

    /// Returns a nested value by appending `key` to the current editor's path.
    ///
    /// This is a convenience method that uses `self.get()` to find the map at the current
    /// editor's path, and then retrieves `key` from that map.
    pub async fn get_value(&self, key: impl AsRef<str>) -> Result<Value> {
        let key = key.as_ref();
        if self.keys.is_empty() {
            // If the base path is empty, trying to get a sub-key implies trying to get a top-level key.
            return self.kv_store.get_at_path([key]).await;
        }

        let mut path_to_value = self.keys.clone();
        path_to_value.push(key.to_string());
        self.kv_store.get_at_path(&path_to_value).await
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
    /// The change is staged in the `Transaction` and needs to be committed.
    pub async fn delete_self(&self) -> Result<()> {
        self.set(Value::Deleted).await
    }

    /// Marks the value at the specified child `key` (relative to the editor's current path) as deleted.
    /// This is achieved by setting its value to `Value::Deleted`.
    /// The change is staged in the `Transaction` and needs to be committed.
    ///
    /// If the editor points to the root (empty path), this will delete the top-level `key`.
    pub async fn delete_child(&self, key: impl Into<String>) -> Result<()> {
        let mut path_to_delete = self.keys.clone();
        path_to_delete.push(key.into());
        self.kv_store
            .set_at_path(&path_to_delete, Value::Deleted)
            .await
    }
}

impl DocStore {
    /// Gets a mutable editor for a value associated with the given key.
    ///
    /// If the key does not exist, the editor will be initialized with an empty map,
    /// allowing immediate use of map-modifying methods. The type can be changed
    /// later using `ValueEditor::set()`.
    ///
    /// Changes made via the `ValueEditor` are staged in the `Transaction` by its `set` method
    /// and must be committed via `Transaction::commit()` to be persisted to the `Doc`'s backend.
    pub fn get_value_mut(&self, key: impl Into<String>) -> ValueEditor<'_> {
        ValueEditor::new(self, vec![key.into()])
    }

    /// Gets a mutable editor for the root of this Doc's subtree.
    ///
    /// Changes made via the `ValueEditor` are staged in the `Transaction` by its `set` method
    /// and must be committed via `Transaction::commit()` to be persisted to the `Doc`'s backend.
    pub fn get_root_mut(&self) -> ValueEditor<'_> {
        ValueEditor::new(self, Vec::new())
    }

    /// Retrieves a `Value` from the Doc using a specified path.
    ///
    /// The path is a slice of strings, where each string is a key in the
    /// nested map structure. If the path is empty, it retrieves the entire
    /// content of this Doc's named subtree as a `Value::Doc`.
    ///
    /// This method operates on the fully merged view of the Doc's data,
    /// including any local changes from the current `Transaction` layered on top
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
    pub async fn get_at_path<S, P>(&self, path: P) -> Result<Value>
    where
        S: AsRef<str>,
        P: AsRef<[S]>,
    {
        let path_slice = path.as_ref();
        if path_slice.is_empty() {
            // Requesting the root of this Doc's named subtree
            return Ok(Value::Doc(self.get_all().await?));
        }

        let mut current_value_view = Value::Doc(self.get_all().await?);

        for key_segment_s in path_slice.iter() {
            match current_value_view {
                Value::Doc(node) => match node.get(key_segment_s.as_ref()) {
                    Some(next_value) => {
                        current_value_view = next_value.clone();
                    }
                    None => {
                        return Err(StoreError::KeyNotFound {
                            store: self.name.clone(),
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
                    return Err(StoreError::KeyNotFound {
                        store: self.name.clone(),
                        key: path_slice
                            .iter()
                            .map(|s| s.as_ref())
                            .collect::<Vec<_>>()
                            .join("."),
                    }
                    .into());
                }
                _ => {
                    // Expected a node to continue traversal, but found something else.
                    return Err(StoreError::TypeMismatch {
                        store: self.name.clone(),
                        expected: "Doc".to_string(),
                        actual: "non-node value".to_string(),
                    }
                    .into());
                }
            }
        }

        // Check if the final resolved value is a tombstone.
        match current_value_view {
            Value::Deleted => Err(StoreError::KeyNotFound {
                store: self.name.clone(),
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

    /// Sets a `Value` at a specified path within the `Doc`'s `Transaction`.
    ///
    /// The path is a slice of strings, where each string is a key in the
    /// nested map structure.
    ///
    /// This method modifies the local data associated with the `Transaction`. The changes
    /// are not persisted to the backend until `Transaction::commit()` is called.
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
    /// * `Error::InvalidOperation` if the `path` is empty and `value` is not a `Value::Doc`.
    /// * `Error::Serialize` if the updated subtree data cannot be serialized to JSON.
    /// * Potentially other errors from `Transaction::update_subtree`.
    pub async fn set_at_path<S, P>(&self, path: P, value: Value) -> Result<()>
    where
        S: Into<String> + Clone,
        P: AsRef<[S]>,
    {
        let path_slice = path.as_ref();
        if path_slice.is_empty() {
            // Setting the root of this Doc's named subtree.
            // The value must be a node.
            if let Value::Doc(node) = value {
                let serialized_data = serde_json::to_string(&node)?;
                return self.txn.update_subtree(&self.name, &serialized_data).await;
            } else {
                return Err(StoreError::TypeMismatch {
                    store: self.name.clone(),
                    expected: "Doc".to_string(),
                    actual: "non-doc value".to_string(),
                }
                .into());
            }
        }

        let mut subtree_data = self
            .txn
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        // Build the dot-separated path string
        let path_str: String = path_slice
            .iter()
            .map(|s| {
                let s_string: String = s.clone().into();
                s_string
            })
            .collect::<Vec<_>>()
            .join(".");

        // Use Doc::set which now creates intermediate nodes automatically
        subtree_data.set(&path_str, value);

        let serialized_data = serde_json::to_string(&subtree_data)?;
        self.txn.update_subtree(&self.name, &serialized_data).await
    }
}
