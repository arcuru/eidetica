use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Eidetica Database
///
/// This is the interface and implementation of the Metadata Table in the database.
/// We run on postgresql, and store the blob data as described in the design doc.

/// Represents a single entry in the metadata table
#[derive(Debug, Clone)]
pub struct MetadataEntry {
    /// UUIDv7 that serves as unique identifier across all devices
    pub id: Uuid,

    /// UUID identifying the device that created this entry
    pub device_id: Uuid,

    /// Whether this entry has been superseded by a newer version
    pub archived: bool,

    /// Optional reference to parent entry's UUID
    pub parent_id: Option<Uuid>,

    /// JSON metadata about the referenced data
    pub metadata: Value,

    /// The actual data or reference to it
    pub data: Option<Vec<u8>>,
}

/// Interface for interacting with the metadata table
#[allow(dead_code, async_fn_in_trait)]
pub trait MetadataTable {
    /// Create a new metadata entry
    async fn create_entry(&mut self, entry: MetadataEntry) -> Result<(), Error>;

    /// Retrieve an entry by its ID
    async fn get_entry(&self, id: Uuid) -> Result<Option<MetadataEntry>, Error>;

    /// Mark an entry as archived
    async fn archive_entry(&mut self, id: Uuid) -> Result<(), Error>;

    /// Get full history chain for an entry by following parent_ids
    async fn get_entry_history(&self, id: Uuid) -> Result<Vec<MetadataEntry>, Error>;

    /// Get all the children of an entry
    async fn get_child_entries(&self, id: Uuid) -> Result<Vec<MetadataEntry>, Error>;
}

/// PostgreSQL implementation of the metadata table
pub struct PostgresMetadataTable {
    pool: PgPool,
}

#[allow(dead_code)]
impl PostgresMetadataTable {
    /// Create a new PostgresMetadataTable instance
    pub async fn new(connection_string: &str) -> Result<Self, Error> {
        let pool = PgPool::connect(connection_string)
            .await
            .map_err(|e| Error::Database(Box::new(e)))?;

        // Ensure table exists
        Self::create_table(&pool).await?;

        Ok(Self { pool })
    }

    /// Create a new PostgresMetadataTable from an existing pool connection
    pub async fn from_pool(pool: PgPool) -> Result<Self, Error> {
        Self::create_table(&pool).await?;
        Ok(Self { pool })
    }

