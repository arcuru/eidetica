use super::*;
use anyhow::{anyhow, Context, Result};
use data::{DataTable, PostgresDataTable};
use data_handler::{DataLocation, DataTableHandler};
use metadata::{MetadataTable, PostgresMetadataTable};
use schema::DeviceId;
use schema::MetadataEntry;
use serde_json::Value;
use settings::{Setting, SettingsTable};
use sqlx::PgPool;
use std::path::PathBuf;
use uuid::Uuid;

/// Constant key for the local path setting
const SETTING_LOCAL_PATH: &str = "local_path";

/// Data Store
///
/// This is a logical set of data, with its own device id, metadata table,
/// settings table, and either a private or shared data table.
#[allow(dead_code)]
pub struct DataStore<D: DataTable, M: MetadataTable> {
    /// Unique identifier for this store
    device_id: DeviceId,
    /// Table for storing actual data or references to data
    data_table: DataTableHandler<D>,
    /// Table for storing metadata about the data
    metadata_table: M,
    /// Table for storing settings for this data store
    settings_table: SettingsTable<M>,
}

#[allow(dead_code)]
impl DataStore<PostgresDataTable, PostgresMetadataTable> {
    /// Initialize the DataStore by setting the local_path in settings.
    ///
    /// This function should be called once to set up the initial settings.
    ///
    /// # Arguments
    /// * `pool` - PostgreSQL connection pool
    /// * `name` - Name of this data store (used as table prefix)
    /// * `device_id` - Unique identifier for this device
    /// * `local_path` - The local path to store data
    pub async fn init(
        pool: PgPool,
        name: &str,
        device_id: DeviceId,
        local_path: PathBuf,
    ) -> Result<Self> {
        // Initialize the settings table
        let mut settings_table = SettingsTable::from_postgres(pool.clone(), device_id).await?;

        // Create the local_path setting
        let setting = Setting {
            key: SETTING_LOCAL_PATH.to_string(),
            value: Value::String(local_path.to_string_lossy().into_owned()),
            description: Some("Local path for storing data".to_string()),
        };
        settings_table
            .set_setting(setting)
            .await
            .context("Failed to set local_path in settings")?;

        // Proceed to create the DataStore using from_pool
        Self::from_pool(pool, name, device_id).await
    }

