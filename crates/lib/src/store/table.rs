use std::marker::PhantomData;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Result, Store, Transaction,
    crdt::{CRDT, Doc},
    store::{Registered, errors::StoreError},
};

/// A Row-based Store
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
    txn: Transaction,
    phantom: PhantomData<T>,
}

impl<T> Registered for Table<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone,
{
    fn type_id() -> &'static str {
        "table:v0"
    }
}

#[async_trait]
impl<T> Store for Table<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync,
{
    async fn new(txn: &Transaction, subtree_name: String) -> Result<Self> {
        Ok(Self {
            name: subtree_name,
            txn: txn.clone(),
            phantom: PhantomData,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn transaction(&self) -> &Transaction {
        &self.txn
    }
}

impl<T> Table<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone,
{
    /// Retrieves a row from the Table by its primary key.
    ///
    /// This method first checks for the record in the current transaction's
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
    pub async fn get(&self, key: impl AsRef<str>) -> Result<T> {
        let key = key.as_ref();

        // Get local data from the transaction if it exists
        let local_data: Result<Doc> = self.txn.get_local_data(&self.name);

        // If there's a tombstone in local data, the record is deleted
        if let Ok(ref data) = local_data
            && data.is_tombstone(key)
        {
            return Err(StoreError::KeyNotFound {
                store: self.name.clone(),
                key: key.to_string(),
            }
            .into());
        }

        // If there's a value in local data, return that
        if let Ok(ref data) = local_data
            && let Some(map_value) = data.get(key)
            && let Some(value) = map_value.as_text()
        {
            return serde_json::from_str(value).map_err(|e| {
                StoreError::DeserializationFailed {
                    store: self.name.clone(),
                    reason: format!("Failed to deserialize record for key '{key}': {e}"),
                }
                .into()
            });
        }

        // Otherwise, get the full state from the backend
        let data: Doc = self.txn.get_full_state(&self.name).await?;

        // Get the value
        match data.get(key).and_then(|v| v.as_text()) {
            Some(value) => serde_json::from_str(value).map_err(|e| {
                StoreError::DeserializationFailed {
                    store: self.name.clone(),
                    reason: format!("Failed to deserialize record for key '{key}': {e}"),
                }
                .into()
            }),
            None => Err(StoreError::KeyNotFound {
                store: self.name.clone(),
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
    /// 3. Stores it in the local transaction
    ///
    /// # Arguments
    /// * `row` - The record to insert
    ///
    /// # Returns
    /// * `Ok(String)` - The generated UUID primary key as a string
    ///
    /// # Errors
    /// Returns an error if there's a serialization error or the operation fails
    pub async fn insert(&self, row: T) -> Result<String> {
        // Generate a UUIDv4 for the primary key
        let primary_key = Uuid::new_v4().to_string();

        // Get current data from the transaction, or create new if not existing
        let mut data = self
            .txn
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        // Serialize the row
        let serialized_row =
            serde_json::to_string(&row).map_err(|e| StoreError::SerializationFailed {
                store: self.name.clone(),
                reason: format!("Failed to serialize record: {e}"),
            })?;

        // Update the data with the new row
        data.set(primary_key.clone(), serialized_row);

        // Serialize and update the transaction
        let serialized_data =
            serde_json::to_string(&data).map_err(|e| StoreError::SerializationFailed {
                store: self.name.clone(),
                reason: format!("Failed to serialize subtree data: {e}"),
            })?;
        self.txn
            .update_subtree(&self.name, &serialized_data)
            .await?;

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
    pub async fn set(&self, key: impl AsRef<str>, row: T) -> Result<()> {
        let key_str = key.as_ref();
        // Get current data from the transaction, or create new if not existing
        let mut data = self
            .txn
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        // Serialize the row
        let serialized_row =
            serde_json::to_string(&row).map_err(|e| StoreError::SerializationFailed {
                store: self.name.clone(),
                reason: format!("Failed to serialize record for key '{key_str}': {e}"),
            })?;

        // Update the data
        data.set(key_str, serialized_row);

        // Serialize and update the transaction
        let serialized_data =
            serde_json::to_string(&data).map_err(|e| StoreError::SerializationFailed {
                store: self.name.clone(),
                reason: format!("Failed to serialize subtree data: {e}"),
            })?;
        self.txn.update_subtree(&self.name, &serialized_data).await
    }

    /// Deletes a row from the Table by its primary key.
    ///
    /// This method marks the record as deleted using CRDT tombstone semantics,
    /// ensuring the deletion is properly synchronized across distributed nodes.
    ///
    /// # Arguments
    /// * `key` - The primary key of the record to delete
    ///
    /// # Returns
    /// * `Ok(true)` - If a record existed and was deleted
    /// * `Ok(false)` - If no record existed with the given key
    ///
    /// # Errors
    /// Returns an error if there's a serialization error or the operation fails
    pub async fn delete(&self, key: impl AsRef<str>) -> Result<bool> {
        let key_str = key.as_ref();

        // Check if the record exists (checks both local and full state)
        let exists = self.get(key_str).await.is_ok();

        // If the record doesn't exist, return false early
        if !exists {
            return Ok(false);
        }

        // Get current data from the transaction, or create new if not existing
        let mut data = self
            .txn
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        // Remove the key (creates tombstone for CRDT semantics)
        data.remove(key_str);

        // Serialize and update the transaction
        let serialized_data =
            serde_json::to_string(&data).map_err(|e| StoreError::SerializationFailed {
                store: self.name.clone(),
                reason: format!("Failed to serialize subtree data: {e}"),
            })?;
        self.txn
            .update_subtree(&self.name, &serialized_data)
            .await?;

        // Return true since we confirmed the record existed
        Ok(true)
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
    pub async fn search(&self, query: impl Fn(&T) -> bool) -> Result<Vec<(String, T)>> {
        // Get the full state combining local and backend data
        let mut result = Vec::new();

        // Get data from the transaction if it exists
        let local_data = self.txn.get_local_data::<Doc>(&self.name);

        // Get the full state from the backend
        let mut data = self.txn.get_full_state::<Doc>(&self.name).await?;

        // If there's also local data, merge it with the full state
        if let Ok(local) = local_data {
            data = data.merge(&local)?;
        }

        // Iterate through all key-value pairs
        for (key, map_value) in data.iter() {
            // Skip non-text values
            if let Some(value) = map_value.as_text() {
                // Deserialize the row
                let row: T =
                    serde_json::from_str(value).map_err(|e| StoreError::DeserializationFailed {
                        store: self.name.clone(),
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
