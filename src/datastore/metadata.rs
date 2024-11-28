use crate::datastore::schema::MetadataEntry;
use anyhow::Result;
use serde_json::Value;
use sqlx::{Error, PgPool, Row};
use uuid::Uuid;

/// Eidetica Database
///
/// This is the interface and implementation of the Metadata Table in the database.
/// We run on postgresql, and store the blob data as described in the design doc.

const METADATA_TABLE: &str = "metadata_table";

/// Interface for interacting with the metadata table
#[allow(dead_code, async_fn_in_trait)]
pub trait MetadataTable {
    /// Create a new metadata table with the given name if it doesn't exist
    async fn create_table(&mut self) -> Result<()>;

    /// Create a new metadata entry
    async fn create_entry(&mut self, entry: MetadataEntry) -> Result<()>;

    /// Retrieve an entry by its ID
    async fn get_entry(&self, id: Uuid) -> Result<Option<MetadataEntry>>;

    // Set whether an entry's data should be kept locally
    async fn set_local(&mut self, id: Uuid, local: bool) -> Result<()>;

    /// Mark an entry as archived
    /// FIXME: ...doesn't work with stream views
    async fn archive_entry(&mut self, id: Uuid) -> Result<()>;

    /// Get full history chain for an entry by following parent_ids
    async fn get_entry_history(&self, streams: &[i64], id: Uuid) -> Result<Vec<MetadataEntry>>;

    /// Get all the children of an entry
    async fn get_child_entries(&self, streams: &[i64], id: Uuid) -> Result<Vec<MetadataEntry>>;

    /// Get all the entries that are not archived
    ///
    /// Be advised that this will return _all_ active entries and may be expensive on large databases.
    async fn get_active_entries(&self, streams: &[i64]) -> Result<Vec<MetadataEntry>>;

    /// Get all the archived entries
    async fn get_archived_entries(&self, streams: &[i64]) -> Result<Vec<MetadataEntry>>;

    /// Get entries by 1 or more metadata conditions
    async fn get_entries_by_metadata_conditions(
        &self,
        streams: &[i64],
        conditions: &Value,
        include_archived: bool,
    ) -> Result<Vec<MetadataEntry>>;
}

impl From<sqlx::postgres::PgRow> for MetadataEntry {
    fn from(row: sqlx::postgres::PgRow) -> Self {
        Self {
            id: row.get("id"),
            stream: row.get("stream"),
            archived: row.get("archived"),
            local: row.get("local"),
            parent_id: row.get("parent_id"),
            metadata: row.get("metadata"),
            data_hash: row.get("data_hash"),
        }
    }
}

/// PostgreSQL implementation of the metadata table
pub struct PostgresMetadataTable {
    pub pool: PgPool,
}

#[allow(dead_code)]
impl PostgresMetadataTable {
    /// Create a new PostgresMetadataTable instance
    pub async fn new(connection_string: &str) -> Result<Self> {
        let pool = PgPool::connect(connection_string).await?;

        let mut table = Self { pool };
        table.create_table().await?;
        Ok(table)
    }

    /// Create a new PostgresMetadataTable from an existing pool connection
    pub async fn from_pool(pool: PgPool) -> Result<Self> {
        let mut table = Self { pool };
        table.create_table().await?;
        Ok(table)
    }
}

