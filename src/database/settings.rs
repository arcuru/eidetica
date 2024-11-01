use log;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;
use validator::Validate;

use crate::database::{
    error::Error,
    metadata::{MetadataTable, PostgresMetadataTable},
    schema::MetadataEntry,
}; // Import both trait and implementation

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct Setting {
    /// Unique key identifying this setting
    #[validate(length(min = 1, max = 255))]
    pub key: String,

    /// The setting's value stored as arbitrary JSON
    pub value: Value,

    /// Optional description of what this setting controls
    pub description: Option<String>,
}

pub struct SettingsTable<T: MetadataTable> {
    table: T,
    device_id: Uuid,
    // We could comingle this with the normal metadata table and use a root ID
    // To find settings (all the children) or just search in metadata.
    // However I want to keep the separation for now
}

#[allow(dead_code)]
pub trait Settings {
    /// Retrieves a setting by its key
    ///
    /// # Arguments
    /// * `key` - The unique identifier for the setting to retrieve
    ///
    /// # Returns
    /// * `Ok(Some(Setting))` - The setting was found and returned
    /// * `Ok(None)` - No setting exists with the given key
    /// * `Err(Error)` - A database error occurred
    async fn get_setting(&self, key: &str) -> Result<Option<Setting>, Error>;

    /// Creates or updates a setting
    ///
    /// If a setting with the same key already exists, it will be archived and replaced
    /// with the new value.
    ///
    /// # Arguments
    /// * `setting` - The setting to create or update
    ///
    /// # Returns
    /// * `Ok(())` - The setting was successfully created/updated
    /// * `Err(Error)` - Failed to create/update the setting
    async fn set_setting(&mut self, setting: Setting) -> Result<(), Error>;

    /// Deletes a setting by its key
    ///
    /// The setting is marked as deleted by creating a new entry with deleted=true.
    /// The previous entry is archived.
    ///
    /// # Arguments
    /// * `key` - The unique identifier of the setting to delete
    ///
    /// # Returns
    /// * `Ok(())` - The setting was successfully deleted
    /// * `Err(Error::NotFound)` - No setting exists with the given key
    /// * `Err(Error)` - Failed to delete the setting
    async fn delete_setting(&mut self, key: &str) -> Result<(), Error>;

    /// Returns a list of all active (non-deleted) settings
    ///
    /// # Returns
    /// * `Ok(Vec<Setting>)` - List of all active settings
    /// * `Err(Error)` - Failed to retrieve the settings
    async fn list_settings(&self) -> Result<Vec<Setting>, Error>;

    /// Retrieves the history of a setting, including all versions (active and archived)
    ///
    /// # Arguments
    /// * `key` - The unique identifier for the setting to retrieve history for
    ///
    /// # Returns
    /// * `Ok(Vec<(Setting, bool)>)` - List of all versions of the setting in chronological order,
    ///                                where the bool indicates whether this version is the current active setting
    /// * `Err(Error)` - Failed to retrieve the setting history
    async fn get_setting_history(&self, key: &str) -> Result<Vec<(Setting, bool)>, Error>;
}

#[allow(dead_code)]
impl<T: MetadataTable> SettingsTable<T> {
    /// Create a new SettingsTable from any MetadataTable implementation
    pub async fn new(mut table: T, device_id: Uuid) -> Result<Self, Error> {
        // Ensure the table exists
        table.create_table().await?;

        Ok(Self { table, device_id })
    }

    /// Convert a Setting field into a MetadataEntry for storing
    fn setting_to_metadata_entry(&self, setting: &Setting) -> MetadataEntry {
        let metadata = if let Some(description) = &setting.description {
            serde_json::json!({
                "type": "setting",
                "key": setting.key,
                "value": setting.value,
                "description": description,
            })
        } else {
            serde_json::json!({
                "type": "setting",
                "key": setting.key,
                "value": setting.value,
            })
        };

        MetadataEntry {
            id: Uuid::now_v7(),
            device_id: self.device_id,
            archived: false,
            parent_id: None,
            metadata,
            data_hash: "empty".to_string(),
        }
    }

    /// Convert a MetadataEntry into a Setting
    fn metadata_entry_to_setting(entry: &MetadataEntry) -> Option<Setting> {
        // Verify this is a setting type entry
        if entry.metadata.get("type")?.as_str()? != "setting" {
            return None;
        }

        // If this is a deleted entry, return a Setting with null value
        if entry
            .metadata
            .get("deleted")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let key = entry.metadata.get("key")?.as_str()?.to_string();
            return Some(Setting {
                key,
                value: serde_json::Value::Null,
                description: None,
            });
        }

        let key = entry.metadata.get("key")?.as_str()?.to_string();
        let value = entry.metadata.get("value")?.clone();
        let description = entry
            .metadata
            .get("description")
            .and_then(|d| d.as_str())
            .map(String::from);

