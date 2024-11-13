use super::*;
use anyhow::{Context, Result};
use data::{DataTable, PostgresDataTable};
use data_handler::{DataLocation, DataTableHandler};
use metadata::{MetadataTable, PostgresMetadataTable};
use schema::MetadataEntry;
use serde_json::Value;
use settings::SettingsTable;
use sqlx::PgPool;
use std::path::PathBuf;
use uuid::Uuid;

/// Data Store
///
/// This is a logical set of data, with it's own device id, metadata table,
/// settings table, and either a private or shared data table.
#[allow(dead_code)]
pub struct DataStore<D: DataTable, M: MetadataTable> {
    /// Unique identifier for this device
    device_id: uuid::Uuid,
    /// Table for storing actual data or references to data
    data_table: DataTableHandler<D>,
    /// Table for storing metadata about the data
    metadata_table: M,
    /// Table for storing settings for this data store
    settings_table: SettingsTable<M>,
}

#[allow(dead_code)]
impl DataStore<PostgresDataTable, PostgresMetadataTable> {
    /// Create a new DataStore from a Postgres connection pool
    ///
    /// # Arguments
    /// * `pool` - PostgreSQL connection pool
    /// * `name` - Name of this data store (used as table prefix)
    /// * `device_id` - Unique identifier for this device
    pub async fn from_pool(pool: PgPool, name: &str, device_id: Uuid) -> Result<Self> {
        let local_path = PathBuf::from("/tmp/eidetica"); // FIXME: hardcoded
                                                         // Create the individual tables
        let metadata_table = PostgresMetadataTable::from_pool(pool.clone(), name).await?;
        let data_table = PostgresDataTable::from_pool(pool.clone()).await?;
        let settings_table = SettingsTable::from_postgres(pool, device_id).await?;
        let data_table = DataTableHandler::new(data_table, local_path);

        Ok(Self {
            device_id,
            data_table,
            metadata_table,
            settings_table,
        })
    }
}

#[allow(dead_code)]
impl<D: DataTable, M: MetadataTable> DataStore<D, M> {
    /// Store a new piece of data
    ///
    /// # Arguments
    /// * `data` - The raw data to store
    /// * `metadata` - JSON metadata about the data (type, store name, etc)
    /// * `parent_id` - Optional parent entry this is updating
    ///
    /// # Returns
    /// The UUID of the newly created entry
    pub async fn store_data(
        &mut self,
        data: DataLocation,
        metadata: Value,
        parent_id: Option<Uuid>,
    ) -> Result<Uuid> {
        // Insert data, acquiring it from the DataLocation
        let entry = self.data_table.copy_file(data).await?;
        let hash = entry.hash.clone();

        // Create a MetadataEntry with the generated hash and provided metadata
        let entry = MetadataEntry {
            id: Uuid::now_v7(),
            device_id: self.device_id,
            archived: false,
            local: true, // Assuming the data is stored locally on creation
            parent_id,
            metadata,
            data_hash: entry.hash,
        };
        let id = entry.id;

        // Insert the MetadataEntry into the metadata table
        self.metadata_table.create_entry(entry).await?;

        // Now increment the ref_count
        self.data_table.set_local_needed(&hash).await?;

        // Return the UUID of the newly created entry
        Ok(id)
    }

    /// Just get a copy of all the active metadata entries.
    ///
    /// Not the data, that is queried only individually.
    pub async fn get_active_entries(&self) -> Result<Vec<MetadataEntry>> {
        self.metadata_table.get_active_entries().await
    }

    /// Get the history of a piece of data by following its parent chain
    ///
    /// Returns a vector of MetadataEntry objects in descending order (newest to oldest).
    /// Each entry contains:
    /// - id: The unique identifier for this version
    /// - device_id: The device that created this version
    /// - archived: Whether this version is archived
    /// - parent_id: Reference to the previous version
    /// - metadata: User-defined metadata for this version
    /// - data_hash: Reference to the underlying data
    ///
    /// # Arguments
    /// * `id` - UUID of the entry to get history for
    ///
    /// # Returns
    /// Vector of MetadataEntry in descending order (newest to oldest)
    pub async fn get_history(&self, id: Uuid) -> Result<Vec<MetadataEntry>> {
        // Use the metadata table's recursive query capability to get the full history
        self.metadata_table.get_entry_history(id).await
    }

    /// Mark a piece of data as archived/deleted by creating a new entry that archives the old one
    ///
    /// # Arguments
    /// * `id` - UUID of the entry to archive
    pub async fn archive(&mut self, id: Uuid) -> Result<()> {
        // First check if the entry exists and get its metadata
        let existing_entry = self
            .metadata_table
            .get_entry(id)
            .await?
            .context("Not found")?;

        // Create a new metadata entry that marks this as archived
        // TODO: I'm not certain this is the right new entry, leaving for now
        let archive_entry = MetadataEntry {
            id: Uuid::now_v7(),
            device_id: self.device_id,
            // The newly entered entry is _also_ archived...
            archived: true,
            parent_id: Some(id),
            // Preserve the original metadata but add archived flag
            metadata: {
                let mut metadata = existing_entry.metadata.clone();
                if let Value::Object(ref mut map) = metadata {
                    map.insert("archived".to_string(), Value::Bool(true));
                }
                metadata
            },
            data_hash: "".to_string(),
            local: false,
        };

        // Create the new entry - this will automatically mark the parent as archived
        self.metadata_table.create_entry(archive_entry).await?;

        Ok(())
    }

    /// Query active entries by metadata conditions
    ///
    /// # Arguments
    /// * `conditions` - JSON metadata conditions to match against
    /// * `include_archived` - Whether to include archived entries
    ///
    /// # Returns
    /// Vector of matching entries' (id, metadata) pairs
    pub async fn query_by_metadata(
        &self,
        conditions: &Value,
        include_archived: bool,
    ) -> Result<Vec<(Uuid, Value)>> {
        // Use the metadata table's query capability
        let entries = self
            .metadata_table
            .get_entries_by_metadata_conditions(conditions, include_archived)
            .await?;

        // Convert the entries into the simpler (id, metadata) format for the public API
        Ok(entries
            .into_iter()
            .map(|entry| (entry.id, entry.metadata))
            .collect())
    }

    /// Get all locations where a piece of data is stored
    /// (inline, local paths, S3 paths, other devices)
    ///
    /// # Arguments
    /// * `id` - UUID of the entry to get locations for
    ///
    /// # Returns
    /// The various locations where this data can be found
    pub async fn get_data_locations(&self, id: Uuid) -> Result<Vec<DataLocation>> {
        // First get the metadata entry to get the data hash
        let entry = self
            .metadata_table
            .get_entry(id)
            .await?
            .context("Not found")?;

        // Use the hash to get the data entry
        self.data_table.get_data_locations(&entry.data_hash).await
    }

    /// Add a new storage location for a piece of data
    ///
    /// # Arguments
    /// * `id` - UUID of the entry
    /// * `location` - The new location to add (S3, local path, or device)
    pub async fn add_data_location(&mut self, id: Uuid, location: DataLocation) -> Result<()> {
        // First get the metadata entry to get the data hash
        let entry = self
            .metadata_table
            .get_entry(id)
            .await?
            .context("Not found")?;
        self.data_table
            .add_data_location(&entry.data_hash, location)
            .await
    }

    /// Search the metadata entries for those with matching conditions
    pub async fn get_entries_by_metadata_conditions(
        &self,
        conditions: Value,
    ) -> Result<Vec<MetadataEntry>> {
        self.metadata_table
            .get_entries_by_metadata_conditions(&conditions, false)
            .await
    }
}
