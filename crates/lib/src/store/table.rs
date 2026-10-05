use std::{marker::PhantomData, sync::Arc};

use async_trait::async_trait;
use serde_bytes::ByteBuf;
use uuid::Uuid;

use crate::{
    Result, Store, Transaction,
    backend::RecordMutation,
    crdt::{Doc, Lww},
    store::{
        ProjectionDescriptor, RecordProjection, Registered, RowCodec, SerdeJson, StoreStateModel,
        TableData, errors::StoreError,
    },
};

const DEFAULT_SCAN_PAGE_SIZE: usize = 128;

async fn check_row_codec(
    txn: &Transaction,
    name: &str,
    selected: Option<&str>,
    allow_absent: bool,
) -> Result<Option<String>> {
    txn.init_subtree_parents(crate::constants::INDEX).await?;
    let required = loop {
        let stamp = txn.table_format_stamp()?;
        if let Some(required) = txn.cached_row_format(name, &stamp) {
            break Some(required);
        }
        let required = read_row_codec(txn, name, allow_absent).await?;
        // Registry initialization can add parent pointers; edits made while the
        // read was awaiting must never be certified by the old metadata stamp.
        if txn.table_format_stamp()? != stamp {
            continue;
        }
        if let Some(required) = &required {
            txn.cache_row_format(name, stamp, required.clone());
        }
        break required;
    };
    if let (Some(selected), Some(required)) = (selected, &required)
        && selected != required
    {
        return Err(StoreError::TypeMismatch {
            store: name.into(),
            expected: required.clone(),
            actual: selected.into(),
        }
        .into());
    }
    Ok(required)
}

async fn read_row_codec(
    txn: &Transaction,
    name: &str,
    allow_absent: bool,
) -> Result<Option<String>> {
    let info = match txn.get_index().await?.get_entry(name).await {
        Ok(info) => info,
        Err(crate::Error::Store(error)) if error.is_not_found() && allow_absent => {
            if !txn.store_has_source(name).await? {
                return Ok(None);
            }
            return Err(StoreError::InvalidConfiguration {
                store: name.into(),
                reason: "Table data has no registered row format".into(),
            }
            .into());
        }
        Err(error) => return Err(error),
    };
    let (store_type, config) = if info.type_id == super::PasswordStore::<RawTable>::type_id() {
        super::password_store::unlocked_config(txn, name, info.config)?
    } else {
        (info.type_id, info.config)
    };
    if store_type != RawTable::type_id() {
        return Err(StoreError::TypeMismatch {
            store: name.into(),
            expected: RawTable::type_id().into(),
            actual: store_type,
        }
        .into());
    }
    let required = config
        .get("row_codec")
        .and_then(|value| value.as_text())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| StoreError::InvalidConfiguration {
            store: name.into(),
            reason: "Table requires a non-empty row_codec identity".into(),
        })?;
    Ok(Some(required.into()))
}

/// Codec-independent, read-only row access for database inspectors.
///
/// Unlike `Table<Vec<u8>, RawBytes>`, this does not select a row format. It
/// exposes the bytes of any configured row codec without decoding them.
/// Open an existing Store with `tx.get_store::<RawTable>(name)`. For encrypted
/// Tables, open `PasswordStore<RawTable>`, unlock it, then call `inner()`.
/// Point reads and scans use the same projections, authorization and cursors as
/// typed Tables; no whole-Table materialization is introduced by this facade.
#[derive(Clone)]
pub struct RawTable {
    name: String,
    txn: Transaction,
}

impl Registered for RawTable {
    fn type_id() -> &'static str {
        "table:v1"
    }
}

#[async_trait]
impl Store for RawTable {
    type Data = TableData;

    fn state_model() -> StoreStateModel<Self::Data> {
        StoreStateModel::Records(Arc::new(TableProjection))
    }

    async fn load(txn: &Transaction, name: String) -> Result<Self> {
        check_row_codec(txn, &name, None, false).await?;
        Ok(Self {
            name,
            txn: txn.clone(),
        })
    }

    async fn register(_txn: &Transaction, name: String) -> Result<Self> {
        Err(StoreError::InvalidOperation {
            store: name,
            operation: "inspect".into(),
            reason: "raw inspection cannot create a Table; choose a row codec when creating it"
                .into(),
        }
        .into())
    }

    fn name(&self) -> &str {
        &self.name
    }
    fn transaction(&self) -> &Transaction {
        &self.txn
    }
}

impl RawTable {
    /// The persisted row codec identity, not an inferred application schema.
    pub async fn row_codec_id(&self) -> Result<String> {
        check_row_codec(&self.txn, &self.name, None, false)
            .await?
            .ok_or_else(|| {
                StoreError::InvalidConfiguration {
                    store: self.name.clone(),
                    reason: "missing row format".into(),
                }
                .into()
            })
    }

    /// Read exact row bytes without invoking an application decoder.
    pub async fn get(&self, key: impl AsRef<str>) -> Result<Vec<u8>> {
        self.row_codec_id().await?;
        let key = key.as_ref();
        self.txn
            .projected_get(&self.name, &TableProjection, key.as_bytes())
            .await?
            .ok_or_else(|| {
                StoreError::KeyNotFound {
                    store: self.name.clone(),
                    key: key.into(),
                }
                .into()
            })
    }

    /// Read one bounded page in persisted record-key order.
    pub async fn scan_page(
        &self,
        cursor: Option<&TableCursor>,
        limit: usize,
    ) -> Result<TablePage<Vec<u8>>> {
        self.row_codec_id().await?;
        raw_scan_page(&self.txn, &self.name, cursor, limit).await
    }
}