        Some(Setting {
            key,
            value,
            description,
        })
    }
}

#[allow(dead_code)]
impl SettingsTable<PostgresMetadataTable> {
    /// Create a new SettingsTable from a Postgres pool
    pub async fn from_postgres(pool: PgPool, device_id: Uuid) -> Result<Self, Error> {
        let table = PostgresMetadataTable::from_pool(pool, "settings").await?;
        Self::new(table, device_id).await
    }
}

impl<T: MetadataTable> Settings for SettingsTable<T> {
    async fn get_setting(&self, key: &str) -> Result<Option<Setting>, Error> {
        let conditions = serde_json::json!({
            "type": "setting",
            "key": key,
        });

        // Get only active entries
        let entries = self
            .table
            .get_entries_by_metadata_conditions(&conditions, false)
            .await?;

        // Print an error if multiple
        if entries.len() > 1 {
            // TODO: return them all
            log::error!("Multiple settings found for key: {}", key);
        }

        if let Some(entry) = entries.first() {
            Ok(Self::metadata_entry_to_setting(entry))
        } else {
            Ok(None)
        }
    }

    async fn set_setting(&mut self, setting: Setting) -> Result<(), Error> {
        // Validate key length
        // Validate the setting against the defined rules
        if let Err(validation_errors) = setting.validate() {
            log::error!("Setting validation failed: {:?}", validation_errors);
            return Err(Error::InvalidData);
        }

        // Validate key format
        if setting.key.is_empty()
            || !setting
                .key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(Error::InvalidData);
        }

        // Find any existing active setting with this key
        let conditions = serde_json::json!({
                "type": &"setting",
                "key": &setting.key,
        });

        // Get any existing active settings with this key
        let existing_settings = self
            .table
            .get_entries_by_metadata_conditions(&conditions, false)
            .await?;

        // Get the ID of the existing setting if there is one
        let parent_id = existing_settings.first().map(|entry| entry.id);

        // Convert the setting to a metadata entry with the parent ID
        let metadata_entry = MetadataEntry {
            id: Uuid::now_v7(),
            device_id: self.device_id,
            archived: false,
            parent_id,
            metadata: serde_json::json!({
                "type": "setting",
                "key": setting.key,
                "value": setting.value,
                "description": setting.description,
            }),
            data_hash: "empty".to_string(),
        };

        // Create the new entry - this will automatically archive the parent if it exists
        self.table.create_entry(metadata_entry).await?;