    /// Create a new DataStore from a Postgres connection pool
    ///
    /// This function requires that the "local_path" is already set in settings.
    ///
    /// # Arguments
    /// * `pool` - PostgreSQL connection pool
    /// * `name` - Name of this data store (used as table prefix)
    /// * `device_id` - Unique identifier for this device
    pub async fn from_pool(pool: PgPool, name: &str, device_id: DeviceId) -> Result<Self> {
        // Initialize the settings table
        let settings_table = SettingsTable::from_postgres(pool.clone(), device_id).await?;

        // Retrieve the local_path from settings
        let local_path_setting = settings_table
            .get_setting(SETTING_LOCAL_PATH)
            .await
            .context("Failed to retrieve local_path from settings")?
            .ok_or_else(|| anyhow!("DataStore not initialized: 'local_path' not set"))?;

        // Convert the setting value to PathBuf
        let local_path = match local_path_setting.value {
            Value::String(s) => PathBuf::from(s),
            _ => return Err(anyhow!("local_path setting is not a string")),
        };

        // Create the other tables
        let metadata_table = PostgresMetadataTable::from_pool(pool.clone(), name).await?;
        let data_table = PostgresDataTable::from_pool(pool.clone()).await?;
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

    /// Get a copy of all the active metadata entries.
    ///
    /// Not the data, that is queried only individually.
    pub async fn get_active_entries(&self) -> Result<Vec<MetadataEntry>> {
        self.metadata_table.get_active_entries().await
    }

    /// Get a copy of all the archived metadata entries.
    pub async fn get_archived_entries(&self) -> Result<Vec<MetadataEntry>> {
        self.metadata_table.get_archived_entries().await
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

    /// Get a setting value by key
    pub async fn get_setting(&self, name: &str) -> Result<Option<Setting>> {
        self.settings_table.get_setting(name).await
    }

    /// Set a setting value by key
    pub async fn set_setting(&mut self, setting: Setting) -> Result<()> {
        self.settings_table.set_setting(setting).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::generate_key;
    use sqlx::PgPool;
    use tempfile::{tempdir, TempDir};
    use uuid::Uuid;

    /// TestDataStore struct to hold the temporary directory and the datastore
    /// This allows us to keep the tempdir around so it's not deleted after setup
    #[allow(dead_code)]
    struct TestDataStore {
        temp_dir: TempDir,
        datastore: DataStore<PostgresDataTable, PostgresMetadataTable>,
    }

    pub type TestResult<T> = std::result::Result<T, sqlx::Error>;

    fn generate_test_device_id() -> DeviceId {
        generate_key().verifying_key().to_bytes()
    }

    async fn setup_datastore(pool: PgPool) -> TestResult<TestDataStore> {
        let device_id = generate_test_device_id();
        // Initialize the datastore with a temporary directory
        let temp_dir = tempdir().expect("Failed to create temp dir");
        let local_path = temp_dir.path().to_path_buf();
        Ok(TestDataStore {
            temp_dir,
            datastore: DataStore::init(pool, "test", device_id, local_path)
                .await
                .expect("Failed to initialize test datastore"),
        })
    }

    #[sqlx::test]
    async fn test_store_data_basic(pool: PgPool) -> TestResult<()> {
        let TestDataStore {
            datastore: mut store,
            temp_dir: _,
        } = setup_datastore(pool).await?;

        // Create test data
        let data = "Hello world!".as_bytes().to_vec();
        let metadata = serde_json::json!({
            "type": "text",
            "name": "test.txt"
        });

        // Store the data
        let id = store
            .store_data(DataLocation::Inline(data.clone()), metadata.clone(), None)
            .await
            .expect("Failed to store data");

        // Verify the entry exists
        let entries = store
            .get_active_entries()
            .await
            .expect("Failed to get entries");
        assert_eq!(entries.len(), 1);

        let entry = &entries[0];
        assert_eq!(entry.id, id);
        assert_eq!(entry.metadata, metadata);
        assert!(!entry.archived);
        assert!(entry.local);
        assert_eq!(entry.parent_id, None);

        Ok(())
    }

    #[sqlx::test]
    async fn test_store_data_with_parent(pool: PgPool) -> TestResult<()> {
        let TestDataStore {
            datastore: mut store,
            temp_dir: _,
        } = setup_datastore(pool).await?;

        // Store initial version
        let initial_data = "Initial".as_bytes().to_vec();
        let initial_id = store
            .store_data(
                DataLocation::Inline(initial_data),
                serde_json::json!({"version": 1}),
                None,
            )
            .await
            .expect("Failed to store initial data");

        // Store update version
        let update_data = "Updated".as_bytes().to_vec();
        let update_id = store
            .store_data(
                DataLocation::Inline(update_data),
                serde_json::json!({"version": 2}),
                Some(initial_id),
            )
            .await
            .expect("Failed to store updated data");

        // Verify history
        let history = store
            .get_history(update_id)
            .await
            .expect("Failed to get history");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].id, update_id);
        assert_eq!(history[1].id, initial_id);

        Ok(())
    }

    #[sqlx::test]
    async fn test_archive_entry(pool: PgPool) -> TestResult<()> {
        let TestDataStore {
            datastore: mut store,
            temp_dir: _,
        } = setup_datastore(pool).await?;

        // Store test data
        let id = store
            .store_data(
                DataLocation::Inline("test".as_bytes().to_vec()),
                serde_json::json!({"test": true}),
                None,
            )
            .await
            .expect("Failed to store data");

        // Archive it
        store.archive(id).await.expect("Failed to archive entry");

        // Verify it's not in active entries
        let active = store
            .get_active_entries()
            .await
            .expect("Failed to get entries");
        assert!(active.is_empty());

        // But should be found when explicitly querying archived
        let archived = store
            .get_archived_entries()
            .await
            .expect("Failed to query archived");
        assert!(!archived.is_empty());

        Ok(())
    }

    #[sqlx::test]
    async fn test_query_by_metadata(pool: PgPool) -> TestResult<()> {
        let TestDataStore {
            datastore: mut store,
            temp_dir: _,
        } = setup_datastore(pool).await?;

        // Store entries with different metadata
        let test1_id = store
            .store_data(
                DataLocation::Inline("test1".as_bytes().to_vec()),
                serde_json::json!({"type": "text", "tag": "a"}),
                None,
            )
            .await
            .expect("Failed to store data 1");

        store
            .store_data(
                DataLocation::Inline("test2".as_bytes().to_vec()),
                serde_json::json!({"type": "text", "tag": "b"}),
                None,
            )
            .await
            .expect("Failed to store data 2");

        // Query for specific tag
        let results: Vec<(Uuid, Value)> = store
            .query_by_metadata(&serde_json::json!({"tag": "a"}), false)
            .await
            .expect("Failed to query");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1["tag"], "a");
        assert_eq!(results[0].0, test1_id);

        Ok(())
    }

