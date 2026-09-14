use std::{marker::PhantomData, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Result, Store, Transaction,
    backend::RecordMutations,
    crdt::{
        Doc,
        doc::{Value, path::normalize_path},
    },
    store::{
        ProjectionDescriptor, RecordProjection, Registered, StoreStateModel, errors::StoreError,
    },
};

const DEFAULT_SCAN_PAGE_SIZE: usize = 128;

struct TableProjection;

fn project_doc_delta(delta: &Doc, prefix: &str, out: &mut RecordMutations) {
    for (key, value) in delta.iter_all() {
        let key = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Doc(doc) => project_doc_delta(doc, &key, out),
            Value::Text(value) => {
                insert_projected_record(out, key.into_bytes(), Some(value.as_bytes().to_vec()))
            }
            Value::Deleted => insert_projected_record(out, key.into_bytes(), None),
            _ => {}
        }
    }
}

fn insert_projected_record(out: &mut RecordMutations, key: Vec<u8>, value: Option<Vec<u8>>) {
    out.retain(|existing_key, _| !TableProjection.staged_keys_conflict(existing_key, &key));
    out.insert(key, value);
}

pub(crate) fn encode_entry_delta(mutations: &RecordMutations) -> Result<Doc> {
    TableProjection.encode_entry_delta(mutations)
}

impl RecordProjection<Doc> for TableProjection {
    fn descriptor(&self) -> ProjectionDescriptor {
        ProjectionDescriptor {
            name: "eidetica/table/rows".to_string(),
            version: 0,
        }
    }

    fn project_delta(&self, delta: &Doc, out: &mut RecordMutations) -> Result<()> {
        project_doc_delta(delta, "", out);
        Ok(())
    }

    fn encode_entry_delta(&self, mutations: &RecordMutations) -> Result<Doc> {
        let mut delta = Doc::new();
        for (key, value) in mutations {
            let key =
                std::str::from_utf8(key).map_err(|error| StoreError::SerializationFailed {
                    store: "Table".to_string(),
                    reason: error.to_string(),
                })?;
            let _ = match value {
                Some(value) => delta.set(
                    key,
                    std::str::from_utf8(value).map_err(|error| {
                        StoreError::SerializationFailed {
                            store: "Table".to_string(),
                            reason: error.to_string(),
                        }
                    })?,
                ),
                None => delta.remove(key),
            };
        }
        Ok(delta)
    }

    fn normalize_record_key(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let key = std::str::from_utf8(key).map_err(|error| StoreError::SerializationFailed {
            store: "Table".to_string(),
            reason: error.to_string(),
        })?;
        let normalized = normalize_path(key);
        Ok((key.is_empty() || !normalized.is_empty()).then_some(normalized.into_bytes()))
    }

    fn staged_keys_conflict(&self, left: &[u8], right: &[u8]) -> bool {
        left == right
            || left
                .strip_prefix(right)
                .is_some_and(|suffix| suffix.starts_with(b"."))
            || right
                .strip_prefix(left)
                .is_some_and(|suffix| suffix.starts_with(b"."))
    }

    fn staged_key_descends_from(&self, staged_key: &[u8], key: &[u8]) -> bool {
        staged_key
            .strip_prefix(key)
            .is_some_and(|suffix| suffix.starts_with(b"."))
    }
}

/// Exclusive continuation for ordered Table scans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableCursor(Vec<u8>);

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
    type Data = Doc;

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
            .record_get(&self.name, &projection, key.as_bytes())
            .await?
        {
            Some(value) => serde_json::from_slice(&value).map_err(|e| {
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

        let serialized_row =
            serde_json::to_vec(&row).map_err(|e| StoreError::SerializationFailed {
                store: self.name.clone(),
                reason: format!("Failed to serialize record: {e}"),
            })?;
        self.txn.stage_record(
            &self.name,
            &TableProjection,
            primary_key.as_bytes().to_vec(),
            Some(serialized_row),
        )?;

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
        let serialized_row =
            serde_json::to_vec(&row).map_err(|e| StoreError::SerializationFailed {
                store: self.name.clone(),
                reason: format!("Failed to serialize record for key '{key_str}': {e}"),
            })?;
        self.txn.stage_record(
            &self.name,
            &TableProjection,
            key_str.as_bytes().to_vec(),
            Some(serialized_row),
        )
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
        let exists = self.get(key_str).await.is_ok()
            || self.txn.record_has_staged_descendant(
                &self.name,
                &TableProjection,
                key_str.as_bytes(),
            )?;

        // If the record doesn't exist, return false early
        if !exists {
            return Ok(false);
        }

        self.txn.stage_record(
            &self.name,
            &TableProjection,
            key_str.as_bytes().to_vec(),
            None,
        )?;

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
        let page = self
            .txn
            .record_scan(
                &self.name,
                &projection,
                cursor.map(|cursor| cursor.0.as_slice()),
                limit,
            )
            .await?;
        let mut rows = Vec::with_capacity(page.records.len());
        for (key, value) in page.records {
            let key =
                String::from_utf8(key).map_err(|error| StoreError::DeserializationFailed {
                    store: self.name.clone(),
                    reason: error.to_string(),
                })?;
            let row = serde_json::from_slice(&value).map_err(|error| {
                StoreError::DeserializationFailed {
                    store: self.name.clone(),
                    reason: format!("Failed to deserialize record for key '{key}': {error}"),
                }
            })?;
            rows.push((key, row));
        }
        Ok(TablePage {
            rows,
            next: page.next.map(TableCursor),
        })
    }
}