async fn raw_scan_page(
    txn: &Transaction,
    name: &str,
    cursor: Option<&TableCursor>,
    limit: usize,
) -> Result<TablePage<Vec<u8>>> {
    let (page, next) = txn
        .projected_record_scan_page(name, &TableProjection, cursor, limit)
        .await?;
    let rows = page
        .records
        .into_iter()
        .map(|(key, value)| {
            String::from_utf8(key)
                .map(|key| (key, value))
                .map_err(|error| {
                    StoreError::DeserializationFailed {
                        store: name.into(),
                        reason: error.to_string(),
                    }
                    .into()
                })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(TablePage { rows, next })
}

struct TableProjection;

impl RecordProjection<TableData> for TableProjection {
    fn server_store_type(&self) -> Option<&'static str> {
        Some(<Table<Vec<u8>, super::RawBytes> as Registered>::type_id())
    }

    fn descriptor(&self) -> ProjectionDescriptor {
        ProjectionDescriptor {
            name: "eidetica/table/rows/opaque:v1".to_string(),
            version: 1,
        }
    }

    fn mutations<'a>(
        &'a self,
        delta: &'a TableData,
    ) -> Result<Box<dyn Iterator<Item = Result<RecordMutation>> + Send + 'a>> {
        Ok(Box::new(delta.0.operations().map(|(key, operation)| {
            Ok(match operation {
                Lww::Set(value) => RecordMutation::Put {
                    key: key.as_bytes().to_vec(),
                    value: value.to_vec(),
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
/// - `T`: The application row type, with no intrinsic Serde or Clone requirement.
/// - `C`: A stateless row codec; defaults to direct [`SerdeJson`] encoding.
///
/// This abstraction simplifies working with collections of similarly structured data
/// by handling the details of:
/// - Primary key generation and management
/// - Serialization/deserialization of records
/// - Storage within the underlying LWW map
///
/// Rows persist exactly as `C` encodes them, inside strict DAG-CBOR [`TableData`].
/// Projection is independent of `T` and `C`. Configuration binds typed access
/// to `C::FORMAT_ID`; [`RawTable`] inspects rows without an application decoder.
/// Full causal historical identity enforcement is still being integrated.
/// No decoder or migration accepts `table:v0` data.
pub struct Table<T, C = SerdeJson> {
    name: String,
    txn: Transaction,
    phantom: PhantomData<fn() -> (T, C)>,
}

impl<T, C> Clone for Table<T, C> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            txn: self.txn.clone(),
            phantom: PhantomData,
        }
    }
}

impl<T, C: RowCodec<T>> Registered for Table<T, C> {
    fn type_id() -> &'static str {
        "table:v1"
    }
}

#[async_trait]
impl<T, C: RowCodec<T>> Store for Table<T, C> {
    type Data = TableData;

    fn state_model() -> StoreStateModel<Self::Data> {
        StoreStateModel::Records(Arc::new(TableProjection))
    }

    fn default_config() -> Doc {
        let mut config = Doc::new();
        config.set("row_codec", C::FORMAT_ID);
        config
    }

    async fn register(txn: &Transaction, subtree_name: String) -> Result<Self> {
        if C::FORMAT_ID.is_empty() {
            return Err(StoreError::InvalidConfiguration {
                store: subtree_name,
                reason: "row codec identity must not be empty".into(),
            }
            .into());
        }
        txn.get_index()
            .await?
            .set_entry(&subtree_name, Self::type_id(), Self::default_config())
            .await?;
        Self::load(txn, subtree_name).await
    }

    async fn load(txn: &Transaction, subtree_name: String) -> Result<Self> {
        check_row_codec(txn, &subtree_name, Some(C::FORMAT_ID), true).await?;
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

impl<T, C: RowCodec<T>> Table<T, C> {
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

        if check_row_codec(&self.txn, &self.name, Some(C::FORMAT_ID), true)
            .await?
            .is_none()
        {
            return Err(StoreError::KeyNotFound {
                store: self.name.clone(),
                key: key.into(),
            }
            .into());
        }
        match self
            .txn
            .projected_get(&self.name, &TableProjection, key.as_bytes())
            .await?
        {
            Some(value) => C::decode(&value).map_err(|e| {
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
        check_row_codec(&self.txn, &self.name, Some(C::FORMAT_ID), false).await?;
        let bytes = C::encode(&row).map_err(|e| StoreError::SerializationFailed {
            store: self.name.clone(),
            reason: format!("Failed to serialize record for key '{key_str}': {e}"),
        })?;
        let mut delta = TableData::default();
        delta.0.set(key_str.to_string(), ByteBuf::from(bytes));
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

        let mut delta = TableData::default();
        delta.0.delete(key_str.to_string());
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
        if check_row_codec(&self.txn, &self.name, Some(C::FORMAT_ID), true)
            .await?
            .is_none()
        {
            if cursor.is_some() {
                return Err(StoreError::StaleCursor {
                    store: self.name.clone(),
                }
                .into());
            }
            return Ok(TablePage {
                rows: vec![],
                next: None,
            });
        }
        let TablePage {
            rows: records,
            next,
        } = raw_scan_page(&self.txn, &self.name, cursor, limit).await?;
        let mut rows = Vec::with_capacity(records.len());
        for (key, value) in records {
            let row = C::decode(&value).map_err(|error| StoreError::DeserializationFailed {
                store: self.name.clone(),
                reason: format!("Failed to deserialize record for key '{key}': {error}"),
            })?;
            rows.push((key, row));
        }
        Ok(TablePage { rows, next })
    }
}
