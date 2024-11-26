use crate::datastore::schema::{DeviceId, PrivateKey, StreamEntry, StreamType};
use anyhow::Result;
use sqlx::{Error, PgPool};

/// Interface for interacting with the stream table
#[allow(dead_code, async_fn_in_trait)]
pub trait StreamTable {
    /// Create a new stream table with the given name if it doesn't exist
    async fn create_table(&mut self) -> Result<()>;

    /// Create a new stream entry
    /// Returns the created entry with its assigned index
    async fn create_entry(
        &mut self,
        device_id: DeviceId,
        stream_type: StreamType,
        secret_key: Option<PrivateKey>,
    ) -> Result<StreamEntry>;

    /// Retrieve an entry by its device ID if it exists
    async fn get_entry_by_device_id(&self, device_id: &DeviceId) -> Result<Option<StreamEntry>>;

    /// Retrieve an entry by its index if it exists
    async fn get_entry_by_index(&self, index: i64) -> Result<Option<StreamEntry>>;

    /// Get all entries of a specific stream type
    async fn get_entries_by_type(&self, stream_type: StreamType) -> Result<Vec<StreamEntry>>;

    /// Update an existing stream entry
    /// Returns Error::RowNotFound if the entry doesn't exist
    async fn update_entry(&self, entry: StreamEntry) -> Result<()>;

    /// Delete a stream entry
    /// Returns Error::RowNotFound if the entry doesn't exist
    async fn delete_entry(&mut self, index: i64) -> Result<()>;
}

/// PostgreSQL implementation of the stream table
pub struct PostgresStreamTable {
    pub pool: PgPool,
}

#[allow(dead_code)]
impl PostgresStreamTable {
    /// Create a new PostgresStreamTable instance
    pub async fn new(connection_string: &str) -> Result<Self> {
        let pool = PgPool::connect(connection_string).await?;

        let mut table = Self { pool };
        table.create_table().await?;
        Ok(table)
    }

    /// Create a new PostgresStreamTable from an existing pool connection
    pub async fn from_pool(pool: PgPool) -> Result<Self> {
        let mut table = Self { pool };
        table.create_table().await?;
        Ok(table)
    }
}

