use std::{marker::PhantomData, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Result, Store, Transaction,
    backend::RecordMutation,
    crdt::{CanonicalJson, Lww, LwwMap},
    store::{
        ProjectionDescriptor, RecordProjection, Registered, StoreStateModel, errors::StoreError,
    },
};

const DEFAULT_SCAN_PAGE_SIZE: usize = 128;

struct TableProjection;

impl RecordProjection<LwwMap<String, CanonicalJson>> for TableProjection {
    fn descriptor(&self) -> ProjectionDescriptor {
        ProjectionDescriptor {
            name: "eidetica/table/rows/canonical-json:v0".to_string(),
            version: 0,
        }
    }

    fn mutations<'a>(
        &'a self,
        delta: &'a LwwMap<String, CanonicalJson>,
    ) -> Result<Box<dyn Iterator<Item = Result<RecordMutation>> + Send + 'a>> {
        Ok(Box::new(delta.operations().map(|(key, operation)| {
            Ok(match operation {
                Lww::Set(value) => RecordMutation::Put {
                    key: key.as_bytes().to_vec(),
                    value: value.as_bytes().to_vec(),
                },
                Lww::Delete => RecordMutation::Delete {
                    key: key.as_bytes().to_vec(),
                },
                Lww::NoOp => unreachable!("keyed NoOp is not a canonical map operation"),
            })
        })))
    }
}

/// Opaque exclusive continuation for ordered Table scans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableCursor(pub(crate) CursorKind);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CursorKind {
    Projected {
        view: Uuid,
        revision: u64,
        store: String,
        projection: ProjectionDescriptor,
        /// Verified database frontier for remote client-side history projection.
        frontier: Option<crate::Snapshot>,
        last_physical_key: Vec<u8>,
    },
}

/// One bounded page of rows in the Store's persisted record-key order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TablePage<T> {
    pub rows: Vec<(String, T)>,
    pub next: Option<TableCursor>,
}

/// A row-based Store
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
/// - Storage within the underlying LWW map
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
    type Data = LwwMap<String, CanonicalJson>;

    fn state_model() -> StoreStateModel<Self::Data> {
        StoreStateModel::Records(Arc::new(TableProjection))
    }

    async fn load(txn: &Transaction, subtree_name: String) -> Result<Self> {
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
    T: Serialize + for<'de> Deserialize<'de> + Clone + Send + Sync,
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

        let projection = TableProjection;
        match self
            .txn
            .projected_get(&self.name, &projection, key.as_bytes())
            .await?
        {
            Some(value) => CanonicalJson::parse(&value)
                .and_then(|row| row.to_value())
                .map_err(|e| {
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

        self.set(&primary_key, row).await?;

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
        let canonical =
            CanonicalJson::from_value(&row).map_err(|e| StoreError::SerializationFailed {
                store: self.name.clone(),
                reason: format!("Failed to serialize record for key '{key_str}': {e}"),
            })?;
        let mut delta = LwwMap::new();
        delta.set(key_str.to_string(), canonical);
        self.txn
            .stage_projected_delta(&self.name, &TableProjection, delta)
            .await
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
        let exists = match self.get(key_str).await {
            Ok(_) => true,
            Err(crate::Error::Store(error)) if matches!(*error, StoreError::KeyNotFound { .. }) => {
                false
            }
            Err(error) => return Err(error),
        };
        if !exists {
            return Ok(false);
        }

        let mut delta = LwwMap::new();
        delta.delete(key_str.to_string());
        self.txn
            .stage_projected_delta(&self.name, &TableProjection, delta)
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
        let mut result = Vec::new();
        let mut cursor = None;
        loop {
            let page = self
                .scan_page(cursor.as_ref(), DEFAULT_SCAN_PAGE_SIZE)
                .await?;
            for (key, row) in page.rows {
                if query(&row) {
                    result.push((key, row));
                }
            }
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        Ok(result)
    }

    /// Reads at most `limit` rows in deterministic persisted record-key order.
    ///
    /// Plain `Table` records use primary-key byte order. Wrappers such as
    /// `PasswordStore<Table<T>>` may transform keys, so their order is not
    /// logical primary-key order.
    pub async fn scan_page(
        &self,
        cursor: Option<&TableCursor>,
        limit: usize,
    ) -> Result<TablePage<T>> {
        let projection = TableProjection;
        let (page, next) = self
            .txn
            .projected_record_scan_page(&self.name, &projection, cursor, limit)
            .await?;
        let mut rows = Vec::with_capacity(page.records.len());
        for (key, value) in page.records {
            let key =
                String::from_utf8(key).map_err(|error| StoreError::DeserializationFailed {
                    store: self.name.clone(),
                    reason: error.to_string(),
                })?;
            let row = CanonicalJson::parse(&value)
                .and_then(|row| row.to_value())
                .map_err(|error| StoreError::DeserializationFailed {
                    store: self.name.clone(),
                    reason: format!("Failed to deserialize record for key '{key}': {error}"),
                })?;
            rows.push((key, row));
        }
        Ok(TablePage { rows, next })
    }
}