impl MetadataTable for PostgresMetadataTable {
    async fn create_entry(&mut self, entry: MetadataEntry) -> Result<()> {
        // Convert the metadata to a sqlx::types::Json
        let metadata_json = serde_json::to_value(&entry.metadata)?;

        // Start a transaction since we might need to update two rows
        let mut tx = self.pool.begin().await?;

        // Insert the new entry using the table name from the struct
        let query = format!(
            r#"
            INSERT INTO {}
            (id, stream, archived, local, parent_id, metadata, data_hash)
        VALUES
            ($1, $2, $3, $4, $5, $6, $7)
        "#,
            METADATA_TABLE
        );

        let result = sqlx::query(&query)
            .bind(entry.id)
            .bind(entry.stream)
            .bind(entry.archived)
            .bind(entry.local)
            .bind(entry.parent_id)
            .bind(metadata_json)
            .bind(entry.data_hash)
            .execute(&mut *tx)
            .await?;

        // Verify one row was inserted
        if result.rows_affected() != 1 {
            return Err(Error::RowNotFound.into());
        }

        // If there's a parent_id, archive it
        if let Some(parent_id) = entry.parent_id {
            let update_query = format!(
                "UPDATE {} SET archived = TRUE WHERE id = $1",
                METADATA_TABLE
            );
            sqlx::query(&update_query)
                .bind(parent_id)
                .execute(&mut *tx)
                .await?;
        }

        // Commit the transaction
        tx.commit().await?;

        Ok(())
    }

    /// Create the metadata table if it doesn't exist
    async fn create_table(&mut self) -> Result<()> {
        // This command may fail if we're trying to run this in parallel (as in testing).
        // If postgres is already creating a table it throws an error.
        const MAX_RETRIES: u32 = 3;
        const RETRY_DELAY_MS: u64 = 500;

        let mut attempts = 0;
        let mut last_error = None;

        let query = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {} (
                id UUID PRIMARY KEY,
                stream BIGINT NOT NULL,
                archived BOOLEAN NOT NULL DEFAULT FALSE,
                local BOOLEAN NOT NULL DEFAULT FALSE,
                parent_id UUID,
                metadata JSONB NOT NULL,
                data_hash CHAR(67),
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

                FOREIGN KEY (parent_id) REFERENCES {}(id)
            );"#,
            METADATA_TABLE, METADATA_TABLE
        );

