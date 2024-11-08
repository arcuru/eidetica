use crate::utils;

use super::data::DataTable;
use std::path::PathBuf;

/// Manged Handler for the DataTable
///
/// This is where we manage the logic on top of DataTable. Ingesting data from,
/// the filesystem/S3/etc, and then managing their lifetimes/cleanup/etc.
pub struct DataTableHandler<T: DataTable> {
    data_table: T,
    local_path: PathBuf,
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

    /// Copy a file into the data store
    ///
    /// This will:
    /// 1. Calculate the Blake2b hash of the file
    /// 2. Create an entry in the data table
    /// 3. Move the file to the correct local path
    pub async fn copy_file(&mut self, input_file: PathBuf) -> std::io::Result<PathBuf> {
        let hash = utils::generate_hash_from_path(&input_file)?;

        let entry = match self.data_table.get_or_insert_entry(&hash.clone()).await {
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

        if entry.local_path.is_empty() {
            // Create parent directories if they don't exist
            if let Some(parent) = dest_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            // Copy the file
            std::fs::copy(&input_file, &dest_path)?;

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
        } else {
            eprintln!("ASDF: {:?}", entry.local_path);
        }

        Ok(dest_path)
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
    fn hash_to_path(&self, hash: &str) -> Result<PathBuf, ()> {
        let mut path = self.local_path.clone();

        // Return error if hash is empty or doesn't start with b2_
        if hash.is_empty() || !hash.starts_with("b2_") {
            return Err(());
        }

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

    // Helper function to create a DataTableHandler with a mock DataTable
    struct MockDataTable;
    impl DataTable for MockDataTable {
        async fn create_entry(
            &mut self,
            _: crate::datastore::schema::DataEntry,
        ) -> Result<(), crate::datastore::error::Error> {
            todo!()
        }

        async fn get_or_insert_entry(
            &mut self,
            _: &str,
        ) -> Result<crate::datastore::schema::DataEntry, crate::datastore::error::Error> {
            todo!()
        }

        async fn get_entry(
            &self,
            _: &str,
        ) -> Result<Option<crate::datastore::schema::DataEntry>, crate::datastore::error::Error>
        {
            todo!()
        }

        async fn increase_ref_count(
            &mut self,
            _: &str,
        ) -> Result<i32, crate::datastore::error::Error> {
            todo!()
        }

        async fn decrease_ref_count(
            &mut self,
            _: &str,
        ) -> Result<i32, crate::datastore::error::Error> {
            todo!()
        }

        async fn add_device(
            &mut self,
            _: &str,
            _: uuid::Uuid,
        ) -> Result<(), crate::datastore::error::Error> {
            todo!()
        }

        async fn remove_device(
            &mut self,
            _: &str,
            _: uuid::Uuid,
        ) -> Result<(), crate::datastore::error::Error> {
            todo!()
        }

        async fn add_local_path(
            &mut self,
            _: &str,
            _: String,
        ) -> Result<(), crate::datastore::error::Error> {
            todo!()
        }

        async fn remove_local_path(
            &mut self,
            _: &str,
            _: &str,
        ) -> Result<(), crate::datastore::error::Error> {
            todo!()
        }

        async fn add_s3_path(
            &mut self,
            _: &str,
            _: String,
        ) -> Result<(), crate::datastore::error::Error> {
            todo!()
        }

        async fn remove_s3_path(
            &mut self,
            _: &str,
            _: &str,
        ) -> Result<(), crate::datastore::error::Error> {
            todo!()
        }

        async fn add_inline_data(
            &mut self,
            _: &str,
            _: Vec<u8>,
        ) -> Result<(), crate::datastore::error::Error> {
            todo!()
        }

        async fn remove_inline_data(
            &mut self,
            _: &str,
        ) -> Result<(), crate::datastore::error::Error> {
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

        let expected = Ok(PathBuf::from("/test/path").join("b2").join("ab").join("cd"));

        assert_eq!(result, expected);
    }

    #[test]
    fn test_hash_to_path_odd_length() {
        let handler = setup_handler();
        let result = handler.hash_to_path("b2_abcde");

        let expected = Ok(PathBuf::from("/test/path")
            .join("b2")
            .join("ab")
            .join("cd")
            .join("e"));

        assert_eq!(result, expected);
    }

    #[test]
    fn test_hash_to_path_long_hash() {
        let handler = setup_handler();
        let result = handler.hash_to_path("b2_abcdefgh");

        let expected = Ok(PathBuf::from("/test/path")
            .join("b2")
            .join("ab")
            .join("cd")
            .join("ef")
            .join("gh"));

        assert_eq!(result, expected);
    }

    #[test]
    fn test_hash_to_path_without_prefix() {
        let handler = setup_handler();
        let result = handler.hash_to_path("abcd");

        assert_eq!(result, Err(()));
    }

    #[test]
    fn test_hash_to_path_empty_string() {
        let handler = setup_handler();
        let result = handler.hash_to_path("");

        assert_eq!(result, Err(()));
    }

    async fn setup_handler_postgres(pool: PgPool) -> DataTableHandler<PostgresDataTable> {
        let base_path = tempdir().unwrap().into_path();
        let postgres = PostgresDataTable::from_pool(pool)
            .await
            .expect("Failed to connect to test database");

        DataTableHandler::new(postgres, base_path)
    }

    #[sqlx::test]
    async fn test_copy_file_basic(pool: PgPool) -> Result<(), sqlx::Error> {
        let mut handler = setup_handler_postgres(pool).await;

        // Create a temporary directory and file
        let temp_dir = tempdir()?;
        let source_path = temp_dir.path().join("test.txt");
        let mut file = File::create(&source_path)?;
        file.write_all(b"Hello, world!")?;

        // Copy the file
        let result = handler.copy_file(source_path).await?;

        // Verify the file was copied and exists at the new location
        assert!(
            result.exists(),
            "Copied file does not exist at {:?}",
            result
        );

        // Verify content was copied correctly
        let content = fs::read_to_string(&result)?;
        assert_eq!(content, "Hello, world!");

        Ok(())
    }

    #[sqlx::test]
    async fn test_copy_file_idempotent(pool: PgPool) -> Result<(), sqlx::Error> {
        let mut handler = setup_handler_postgres(pool).await;

        // Create a temporary file
        let temp_dir = tempdir()?;
        let source_path = temp_dir.path().join("test.txt");
        let mut file = File::create(&source_path)?;
        file.write_all(b"Hello, world!")?;

        // Copy the file twice
        let first_result = handler.copy_file(source_path.clone()).await?;
        let second_result = handler.copy_file(source_path).await?;

        // Should return the same path
        assert_eq!(first_result, second_result);

        // Verify the database entry
        let hash = utils::generate_hash_from_path(&first_result)?;
        let entry = handler
            .data_table
            .get_entry(&hash)
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
    async fn test_copy_file_different_content(pool: PgPool) -> Result<(), sqlx::Error> {
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
        let first_result = handler.copy_file(first_path).await?;
        let second_result = handler.copy_file(second_path).await?;

        // Should be stored at different paths
        assert_ne!(first_result, second_result);

        // Verify both entries exist in database
        let first_hash = utils::generate_hash_from_path(&first_result)?;
        let second_hash = utils::generate_hash_from_path(&second_result)?;

        assert!(handler
            .data_table
            .get_entry(&first_hash)
            .await
            .expect("Failed to get first entry")
            .is_some());
        assert!(handler
            .data_table
            .get_entry(&second_hash)
            .await
            .expect("Failed to get second entry")
            .is_some());

        Ok(())
    }

    #[sqlx::test]
    async fn test_copy_file_nonexistent(pool: PgPool) {
        let mut handler = setup_handler_postgres(pool).await;
        let result = handler
            .copy_file(PathBuf::from("nonexistent_file.txt"))
            .await;
        assert!(result.is_err());
    }
}
