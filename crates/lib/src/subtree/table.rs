use crate::Result;
use crate::Store;
use crate::Transaction;
use crate::crdt::{CRDT, Doc};
use crate::subtree::errors::StoreError;
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use uuid::Uuid;

/// A Row-based SubTree
///
/// `Table` provides a record-oriented storage abstraction for entries in a subtree,
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
/// - Storage within the underlying CRDT (Doc)
pub struct Table<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone,
{
    name: String,
    atomic_op: Transaction,
    phantom: PhantomData<T>,
}

impl<T> Store for Table<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone,
{
    fn new(op: &Transaction, subtree_name: impl Into<String>) -> Result<Self> {
        Ok(Self {
            name: subtree_name.into(),
            atomic_op: op.clone(),
            phantom: PhantomData,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}

impl<T> Table<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone,
{
    /// Retrieves a row from the Table by its primary key.
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
    pub fn get(&self, key: impl AsRef<str>) -> Result<T> {
        let key = key.as_ref();
        // First check if there's any data in the atomic op itself
        let local_data: Result<Doc> = self.atomic_op.get_local_data(&self.name);

        // If there's data in the operation and it contains the key, return that
        if let Ok(data) = local_data
            && let Some(map_value) = data.get(key)
            && let Some(value) = map_value.as_text()
        {
            return serde_json::from_str(value).map_err(|e| {
                StoreError::DeserializationFailed {
                    subtree: self.name.clone(),
                    reason: format!("Failed to deserialize record for key '{key}': {e}"),
                }
                .into()
            });
        }

        // Otherwise, get the full state from the backend
        let data: Doc = self.atomic_op.get_full_state(&self.name)?;

        // Get the value
        match data.get(key).and_then(|v| v.as_text()) {
            Some(value) => serde_json::from_str(value).map_err(|e| {
                StoreError::DeserializationFailed {
                    subtree: self.name.clone(),
                    reason: format!("Failed to deserialize record for key '{key}': {e}"),
                }
                .into()
            }),
            None => Err(StoreError::KeyNotFound {
                subtree: self.name.clone(),
                key: key.to_string(),
            }
            .into()),
        }
    }

    /// Inserts a new row into the Table and returns its generated primary key.
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
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        // Serialize the row
        let serialized_row =
            serde_json::to_string(&row).map_err(|e| StoreError::SerializationFailed {
                subtree: self.name.clone(),
                reason: format!("Failed to serialize record: {e}"),
            })?;

        // Update the data with the new row
        data.set(primary_key.clone(), serialized_row);

        // Serialize and update the atomic op
        let serialized_data =
            serde_json::to_string(&data).map_err(|e| StoreError::SerializationFailed {
                subtree: self.name.clone(),
                reason: format!("Failed to serialize subtree data: {e}"),
            })?;
        self.atomic_op
            .update_subtree(&self.name, &serialized_data)?;

        // Return the primary key
        Ok(primary_key)
    }

    /// Updates an existing row in the Table with a new value.
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
    pub fn set(&self, key: impl AsRef<str>, row: T) -> Result<()> {
        let key_str = key.as_ref();
        // Get current data from the atomic op, or create new if not existing
        let mut data = self
            .atomic_op
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        // Serialize the row
        let serialized_row =
            serde_json::to_string(&row).map_err(|e| StoreError::SerializationFailed {
                subtree: self.name.clone(),
                reason: format!("Failed to serialize record for key '{key_str}': {e}"),
            })?;

        // Update the data
        data.set(key_str.to_string(), serialized_row);

        // Serialize and update the atomic op
        let serialized_data =
            serde_json::to_string(&data).map_err(|e| StoreError::SerializationFailed {
                subtree: self.name.clone(),
                reason: format!("Failed to serialize subtree data: {e}"),
            })?;
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
        let local_data = self.atomic_op.get_local_data::<Doc>(&self.name);

        // Get the full state from the backend
        let mut data = self.atomic_op.get_full_state::<Doc>(&self.name)?;

        // If there's also local data, merge it with the full state
        if let Ok(local) = local_data {
            data = data.merge(&local)?;
        }

        // Iterate through all key-value pairs
        for (key, map_value) in data.as_hashmap().iter() {
            // Skip non-text values
            if let Some(value) = map_value.as_text() {
                // Deserialize the row
                let row: T =
                    serde_json::from_str(value).map_err(|e| StoreError::DeserializationFailed {
                        subtree: self.name.clone(),
                        reason: format!(
                            "Failed to deserialize record for key '{key}' during search: {e}"
                        ),
                    })?;

                // Check if the row matches the query
                if query(&row) {
                    result.push((key.clone(), row));
                }
            }
        }

        Ok(result)
    }
}