    #[sqlx::test]
    async fn test_data_locations_management(pool: PgPool) -> TestResult<()> {
        let TestDataStore {
            datastore: mut store,
            temp_dir: _,
        } = setup_datastore(pool).await?;

        // Store initial data as inline
        let data = "Test data".as_bytes().to_vec();
        let id = store
            .store_data(
                DataLocation::Inline(data.clone()),
                serde_json::json!({"type": "text"}),
                None,
            )
            .await
            .expect("Failed to store data");

        // Get locations - should have been converted to local path
        let locations = store
            .get_data_locations(id)
            .await
            .expect("Failed to get locations");
        assert_eq!(locations.len(), 1);
        match &locations[0] {
            DataLocation::LocalPath(path) => {
                assert!(path.exists());
                let contents = std::fs::read(path).expect("Failed to read file");
                assert_eq!(contents, data);
            }
            _ => panic!("Expected local path data"),
        }

        // Add an S3 location
        let s3_path = "s3://bucket/test.txt".to_string();
        store
            .add_data_location(id, DataLocation::S3(s3_path.clone()))
            .await
            .expect("Failed to add S3 location");

        // Verify both locations exist
        let locations = store
            .get_data_locations(id)
            .await
            .expect("Failed to get locations");
        assert_eq!(locations.len(), 2);

        // Verify local path still exists
        let has_local = locations
            .iter()
            .any(|loc| matches!(loc, DataLocation::LocalPath(_)));
        assert!(has_local, "Local path not found");

        // Verify S3 location exists
        let has_s3 = locations.iter().any(|loc| match loc {
            DataLocation::S3(path) => path == &s3_path,
            _ => false,
        });
        assert!(has_s3, "S3 location not found");

        Ok(())
    }

    #[sqlx::test]
    async fn test_data_storage_with_different_types(pool: PgPool) -> TestResult<()> {
        let TestDataStore {
            datastore: mut store,
            temp_dir: _,
        } = setup_datastore(pool).await?;
        let temp_dir = tempdir().expect("Failed to create temp dir");

        // Test inline data
        let inline_data = "Inline content".as_bytes().to_vec();
        let inline_id = store
            .store_data(
                DataLocation::Inline(inline_data.clone()),
                serde_json::json!({"type": "inline"}),
                None,
            )
            .await
            .expect("Failed to store inline data");

        // Test local file
        let file_path = temp_dir.path().join("test.txt");
        std::fs::write(&file_path, "Local file content").expect("Failed to write test file");
        let file_id = store
            .store_data(
                DataLocation::LocalPath(file_path),
                serde_json::json!({"type": "file"}),
                None,
            )
            .await
            .expect("Failed to store file");

        // Verify both entries exist and have correct metadata
        let entries = store
            .get_active_entries()
            .await
            .expect("Failed to get entries");
        assert_eq!(entries.len(), 2);

        // Verify inline data entry
        let inline_entry = entries
            .iter()
            .find(|e| e.id == inline_id)
            .expect("Inline entry not found");
        assert_eq!(inline_entry.metadata["type"], "inline");

        // Verify file entry
        let file_entry = entries
            .iter()
            .find(|e| e.id == file_id)
            .expect("File entry not found");
        assert_eq!(file_entry.metadata["type"], "file");

        Ok(())
    }