impl StreamTable for PostgresStreamTable {
    async fn create_table(&mut self) -> Result<()> {
        const MAX_RETRIES: u32 = 3;
        const RETRY_DELAY_MS: u64 = 500;

        let mut attempts = 0;
        let mut last_error = None;

        // Modified table creation to ensure index is handled properly
        let query = r#"
            CREATE TABLE IF NOT EXISTS streams (
                index BIGSERIAL PRIMARY KEY,  -- Auto-incrementing primary key
                id BYTEA NOT NULL UNIQUE,     -- Device ID is unique but not primary key
                secret_key BYTEA,
                stream_type TEXT NOT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

                -- Ensure index is always positive and starts at 1
                CONSTRAINT positive_index CHECK (index > 0)
            );"#;

        while attempts < MAX_RETRIES {
            match sqlx::query(query).execute(&self.pool).await {
                Ok(_) => return Ok(()),
                Err(e) => {
                    last_error = Some(e);
                    attempts += 1;
                    if attempts < MAX_RETRIES {
                        tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS)).await;
                    }
                }
            }
        }
        Err(last_error.unwrap().into())
    }

    async fn create_entry(
        &mut self,
        device_id: DeviceId,
        stream_type: StreamType,
        secret_key: Option<PrivateKey>,
    ) -> Result<StreamEntry> {
        let stream_type_str = match stream_type {
            StreamType::Stream => "Stream",
            StreamType::User => "User",
            StreamType::Instance => "Instance",
            StreamType::Store => "Store",
        };

        let query = r#"
            INSERT INTO streams (id, secret_key, stream_type)
            VALUES ($1, $2, $3)
            RETURNING index, id, secret_key, stream_type"#;

        let row = sqlx::query(query)
            .bind(device_id)
            .bind(secret_key.as_ref())
            .bind(stream_type_str)
            .fetch_one(&self.pool)
            .await?;

        Ok(StreamEntry::from(row))
    }

    async fn get_entry_by_device_id(&self, device_id: &DeviceId) -> Result<Option<StreamEntry>> {
        let query = r#"
            SELECT index, id, secret_key, stream_type
            FROM streams 
            WHERE id = $1"#;

        let row = sqlx::query(query)
            .bind(device_id)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(StreamEntry::from))
    }

    async fn get_entry_by_index(&self, index: i64) -> Result<Option<StreamEntry>> {
        let query = r#"
            SELECT index, id, secret_key, stream_type
            FROM streams
            WHERE index = $1"#;

        let row = sqlx::query(query)
            .bind(index)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(StreamEntry::from))
    }

    async fn get_entries_by_type(&self, stream_type: StreamType) -> Result<Vec<StreamEntry>> {
        let stream_type_str = match stream_type {
            StreamType::Stream => "Stream",
            StreamType::User => "User",
            StreamType::Instance => "Instance",
            StreamType::Store => "Store",
        };

        let query = r#"
            SELECT index, id, secret_key, stream_type 
            FROM streams
            WHERE stream_type = $1
            ORDER BY index"#;

        let rows = sqlx::query(query)
            .bind(stream_type_str)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(StreamEntry::from).collect())
    }

    async fn update_entry(&self, entry: StreamEntry) -> Result<()> {
        let stream_type_str = match entry.stream_type {
            StreamType::Stream => "Stream",
            StreamType::User => "User",
            StreamType::Instance => "Instance",
            StreamType::Store => "Store",
        };

        let query = r#"
            INSERT INTO streams (index, id, secret_key, stream_type)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (index) DO UPDATE
            SET id = EXCLUDED.id,
                secret_key = EXCLUDED.secret_key,
                stream_type = EXCLUDED.stream_type"#;

        let result = sqlx::query(query)
            .bind(entry.index)
            .bind(entry.id)
            .bind(entry.secret_key.as_ref())
            .bind(stream_type_str)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            return Err(Error::RowNotFound.into());
        }

        Ok(())
    }

    async fn delete_entry(&mut self, index: i64) -> Result<()> {
        let query = "DELETE FROM streams WHERE index = $1";

        let result = sqlx::query(query).bind(index).execute(&self.pool).await?;

        if result.rows_affected() == 0 {
            return Err(Error::RowNotFound.into());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::generate_key;

    fn generate_test_device_id() -> DeviceId {
        generate_key().verifying_key().to_bytes()
    }

    fn generate_test_secret_key() -> PrivateKey {
        generate_key().to_bytes()
    }

    #[sqlx::test]
    async fn test_create_entry(pool: PgPool) {
        let mut table = PostgresStreamTable::from_pool(pool).await.unwrap();
        let device_id = generate_test_device_id();
        let secret_key = Some(generate_test_secret_key());

        // Create entry
        let entry = table
            .create_entry(device_id, StreamType::Stream, secret_key)
            .await
            .unwrap();

        // Verify fields
        assert_eq!(entry.id, device_id);
        assert_eq!(entry.secret_key, secret_key);
        assert!(matches!(entry.stream_type, StreamType::Stream));
        assert!(entry.index > 0);
    }

    #[sqlx::test]
    async fn test_get_entry_by_device_id(pool: PgPool) {
        let mut table = PostgresStreamTable::from_pool(pool).await.unwrap();
        let device_id = generate_test_device_id();

        // Create entry
        let original_entry = table
            .create_entry(device_id, StreamType::User, None)
            .await
            .unwrap();

        // Retrieve entry
        let retrieved_entry = table.get_entry_by_device_id(&device_id).await.unwrap();
        assert!(retrieved_entry.is_some());
        let retrieved_entry = retrieved_entry.unwrap();

        // Verify entries match
        assert_eq!(retrieved_entry.id, original_entry.id);
        assert_eq!(retrieved_entry.secret_key, original_entry.secret_key);
        assert_eq!(retrieved_entry.index, original_entry.index);
        assert!(matches!(retrieved_entry.stream_type, StreamType::User));

        // Test non-existent entry
        let nonexistent = table
            .get_entry_by_device_id(&generate_test_device_id())
            .await
            .unwrap();
        assert!(nonexistent.is_none());
    }

    #[sqlx::test]
    async fn test_get_entry_by_index(pool: PgPool) {
        let mut table = PostgresStreamTable::from_pool(pool).await.unwrap();
        let device_id = generate_test_device_id();

        // Create entry
        let original_entry = table
            .create_entry(device_id, StreamType::Instance, None)
            .await
            .unwrap();

        // Retrieve by index
        let retrieved_entry = table
            .get_entry_by_index(original_entry.index)
            .await
            .unwrap();
        assert!(retrieved_entry.is_some());
        let retrieved_entry = retrieved_entry.unwrap();

        // Verify entries match
        assert_eq!(retrieved_entry.id, original_entry.id);
        assert_eq!(retrieved_entry.secret_key, original_entry.secret_key);
        assert_eq!(retrieved_entry.index, original_entry.index);
        assert!(matches!(retrieved_entry.stream_type, StreamType::Instance));

        // Test non-existent index
        let nonexistent = table.get_entry_by_index(99999).await.unwrap();
        assert!(nonexistent.is_none());
    }

    #[sqlx::test]
    async fn test_get_entries_by_type(pool: PgPool) {
        let mut table = PostgresStreamTable::from_pool(pool).await.unwrap();

        // Create entries of different types
        let stream_entry = table
            .create_entry(generate_test_device_id(), StreamType::Stream, None)
            .await
            .unwrap();
        let user_entry = table
            .create_entry(generate_test_device_id(), StreamType::User, None)
            .await
            .unwrap();
        let instance_entry = table
            .create_entry(generate_test_device_id(), StreamType::Instance, None)
            .await
            .unwrap();

        // Get Stream entries
        let stream_entries = table.get_entries_by_type(StreamType::Stream).await.unwrap();
        assert_eq!(stream_entries.len(), 1);
        assert_eq!(stream_entries[0].id, stream_entry.id);

        // Get User entries
        let user_entries = table.get_entries_by_type(StreamType::User).await.unwrap();
        assert_eq!(user_entries.len(), 1);
        assert_eq!(user_entries[0].id, user_entry.id);

        // Get Instance entries
        let instance_entries = table
            .get_entries_by_type(StreamType::Instance)
            .await
            .unwrap();
        assert_eq!(instance_entries.len(), 1);
        assert_eq!(instance_entries[0].id, instance_entry.id);

        // Get entries of type with no entries
        let store_entries = table.get_entries_by_type(StreamType::Store).await.unwrap();
        assert!(store_entries.is_empty());
    }

    #[sqlx::test]
    async fn test_update_entry(pool: PgPool) {
        let mut table = PostgresStreamTable::from_pool(pool).await.unwrap();
        let device_id = generate_test_device_id();

        // Create initial entry
        let mut entry = table
            .create_entry(device_id, StreamType::Stream, None)
            .await
            .unwrap();

        // Modify entry
        // Changing the device id should never happen, but we'll try it for testing
        let new_device_id = generate_test_device_id();
        let new_secret_key = Some(generate_test_secret_key());
        entry.id = new_device_id;
        entry.secret_key = new_secret_key;
        entry.stream_type = StreamType::User;

        // Update entry
        assert!(table.update_entry(entry.clone()).await.is_ok());

        // Verify changes
        let updated = table
            .get_entry_by_index(entry.index)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated, entry);

        // Test updating non-existent entry
        let mut nonexistent = entry;
        nonexistent.index = 99999;
        assert!(table.update_entry(nonexistent).await.is_err());
    }

    #[sqlx::test]
    async fn test_delete_entry(pool: PgPool) {
        let mut table = PostgresStreamTable::from_pool(pool).await.unwrap();
        let device_id = generate_test_device_id();

        // Create entry
        let entry = table
            .create_entry(device_id, StreamType::Stream, None)
            .await
            .unwrap();

        // Delete entry
        assert!(table.delete_entry(entry.index).await.is_ok());

        // Verify entry is deleted
        let deleted = table.get_entry_by_index(entry.index).await.unwrap();
        assert!(deleted.is_none());

        // Test deleting non-existent entry
        assert!(table.delete_entry(99999).await.is_err());
    }

    #[sqlx::test]
    async fn test_create_table_duplicate(pool: PgPool) {
        // Test that creating multiple table instances works
        let mut table1 = PostgresStreamTable::from_pool(pool.clone()).await.unwrap();
        let mut table2 = PostgresStreamTable::from_pool(pool).await.unwrap();

        // Both creations should succeed
        assert!(table1.create_table().await.is_ok());
        assert!(table2.create_table().await.is_ok());
    }
}
