use data::{DataLocation, DataLocations, DataTable, PostgresDataTable};
use error::Error;
use metadata::{MetadataTable, PostgresMetadataTable};
use schema::MetadataEntry;
use serde_json::Value;
use settings::SettingsTable;
use sqlx::PgPool;
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
    data_table: D,
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
    pub async fn from_pool(pool: PgPool, name: &str, device_id: Uuid) -> Result<Self, Error> {
        // Create the individual tables
        let metadata_table = PostgresMetadataTable::from_pool(pool.clone(), name).await?;
        let data_table = PostgresDataTable::from_pool(pool.clone()).await?;
        let settings_table = SettingsTable::from_postgres(pool, device_id).await?;

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
        data: Vec<u8>,
        metadata: Value,
        parent_id: Option<Uuid>,
    ) -> Result<Uuid, Error> {
        unimplemented!();
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
    pub async fn get_history(&self, id: Uuid) -> Result<Vec<MetadataEntry>, Error> {
        // Use the metadata table's recursive query capability to get the full history
        self.metadata_table.get_entry_history(id).await
    }

    /// Mark a piece of data as archived/deleted by creating a new entry that archives the old one
    ///
    /// # Arguments
    /// * `id` - UUID of the entry to archive
    pub async fn archive(&mut self, id: Uuid) -> Result<(), Error> {
        // First check if the entry exists and get its metadata
        let existing_entry = match self.metadata_table.get_entry(id).await? {
            Some(entry) => entry,
            None => return Err(Error::NotFound),
        };

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
        };

        // Create the new entry - this will automatically archive the parent
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
    ) -> Result<Vec<(Uuid, Value)>, Error> {
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
    pub async fn get_data_locations(&self, id: Uuid) -> Result<DataLocations, Error> {
        // First get the metadata entry to get the data hash
        let entry = match self.metadata_table.get_entry(id).await? {
            Some(e) => e,
            None => return Err(Error::NotFound),
        };

        // Use the hash to get the data entry
        let data_entry = match self.data_table.get_entry(&entry.data_hash).await? {
            Some(e) => e,
            None => return Err(Error::NotFound),
        };

        // Convert the data entry into our DataLocations struct
        Ok(DataLocations {
            inline_data: data_entry.inline_data,
            s3_paths: data_entry.s3_path,
            local_paths: data_entry.local_path,
            devices: data_entry.devices,
        })
    }

    /// Add a new storage location for a piece of data
    ///
    /// # Arguments
    /// * `id` - UUID of the entry
    /// * `location` - The new location to add (S3, local path, or device)
    pub async fn add_data_location(
        &mut self,
        id: Uuid,
        location: DataLocation,
    ) -> Result<(), Error> {
        // First get the metadata entry to get the data hash
        let entry = match self.metadata_table.get_entry(id).await? {
            Some(e) => e,
            None => return Err(Error::NotFound),
        };

        // Add the location based on its type
        match location {
            DataLocation::Inline(data) => {
                self.data_table
                    .add_inline_data(&entry.data_hash, data)
                    .await?;
            }
            DataLocation::S3(path) => {
                self.data_table.add_s3_path(&entry.data_hash, path).await?;
            }
            DataLocation::LocalPath(path) => {
                self.data_table
                    .add_local_path(&entry.data_hash, path)
                    .await?;
            }
            DataLocation::Device(device_id) => {
                self.data_table
                    .add_device(&entry.data_hash, device_id)
                    .await?;
            }
        }

        Ok(())
    }
}