    #[sqlx::test]
    async fn test_data_versioning_and_history(pool: PgPool) -> TestResult<()> {
        let TestDataStore {
            datastore: mut store,
            temp_dir: _,
        } = setup_datastore(pool).await?;

        // Create initial version
        let v1_data = "Version 1".as_bytes().to_vec();
        let v1_id = store
            .store_data(
                DataLocation::Inline(v1_data),
                serde_json::json!({
                    "version": 1,
                    "type": "document"
                }),
                None,
            )
            .await
            .expect("Failed to store v1");

        // Create version 2
        let v2_data = "Version 2".as_bytes().to_vec();
        let v2_id = store
            .store_data(
                DataLocation::Inline(v2_data),
                serde_json::json!({
                    "version": 2,
                    "type": "document"
                }),
                Some(v1_id),
            )
            .await
            .expect("Failed to store v2");

        // Create version 3
        let v3_data = "Version 3".as_bytes().to_vec();
        let v3_id = store
            .store_data(
                DataLocation::Inline(v3_data),
                serde_json::json!({
                    "version": 3,
                    "type": "document"
                }),
                Some(v2_id),
            )
            .await
            .expect("Failed to store v3");

        // Get history starting from v3
        let history = store
            .get_history(v3_id)
            .await
            .expect("Failed to get history");

        // Verify history order and content
        assert_eq!(history.len(), 3, "Expected 3 versions in history");
        assert_eq!(history[0].id, v3_id, "Latest version should be first");
        assert_eq!(history[1].id, v2_id, "Second version should be second");
        assert_eq!(history[2].id, v1_id, "First version should be last");

        // Verify version numbers in metadata
        assert_eq!(
            history[0].metadata["version"], 3,
            "Latest version should have version 3"
        );
        assert_eq!(
            history[1].metadata["version"], 2,
            "Second version should have version 2"
        );
        assert_eq!(
            history[2].metadata["version"], 1,
            "First version should have version 1"
        );

        Ok(())
    }

    #[sqlx::test]
    async fn test_metadata_query_complex(pool: PgPool) -> TestResult<()> {
        let TestDataStore {
            datastore: mut store,
            temp_dir: _,
        } = setup_datastore(pool).await?;

        // Store multiple entries with different metadata
        for i in 1..=3 {
            store
                .store_data(
                    DataLocation::Inline(format!("Data {}", i).as_bytes().to_vec()),
                    serde_json::json!({
                        "type": "document",
                        "category": if i % 2 == 0 { "even" } else { "odd" },
                        "priority": i
                    }),
                    None,
                )
                .await
                .expect("Failed to store data");
        }

        // Query by category
        let odd_results = store
            .query_by_metadata(&serde_json::json!({"category": "odd"}), false)
            .await
            .expect("Failed to query odd entries");
        assert_eq!(odd_results.len(), 2, "Expected 2 odd entries");

        // Query by priority
        let high_priority = store
            .query_by_metadata(&serde_json::json!({"priority": 3}), false)
            .await
            .expect("Failed to query high priority entries");
        assert_eq!(high_priority.len(), 1, "Expected 1 high priority entry");

        // Query with multiple conditions
        let specific_entries = store
            .query_by_metadata(
                &serde_json::json!({
                    "type": "document",
                    "category": "odd"
                }),
                false,
            )
            .await
            .expect("Failed to query specific entries");
        assert_eq!(
            specific_entries.len(),
            2,
            "Expected 2 entries matching both conditions"
        );

        Ok(())
    }
}
