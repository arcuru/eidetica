use crate::datastore::schema::{DataEntry, DeviceId};
use anyhow::{anyhow, bail, Error, Result};
use sqlx::{PgPool, Row};

/// Data Table
///
/// This is the interface and implementation of the Data Table in the database.
/// We store the actual data and tracking information about where copies exist.
/// Everything is indexed on the hash

/// Interface for interacting with the data table
#[allow(dead_code, async_fn_in_trait)]
pub trait DataTable {
    /// Create a new data entry
    async fn create_entry(&mut self, entry: DataEntry) -> Result<()>;

    // Delete a data entry
    async fn delete_entry(&mut self, hash: &str) -> Result<()>;

    /// Get an entry or insert a new one if it doesn't exist
    async fn get_or_insert_entry(&mut self, hash: &str) -> Result<DataEntry>;

    /// Retrieve an entry by its hash
    async fn get_entry(&self, hash: &str) -> Result<Option<DataEntry>>;

    /// Increase the ref_count
    async fn increase_ref_count(&mut self, hash: &str) -> Result<i32>;

    /// Decrease the ref_count
    async fn decrease_ref_count(&mut self, hash: &str) -> Result<i32>;

    /// Add a device to the list of devices that have this data
    async fn add_device(&mut self, hash: &str, device_id: DeviceId) -> Result<()>;

    /// Remove a device from the list of devices that have this data
    async fn remove_device(&mut self, hash: &str, device_id: DeviceId) -> Result<()>;

    /// Add a local path for this data
    async fn add_local_path(&mut self, hash: &str, path: String) -> Result<()>;

    /// Remove a local path for this data
    async fn remove_local_path(&mut self, hash: &str, path: &str) -> Result<()>;

    /// Add an S3 path for this data
    async fn add_s3_path(&mut self, hash: &str, path: String) -> Result<()>;

    /// Remove an S3 path for this data
    async fn remove_s3_path(&mut self, hash: &str, path: &str) -> Result<()>;

    /// Add inline data for this entry
    async fn add_inline_data(&mut self, hash: &str, data: Vec<u8>) -> Result<()>;

    /// Remove inline data for this entry
    async fn remove_inline_data(&mut self, hash: &str) -> Result<()>;
}

/// PostgreSQL implementation of the data table
pub struct PostgresDataTable {
    pool: PgPool,
}

#[allow(dead_code)]
impl PostgresDataTable {
    /// Create a new PostgresDataTable instance
    pub async fn new(connection_string: &str) -> Result<Self> {
        let pool = PgPool::connect(connection_string).await?;

        // Ensure table exists
        Self::create_table(&pool).await?;

        Ok(Self { pool })
    }

    /// Create a new PostgresDataTable from an existing pool connection
    pub async fn from_pool(pool: PgPool) -> Result<Self> {
        Self::create_table(&pool).await?;
        Ok(Self { pool })
    }

