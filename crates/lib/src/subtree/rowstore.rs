use crate::atomicop::AtomicOp;
use crate::data::{CRDT, KVOverWrite};
use crate::subtree::SubTree;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use uuid::Uuid;

/// A Row-based SubTree
///
/// `RowStore` provides a record-oriented storage abstraction for entries in a subtree,
/// similar to a database table with automatic primary key generation.
///
/// # Features
/// - Automatically generates UUIDv4 primary keys for new records
/// - Provides CRUD operations (Create, Read, Update, Delete) for record-based data
/// - Supports searching across all records with a predicate function
///
/// # Type Parameters
/// - `T`: The record type to be stored, which must be serializable, deserializable, and cloneable
///
/// This abstraction simplifies working with collections of similarly structured data
/// by handling the details of:
/// - Primary key generation and management
/// - Serialization/deserialization of records
/// - Storage within the underlying CRDT (KVOverWrite)
pub struct RowStore<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone,
{
    name: String,
    atomic_op: AtomicOp,
    phantom: PhantomData<T>,
}

impl<T> SubTree for RowStore<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone,
{
    fn new(op: &AtomicOp, subtree_name: &str) -> Result<Self> {
        Ok(Self {
            name: subtree_name.to_string(),
            atomic_op: op.clone(),
            phantom: PhantomData,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

impl<T> RowStore<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone,
{
    /// Retrieves a row from the RowStore by its primary key.
    ///
    /// This method first checks for the record in the current atomic operation's
    /// local changes, and if not found, retrieves it from the persistent state.
    ///
    /// # Arguments
    /// * `key` - The primary key (UUID string) of the record to retrieve
    ///
    /// # Returns
    /// * `Ok(T)` - The retrieved record if found
    /// * `Err(Error::NotFound)` - If no record exists with the given key
    ///
    /// # Errors
    /// Returns an error if:
    /// * The record doesn't exist (`Error::NotFound`)
    /// * There's a serialization/deserialization error
    pub fn get(&self, key: &str) -> Result<T> {
        // First check if there's any data in the atomic op itself
        let local_data: Result<KVOverWrite> = self.atomic_op.get_local_data(&self.name);

        // If there's data in the operation and it contains the key, return that
        if let Ok(data) = local_data {
            if let Some(value) = data.get(key) {
                return Ok(serde_json::from_str(value)?);
            }
        }

        // Otherwise, get the full state from the backend
        let data: KVOverWrite = self.atomic_op.get_full_state(&self.name)?;

        // Get the value
        match data.get(key) {
            Some(value) => Ok(serde_json::from_str(value)?),
            None => Err(Error::NotFound),
        }
    }

    /// Inserts a new row into the RowStore and returns its generated primary key.
    ///
    /// This method:
    /// 1. Generates a new UUIDv4 as the primary key
    /// 2. Serializes the record
    /// 3. Stores it in the local atomic operation
    ///
    /// # Arguments
    /// * `row` - The record to insert
    ///
    /// # Returns
    /// * `Ok(String)` - The generated UUID primary key as a string
    ///
    /// # Errors
    /// Returns an error if there's a serialization error or the operation fails
    pub fn insert(&self, row: T) -> Result<String> {
        // Generate a UUIDv4 for the primary key
        let primary_key = Uuid::new_v4().to_string();

        // Get current data from the atomic op, or create new if not existing
        let mut data = self
            .atomic_op
            .get_local_data::<KVOverWrite>(&self.name)
            .unwrap_or_default();

        // Serialize the row
        let serialized_row = serde_json::to_string(&row)?;

        // Update the data with the new row
        data.set(primary_key.clone(), serialized_row);

        // Serialize and update the atomic op
        let serialized_data = serde_json::to_string(&data)?;
        self.atomic_op
            .update_subtree(&self.name, &serialized_data)?;

        // Return the primary key
        Ok(primary_key)
    }

    /// Updates an existing row in the RowStore with a new value.
    ///
    /// This method completely replaces the existing record with the provided one.
    /// If the record doesn't exist yet, it will be created with the given key.
    ///
    /// # Arguments
    /// * `key` - The primary key of the record to update
    /// * `row` - The new record value
    ///
    /// # Returns
    /// * `Ok(())` - If the update was successful
    ///
    /// # Errors
    /// Returns an error if there's a serialization error or the operation fails
    pub fn set(&self, key: &str, row: T) -> Result<()> {
        // Get current data from the atomic op, or create new if not existing
        let mut data = self
            .atomic_op
            .get_local_data::<KVOverWrite>(&self.name)
            .unwrap_or_default();

        // Serialize the row
        let serialized_row = serde_json::to_string(&row)?;

        // Update the data
        data.set(key.to_string(), serialized_row);

        // Serialize and update the atomic op
        let serialized_data = serde_json::to_string(&data)?;
        self.atomic_op.update_subtree(&self.name, &serialized_data)
    }

    /// Searches for rows matching a predicate function.
    ///
    /// # Arguments
    /// * `query` - A function that takes a reference to a record and returns a boolean
    ///
    /// # Returns
    /// * `Ok(Vec<(String, T)>)` - A vector of (primary_key, record) pairs that match the predicate
    ///
    /// # Errors
    /// Returns an error if there's a serialization error or the operation fails
    pub fn search(&self, query: impl Fn(&T) -> bool) -> Result<Vec<(String, T)>> {
        // Get the full state combining local and backend data
        let mut result = Vec::new();

        // Get data from the atomic op if it exists
        let local_data = self.atomic_op.get_local_data::<KVOverWrite>(&self.name);

        // Get the full state from the backend
        let mut data = self.atomic_op.get_full_state::<KVOverWrite>(&self.name)?;

        // If there's also local data, merge it with the full state
        if let Ok(local) = local_data {
            data = data.merge(&local)?;
        }

        // Iterate through all key-value pairs
        for (key, value_opt) in data.as_hashmap().iter() {
            // Skip tombstones (None values)
            if let Some(value) = value_opt {
                // Deserialize the row
                let row: T = serde_json::from_str(value)?;

                // Check if the row matches the query
                if query(&row) {
                    result.push((key.to_string(), row));
                }
            }
        }

        Ok(result)
    }
}