        while attempts < MAX_RETRIES {
            match sqlx::query(&query).execute(&self.pool).await {
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

    async fn get_entry(&self, id: Uuid) -> Result<Option<MetadataEntry>> {
        let query = format!(
            r#"
            SELECT
                id,
                stream,
                archived,
                local,
                parent_id,
                metadata,
                data_hash
            FROM {}
            WHERE id = $1
            "#,
            METADATA_TABLE
        );

        let row = sqlx::query(&query)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        match row {
            Some(row) => Ok(Some(MetadataEntry::from(row))),
            None => Ok(None),
        }
    }

    async fn archive_entry(&mut self, id: Uuid) -> Result<()> {
        let query = format!(
            "UPDATE {} SET archived = TRUE WHERE id = $1",
            METADATA_TABLE
        );

        let result = sqlx::query(&query).bind(id).execute(&self.pool).await?;

        if result.rows_affected() == 0 {
            return Err(Error::RowNotFound.into());
        }

        Ok(())
    }

    async fn set_local(&mut self, id: Uuid, local: bool) -> Result<()> {
        let query = format!("UPDATE {} SET local = $1 WHERE id = $2", METADATA_TABLE);

        let result = sqlx::query(&query)
            .bind(local)
            .bind(id)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            return Err(Error::RowNotFound.into());
        }

        Ok(())
    }

    async fn get_entry_history(&self, streams: &[i64], id: Uuid) -> Result<Vec<MetadataEntry>> {
        // Using a WITH RECURSIVE query to follow the parent_id chain
        let query = format!(
            r#"
            WITH RECURSIVE history AS (
                -- Base case: start with the entry we want
                SELECT
                    id, stream, archived, local, parent_id, metadata, data_hash
                FROM {}
                WHERE id = $1 AND stream = ANY($2)

                UNION ALL

                -- Recursive case: join with parent entries
                SELECT
                    e.id, e.stream, e.archived, e.local, e.parent_id, e.metadata, e.data_hash
                FROM {} e
                INNER JOIN history h ON h.parent_id = e.id
                WHERE e.stream = ANY($2) 
            )
            SELECT * FROM history
            ORDER BY id DESC
            "#,
            METADATA_TABLE, METADATA_TABLE
        );

        let rows = sqlx::query(&query)
            .bind(id)
            .bind(streams)
            .fetch_all(&self.pool)
            .await?;

        if rows.is_empty() {
            return Err(Error::RowNotFound.into());
        }

        let entries = rows.into_iter().map(MetadataEntry::from).collect();

        Ok(entries)
    }

    async fn get_child_entries(&self, streams: &[i64], id: Uuid) -> Result<Vec<MetadataEntry>> {
        let query = format!(
            r#"
            SELECT
            id, stream, archived, local, parent_id, metadata, data_hash
            FROM {}
            WHERE parent_id = $1 
            AND stream = ANY($2)
            ORDER BY id DESC
            "#,
            METADATA_TABLE
        );

        let rows = sqlx::query(&query)
            .bind(id)
            .bind(streams)
            .fetch_all(&self.pool)
            .await?;

        let entries = rows.into_iter().map(MetadataEntry::from).collect();

        Ok(entries)
    }

    async fn get_active_entries(&self, streams: &[i64]) -> Result<Vec<MetadataEntry>> {
        let query = format!(
            r#"
        SELECT
            id, stream, archived, local, parent_id, metadata, data_hash
        FROM {}
        WHERE archived = FALSE
        AND stream = ANY($1)
        ORDER BY id DESC
        "#,
            METADATA_TABLE
        );

        let rows = sqlx::query(&query)
            .bind(streams)
            .fetch_all(&self.pool)
            .await?;

        let entries = rows.into_iter().map(MetadataEntry::from).collect();

        Ok(entries)
    }

    async fn get_archived_entries(&self, streams: &[i64]) -> Result<Vec<MetadataEntry>> {
        let query = format!(
            r#"
        SELECT
            id, stream, archived, local, parent_id, metadata, data_hash
        FROM {}
        WHERE archived = TRUE
        AND stream = ANY($1)
        ORDER BY id DESC
        "#,
            METADATA_TABLE
        );

        let rows = sqlx::query(&query)
            .bind(streams)
            .fetch_all(&self.pool)
            .await?;

        let entries = rows.into_iter().map(MetadataEntry::from).collect();

        Ok(entries)
    }

    /// Query entries by multiple metadata key-value pairs
    async fn get_entries_by_metadata_conditions(
        &self,
        streams: &[i64],
        conditions: &Value,
        include_archived: bool,
    ) -> Result<Vec<MetadataEntry>> {
        let archived_clause = if !include_archived {
            "AND archived = FALSE"
        } else {
            ""
        };

        // Build conditions for each key-value pair in the JSON object
        let mut condition_parts = Vec::new();
        let mut bind_values = Vec::new();

        if let Value::Object(map) = conditions {
            for (key, value) in map {
                match value {
                    Value::Number(_) => {
                        // Cast the parameter to JSONB for proper comparison
                        condition_parts.push(format!(
                            "metadata->'{}' = ${}::jsonb",
                            key,
                            bind_values.len() + 2
                        ));
                        bind_values.push(value.to_string());
                    }
                    Value::Object(obj) if obj.contains_key("$regex") => {
                        // Handle regex pattern matching
                        condition_parts.push(format!(
                            "metadata->>'{}' ~ ${}",
                            key,
                            bind_values.len() + 2
                        ));
                        bind_values.push(obj["$regex"].as_str().unwrap_or_default().to_string());
                    }
                    _ => {
                        // String comparisons remain the same
                        condition_parts.push(format!(
                            "metadata->>'{}' = ${}",
                            key,
                            bind_values.len() + 2
                        ));
                        bind_values.push(value.as_str().unwrap_or_default().to_string());
                    }
                }
            }
        }

        let query = format!(
            r#"
            SELECT
                id, stream, archived, local, parent_id, metadata, data_hash
            FROM {}
            WHERE {}
            AND stream = ANY($1)
            {}
            ORDER BY id DESC
            "#,
            METADATA_TABLE,
            condition_parts.join(" AND "),
            archived_clause
        );

        let mut query_builder = sqlx::query(&query);

        // Bind the stream list
        query_builder = query_builder.bind(streams);

        // Bind all values in order
        for value in bind_values {
            query_builder = query_builder.bind(value);
        }

        let rows = query_builder.fetch_all(&self.pool).await?;

        let entries = rows.into_iter().map(MetadataEntry::from).collect();

        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::generate_hash;

    fn generate_test_stream() -> i64 {
        rand::random()
    }

    #[sqlx::test]
    async fn test_create_entry(pool: PgPool) {
        let mut table = PostgresMetadataTable::from_pool(pool).await.unwrap();

        let entry = MetadataEntry {
            id: Uuid::now_v7(),
            stream: generate_test_stream(),
            archived: false,
            local: false,
            parent_id: None,
            metadata: serde_json::json!({
                "type": "test",
                "name": "test_entry"
            }),
            data_hash: generate_hash("entry".as_bytes()).unwrap(),
        };

        assert!(table.create_entry(entry).await.is_ok());
    }

    #[sqlx::test]
    async fn test_create_entry_archives_parent(pool: PgPool) {
        let mut table = PostgresMetadataTable::from_pool(pool).await.unwrap();

        let stream = generate_test_stream();

        // Create a parent entry
        let parent_entry = MetadataEntry {
            id: Uuid::now_v7(),
            stream,
            archived: false,
            local: false,
            parent_id: None,
            metadata: serde_json::json!({
                "type": "test",
                "name": "parent_entry"
            }),
            data_hash: generate_hash("parent_entry".as_bytes()).unwrap(),
        };

        // Insert the parent entry
        table.create_entry(parent_entry.clone()).await.unwrap();

        // Create a child entry
        let child_entry = MetadataEntry {
            id: Uuid::now_v7(),
            stream,
            archived: false,
            local: false,
            parent_id: Some(parent_entry.id),
            metadata: serde_json::json!({
                "type": "test",
                "name": "child_entry"
            }),
            data_hash: generate_hash("child_entry".as_bytes()).unwrap(),
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
            stream: generate_test_stream(),
            archived: false,
            local: false,
            parent_id: None,
            metadata: serde_json::json!({
                "type": "test",
                "name": "test_entry"
            }),
            data_hash: generate_hash("original_entry".as_bytes()).unwrap(),
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
        assert_eq!(retrieved_entry.stream, original_entry.stream);
        assert_eq!(retrieved_entry.archived, original_entry.archived);
        assert_eq!(retrieved_entry.local, original_entry.local);
        assert_eq!(retrieved_entry.parent_id, original_entry.parent_id);
        assert_eq!(retrieved_entry.metadata, original_entry.metadata);
        assert_eq!(retrieved_entry.data_hash, original_entry.data_hash);

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
            stream: generate_test_stream(),
            archived: false,
            local: false,
            parent_id: None,
            metadata: serde_json::json!({"test": "data"}),
            data_hash: generate_hash("entry".as_bytes()).unwrap(),
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
        assert!(table.archive_entry(Uuid::new_v4()).await.is_err());
    }

    #[sqlx::test]
    async fn test_set_local(pool: PgPool) {
        let mut table = PostgresMetadataTable::from_pool(pool).await.unwrap();

        // Create a test entry
        let entry = MetadataEntry {
            id: Uuid::now_v7(),
            stream: generate_test_stream(),
            archived: false,
            local: false,
            parent_id: None,
            metadata: serde_json::json!({"test": "data"}),
            data_hash: generate_hash("entry".as_bytes()).unwrap(),
        };

        // Insert the entry
        table.create_entry(entry.clone()).await.unwrap();

        // Set the entry to local
        assert!(table.set_local(entry.id, true).await.is_ok());

        // Verify the entry is now local
        let updated_entry = table.get_entry(entry.id).await.unwrap().unwrap();
        assert!(updated_entry.local);

        // Set it back to non-local
        assert!(table.set_local(entry.id, false).await.is_ok());

        // Verify the entry is no longer local
        let updated_entry = table.get_entry(entry.id).await.unwrap().unwrap();
        assert!(!updated_entry.local);

        // Try to set local on a non-existent entry
        assert!(table.set_local(Uuid::new_v4(), true).await.is_err());
    }

    #[sqlx::test]
    async fn test_get_entry_history(pool: PgPool) {
        let mut table = PostgresMetadataTable::from_pool(pool).await.unwrap();
        let stream = generate_test_stream();
        let streams = vec![stream];

        // Create a chain of entries
        let entry1 = MetadataEntry {
            id: Uuid::now_v7(),
            stream,
            archived: false,
            local: false,
            parent_id: None,
            metadata: serde_json::json!({"version": 1}),
            data_hash: generate_hash("entry1".as_bytes()).unwrap(),
        };

        let entry2 = MetadataEntry {
            id: Uuid::now_v7(),
            stream,
            archived: false,
            local: false,
            parent_id: Some(entry1.id),
            metadata: serde_json::json!({"version": 2}),
            data_hash: generate_hash("entry2".as_bytes()).unwrap(),
        };

        let entry3 = MetadataEntry {
            id: Uuid::now_v7(),
            stream,
            archived: false,
            local: false,
            parent_id: Some(entry2.id),
            metadata: serde_json::json!({"version": 3}),
            data_hash: generate_hash("entry3".as_bytes()).unwrap(),
        };

        // Insert entries
        table.create_entry(entry1.clone()).await.unwrap();
        table.create_entry(entry2.clone()).await.unwrap();
        table.create_entry(entry3.clone()).await.unwrap();

        // Get history starting from entry3
        let history = table.get_entry_history(&streams, entry3.id).await.unwrap();

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
        assert!(table
            .get_entry_history(&streams, Uuid::new_v4())
            .await
            .is_err());
    }

    #[sqlx::test]
    async fn test_get_child_entries(pool: PgPool) {
        // Create table instance directly with the injected pool
        let mut table = PostgresMetadataTable::from_pool(pool).await.unwrap();
        let stream = generate_test_stream();
        let streams = vec![stream];

        // Create a parent entry
        let parent_id = Uuid::now_v7();
        let parent_entry = MetadataEntry {
            id: parent_id,
            stream,
            archived: false,
            local: false,
            parent_id: None,
            metadata: serde_json::json!({
                "type": "parent",
                "name": "parent_entry"
            }),
            data_hash: generate_hash("parent_entry".as_bytes()).unwrap(),
        };

        // Create child entries
        let child_entry1 = MetadataEntry {
            id: Uuid::now_v7(),
            stream,
            archived: false,
            local: false,
            parent_id: Some(parent_id),
            metadata: serde_json::json!({
                "type": "child",
                "name": "child_entry1"
            }),
            data_hash: generate_hash("child_entry1".as_bytes()).unwrap(),
        };

        let child_entry2 = MetadataEntry {
            id: Uuid::now_v7(),
            stream,
            archived: false,
            local: false,
            parent_id: Some(parent_id),
            metadata: serde_json::json!({
                "type": "child",
                "name": "child_entry2"
            }),
            data_hash: generate_hash("child_entry2".as_bytes()).unwrap(),
        };

        // Insert the entries
        assert!(table.create_entry(parent_entry).await.is_ok());
        assert!(table.create_entry(child_entry1.clone()).await.is_ok());
        assert!(table.create_entry(child_entry2.clone()).await.is_ok());

        // Test getting child entries
        let children = table.get_child_entries(&streams, parent_id).await.unwrap();
        assert_eq!(children.len(), 2);

        // Verify the children are the ones we created
        let child_ids: Vec<Uuid> = children.iter().map(|c| c.id).collect();
        assert!(child_ids.contains(&child_entry1.id));
        assert!(child_ids.contains(&child_entry2.id));

        // Test getting children of an entry with no children
        let no_children = table
            .get_child_entries(&streams, Uuid::new_v4())
            .await
            .unwrap();
        assert!(no_children.is_empty());
    }

    #[sqlx::test]
    async fn test_get_active_entries(pool: PgPool) {
        let mut table = PostgresMetadataTable::from_pool(pool).await.unwrap();
        let stream = generate_test_stream();
        let streams = vec![stream];

        // Create some test entries, both archived and active
        let active_entry1 = MetadataEntry {
            id: Uuid::now_v7(),
            stream,
            archived: false,
            local: false,
            parent_id: None,
            metadata: serde_json::json!({
                "type": "active",
                "name": "active_entry1"
            }),
            data_hash: generate_hash("active_entry1".as_bytes()).unwrap(),
        };

        let active_entry2 = MetadataEntry {
            id: Uuid::now_v7(),
            stream,
            archived: false,
            local: false,
            parent_id: None,
            metadata: serde_json::json!({
                "type": "active",
                "name": "active_entry2"
            }),
            data_hash: generate_hash("active_entry2".as_bytes()).unwrap(),
        };

        let archived_entry = MetadataEntry {
            id: Uuid::now_v7(),
            stream,
            archived: true,
            local: false,
            parent_id: None,
            metadata: serde_json::json!({
                "type": "archived",
                "name": "archived_entry"
            }),
            data_hash: generate_hash("archived_entry".as_bytes()).unwrap(),
        };

        // Insert all entries
        table.create_entry(active_entry1.clone()).await.unwrap();
        table.create_entry(active_entry2.clone()).await.unwrap();
        table.create_entry(archived_entry.clone()).await.unwrap();

        // Get active entries
        let active_entries = table.get_active_entries(&streams).await.unwrap();

        // Verify we got the correct number of entries
        assert_eq!(active_entries.len(), 2);

        // Verify only active entries are returned
        let active_ids: Vec<Uuid> = active_entries.iter().map(|e| e.id).collect();
        assert!(active_ids.contains(&active_entry1.id));
        assert!(active_ids.contains(&active_entry2.id));
        assert!(!active_ids.contains(&archived_entry.id));

        // Verify all returned entries are not archived
        for entry in active_entries {
            assert!(!entry.archived);
        }
    }

    #[sqlx::test]
    async fn test_get_archived_entries(pool: PgPool) {
        let mut table = PostgresMetadataTable::from_pool(pool).await.unwrap();
        let stream = generate_test_stream();
        let streams = vec![stream];

        // Create some test entries, both archived and active
        let active_entry1 = MetadataEntry {
            id: Uuid::now_v7(),
            stream,
            archived: false,
            local: false,
            parent_id: None,
            metadata: serde_json::json!({
                "type": "active",
                "name": "active_entry1"
            }),
            data_hash: generate_hash("active_entry1".as_bytes()).unwrap(),
        };

        let active_entry2 = MetadataEntry {
            id: Uuid::now_v7(),
            stream,
            archived: false,
            local: false,
            parent_id: None,
            metadata: serde_json::json!({
                "type": "active",
                "name": "active_entry2"
            }),
            data_hash: generate_hash("active_entry2".as_bytes()).unwrap(),
        };

        let archived_entry = MetadataEntry {
            id: Uuid::now_v7(),
            stream,
            archived: true,
            local: false,
            parent_id: None,
            metadata: serde_json::json!({
                "type": "archived",
                "name": "archived_entry"
            }),
            data_hash: generate_hash("archived_entry".as_bytes()).unwrap(),
        };

        // Insert all entries
        table.create_entry(active_entry1.clone()).await.unwrap();
        table.create_entry(active_entry2.clone()).await.unwrap();
        table.create_entry(archived_entry.clone()).await.unwrap();

        // Get archived entries
        let archived_entries = table.get_archived_entries(&streams).await.unwrap();

        // Verify we got the correct number of entries
        assert_eq!(archived_entries.len(), 1);

        // Verify only archived entry is returned
        let archived_ids: Vec<Uuid> = archived_entries.iter().map(|e| e.id).collect();
        assert!(archived_ids.contains(&archived_entry.id));
        assert!(!archived_ids.contains(&active_entry1.id));
        assert!(!archived_ids.contains(&active_entry2.id));

        // Verify all returned entries are archived
        for entry in archived_entries {
            assert!(entry.archived);
        }
    }

    #[sqlx::test]
    async fn test_get_entries_by_metadata_conditions(pool: PgPool) {
        let mut table = PostgresMetadataTable::from_pool(pool).await.unwrap();
        let stream = generate_test_stream();
        let streams = vec![stream];

        // Create several test entries with different metadata
        let entry1 = MetadataEntry {
            id: Uuid::now_v7(),
            stream,
            archived: false,
            local: false,
            parent_id: None,
            metadata: serde_json::json!({
                "type": "document",
                "category": "important",
                "status": "active"
            }),
            data_hash: generate_hash("entry1".as_bytes()).unwrap(),
        };

        let entry2 = MetadataEntry {
            id: Uuid::now_v7(),
            stream,
            archived: false,
            local: false,
            parent_id: None,
            metadata: serde_json::json!({
                "type": "document",
                "category": "normal",
                "status": "active"
            }),
            data_hash: generate_hash("entry2".as_bytes()).unwrap(),
        };

        let entry3 = MetadataEntry {
            id: Uuid::now_v7(),
            stream,
            archived: true,
            local: false,
            parent_id: None,
            metadata: serde_json::json!({
                "type": "document",
                "category": "important",
                "status": "archived"
            }),
            data_hash: generate_hash("entry3".as_bytes()).unwrap(),
        };

        // Insert all entries
        table.create_entry(entry1.clone()).await.unwrap();
        table.create_entry(entry2.clone()).await.unwrap();
        table.create_entry(entry3.clone()).await.unwrap();

        // Test single condition query
        let conditions = serde_json::json!({
            "type": "document"
        });
        let results = table
            .get_entries_by_metadata_conditions(&streams, &conditions, true)
            .await
            .unwrap();
        assert_eq!(results.len(), 3);

        // Test multiple conditions
        let conditions = serde_json::json!({
            "type": "document",
            "category": "important"
        });
        let results = table
            .get_entries_by_metadata_conditions(&streams, &conditions, true)
            .await
            .unwrap();
        assert_eq!(results.len(), 2);

        let conditions = serde_json::json!({
            "category": "important"
        });
        let results = table
            .get_entries_by_metadata_conditions(&streams, &conditions, false)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, entry1.id);

        // Test condition that should return no results
        let conditions = serde_json::json!({
            "category": "nonexistent"
        });
        let results = table
            .get_entries_by_metadata_conditions(&streams, &conditions, true)
            .await
            .unwrap();
        assert!(results.is_empty());

        // Test multiple conditions that narrow down to one result
        let conditions = serde_json::json!({
            "category": "important",
            "status": "active"
        });
        let results = table
            .get_entries_by_metadata_conditions(&streams, &conditions, true)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, entry1.id);
    }

    #[sqlx::test]
    async fn test_create_table_duplicate(pool: PgPool) {
        // Test that creating the same table multiple times works
        let mut table1 = PostgresMetadataTable::from_pool(pool.clone())
            .await
            .unwrap();

        let mut table2 = PostgresMetadataTable::from_pool(pool).await.unwrap();

        // Both creations should succeed
        assert!(table1.create_table().await.is_ok());
        assert!(table2.create_table().await.is_ok());
    }
}