    /// Create the data table if it doesn't exist
    async fn create_table(pool: &PgPool) -> Result<()> {
        // Retrying here a few times. Postgres will fail if we hit this while creating the same table in a different thread.
        const MAX_RETRIES: u32 = 3;
        const RETRY_DELAY_MS: u64 = 500;

        let mut attempts = 0;
        let mut last_error = None;

        while attempts < MAX_RETRIES {
            match sqlx::query(
                r#"
                CREATE TABLE IF NOT EXISTS data_entries (
                    hash CHAR(67) PRIMARY KEY,
                    ref_count INT NOT NULL DEFAULT 0,
                    inline_data BYTEA,
                    devices BYTEA[] NOT NULL DEFAULT '{}',
                    local_path TEXT[] NOT NULL DEFAULT '{}',
                    s3_path TEXT[] NOT NULL DEFAULT '{}',
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );"#,
            )
            .execute(pool)
            .await
            {
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
}

impl DataTable for PostgresDataTable {
    async fn get_or_insert_entry(&mut self, hash: &str) -> Result<DataEntry> {
        // Use a single query that will either insert a new entry or return the existing one
        let row = sqlx::query(
            r#"
        INSERT INTO data_entries
            (hash, ref_count, inline_data, devices, local_path, s3_path)
        VALUES
            ($1, 0, NULL, ARRAY[]::BYTEA[], ARRAY[]::TEXT[], ARRAY[]::TEXT[])
        ON CONFLICT (hash) DO UPDATE SET
            -- Set hash to itself to trigger the RETURNING clause
            hash = EXCLUDED.hash
        RETURNING
            hash,
            ref_count,
            inline_data,
            devices,
            local_path,
            s3_path
        "#,
        )
        .bind(hash)
        .fetch_one(&self.pool)
        .await?;

        Ok(DataEntry {
            hash: row.get("hash"),
            ref_count: row.get("ref_count"),
            inline_data: row.get("inline_data"),
            devices: row.get("devices"),
            local_path: row.get("local_path"),
            s3_path: row.get("s3_path"),
        })
    }

    async fn get_entry(&self, hash: &str) -> Result<Option<DataEntry>> {
        let row = sqlx::query(
            r#"
            SELECT
                hash,
                ref_count,
                inline_data,
                devices,
                local_path,
                s3_path
            FROM data_entries
            WHERE hash = $1
            "#,
        )
        .bind(hash)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => {
                let entry = DataEntry {
                    hash: row.get("hash"),
                    ref_count: row.get("ref_count"),
                    inline_data: row.get("inline_data"),
                    devices: row.get("devices"),
                    local_path: row.get("local_path"),
                    s3_path: row.get("s3_path"),
                };
                Ok(Some(entry))
            }
            None => Ok(None),
        }
    }

    async fn create_entry(&mut self, entry: DataEntry) -> Result<()> {
        // Start a transaction
        let mut tx = self.pool.begin().await?;

        // Insert the new entry
        let result = sqlx::query(
            r#"
            INSERT INTO data_entries
                (hash, ref_count, inline_data, devices, local_path, s3_path)
            VALUES
                ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(&entry.hash)
        .bind(entry.ref_count)
        .bind(entry.inline_data)
        .bind(&entry.devices)
        .bind(&entry.local_path)
        .bind(&entry.s3_path)
        .execute(&mut *tx)
        .await;

        // Add context for certain error codes
        if let Err(e) = &result {
            if let Some(db_error) = e.as_database_error() {
                if db_error.code().as_deref() == Some("23505") {
                    // PostgreSQL unique violation code
                    return Err(anyhow!("Entry already exists").context(db_error.to_string()));
                }
            }
        }

        // Handle other potential errors
        result?;

        // Commit the transaction
        tx.commit().await?;

        Ok(())
    }

    async fn delete_entry(&mut self, hash: &str) -> Result<()> {
        let result = sqlx::query(
            r#"
            DELETE FROM data_entries
            WHERE hash = $1
            "#,
        )
        .bind(hash)
        .execute(&self.pool)
        .await;

        match result {
            Ok(rows_affected) if rows_affected.rows_affected() == 1 => Ok(()),
            Ok(_) => Err(anyhow!("Record not found")),
            Err(e) => Err(Error::from(e)),
        }
    }

    async fn increase_ref_count(&mut self, hash: &str) -> Result<i32> {
        let row = sqlx::query(
            r#"
            UPDATE data_entries
            SET ref_count = ref_count + 1
            WHERE hash = $1
            RETURNING ref_count
            "#,
        )
        .bind(hash)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(row.get("ref_count")),
            None => Err(anyhow!("Not found")),
        }
    }

    async fn decrease_ref_count(&mut self, hash: &str) -> Result<i32> {
        let row = sqlx::query(
            r#"
            UPDATE data_entries
            SET ref_count = GREATEST(ref_count - 1, 0)
            WHERE hash = $1
            RETURNING ref_count
            "#,
        )
        .bind(hash)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(row.get("ref_count")),
            None => Err(anyhow!("Not found")),
        }
    }

    async fn add_inline_data(&mut self, hash: &str, data: Vec<u8>) -> Result<()> {
        let result = sqlx::query(
            r#"
            UPDATE data_entries
            SET inline_data = $2
            WHERE hash = $1
            "#,
        )
        .bind(hash)
        .bind(&data)
        .execute(&self.pool)
        .await?;

        // Verify one row was affected
        if result.rows_affected() != 1 {
            bail!("Not found")
        }

        Ok(())
    }

    async fn remove_inline_data(&mut self, hash: &str) -> Result<()> {
        let result = sqlx::query(
            r#"
            UPDATE data_entries
            SET inline_data = NULL
            WHERE hash = $1
            "#,
        )
        .bind(hash)
        .execute(&self.pool)
        .await?;

        // Verify one row was affected
        if result.rows_affected() != 1 {
            bail!("Not found")
        }

        Ok(())
    }

    async fn add_s3_path(&mut self, hash: &str, path: String) -> Result<()> {
        self.append_to_array(hash, "s3_path", path, "TEXT").await
    }

    async fn add_local_path(&mut self, hash: &str, path: String) -> Result<()> {
        self.append_to_array(hash, "local_path", path, "TEXT").await
    }

    async fn add_device(&mut self, hash: &str, device_id: DeviceId) -> Result<()> {
        self.append_to_array(hash, "devices", device_id, "BYTEA")
            .await
    }

    async fn remove_s3_path(&mut self, hash: &str, path: &str) -> Result<()> {
        self.remove_from_array(hash, "s3_path", path, "TEXT").await
    }

    async fn remove_local_path(&mut self, hash: &str, path: &str) -> Result<()> {
        self.remove_from_array(hash, "local_path", path, "TEXT")
            .await
    }

    async fn remove_device(&mut self, hash: &str, device_id: DeviceId) -> Result<()> {
        self.remove_from_array(hash, "devices", device_id, "BYTEA")
            .await
    }
}

impl PostgresDataTable {
    /// Append to an array in the DB
    async fn append_to_array<T>(
        &mut self,
        hash: &str,
        column: &str,
        value: T,
        array_type: &str,
    ) -> Result<()>
    where
        T: sqlx::Type<sqlx::Postgres>,
        for<'r> &'r T: sqlx::Encode<'r, sqlx::Postgres>,
        T: Send,
    {
        let query = format!(
            r#"
            UPDATE data_entries
            SET {column} = array_append(
                CASE
                    WHEN $2 = ANY(COALESCE({column}, ARRAY[]::{array_type}[]))
                    THEN {column}
                    ELSE COALESCE({column}, ARRAY[]::{array_type}[])
                END,
                $2
            )
            WHERE hash = $1
            AND NOT ($2 = ANY(COALESCE({column}, ARRAY[]::{array_type}[])))
            "#
        );

        let result = sqlx::query(&query)
            .bind(hash)
            .bind(&value)
            .execute(&self.pool)
            .await?;

        // If no rows were affected, check if the entry exists
        if result.rows_affected() == 0 {
            let exists = sqlx::query("SELECT 1 FROM data_entries WHERE hash = $1")
                .bind(hash)
                .fetch_optional(&self.pool)
                .await?;

            if exists.is_none() {
                bail!("Not found")
            }
        }

        Ok(())
    }