        Ok(())
    }

    async fn list_settings(&self) -> Result<Vec<Setting>, Error> {
        let conditions = serde_json::json!({
            "type": "setting"
        });
        // Get only active settings
        let entries = self
            .table
            .get_entries_by_metadata_conditions(&conditions, false)
            .await?;

        // Convert entries to settings
        let settings = entries
            .iter()
            .filter_map(Self::metadata_entry_to_setting)
            .collect();

        Ok(settings)
    }

    async fn get_setting_history(&self, key: &str) -> Result<Vec<(Setting, bool)>, Error> {
        let conditions = serde_json::json!({
            "type": "setting",
            "key": key
        });

        // Get all versions including archived
        let entries = self
            .table
            .get_entries_by_metadata_conditions(&conditions, true)
            .await?;

        // Convert entries to settings, including the archived status
        Ok(entries
            .iter()
            .filter_map(|entry| {
                Self::metadata_entry_to_setting(entry).map(|setting| (setting, !entry.archived))
            })
            .collect())
    }

    async fn delete_setting(&mut self, key: &str) -> Result<(), Error> {
        // Find the current active setting with this key
        let conditions = serde_json::json!({
                "type": &"setting",
                "key": &key,
        });

        // Get any existing active settings with this key
        let existing_settings = self
            .table
            .get_entries_by_metadata_conditions(&conditions, false)
            .await?;

        if existing_settings.len() > 1 {
            log::error!("Multiple active settings found for key: {}", key);
        }

        // Get the ID of the existing setting if there is one
        let parent_id = match existing_settings.first() {
            Some(entry) => entry.id,
            None => return Err(Error::NotFound), // Setting not found
        };

        // Create a new metadata entry marking this setting as deleted
        let metadata_entry = MetadataEntry {
            id: Uuid::now_v7(),
            device_id: self.device_id,
            archived: false,
            parent_id: Some(parent_id),
            metadata: serde_json::json!({
                "type": "setting",
                "key": &key,
                "deleted": true
            }),
            data_hash: "empty".to_string(),
        };

        // Create the new entry - this will automatically archive the parent
        self.table.create_entry(metadata_entry).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[sqlx::test]
    fn test_setting_metadata_conversion(pool: PgPool) {
        let device_id = Uuid::new_v4();
        let table = SettingsTable {
            table: PostgresMetadataTable {
                table_name: "test_settings".to_string(),
                pool,
            },
            device_id,
        };

        // Test simple setting
        let simple_setting = Setting {
            key: "test_key".to_string(),
            value: json!("test_value"),
            description: None,
        };

        let metadata_entry = table.setting_to_metadata_entry(&simple_setting);

        // Verify metadata entry fields
        assert_eq!(metadata_entry.device_id, device_id);
        assert!(!metadata_entry.archived);
        assert_eq!(metadata_entry.parent_id, None);
        assert_eq!(metadata_entry.data_hash, "empty");
        assert_eq!(metadata_entry.metadata["type"], "setting");
        assert_eq!(metadata_entry.metadata["key"], "test_key");
        assert_eq!(metadata_entry.metadata["value"], "test_value");
        assert!(metadata_entry.metadata.get("description").is_none());

        // Convert back to setting
        let converted_setting =
            SettingsTable::<PostgresMetadataTable>::metadata_entry_to_setting(&metadata_entry)
                .unwrap();
        assert_eq!(converted_setting.key, simple_setting.key);
        assert_eq!(converted_setting.value, simple_setting.value);
        assert_eq!(converted_setting.description, simple_setting.description);

        // Test setting with description and complex value
        let complex_setting = Setting {
            key: "complex_key".to_string(),
            value: json!({
                "nested": {
                    "field": 42,
                    "array": [1, 2, 3]
                }
            }),
            description: Some("Test description".to_string()),
        };

        let metadata_entry = table.setting_to_metadata_entry(&complex_setting);

        // Verify metadata entry fields for complex setting
        assert_eq!(metadata_entry.device_id, device_id);
        assert!(!metadata_entry.archived);
        assert_eq!(metadata_entry.parent_id, None);
        assert_eq!(metadata_entry.metadata["type"], "setting");
        assert_eq!(metadata_entry.metadata["key"], "complex_key");
        assert_eq!(metadata_entry.metadata["value"], complex_setting.value);
        assert_eq!(metadata_entry.metadata["description"], "Test description");

        // Convert back to setting
        let converted_setting =
            SettingsTable::<PostgresMetadataTable>::metadata_entry_to_setting(&metadata_entry)
                .unwrap();
        assert_eq!(converted_setting.key, complex_setting.key);
        assert_eq!(converted_setting.value, complex_setting.value);
        assert_eq!(converted_setting.description, complex_setting.description);
    }

    #[sqlx::test]
    async fn test_invalid_setting_metadata(pool: PgPool) {
        let device_id = Uuid::new_v4();
        let mut settings = SettingsTable::from_postgres(pool, device_id).await.unwrap();

        // Test with malformed JSON metadata
        let entry = MetadataEntry {
            id: Uuid::now_v7(),
            device_id,
            archived: false,
            parent_id: None,
            metadata: json!({
                "type": "setting",
                // Missing required "value" field
                "key": "test"
            }),
            data_hash: "empty".to_string(),
        };

        // Insert the malformed entry directly
        settings.table.create_entry(entry).await.unwrap();

        // Attempting to retrieve the setting should return None since it's invalid
        let result = settings.get_setting("test").await.unwrap();
        assert!(result.is_none());

        // Test with invalid type field
        let entry_wrong_type = MetadataEntry {
            id: Uuid::now_v7(),
            device_id,
            archived: false,
            parent_id: None,
            metadata: json!({
                "type": "not_a_setting",
                "key": "test2",
                "value": "test_value"
            }),
            data_hash: "empty".to_string(),
        };

        // Insert the entry with wrong type
        settings.table.create_entry(entry_wrong_type).await.unwrap();

        // Should return None for wrong type
        let result = settings.get_setting("test2").await.unwrap();
        assert!(result.is_none());

        // Test with null value
        let entry_null_value = MetadataEntry {
            id: Uuid::now_v7(),
            device_id,
            archived: false,
            parent_id: None,
            metadata: json!({
                "type": "setting",
                "key": "test3",
                "value": null
            }),
            data_hash: "empty".to_string(),
        };

        // Insert the entry with null value
        settings.table.create_entry(entry_null_value).await.unwrap();

        // Should still be able to retrieve null value
        let result = settings.get_setting("test3").await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().value, json!(null));
    }

    #[sqlx::test]
    async fn test_invalid_setting_keys(pool: PgPool) {
        let mut settings = SettingsTable::from_postgres(pool, Uuid::new_v4())
            .await
            .unwrap();

        // Should fail for empty key
        let result = settings
            .set_setting(Setting {
                key: "".to_string(),
                value: json!("test"),
                description: None,
            })
            .await;
        assert!(matches!(result, Err(Error::InvalidData)));

        // Should fail for invalid characters
        let result = settings
            .set_setting(Setting {
                key: "invalid-key".to_string(), // Contains hyphen
                value: json!("test"),
                description: None,
            })
            .await;
        assert!(matches!(result, Err(Error::InvalidData)));
    }

    #[sqlx::test]
    fn test_invalid_metadata_entry_conversion() {
        // Test missing required fields
        let invalid_entry = MetadataEntry {
            id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
            archived: false,
            parent_id: None,
            metadata: json!({
                "type": "setting",
                // Missing key field
                "value": "test"
            }),
            data_hash: "empty".to_string(),
        };

        assert!(
            SettingsTable::<PostgresMetadataTable>::metadata_entry_to_setting(&invalid_entry)
                .is_none()
        );

        // Test wrong type field
        let wrong_type_entry = MetadataEntry {
            id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
            archived: false,
            parent_id: None,
            metadata: json!({
                "type": "not_a_setting",
                "key": "test",
                "value": "test"
            }),
            data_hash: "empty".to_string(),
        };

        assert!(
            SettingsTable::<PostgresMetadataTable>::metadata_entry_to_setting(&wrong_type_entry)
                .is_none()
        );

        // Test invalid value type
        let invalid_value_entry = MetadataEntry {
            id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
            archived: false,
            parent_id: None,
            metadata: json!({
                "type": "setting",
                "key": true, // key should be a string
                "value": "test"
            }),
            data_hash: "empty".to_string(),
        };

        assert!(
            SettingsTable::<PostgresMetadataTable>::metadata_entry_to_setting(&invalid_value_entry)
                .is_none()
        );
    }

    #[sqlx::test]
    async fn test_get_setting(pool: PgPool) {
        let device_id = Uuid::new_v4();
        let mut settings = SettingsTable::from_postgres(pool, device_id).await.unwrap();

        // Test getting non-existent setting
        let result = settings.get_setting("nonexistent").await.unwrap();
        assert!(result.is_none());

        // Create a test setting
        let test_setting = Setting {
            key: "test_key".to_string(),
            value: json!("test_value"),
            description: Some("test description".to_string()),
        };

        // Set the setting
        settings.set_setting(test_setting.clone()).await.unwrap();

        // Get the setting and verify
        let retrieved = settings.get_setting("test_key").await.unwrap().unwrap();
        assert_eq!(retrieved.key, test_setting.key);
        assert_eq!(retrieved.value, test_setting.value);
        assert_eq!(retrieved.description, test_setting.description);

        // Test case with multiple active settings (should log an error but return the first one)
        // This simulates a corrupted state that shouldn't happen in normal operation
        let metadata_entry = MetadataEntry {
            id: Uuid::now_v7(),
            device_id,
            archived: false,
            parent_id: None,
            metadata: json!({
                "type": "setting",
                "key": "test_key",
                "value": "duplicate_value",
                "description": "duplicate description"
            }),
            data_hash: "empty".to_string(),
        };

        // Insert duplicate directly through metadata table
        settings.table.create_entry(metadata_entry).await.unwrap();

        // Should still get a result (and log an error)
        let retrieved = settings.get_setting("test_key").await.unwrap();
        assert!(retrieved.is_some());
    }

    #[sqlx::test]
    async fn test_get_setting_history(pool: PgPool) {
        let mut settings = SettingsTable::from_postgres(pool, Uuid::new_v4())
            .await
            .unwrap();

        // Create initial setting
        let initial = Setting {
            key: "test_key".to_string(),
            value: json!("initial_value"),
            description: None,
        };
        settings.set_setting(initial.clone()).await.unwrap();

        // Update setting multiple times
        let update1 = Setting {
            key: "test_key".to_string(),
            value: json!("update1_value"),
            description: Some("first update".to_string()),
        };
        settings.set_setting(update1.clone()).await.unwrap();

        let update2 = Setting {
            key: "test_key".to_string(),
            value: json!("update2_value"),
            description: Some("second update".to_string()),
        };
        settings.set_setting(update2.clone()).await.unwrap();

        // Delete the setting
        settings.delete_setting("test_key").await.unwrap();

        // Get history
        let history = settings.get_setting_history("test_key").await.unwrap();

        // Verify history length (should be 4: initial, 2 updates, and delete)
        assert_eq!(history.len(), 4);

        // Verify history order, first is the most recent update (in this case deleted)
        assert!(history[0].1); // Delete entry should be active (not archived)
        assert!(history[0].0.value.is_null()); // Delete entry should have null value

        assert!(!history[1].1);
        assert_eq!(history[1].0.value, json!("update2_value"));

        assert!(!history[2].1);
        assert_eq!(history[2].0.value, json!("update1_value"));

        assert!(!history[3].1);
        assert_eq!(history[3].0.value, json!("initial_value"));

        // Test getting history for non-existent setting
        let history = settings.get_setting_history("nonexistent").await.unwrap();
        assert!(history.is_empty());
    }
}
