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
    pub fn get(&self, key: &str) -> Result<NestedValue> {
        // First check if there's any data in the atomic op itself
        let local_data: Result<KVNested> = self.atomic_op.get_local_data(&self.name);

        // If there's data in the operation and it contains the key, return that
        if let Ok(data) = local_data {
            if let Some(value) = data.get(key) {
                return Ok(value.clone());
            }
        }

        // Otherwise, get the full state from the backend
        let data: KVNested = self.atomic_op.get_full_state(&self.name)?;

        // Get the value
        match data.get(key) {
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
    pub fn get_string(&self, key: &str) -> Result<String> {
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
    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        // Get current data from the atomic op, or create new if not existing
        let mut data = self
            .atomic_op
            .get_local_data::<KVNested>(&self.name)
            .unwrap_or_default();

        // Update the data
        data.set_string(key.to_string(), value.to_string());

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
    pub fn set_value(&self, key: &str, value: NestedValue) -> Result<()> {
        // Get current data from the atomic op, or create new if not existing
        let mut data = self
            .atomic_op
            .get_local_data::<KVNested>(&self.name)
            .unwrap_or_default();

        // Update the data
        data.set(key.to_string(), value);

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
    /// # use eideticadb::Tree;
    /// # use eideticadb::subtree::KVStore;
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
    pub fn delete(&self, key: &str) -> Result<()> {
        // Get current data from the atomic op, or create new if not existing
        let mut data = self
            .atomic_op
            .get_local_data::<KVNested>(&self.name)
            .unwrap_or_default();

        // Remove the key (creates a tombstone)
        data.remove(key);

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
}