    /// Remove from an array in the DB
    async fn remove_from_array<T>(
        &mut self,
        hash: &str,
        column: &str,
        value: T,
        array_type: &str,
    ) -> Result<()>
    where
        T: sqlx::Type<sqlx::Postgres>,
        for<'r> &'r T: sqlx::Encode<'r, sqlx::Postgres>,
        T: Send,
    {
        let query = format!(
            r#"
            UPDATE data_entries
            SET {column} = array_remove(COALESCE({column}, ARRAY[]::{array_type}[]), $2)
            WHERE hash = $1
            "#
        );

        let result = sqlx::query(&query)
            .bind(hash)
            .bind(&value)
            .execute(&self.pool)
            .await?;

        // Verify one row was affected
        if result.rows_affected() != 1 {
            bail!("Not found")
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::{generate_hash, generate_key};

    fn generate_test_device_id() -> DeviceId {
        generate_key().verifying_key().to_bytes()
    }

    #[sqlx::test]
    async fn test_create_entry(pool: PgPool) {
        let mut table = PostgresDataTable::from_pool(pool).await.unwrap();

        let entry = DataEntry {
            hash: generate_hash("test_data".as_bytes()).unwrap(),
            ref_count: 1,
            inline_data: Some(b"test data".to_vec()),
            devices: vec![generate_test_device_id()],
            local_path: vec!["local/path/to/file".to_string()],
            s3_path: vec!["s3/path/to/file".to_string()],
        };

        // Test successful creation
        assert!(table.create_entry(entry.clone()).await.is_ok());

        // Test duplicate entry
        match table.create_entry(entry).await {
            Err(_) => (),
            other => panic!("Expected error, got {:?}", other),
        }
    }

    #[sqlx::test]
    async fn test_get_entry(pool: PgPool) {
        let mut table = PostgresDataTable::from_pool(pool).await.unwrap();

        let device_id = generate_test_device_id();
        let hash = generate_hash("test_data".as_bytes()).unwrap();

        let original_entry = DataEntry {
            hash: hash.clone(),
            ref_count: 1,
            inline_data: Some(b"test data".to_vec()),
            devices: vec![device_id],
            local_path: vec!["local/path/to/file".to_string()],
            s3_path: vec!["s3/path/to/file".to_string()],
        };

        // Insert the entry
        table.create_entry(original_entry.clone()).await.unwrap();

        // Test retrieving the entry
        let retrieved_entry = table.get_entry(&hash).await.unwrap();

        // Verify we got an entry back
        assert!(retrieved_entry.is_some());
        let retrieved_entry = retrieved_entry.unwrap();

        // Verify they match
        assert_eq!(retrieved_entry, original_entry);

        // Test getting a non-existent entry
        let non_existent = table
            .get_entry(&generate_hash("non_existent".as_bytes()).unwrap())
            .await
            .unwrap();
        assert!(non_existent.is_none());
    }

    #[sqlx::test]
    async fn test_get_or_insert_entry(pool: PgPool) {
        let mut table = PostgresDataTable::from_pool(pool).await.unwrap();
        let hash = generate_hash("test_data".as_bytes()).unwrap();

        // Test inserting a new entry
        let entry = table.get_or_insert_entry(&hash).await.unwrap();

        // Verify the newly inserted entry has the correct hash and empty fields
        assert_eq!(entry.hash, hash);
        assert_eq!(entry.ref_count, 0);
        assert!(entry.inline_data.is_none());
        assert!(entry.devices.is_empty());
        assert!(entry.local_path.is_empty());
        assert!(entry.s3_path.is_empty());

        // Verify that's the same as "new"
        assert_eq!(entry, DataEntry::new(&hash));

        // Test getting an existing entry (should return same entry)
        let existing_entry = table.get_or_insert_entry(&hash).await.unwrap();

        // Verify we got back the same entry
        assert_eq!(existing_entry.hash, entry.hash);
        assert_eq!(existing_entry.ref_count, entry.ref_count);
        assert_eq!(existing_entry.inline_data, entry.inline_data);
        assert_eq!(existing_entry.devices, entry.devices);
        assert_eq!(existing_entry.local_path, entry.local_path);
        assert_eq!(existing_entry.s3_path, entry.s3_path);
    }

    #[sqlx::test]
    async fn test_add_inline_data(pool: PgPool) {
        let mut table = PostgresDataTable::from_pool(pool).await.unwrap();
        let hash = generate_hash("test_data".as_bytes()).unwrap();

        // First create an entry without inline data
        let entry = DataEntry::new(&hash);
        table.create_entry(entry).await.unwrap();

        // Add inline data
        let data = b"test data".to_vec();
        assert!(table.add_inline_data(&hash, data.clone()).await.is_ok());

        // Verify the data was added
        let updated_entry = table.get_entry(&hash).await.unwrap().unwrap();
        assert_eq!(updated_entry.inline_data, Some(data));

        // Test adding to non-existent entry
        let non_existent = generate_hash("non_existent".as_bytes()).unwrap();
        match table.add_inline_data(&non_existent, vec![1, 2, 3]).await {
            Err(_) => (),
            other => panic!("Expected error, got {:?}", other),
        }
    }

    #[sqlx::test]
    async fn test_add_paths_and_device(pool: PgPool) {
        let mut table = PostgresDataTable::from_pool(pool).await.unwrap();
        let hash = generate_hash("test_data".as_bytes()).unwrap();

        // Create an empty entry
        let entry = DataEntry::new(&hash);
        table.create_entry(entry).await.unwrap();

        // Test adding S3 path
        let s3_path = "s3://bucket/test.dat".to_string();
        assert!(table.add_s3_path(&hash, s3_path.clone()).await.is_ok());

        // Test adding local path
        let local_path = "/tmp/test.dat".to_string();
        assert!(table
            .add_local_path(&hash, local_path.clone())
            .await
            .is_ok());

        // Test adding device
        let device_id = generate_test_device_id();
        assert!(table.add_device(&hash, device_id).await.is_ok());

        // Verify all additions
        let updated_entry = table.get_entry(&hash).await.unwrap().unwrap();
        assert!(updated_entry.s3_path.contains(&s3_path));
        assert!(updated_entry.local_path.contains(&local_path));
        assert!(updated_entry.devices.contains(&device_id));

        // Test adding to non-existent entry
        let non_existent = generate_hash("non_existent".as_bytes()).unwrap();
        match table
            .add_s3_path(&non_existent, "s3://test".to_string())
            .await
        {
            Err(_) => (),
            other => panic!("Expected error, got {:?}", other),
        }

        match table
            .add_local_path(&non_existent, "/tmp/test".to_string())
            .await
        {
            Err(_) => (),
            other => panic!("Expected error, got {:?}", other),
        }

        match table
            .add_device(&non_existent, generate_test_device_id())
            .await
        {
            Err(_) => (),
            other => panic!("Expected error, got {:?}", other),
        }
    }

    #[sqlx::test]
    async fn test_duplicate_additions(pool: PgPool) {
        let mut table = PostgresDataTable::from_pool(pool).await.unwrap();
        let hash = generate_hash("test_data".as_bytes()).unwrap();

        // Create an empty entry
        let entry = DataEntry::new(&hash);
        table.create_entry(entry).await.unwrap();

        // Test duplicate additions (should be idempotent)
        let s3_path = "s3://bucket/test.dat".to_string();
        assert!(table.add_s3_path(&hash, s3_path.clone()).await.is_ok());
        assert!(table.add_s3_path(&hash, s3_path.clone()).await.is_ok());

        let device_id = generate_test_device_id();
        assert!(table.add_device(&hash, device_id).await.is_ok());
        assert!(table.add_device(&hash, device_id).await.is_ok());

        // Verify no duplicates in arrays
        let entry = table.get_entry(&hash).await.unwrap().unwrap();
        assert_eq!(entry.s3_path.len(), 1);
        assert_eq!(entry.devices.len(), 1);
    }

    #[sqlx::test]
    async fn test_remove_inline_data(pool: PgPool) {
        let mut table = PostgresDataTable::from_pool(pool).await.unwrap();
        let hash = generate_hash("test_data".as_bytes()).unwrap();

        // Create an entry with inline data
        let entry = DataEntry {
            hash: hash.clone(),
            ref_count: 1,
            inline_data: Some(b"test data".to_vec()),
            devices: vec![],
            local_path: vec![],
            s3_path: vec![],
        };
        table.create_entry(entry).await.unwrap();

        // Remove inline data
        assert!(table.remove_inline_data(&hash).await.is_ok());

        // Verify data was removed
        let updated_entry = table.get_entry(&hash).await.unwrap().unwrap();
        assert!(updated_entry.inline_data.is_none());

        // Test removing from non-existent entry
        let non_existent = generate_hash("non_existent".as_bytes()).unwrap();
        match table.remove_inline_data(&non_existent).await {
            Err(_) => (),
            other => panic!("Expected error, got {:?}", other),
        }

        // Test removing when already None
        match table.remove_inline_data(&hash).await {
            Ok(()) => (), // Should succeed even if already None
            other => panic!("Expected Ok(()), got {:?}", other),
        }
    }

    #[sqlx::test]
    async fn test_remove_paths_and_device(pool: PgPool) {
        let mut table = PostgresDataTable::from_pool(pool).await.unwrap();
        let hash = generate_hash("test_data".as_bytes()).unwrap();
        let device_id = generate_test_device_id();

        // Create an entry with paths and device
        let entry = DataEntry {
            hash: hash.clone(),
            ref_count: 1,
            inline_data: None,
            devices: vec![device_id],
            local_path: vec!["local/path/test.dat".to_string()],
            s3_path: vec!["s3://bucket/test.dat".to_string()],
        };
        table.create_entry(entry).await.unwrap();

        // Test removing paths
        assert!(table
            .remove_local_path(&hash, "local/path/test.dat")
            .await
            .is_ok());
        assert!(table
            .remove_s3_path(&hash, "s3://bucket/test.dat")
            .await
            .is_ok());

        // Test removing device
        assert!(table.remove_device(&hash, device_id).await.is_ok());

        // Verify all removals
        let updated_entry = table.get_entry(&hash).await.unwrap().unwrap();
        assert!(updated_entry.local_path.is_empty());
        assert!(updated_entry.s3_path.is_empty());
        assert!(updated_entry.devices.is_empty());

        // Test removing non-existent values
        match table.remove_local_path(&hash, "non/existent/path").await {
            Ok(()) => (), // Should succeed even if path doesn't exist
            other => panic!("Expected Ok(()), got {:?}", other),
        }

        match table.remove_s3_path(&hash, "s3://non/existent").await {
            Ok(()) => (), // Should succeed even if path doesn't exist
            other => panic!("Expected Ok(()), got {:?}", other),
        }

        match table.remove_device(&hash, generate_test_device_id()).await {
            Ok(()) => (), // Should succeed even if device doesn't exist
            other => panic!("Expected Ok(()), got {:?}", other),
        }

        // Test removing from non-existent entry
        let non_existent = generate_hash("non_existent".as_bytes()).unwrap();
        match table.remove_local_path(&non_existent, "test").await {
            Err(_) => (),
            other => panic!("Expected error, got {:?}", other),
        }
    }

    #[sqlx::test]
    async fn test_multiple_removals(pool: PgPool) {
        let mut table = PostgresDataTable::from_pool(pool).await.unwrap();
        let hash = generate_hash("test_data".as_bytes()).unwrap();

        // Create an entry with multiple paths
        let entry = DataEntry {
            hash: hash.clone(),
            ref_count: 1,
            inline_data: None,
            devices: vec![],
            local_path: vec![
                "path1.dat".to_string(),
                "path2.dat".to_string(),
                "path3.dat".to_string(),
            ],
            s3_path: vec![],
        };
        table.create_entry(entry).await.unwrap();

        // Remove paths in sequence
        assert!(table.remove_local_path(&hash, "path1.dat").await.is_ok());
        let entry1 = table.get_entry(&hash).await.unwrap().unwrap();
        assert_eq!(entry1.local_path.len(), 2);

        assert!(table.remove_local_path(&hash, "path3.dat").await.is_ok());
        let entry2 = table.get_entry(&hash).await.unwrap().unwrap();
        assert_eq!(entry2.local_path.len(), 1);

        // Removing non-existent path is fine and does nothing
        assert!(table.remove_local_path(&hash, "path1.dat").await.is_ok());
        let entry1 = table.get_entry(&hash).await.unwrap().unwrap();
        assert_eq!(entry1.local_path.len(), 1);

        assert!(table.remove_local_path(&hash, "path2.dat").await.is_ok());
        let entry3 = table.get_entry(&hash).await.unwrap().unwrap();
        assert!(entry3.local_path.is_empty());
    }

    #[sqlx::test]
    async fn test_ref_count_operations(pool: PgPool) -> Result<()> {
        let mut table = PostgresDataTable::from_pool(pool.clone()).await?;
        let test_hash = "b3_test_hash_ref_count";

        // Insert initial test entry
        let entry = DataEntry {
            hash: test_hash.to_string(),
            ref_count: 0,
            inline_data: None,
            devices: vec![],
            local_path: vec![],
            s3_path: vec![],
        };
        table.create_entry(entry).await?;

        // Test increasing ref_count
        let count = table.increase_ref_count(test_hash).await?;
        assert_eq!(count, 1, "First increase should result in ref_count = 1");

        let count = table.increase_ref_count(test_hash).await?;
        assert_eq!(count, 2, "Second increase should result in ref_count = 2");

        // Test decreasing ref_count
        let count = table.decrease_ref_count(test_hash).await?;
        assert_eq!(count, 1, "First decrease should result in ref_count = 1");

        let count = table.decrease_ref_count(test_hash).await?;
        assert_eq!(count, 0, "Second decrease should result in ref_count = 0");

        // Test that ref_count doesn't go below 0
        let count = table.decrease_ref_count(test_hash).await?;
        assert_eq!(count, 0, "ref_count should not go below 0");

        // Test operations on non-existent hash
        let non_existent = "bg_nonexistent_hash";
        assert!(table.increase_ref_count(non_existent).await.is_err());
        assert!(table.decrease_ref_count(non_existent).await.is_err());

        // Test concurrent operations
        let mut handles = vec![];
        for _ in 0..5 {
            let pool = pool.clone();
            let hash = test_hash.to_string();
            handles.push(tokio::spawn(async move {
                let mut table = PostgresDataTable::from_pool(pool).await.unwrap();
                let inc = table.increase_ref_count(&hash).await.unwrap();
                let dec = table.decrease_ref_count(&hash).await.unwrap();
                (inc, dec)
            }));
        }

        // Wait for all operations to complete and verify counts
        for handle in handles {
            let (inc, dec) = handle.await.unwrap();
            assert!(inc > 0, "Increased count should be positive");
            assert!(dec >= 0, "Decreased count should not be negative");
        }

        // Verify final count is 0
        let final_count = table.decrease_ref_count(test_hash).await?;
        assert_eq!(final_count, 0, "Final ref_count should be 0");

        Ok(())
    }
}
