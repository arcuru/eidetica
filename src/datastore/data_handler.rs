use super::data::DataTable;
use super::error::Error;
use super::schema::DataEntry;
use crate::datastore::error::Result;
use crate::utils;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

/// Manged Handler for the DataTable
///
/// This is where we manage the logic on top of DataTable. Ingesting data from,
/// the filesystem/S3/etc, and then managing their lifetimes/cleanup/etc.
pub struct DataTableHandler<T: DataTable> {
    data_table: T,
    local_path: PathBuf,
}

/// Represents different types of data storage locations.
#[allow(dead_code)]
pub enum DataLocation {
    Inline(Vec<u8>),    // Inlined binary data
    S3(String),         // Path in an S3 bucket
    LocalPath(PathBuf), // Path on a local filesystem
    Device(uuid::Uuid), // Identifier for a device
}

#[allow(dead_code)]
impl<T: DataTable> DataTableHandler<T> {
    /// Create a new DataTableHandler
    pub fn new(data_table: T, local_path: PathBuf) -> Self {
        Self {
            data_table,
            local_path,
        }
    }

    /// Set a piece of data as 'wanted' to be present locally.
    ///
    /// This increases the refcount for this piece of data or adds it into the
    /// table if necessary.
    pub async fn set_local_needed(&mut self, hash: &str) -> Result<i32> {
        self.data_table.increase_ref_count(hash).await

        // TODO: Poke a bg job to go try to find data
    }

    /// Set a piece of data as 'unwanted' to be present locally.
    /// This decreases the refcount for this piece of data or removes it from the
    /// table if necessary.
    pub async fn set_local_not_needed(&mut self, hash: &str) -> Result<i32> {
        self.data_table.decrease_ref_count(hash).await

        // TODO: Cleanup the table and rows if it's 0, right now this is mostly a no-op.
    }