    /// Create the metadata table if it doesn't exist
    async fn create_table(pool: &PgPool) -> Result<(), Error> {
        // This command may fail if we're trying to run this in parallel (as in testing).
        // If postgres is already creating a table it throws an error.
        const MAX_RETRIES: u32 = 3;
        const RETRY_DELAY_MS: u64 = 500;

        let mut attempts = 0;
        let mut last_error = None;

        while attempts < MAX_RETRIES {
            match sqlx::query(
                r#"
            CREATE TABLE IF NOT EXISTS metadata_entries (
                id UUID PRIMARY KEY,
                device_id UUID NOT NULL,
                archived BOOLEAN NOT NULL DEFAULT FALSE,
                parent_id UUID,
                metadata JSONB NOT NULL,
                data BYTEA,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

                FOREIGN KEY (parent_id) REFERENCES metadata_entries(id)
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

        Err(Error::Database(Box::new(last_error.unwrap())))
    }
}

impl MetadataTable for PostgresMetadataTable {
    async fn create_entry(&mut self, entry: MetadataEntry) -> Result<(), Error> {
        // Convert the metadata to a sqlx::types::Json
        let metadata_json =
            serde_json::to_value(&entry.metadata).map_err(|_| Error::InvalidData)?;

        // Start a transaction since we might need to update two rows
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| Error::Database(Box::new(e)))?;

        // Insert the new entry
        let result = sqlx::query(
            r#"
        INSERT INTO metadata_entries
            (id, device_id, archived, parent_id, metadata, data)
        VALUES
            ($1, $2, $3, $4, $5, $6)
        "#,
        )
        .bind(entry.id)
        .bind(entry.device_id)
        .bind(entry.archived)
        .bind(entry.parent_id)
        .bind(metadata_json)
        .bind(entry.data.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(|e| Error::Database(Box::new(e)))?;

        // Verify one row was inserted
        if result.rows_affected() != 1 {
            return Err(Error::Database(Box::new(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Failed to insert metadata entry",
            ))));
        }

        // If there's a parent_id, archive it
        if let Some(parent_id) = entry.parent_id {
            sqlx::query("UPDATE metadata_entries SET archived = TRUE WHERE id = $1")
                .bind(parent_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| Error::Database(Box::new(e)))?;
        }

        // Commit the transaction
        tx.commit()
            .await
            .map_err(|e| Error::Database(Box::new(e)))?;

        Ok(())
    }

    async fn get_entry(&self, id: Uuid) -> Result<Option<MetadataEntry>, Error> {
        let row = sqlx::query(
            r#"
            SELECT
                id,
                device_id,
                archived,
                parent_id,
                metadata,
                data
            FROM metadata_entries
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| Error::Database(Box::new(e)))?;

        match row {
            Some(row) => {
                let entry = MetadataEntry {
                    id: row.get("id"),
                    device_id: row.get("device_id"),
                    archived: row.get("archived"),
                    parent_id: row.get("parent_id"),
                    metadata: row.get("metadata"),
                    data: row.get("data"),
                };
                Ok(Some(entry))
            }
            None => Ok(None),
        }
    }

    async fn archive_entry(&mut self, id: Uuid) -> Result<(), Error> {
        let result = sqlx::query("UPDATE metadata_entries SET archived = TRUE WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Database(Box::new(e)))?;

        if result.rows_affected() == 0 {
            return Err(Error::NotFound);
        }

        Ok(())
    }

    async fn get_entry_history(&self, id: Uuid) -> Result<Vec<MetadataEntry>, Error> {
        // Using a WITH RECURSIVE query to follow the parent_id chain
        let rows = sqlx::query(
            r#"
            WITH RECURSIVE history AS (
                -- Base case: start with the entry we want
                SELECT
                    id, device_id, archived, parent_id, metadata, data
                FROM metadata_entries
                WHERE id = $1

                UNION ALL

                -- Recursive case: join with parent entries
                SELECT
                    e.id, e.device_id, e.archived, e.parent_id, e.metadata, e.data
                FROM metadata_entries e
                INNER JOIN history h ON h.parent_id = e.id
            )
            SELECT * FROM history
            ORDER BY id DESC
            "#,
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Error::Database(Box::new(e)))?;

        if rows.is_empty() {
            return Err(Error::NotFound);
        }

        let entries = rows
            .into_iter()
            .map(|row| MetadataEntry {
                id: row.get("id"),
                device_id: row.get("device_id"),
                archived: row.get("archived"),
                parent_id: row.get("parent_id"),
                metadata: row.get("metadata"),
                data: row.get("data"),
            })
            .collect();

        Ok(entries)
    }

    async fn get_child_entries(&self, id: Uuid) -> Result<Vec<MetadataEntry>, Error> {
        let rows = sqlx::query(
            r#"
        SELECT 
            id, device_id, archived, parent_id, metadata, data
        FROM metadata_entries
        WHERE parent_id = $1
        ORDER BY id DESC
        "#,
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| Error::Database(Box::new(e)))?;

        let entries = rows
            .into_iter()
            .map(|row| MetadataEntry {
                id: row.get("id"),
                device_id: row.get("device_id"),
                archived: row.get("archived"),
                parent_id: row.get("parent_id"),
                metadata: row.get("metadata"),
                data: row.get("data"),
            })
            .collect();

        Ok(entries)
    }
}

/// Error types that can occur during database operations
#[derive(Debug)]
pub enum Error {
    /// Database connection/query errors
    #[allow(dead_code)]
    Database(Box<dyn std::error::Error>),

    /// Entry not found
    NotFound,

    /// Invalid data format
    InvalidData,
    // Permission denied
    //PermissionDenied,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test]
    async fn test_create_entry(pool: PgPool) {
        let mut table = PostgresMetadataTable::from_pool(pool).await.unwrap();

        let entry = MetadataEntry {
            id: Uuid::now_v7(),
            device_id: Uuid::new_v4(),
            archived: false,
            parent_id: None,
            metadata: serde_json::json!({
                "type": "test",
                "name": "test_entry"
            }),
            data: Some(vec![1, 2, 3, 4]),
        };

        assert!(table.create_entry(entry).await.is_ok());
    }

    #[sqlx::test]
    async fn test_create_entry_archives_parent(pool: PgPool) {
        let mut table = PostgresMetadataTable::from_pool(pool).await.unwrap();

        // Create a parent entry
        let parent_entry = MetadataEntry {
            id: Uuid::now_v7(),
            device_id: Uuid::new_v4(),
            archived: false,
            parent_id: None,
            metadata: serde_json::json!({
                "type": "test",
                "name": "parent_entry"
            }),
            data: Some(vec![1, 2, 3, 4]),
        };

        // Insert the parent entry
        table.create_entry(parent_entry.clone()).await.unwrap();

        // Create a child entry
        let child_entry = MetadataEntry {
            id: Uuid::now_v7(),
            device_id: Uuid::new_v4(),
            archived: false,
            parent_id: Some(parent_entry.id),
            metadata: serde_json::json!({
                "type": "test",
                "name": "child_entry"
            }),
            data: Some(vec![5, 6, 7, 8]),
        };

        // Insert the child entry
        table.create_entry(child_entry).await.unwrap();

        // Verify the parent entry is now archived
        let updated_parent = table.get_entry(parent_entry.id).await.unwrap().unwrap();
        assert!(updated_parent.archived);
    }

    #[sqlx::test]
    async fn test_get_entry(pool: PgPool) {
        let mut table = PostgresMetadataTable::from_pool(pool).await.unwrap();

        // Create a test entrySelf
        let original_entry = MetadataEntry {
            id: Uuid::now_v7(),
            device_id: Uuid::new_v4(),
            archived: false,
            parent_id: None,
            metadata: serde_json::json!({
                "type": "test",
                "name": "test_entry"
            }),
            data: Some(vec![1, 2, 3, 4]),
        };

        // Insert the entry
        table.create_entry(original_entry.clone()).await.unwrap();

        // Retrieve the entry
        let retrieved_entry = table.get_entry(original_entry.id).await.unwrap();

        // Verify we got an entry back
        assert!(retrieved_entry.is_some());
        let retrieved_entry = retrieved_entry.unwrap();

        // Verify the fields match
        assert_eq!(retrieved_entry.id, original_entry.id);
        assert_eq!(retrieved_entry.device_id, original_entry.device_id);
        assert_eq!(retrieved_entry.archived, original_entry.archived);
        assert_eq!(retrieved_entry.parent_id, original_entry.parent_id);
        assert_eq!(retrieved_entry.metadata, original_entry.metadata);
        assert_eq!(retrieved_entry.data, original_entry.data);

        // Test getting a non-existent entry
        let non_existent = table.get_entry(Uuid::new_v4()).await.unwrap();
        assert!(non_existent.is_none());
    }

    #[sqlx::test]
    async fn test_archive_entry(pool: PgPool) {
        let mut table = PostgresMetadataTable::from_pool(pool).await.unwrap();

        // Create a test entry
        let entry = MetadataEntry {
            id: Uuid::now_v7(),
            device_id: Uuid::new_v4(),
            archived: false,
            parent_id: None,
            metadata: serde_json::json!({"test": "data"}),
            data: Some(vec![1, 2, 3, 4]),
        };

        // Insert the entry
        table.create_entry(entry.clone()).await.unwrap();

        // Archive the entry
        table.archive_entry(entry.id).await.unwrap();

        // Verify the entry is now archived
        let archived_entry = table.get_entry(entry.id).await.unwrap().unwrap();
        assert!(archived_entry.archived);

        // Verify archiving an already archived entry succeeds
        assert!(table.archive_entry(entry.id).await.is_ok());

        // Try to archive a non-existent entry
        assert!(matches!(
            table.archive_entry(Uuid::new_v4()).await,
            Err(Error::NotFound)
        ));
    }

    #[sqlx::test]
    async fn test_get_entry_history(pool: PgPool) {
        let mut table = PostgresMetadataTable::from_pool(pool).await.unwrap();

        // Create a chain of entries
        let entry1 = MetadataEntry {
            id: Uuid::now_v7(),
            device_id: Uuid::new_v4(),
            archived: false,
            parent_id: None,
            metadata: serde_json::json!({"version": 1}),
            data: Some(vec![1]),
        };

        let entry2 = MetadataEntry {
            id: Uuid::now_v7(),
            device_id: Uuid::new_v4(),
            archived: false,
            parent_id: Some(entry1.id),
            metadata: serde_json::json!({"version": 2}),
            data: Some(vec![2]),
        };

        let entry3 = MetadataEntry {
            id: Uuid::now_v7(),
            device_id: Uuid::new_v4(),
            archived: false,
            parent_id: Some(entry2.id),
            metadata: serde_json::json!({"version": 3}),
            data: Some(vec![3]),
        };

        // Insert entries
        table.create_entry(entry1.clone()).await.unwrap();
        table.create_entry(entry2.clone()).await.unwrap();
        table.create_entry(entry3.clone()).await.unwrap();

        // Get history starting from entry3
        let history = table.get_entry_history(entry3.id).await.unwrap();

        // Verify history
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].id, entry3.id);
        assert_eq!(history[1].id, entry2.id);
        assert_eq!(history[2].id, entry1.id);

        // Verify metadata versions
        assert_eq!(history[0].metadata["version"], 3);
        assert_eq!(history[1].metadata["version"], 2);
        assert_eq!(history[2].metadata["version"], 1);

        // Test getting history for non-existent entry
        assert!(matches!(
            table.get_entry_history(Uuid::new_v4()).await,
            Err(Error::NotFound)
        ));
    }

    #[sqlx::test]
    async fn test_get_child_entries(pool: PgPool) {
        // Create table instance directly with the injected pool
        let mut table = PostgresMetadataTable::from_pool(pool).await.unwrap();

        // Create a parent entry
        let parent_id = Uuid::now_v7();
        let parent_entry = MetadataEntry {
            id: parent_id,
            device_id: Uuid::new_v4(),
            archived: false,
            parent_id: None,
            metadata: serde_json::json!({
                "type": "parent",
                "name": "parent_entry"
            }),
            data: None,
        };

        // Create child entries
        let child_entry1 = MetadataEntry {
            id: Uuid::now_v7(),
            device_id: Uuid::new_v4(),
            archived: false,
            parent_id: Some(parent_id),
            metadata: serde_json::json!({
                "type": "child",
                "name": "child_entry1"
            }),
            data: Some(vec![1, 2, 3]),
        };

        let child_entry2 = MetadataEntry {
            id: Uuid::now_v7(),
            device_id: Uuid::new_v4(),
            archived: false,
            parent_id: Some(parent_id),
            metadata: serde_json::json!({
                "type": "child",
                "name": "child_entry2"
            }),
            data: Some(vec![4, 5, 6]),
        };

        // Insert the entries
        assert!(table.create_entry(parent_entry).await.is_ok());
        assert!(table.create_entry(child_entry1.clone()).await.is_ok());
        assert!(table.create_entry(child_entry2.clone()).await.is_ok());

        // Test getting child entries
        let children = table.get_child_entries(parent_id).await.unwrap();
        assert_eq!(children.len(), 2);

        // Verify the children are the ones we created
        let child_ids: Vec<Uuid> = children.iter().map(|c| c.id).collect();
        assert!(child_ids.contains(&child_entry1.id));
        assert!(child_ids.contains(&child_entry2.id));

        // Test getting children of an entry with no children
        let no_children = table.get_child_entries(Uuid::new_v4()).await.unwrap();
        assert!(no_children.is_empty());
    }
}