    /// Check how much data is stored locally inside the local_path
    pub async fn local_file_size(&self) -> std::io::Result<usize> {
        let mut size = 0;
        for entry in std::fs::read_dir(&self.local_path)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            size += metadata.len() as usize;
        }
        Ok(size)
    }

    /// Gets a full list of
    pub async fn get_data_locations(&self, hash: &str) -> Result<Vec<DataLocation>> {
        let entry = match self.data_table.get_entry(hash).await? {
            Some(e) => e,
            None => return Err(Error::NotFound),
        };

        // We're handling the conversion at this level for now, so we need to
        // actually convert to a DataLocation here.
        let mut locations = Vec::new();
        if let Some(inline) = entry.inline_data {
            locations.push(DataLocation::Inline(inline));
        }
        for path in entry.local_path {
            locations.push(DataLocation::LocalPath(path.into()));
        }
        for device in entry.devices {
            locations.push(DataLocation::Device(device));
        }
        for s3 in entry.s3_path {
            locations.push(DataLocation::S3(s3));
        }

        Ok(locations)
    }

    /// Get the local path of the hash or return an error
    pub async fn get_local_path(&self, hash: &str) -> std::io::Result<PathBuf> {
        //let entry = self.data_table.get_entry(hash).await?;
        let local_path = self.hash_to_path(hash)?;
        if local_path.exists() {
            Ok(local_path)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("File not found: {}", hash),
            ))
        }
    }

    /// Delete a file
    ///
    /// Deletes the file locally and removes the path from the data table
    pub async fn delete_local_file(&mut self, hash: &str) -> std::io::Result<()> {
        let path = self.hash_to_path(hash)?;
        std::fs::remove_file(&path)?;
        self.data_table
            .remove_local_path(hash, path.as_os_str().to_str().unwrap_or_default())
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        Ok(())
    }

    /// Deletes a full entry from the database
    ///
    /// If the file exists locally, it will be removed
    pub async fn delete_entry(&mut self, hash: &str) -> std::io::Result<()> {
        let path = self.hash_to_path(hash)?;
        std::fs::remove_file(&path)?;
        self.data_table
            .delete_entry(hash)
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        Ok(())
    }

    /// Copy a file into the data store
    ///
    /// This will:
    /// 1. Calculate the Blake2b hash of the file
    /// 2. Create an entry in the data table
    /// 3. Move the file to the correct local path
    pub async fn copy_file(&mut self, data: DataLocation) -> std::io::Result<DataEntry> {
        let raw_data = match data {
            DataLocation::Inline(raw_data) => raw_data,
            DataLocation::LocalPath(path) => {
                let mut file = File::open(path)?;
                let mut buffer = Vec::new();
                file.read_to_end(&mut buffer)?;
                buffer
            }
            _ => todo!(),
        };
        let hash = utils::generate_hash(&raw_data)?;

        match self.data_table.get_or_insert_entry(&hash.clone()).await {
            Ok(entry) => entry,
            Err(e) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Failed to get or insert entry: {e}"),
                ))
            }
        };
        let dest_path = match self.hash_to_path(hash.as_str()) {
            Ok(path) => path,
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Failed to convert hash to path",
                ))
            }
        };

        // Create parent directories if they don't exist
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Copy the file
        std::fs::write(&dest_path, raw_data)?;

        // Update the table
        match self
            .data_table
            .add_local_path(&hash, dest_path.to_str().unwrap().to_string())
            .await
        {
            Ok(_) => {}
            Err(e) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Failed to add local path: {e}"),
                ))
            }
        };

        Ok(
            match self.data_table.get_or_insert_entry(&hash.clone()).await {
                Ok(entry) => entry,
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("Failed to get or insert entry: {e}"),
                    ))
                }
            },
        )
    }

    /// Add a new storage location for a piece of data
    ///
    /// # Arguments
    /// * `id` - UUID of the entry
    /// * `location` - The new location to add (S3, local path, or device)
    pub async fn add_data_location(&mut self, hash: &str, location: DataLocation) -> Result<()> {
        // Add the location based on its type
        match location {
            DataLocation::Inline(data) => {
                self.data_table.add_inline_data(hash, data).await?;
            }
            DataLocation::S3(path) => {
                self.data_table.add_s3_path(hash, path).await?;
            }
            DataLocation::LocalPath(path) => {
                self.data_table
                    .add_local_path(hash, path.as_os_str().to_string_lossy().into_owned())
                    .await?;
            }
            DataLocation::Device(device_id) => {
                self.data_table.add_device(hash, device_id).await?;
            }
        }

        Ok(())
    }

    /// Converts a hash string to a filesystem path.
    ///
    /// The hash must start with the prefix "b2_". The remaining characters are paired into
    /// directories, with any remaining single character becoming the final component.
    ///
    /// # Arguments
    ///
    /// * `hash` - A string slice that holds the hash, must start with "b2_"
    ///
    /// # Returns
    ///
    /// * `Ok(PathBuf)` - A path constructed from the local base path and the hash components
    /// * `Err(())` - If the hash is empty or doesn't start with "b2_"
    ///
    /// # Examples
    ///
    /// ```
    /// let path = handler.hash_to_path("b2_abcd")?;    // Results in "base_path/b2/ab/cd"
    /// let path = handler.hash_to_path("b2_abcde")?;   // Results in "base_path/b2/ab/cd/e"
    /// let path = handler.hash_to_path("");            // Returns Err(())
    /// let path = handler.hash_to_path("abcd");        // Returns Err(())
    /// ```
    ///
    /// # Directory Structure
    ///
    /// * First level is always "b2"
    /// * Subsequent characters are paired to form directory names
    /// * Any remaining single character becomes the final component
    /// * Example: "b2_abcdefg" becomes "base_path/b2/ab/cd/ef/g"
    fn hash_to_path(&self, hash: &str) -> std::io::Result<PathBuf> {
        let mut path = self.local_path.clone();

        // Return error if hash is empty or doesn't start with b2_
        if hash.is_empty() || !hash.starts_with("b2_") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Incorrect hash formatting: {}", hash),
            ));
        };

        path.push("b2");

        // Get hash part after prefix
        let hash_part = &hash[3..];
        let chars: Vec<_> = hash_part.chars().collect();

        // Process complete pairs into directories (all but the last pair)
        let pairs_to_process = chars.len() / 2;
        for i in 0..pairs_to_process {
            path.push(chars[i * 2..i * 2 + 2].iter().collect::<String>());
        }

        // Add remaining characters as the final component if there are any
        let remaining_start = pairs_to_process * 2;
        if remaining_start < chars.len() {
            path.push(chars[remaining_start..].iter().collect::<String>());
        }

        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datastore::data::PostgresDataTable;
    use sqlx::PgPool;
    use std::fs::{self, File};
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::tempdir;
    pub type TestResult<T> = std::result::Result<T, sqlx::Error>;

    // Helper function to create a DataTableHandler with a mock DataTable
    struct MockDataTable;
    impl DataTable for MockDataTable {
        async fn create_entry(&mut self, _: crate::datastore::schema::DataEntry) -> Result<()> {
            todo!()
        }

        async fn get_or_insert_entry(
            &mut self,
            _: &str,
        ) -> Result<crate::datastore::schema::DataEntry> {
            todo!()
        }

        async fn get_entry(&self, _: &str) -> Result<Option<crate::datastore::schema::DataEntry>> {
            todo!()
        }

        async fn delete_entry(&mut self, _: &str) -> Result<()> {
            todo!()
        }

        async fn increase_ref_count(&mut self, _: &str) -> Result<i32> {
            todo!()
        }

        async fn decrease_ref_count(&mut self, _: &str) -> Result<i32> {
            todo!()
        }

        async fn add_device(&mut self, _: &str, _: uuid::Uuid) -> Result<()> {
            todo!()
        }

        async fn remove_device(&mut self, _: &str, _: uuid::Uuid) -> Result<()> {
            todo!()
        }

        async fn add_local_path(&mut self, _: &str, _: String) -> Result<()> {
            todo!()
        }

        async fn remove_local_path(&mut self, _: &str, _: &str) -> Result<()> {
            todo!()
        }

        async fn add_s3_path(&mut self, _: &str, _: String) -> Result<()> {
            todo!()
        }

        async fn remove_s3_path(&mut self, _: &str, _: &str) -> Result<()> {
            todo!()
        }

        async fn add_inline_data(&mut self, _: &str, _: Vec<u8>) -> Result<()> {
            todo!()
        }

        async fn remove_inline_data(&mut self, _: &str) -> Result<()> {
            todo!()
        }
    }

    fn setup_handler() -> DataTableHandler<MockDataTable> {
        let base_path = PathBuf::from("/test/path");
        DataTableHandler::new(MockDataTable, base_path)
    }

    #[test]
    fn test_hash_to_path_basic() {
        let handler = setup_handler();
        let result = handler.hash_to_path("b2_abcd");

        let expected = PathBuf::from("/test/path").join("b2").join("ab").join("cd");

        assert_eq!(result.unwrap(), expected);
    }

    #[test]
    fn test_hash_to_path_odd_length() {
        let handler = setup_handler();
        let result = handler.hash_to_path("b2_abcde");

        let expected = PathBuf::from("/test/path")
            .join("b2")
            .join("ab")
            .join("cd")
            .join("e");

        assert_eq!(result.unwrap(), expected);
    }

    #[test]
    fn test_hash_to_path_long_hash() {
        let handler = setup_handler();
        let result = handler.hash_to_path("b2_abcdefgh");

        let expected = PathBuf::from("/test/path")
            .join("b2")
            .join("ab")
            .join("cd")
            .join("ef")
            .join("gh");

        assert_eq!(result.unwrap(), expected);
    }

    #[test]
    fn test_hash_to_path_without_prefix() {
        let handler = setup_handler();
        let result = handler.hash_to_path("abcd");

        assert!(result.is_err());
    }

    #[test]
    fn test_hash_to_path_empty_string() {
        let handler = setup_handler();
        let result = handler.hash_to_path("");

        assert!(result.is_err());
    }

    async fn setup_handler_postgres(pool: PgPool) -> DataTableHandler<PostgresDataTable> {
        let base_path = tempdir().unwrap().into_path();
        let postgres = PostgresDataTable::from_pool(pool)
            .await
            .expect("Failed to connect to test database");

        DataTableHandler::new(postgres, base_path)
    }

    #[sqlx::test]
    async fn test_copy_file_basic(pool: PgPool) -> TestResult<()> {
        let mut handler = setup_handler_postgres(pool).await;

        // Create a temporary directory and file
        let temp_dir = tempdir()?;
        let source_path = temp_dir.path().join("test.txt");
        let mut file = File::create(&source_path)?;
        file.write_all(b"Hello, world!")?;

        // Copy the file
        let result = handler
            .copy_file(DataLocation::LocalPath(source_path))
            .await?;

        let expected_path = handler.hash_to_path(&result.hash)?;

        // Verify the file was copied and exists at the new location
        assert!(
            expected_path.exists(),
            "Copied file does not exist at {:?}",
            result
        );

        // Verify content was copied correctly
        let content = fs::read_to_string(&expected_path)?;
        assert_eq!(content, "Hello, world!");

        Ok(())
    }

    #[sqlx::test]
    async fn test_copy_file_idempotent(pool: PgPool) -> TestResult<()> {
        let mut handler = setup_handler_postgres(pool).await;

        // Create a temporary file
        let temp_dir = tempdir()?;
        let source_path = temp_dir.path().join("test.txt");
        let mut file = File::create(&source_path)?;
        file.write_all(b"Hello, world!")?;

        // Copy the file twice
        let first_result = handler
            .copy_file(DataLocation::LocalPath(source_path.clone()))
            .await?;
        let second_result = handler
            .copy_file(DataLocation::LocalPath(source_path))
            .await?;

        // Should return the same path
        assert_eq!(first_result, second_result);

        // Verify the database entry
        let entry = handler
            .data_table
            .get_entry(&first_result.hash)
            .await
            .expect("Failed to get entry")
            .expect("Entry not found");

        // Verify there's only one local path entry
        assert_eq!(
            entry.local_path.len(),
            1,
            "Expected 1 local path entry but found {:?}",
            entry.local_path
        );

        Ok(())
    }

    #[sqlx::test]
    async fn test_copy_file_different_content(pool: PgPool) -> TestResult<()> {
        let mut handler = setup_handler_postgres(pool).await;

        let temp_dir = tempdir()?;

        // Create first file
        let first_path = temp_dir.path().join("first.txt");
        let mut file1 = File::create(&first_path)?;
        file1.write_all(b"First content")?;

        // Create second file
        let second_path = temp_dir.path().join("second.txt");
        let mut file2 = File::create(&second_path)?;
        file2.write_all(b"Different content")?;

        // Copy both files
        let first_result = handler
            .copy_file(DataLocation::LocalPath(first_path))
            .await?;
        let second_result = handler
            .copy_file(DataLocation::LocalPath(second_path))
            .await?;

        // Should be stored at different paths
        assert_ne!(first_result, second_result);

        // Verify both entries exist in database
        assert!(handler
            .data_table
            .get_entry(&first_result.hash)
            .await
            .expect("Failed to get first entry")
            .is_some());
        assert!(handler
            .data_table
            .get_entry(&second_result.hash)
            .await
            .expect("Failed to get second entry")
            .is_some());

        Ok(())
    }

    #[sqlx::test]
    async fn test_copy_file_nonexistent(pool: PgPool) {
        let mut handler = setup_handler_postgres(pool).await;
        let result = handler
            .copy_file(DataLocation::LocalPath(PathBuf::from(
                "nonexistent_file.txt",
            )))
            .await;
        assert!(result.is_err());
    }
}
