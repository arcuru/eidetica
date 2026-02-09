use std::str::FromStr;

use crate::{
    Error, Result, Store, Transaction,
    crdt::{
        CRDT, CRDTError, Doc,
        doc::{List, Path, PathBuf, PathError, Value},
    },
    store::{Registered, errors::StoreError},
};
use async_trait::async_trait;

/// A document-oriented Store providing ergonomic access to Doc CRDT data.
///
/// DocStore wraps the [`Doc`](crate::crdt::Doc) CRDT to provide path-based access to nested
/// document structures. It supports string values and deletions via tombstones.
///
/// # API Overview
///
/// - **Basic operations**: [`get`](Self::get), [`set`](Self::set), [`delete`](Self::delete),
///   [`get_all`](Self::get_all), [`contains_key`](Self::contains_key)
/// - **Path operations**: [`get_path`](Self::get_path), [`set_path`](Self::set_path),
///   [`contains_path`](Self::contains_path)
/// - **Path mutation**: [`modify_path`](Self::modify_path),
///   [`get_or_insert_path`](Self::get_or_insert_path),
///   [`modify_or_insert_path`](Self::modify_or_insert_path)
pub struct DocStore {
    pub(crate) name: String,
    pub(crate) txn: Transaction,
}

impl Registered for DocStore {
    fn type_id() -> &'static str {
        "docstore:v0"
    }
}

#[async_trait]
impl Store for DocStore {
    async fn new(txn: &Transaction, subtree_name: String) -> Result<Self> {
        Ok(Self {
            name: subtree_name,
            txn: txn.clone(),
        })
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn transaction(&self) -> &Transaction {
        &self.txn
    }
}

impl DocStore {
    /// Gets a value associated with a key from the Store.
    ///
    /// This method prioritizes returning data staged within the current `Transaction`.
    /// If the key is not found in the staged data it retrieves the fully merged historical
    /// state from the backend up to the point defined by the `Transaction`'s parents and
    /// returns the value from there.
    ///
    /// # Arguments
    /// * `key` - The key to retrieve the value for.
    ///
    /// # Returns
    /// A `Result` containing the MapValue if found, or `Error::NotFound`.
    pub async fn get(&self, key: impl AsRef<str>) -> Result<Value> {
        let key = key.as_ref();
        // First check if there's any data in the transaction itself
        let local_data: Result<Doc> = self.txn.get_local_data(&self.name);

        // If there's local data, try to get the key from it
        if let Ok(data) = local_data {
            match data.get(key) {
                Some(Value::Deleted) => {
                    return Err(StoreError::KeyNotFound {
                        store: self.name.clone(),
                        key: key.to_string(),
                    }
                    .into());
                }
                Some(value) => return Ok(value.clone()),
                None => {
                    // Key not in local data, continue to backend
                }
            }
        }

        // Otherwise, get the full state from the backend
        let data: Doc = self.txn.get_full_state(&self.name).await?;

        // Return the value from the full state
        match data.get(key) {
            Some(value) => Ok(value.clone()),
            None => Err(StoreError::KeyNotFound {
                store: self.name.clone(),
                key: key.to_string(),
            }
            .into()),
        }
    }

    /// Gets a value associated with a key from the Store (HashMap-like API).
    ///
    /// This method returns an Option for compatibility with std::HashMap.
    /// Returns `None` if the key is not found or is deleted.
    ///
    /// # Arguments
    /// * `key` - The key to retrieve the value for.
    ///
    /// # Returns
    /// An `Option` containing the cloned Value if found, or `None`.
    pub async fn get_option(&self, key: impl AsRef<str>) -> Option<Value> {
        self.get(key).await.ok()
    }

    /// Gets a value associated with a key from the Store (Result-based API for backward compatibility).
    ///
    /// This method prioritizes returning data staged within the current `Transaction`.
    /// If the key is not found in the staged data it retrieves the fully merged historical
    /// state from the backend up to the point defined by the `Transaction`'s parents and
    /// returns the value from there.
    ///
    /// # Arguments
    /// * `key` - The key to retrieve the value for.
    ///
    /// # Returns
    /// A `Result` containing the MapValue if found, or `Error::NotFound`.
    pub async fn get_result(&self, key: impl AsRef<str>) -> Result<Value> {
        let key = key.as_ref();
        // First check if there's any data in the transaction itself
        let local_data: Result<Doc> = self.txn.get_local_data(&self.name);

        // If there's data in the transaction and it contains the key, return that
        if let Ok(data) = local_data
            && let Some(value) = data.get(key)
        {
            return Ok(value.clone());
        }

        // Otherwise, get the full state from the backend
        let data: Doc = self.txn.get_full_state(&self.name).await?;

        // Get the value
        match data.get(key) {
            Some(value) => Ok(value.clone()),
            None => Err(StoreError::KeyNotFound {
                store: self.name.clone(),
                key: key.to_string(),
            }
            .into()),
        }
    }

    /// Gets a string value associated with a key from the Store.
    ///
    /// This is a convenience method that calls `get()` and expects the value to be a string.
    ///
    /// # Arguments
    /// * `key` - The key to retrieve the value for.
    ///
    /// # Returns
    /// A `Result` containing the string value if found, or an error if the key is not found
    /// or if the value is not a string.
    pub async fn get_string(&self, key: impl AsRef<str>) -> Result<String> {
        let key_ref = key.as_ref();
        match self.get_result(key_ref).await? {
            Value::Text(value) => Ok(value),
            Value::Doc(_) => Err(StoreError::TypeMismatch {
                store: self.name.clone(),
                expected: "String".to_string(),
                actual: "Doc".to_string(),
            }
            .into()),
            Value::List(_) => Err(StoreError::TypeMismatch {
                store: self.name.clone(),
                expected: "String".to_string(),
                actual: "list".to_string(),
            }
            .into()),
            Value::Deleted => Err(StoreError::KeyNotFound {
                store: self.name.clone(),
                key: key_ref.to_string(),
            }
            .into()),
            _ => Err(StoreError::TypeMismatch {
                store: self.name.clone(),
                expected: "String".to_string(),
                actual: "Other".to_string(),
            }
            .into()),
        }
    }

    /// Stages the setting of a key-value pair within the associated `Transaction`.
    ///
    /// This method updates the `Map` data held within the `Transaction` for this
    /// `Doc` instance's subtree name. The change is **not** persisted to the backend
    /// until the `Transaction::commit()` method is called.
    ///
    /// # Arguments
    /// * `key` - The key to set.
    /// * `value` - The value to associate with the key (can be &str, String, Value, etc.)
    ///
    /// # Returns
    /// A `Result<()>` indicating success or an error during serialization or staging.
    pub async fn set(&self, key: impl Into<String>, value: impl Into<Value>) -> Result<()> {
        let key = key.into();
        let value = value.into();

        // Get current data from the transaction, or create new if not existing
        let mut data = self
            .txn
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        // Update the data using unified path interface
        data.set(&key, value);

        // Serialize and update the transaction
        let serialized = serde_json::to_string(&data)?;
        self.txn.update_subtree(&self.name, &serialized).await
    }

    /// Sets a key-value pair (HashMap-like API).
    ///
    /// Returns the previous value if one existed, or None if the key was not present.
    /// This follows std::HashMap::insert() semantics.
    ///
    /// # Arguments
    /// * `key` - The key to set.
    /// * `value` - The value to associate with the key (can be &str, String, Value, etc.)
    ///
    /// # Returns
    /// An `Option<Value>` containing the previous value, or `None` if no previous value.
    pub async fn insert(&self, key: impl Into<String>, value: impl Into<Value>) -> Option<Value> {
        let key = key.into();
        let value = value.into();

        // Get current data from the transaction, or create new if not existing
        let mut data = self
            .txn
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        // Get the previous value (if any) before setting
        let previous = data
            .get(&key)
            .cloned()
            .filter(|v| !matches!(v, Value::Deleted));

        // Update the data
        data.set(&key, value);

        // Serialize and update the transaction
        let serialized =
            serde_json::to_string(&data).expect("Failed to serialize data during insert operation");
        self.txn
            .update_subtree(&self.name, &serialized)
            .await
            .expect("Failed to update subtree during insert operation");

        previous
    }

    /// Sets a key-value pair (Result-based API for backward compatibility).
    ///
    /// This method updates the `Map` data held within the `Transaction` for this
    /// `Doc` instance's subtree name. The change is **not** persisted to the backend
    /// until the `Transaction::commit()` method is called.
    ///
    /// # Arguments
    /// * `key` - The key to set.
    /// * `value` - The value to associate with the key (can be &str, String, Value, etc.)
    ///
    /// # Returns
    /// A `Result<()>` indicating success or an error during serialization or staging.
    pub async fn set_result(&self, key: impl Into<String>, value: impl Into<Value>) -> Result<()> {
        let key = key.into();
        let value = value.into();

        // Get current data from the transaction, or create new if not existing
        let mut data = self
            .txn
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        // Update the data
        data.set(&key, value);

        // Serialize and update the transaction
        let serialized = serde_json::to_string(&data)?;
        self.txn.update_subtree(&self.name, &serialized).await
    }

    /// Convenience method to set a string value.
    pub async fn set_string(&self, key: impl Into<String>, value: impl Into<String>) -> Result<()> {
        self.set(key, Value::Text(value.into())).await
    }

    /// Stages the setting of a nested value within the associated `Transaction`.
    ///
    /// This method allows setting any valid Value type (String, Map, or Deleted).
    ///
    /// # Arguments
    /// * `key` - The key to set.
    /// * `value` - The Value to associate with the key.
    ///
    /// # Returns
    /// A `Result<()>` indicating success or an error during serialization or staging.
    /// Convenience method to get a List value.
    pub async fn get_list(&self, key: impl AsRef<str>) -> Result<List> {
        match self.get(key).await? {
            Value::List(list) => Ok(list),
            _ => Err(StoreError::TypeMismatch {
                store: self.name.clone(),
                expected: "list".to_string(),
                actual: "Other".to_string(),
            }
            .into()),
        }
    }

    /// Convenience method to get a nested Doc value.
    pub async fn get_node(&self, key: impl AsRef<str>) -> Result<Doc> {
        match self.get(key).await? {
            Value::Doc(node) => Ok(node),
            _ => Err(StoreError::TypeMismatch {
                store: self.name.clone(),
                expected: "Doc".to_string(),
                actual: "Other".to_string(),
            }
            .into()),
        }
    }

    /// Convenience method to set a list value.
    pub async fn set_list(&self, key: impl Into<String>, list: impl Into<List>) -> Result<()> {
        self.set(key, Value::List(list.into())).await
    }

    /// Convenience method to set a nested Doc value.
    pub async fn set_node(&self, key: impl Into<String>, node: impl Into<Doc>) -> Result<()> {
        self.set(key, Value::Doc(node.into())).await
    }

    /// Legacy method for backward compatibility - now just an alias to set
    pub async fn set_value(&self, key: impl Into<String>, value: impl Into<Value>) -> Result<()> {
        self.set(key, value).await
    }

    /// Legacy method for backward compatibility - now just an alias to get
    pub async fn get_value(&self, key: impl AsRef<str>) -> Result<Value> {
        self.get(key).await
    }

    /// Enhanced access methods with type inference
    ///
    /// These methods provide cleaner access with automatic type conversion,
    /// similar to the CRDT Doc interface but adapted for the DocStore transaction model.
    ///
    /// Gets a value by path using dot notation (e.g., "user.profile.name")
    ///
    /// Traverses the DocStore data structure following the path segments separated by dots.
    /// This method follows the DocStore staging model by checking local staged data first,
    /// then falling back to historical data from the backend.
    ///
    /// # Path Syntax
    ///
    /// - **Docs**: Navigate by key name (e.g., "user.profile.name")
    /// - **Lists**: Navigate by index (e.g., "items.0.title")
    /// - **Mixed**: Combine both (e.g., "users.0.tags.1")
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # use eidetica::store::DocStore;
    /// # use eidetica::crdt::doc::path;
    /// # async fn example(database: Database) -> eidetica::Result<()> {
    /// let txn = database.new_transaction().await?;
    /// let store = txn.get_store::<DocStore>("data").await?;
    ///
    /// store.set_path(path!("user.profile.name"), "Alice").await?;
    ///
    /// // Navigate nested structure
    /// let name = store.get_path(path!("user.profile.name")).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Returns
    /// A `Result<Value>` containing the value if found, or an error if not found.
    pub async fn get_path(&self, path: impl AsRef<Path>) -> Result<Value> {
        // First check if there's any local staged data
        let local_data: Result<Doc> = self.txn.get_local_data(&self.name);

        // If there's local data, try to get the path from it
        if let Ok(data) = local_data
            && let Some(value) = data.get(&path)
        {
            return Ok(value.clone());
        }

        // Otherwise, get the full state from the backend
        let data: Doc = self.txn.get_full_state(&self.name).await?;

        // Get the path from the full state
        match data.get(&path) {
            Some(value) => Ok(value.clone()),
            None => Err(StoreError::KeyNotFound {
                store: self.name.clone(),
                key: path.as_ref().as_str().to_string(),
            }
            .into()),
        }
    }

    /// Gets a value by path using dot notation (HashMap-like API).
    ///
    /// # Returns
    /// An `Option<Value>` containing the value if found, or `None` if not found.
    pub async fn get_path_option(&self, path: impl AsRef<Path>) -> Option<Value> {
        self.get_path(path).await.ok()
    }

    /// Gets a value by path using dot notation (Result-based API for backward compatibility).
    ///
    /// # Returns
    /// A `Result<Value>` containing the value if found, or an error if not found.
    pub async fn get_path_result(&self, path: impl AsRef<Path>) -> Result<Value> {
        self.get_path(path).await
    }
}

impl From<PathError> for Error {
    fn from(err: PathError) -> Self {
        // Convert PathError to CRDTError first, then to main Error
        Error::CRDT(err.into())
    }
}

impl DocStore {
    /// Gets a value with automatic type conversion using TryFrom.
    ///
    /// This provides a generic interface that can convert to any type that implements
    /// `TryFrom<&Value>`, making the API more ergonomic by reducing type specification.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # use eidetica::store::DocStore;
    /// # async fn example(database: Database) -> eidetica::Result<()> {
    /// let txn = database.new_transaction().await?;
    /// let store = txn.get_store::<DocStore>("data").await?;
    ///
    /// store.set("name", "Alice").await?;
    /// store.set("age", 30).await?;
    ///
    /// // Type inference makes this clean
    /// let name: String = store.get_as("name").await?;
    /// let age: i64 = store.get_as("age").await?;
    ///
    /// assert_eq!(name, "Alice");
    /// assert_eq!(age, 30);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_as<T>(&self, key: impl AsRef<str>) -> Result<T>
    where
        T: for<'a> TryFrom<&'a Value, Error = CRDTError>,
    {
        let value = self.get(key).await?;
        T::try_from(&value).map_err(Into::into)
    }

    /// Gets a value by path with automatic type conversion using TryFrom
    ///
    /// Similar to `get_as()` but works with dot-notation paths for nested access.
    /// This method follows the DocStore staging model by checking local staged data first,
    /// then falling back to historical data from the backend.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # use eidetica::store::DocStore;
    /// # use eidetica::crdt::doc::path;
    /// # async fn example(database: Database) -> eidetica::Result<()> {
    /// let txn = database.new_transaction().await?;
    /// let store = txn.get_store::<DocStore>("data").await?;
    ///
    /// // Assuming nested structure exists
    /// // Type inference with path access
    /// let name: String = store.get_path_as(path!("user.profile.name")).await?;
    /// let age: i64 = store.get_path_as(path!("user.profile.age")).await?;
    ///
    /// assert_eq!(name, "Alice");
    /// assert_eq!(age, 30);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The path doesn't exist (`SubtreeError::KeyNotFound`)
    /// - The value cannot be converted to type T (`CRDTError::TypeMismatch`)
    /// - The DocStore operation fails
    pub async fn get_path_as<T>(&self, path: impl AsRef<Path>) -> Result<T>
    where
        T: for<'a> TryFrom<&'a Value, Error = CRDTError>,
    {
        let value = self.get_path(path).await?;
        T::try_from(&value).map_err(Into::into)
    }

    /// Mutable access methods for transaction-based modification
    ///
    /// These methods work with DocStore's staging model, where changes are staged
    /// in the Transaction transaction rather than modified in-place.
    ///
    /// Get or insert a value with a default.
    ///
    /// If the key exists (in either local staging area or historical data),
    /// returns the existing value. If the key doesn't exist, sets it to the
    /// default value and returns that.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # use eidetica::store::DocStore;
    /// # async fn example(database: Database) -> eidetica::Result<()> {
    /// let txn = database.new_transaction().await?;
    /// let store = txn.get_store::<DocStore>("data").await?;
    ///
    /// // Key doesn't exist - will set default
    /// let count1: i64 = store.get_or_insert("counter", 0).await?;
    /// assert_eq!(count1, 0);
    ///
    /// // Key exists - will return existing value
    /// store.set("counter", 5).await?;
    /// let count2: i64 = store.get_or_insert("counter", 100).await?;
    /// assert_eq!(count2, 5);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_or_insert<T>(&self, key: impl AsRef<str>, default: T) -> Result<T>
    where
        T: Into<Value> + for<'a> TryFrom<&'a Value, Error = CRDTError> + Clone,
    {
        let key_str = key.as_ref();

        // Try to get existing value first
        match self.get_as::<T>(key_str).await {
            Ok(existing) => Ok(existing),
            Err(_) => {
                // Key doesn't exist or wrong type - set default and return it
                self.set_result(key_str, default.clone()).await?;
                Ok(default)
            }
        }
    }

    /// Modifies a value in-place using a closure
    ///
    /// If the key exists and can be converted to type T, the closure is called
    /// with the value. After the closure returns, the modified value is staged
    /// back to the DocStore.
    ///
    /// This method handles the DocStore staging model by:
    /// 1. Getting the current value (from local staging or historical data)
    /// 2. Converting it to the desired type
    /// 3. Applying the modification closure
    /// 4. Staging the result back to the Transaction
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The key doesn't exist (`SubtreeError::KeyNotFound`)
    /// - The value cannot be converted to type T (`CRDTError::TypeMismatch`)
    /// - Setting the value fails
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # use eidetica::store::DocStore;
    /// # async fn example(database: Database) -> eidetica::Result<()> {
    /// let txn = database.new_transaction().await?;
    /// let store = txn.get_store::<DocStore>("data").await?;
    ///
    /// store.set("count", 5).await?;
    /// store.set("text", "hello").await?;
    ///
    /// // Modify counter
    /// store.modify::<i64, _>("count", |count| {
    ///     *count += 10;
    /// }).await?;
    /// assert_eq!(store.get_as::<i64>("count").await?, 15);
    ///
    /// // Modify string
    /// store.modify::<String, _>("text", |text| {
    ///     text.push_str(" world");
    /// }).await?;
    /// assert_eq!(store.get_as::<String>("text").await?, "hello world");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn modify<T, F>(&self, key: impl AsRef<str>, f: F) -> Result<()>
    where
        T: for<'a> TryFrom<&'a Value, Error = CRDTError> + Into<Value>,
        F: FnOnce(&mut T),
    {
        let key = key.as_ref();

        // Try to get and convert the current value
        let mut value = self.get_as::<T>(key).await?;

        // Apply the modification
        f(&mut value);

        // Stage the modified value back
        self.set(key, value).await?;
        Ok(())
    }

    /// Modify a value or insert a default if it doesn't exist.
    ///
    /// This is a combination of `get_or_insert` and `modify` that ensures
    /// the key exists before modification.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # use eidetica::store::DocStore;
    /// # async fn example(database: Database) -> eidetica::Result<()> {
    /// let txn = database.new_transaction().await?;
    /// let store = txn.get_store::<DocStore>("data").await?;
    ///
    /// // Key doesn't exist - will create with default then modify
    /// store.modify_or_insert::<i64, _>("counter", 0, |count| {
    ///     *count += 5;
    /// }).await?;
    /// assert_eq!(store.get_as::<i64>("counter").await?, 5);
    ///
    /// // Key exists - will just modify
    /// store.modify_or_insert::<i64, _>("counter", 100, |count| {
    ///     *count *= 2;
    /// }).await?;
    /// assert_eq!(store.get_as::<i64>("counter").await?, 10);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn modify_or_insert<T, F>(&self, key: impl AsRef<str>, default: T, f: F) -> Result<()>
    where
        T: Into<Value> + for<'a> TryFrom<&'a Value, Error = CRDTError> + Clone,
        F: FnOnce(&mut T),
    {
        let key = key.as_ref();

        // Get existing value or insert default
        let mut value = self.get_or_insert(key, default).await?;

        // Apply the modification
        f(&mut value);

        // Stage the modified value back
        self.set(key, value).await?;

        Ok(())
    }

    /// Get or insert a value at a path with a default, similar to get_or_insert but for paths
    ///
    /// If the path exists (in either local staging area or historical data),
    /// returns the existing value. If the path doesn't exist, sets it to the
    /// default value and returns that. Intermediate nodes are created as needed.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # use eidetica::store::DocStore;
    /// # use eidetica::crdt::doc::path;
    /// # async fn example(database: Database) -> eidetica::Result<()> {
    /// let txn = database.new_transaction().await?;
    /// let store = txn.get_store::<DocStore>("data").await?;
    ///
    /// // Path doesn't exist - will create structure and set default
    /// let count1: i64 = store.get_or_insert_path(path!("user.stats.score"), 0).await?;
    /// assert_eq!(count1, 0);
    ///
    /// // Path exists - will return existing value
    /// store.set_path(path!("user.stats.score"), 42).await?;
    /// let count2: i64 = store.get_or_insert_path(path!("user.stats.score"), 100).await?;
    /// assert_eq!(count2, 42);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_or_insert_path<T>(&self, path: impl AsRef<Path>, default: T) -> Result<T>
    where
        T: Into<Value> + for<'a> TryFrom<&'a Value, Error = CRDTError> + Clone,
    {
        // Try to get existing value first
        match self.get_path_as(path.as_ref()).await {
            Ok(existing) => Ok(existing),
            Err(_) => {
                // Path doesn't exist or wrong type - set default and return it
                self.set_path(path, default.clone()).await?;
                Ok(default)
            }
        }
    }

    /// Get or insert a value at a path with string paths for runtime normalization
    pub async fn get_or_insert_path_str<T>(&self, path: &str, default: T) -> Result<T>
    where
        T: Into<Value> + for<'a> TryFrom<&'a Value, Error = CRDTError> + Clone,
    {
        let pathbuf = PathBuf::from_str(path).unwrap(); // Infallible
        self.get_or_insert_path(&pathbuf, default).await
    }

    /// Modify a value at a path or insert a default if it doesn't exist.
    ///
    /// This is a combination of `get_or_insert_path` and `modify_path` that ensures
    /// the path exists before modification, creating intermediate structure as needed.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # use eidetica::store::DocStore;
    /// # use eidetica::crdt::doc::path;
    /// # async fn example(database: Database) -> eidetica::Result<()> {
    /// let txn = database.new_transaction().await?;
    /// let store = txn.get_store::<DocStore>("data").await?;
    ///
    /// // Path doesn't exist - will create structure with default then modify
    /// store.modify_or_insert_path::<i64, _>(path!("user.stats.score"), 0, |score| {
    ///     *score += 10;
    /// }).await?;
    /// assert_eq!(store.get_path_as::<i64>(path!("user.stats.score")).await?, 10);
    ///
    /// // Path exists - will just modify
    /// store.modify_or_insert_path::<i64, _>(path!("user.stats.score"), 100, |score| {
    ///     *score *= 2;
    /// }).await?;
    /// assert_eq!(store.get_path_as::<i64>(path!("user.stats.score")).await?, 20);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn modify_or_insert_path<T, F>(
        &self,
        path: impl AsRef<Path>,
        default: T,
        f: F,
    ) -> Result<()>
    where
        T: Into<Value> + for<'a> TryFrom<&'a Value, Error = CRDTError> + Clone,
        F: FnOnce(&mut T),
    {
        // Get existing value or insert default
        let mut value = self.get_or_insert_path(path.as_ref(), default).await?;

        // Apply the modification
        f(&mut value);

        // Stage the modified value back
        self.set_path(path, value).await?;

        Ok(())
    }

    /// Modify a value or insert a default with string paths for runtime normalization
    pub async fn modify_or_insert_path_str<T, F>(&self, path: &str, default: T, f: F) -> Result<()>
    where
        T: Into<Value> + for<'a> TryFrom<&'a Value, Error = CRDTError> + Clone,
        F: FnOnce(&mut T),
    {
        let pathbuf = PathBuf::from_str(path).unwrap(); // Infallible
        self.modify_or_insert_path(&pathbuf, default, f).await
    }

    /// Sets a value at the given path, creating intermediate nodes as needed
    ///
    /// This method stages a path-based set operation in the Transaction transaction.
    /// The path uses dot notation to navigate and create **nested map structures**.
    /// Intermediate maps are created automatically where necessary.
    ///
    /// # Important: Creates Nested Maps, Not Flat Keys
    ///
    /// Using dots in the path creates a **hierarchy of nested maps**, not flat keys with dots.
    /// For example, `set_path("user.name", "Alice")` creates:
    /// ```json
    /// {
    ///   "user": {
    ///     "name": "Alice"
    ///   }
    /// }
    /// ```
    /// NOT: `{ "user.name": "Alice" }`
    ///
    /// # Path Syntax
    ///
    /// - **Docs**: Navigate by key name (e.g., "user.profile.name")
    /// - **Creating structure**: Intermediate nodes are created automatically
    /// - **Overwriting**: If a path segment points to a non-node value, it will be overwritten
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # use eidetica::store::DocStore;
    /// # use eidetica::crdt::doc::path;
    /// # use eidetica::crdt::doc::Value;
    /// # async fn example(database: Database) -> eidetica::Result<()> {
    /// let txn = database.new_transaction().await?;
    /// let store = txn.get_store::<DocStore>("data").await?;
    ///
    /// // Set nested values, creating structure as needed
    /// store.set_path(path!("user.profile.name"), "Alice").await?;
    /// store.set_path(path!("user.profile.age"), 30).await?;
    /// store.set_path(path!("user.settings.theme"), "dark").await?;
    ///
    /// // This creates nested structure:
    /// // {
    /// //   "user": {
    /// //     "profile": { "name": "Alice", "age": 30 },
    /// //     "settings": { "theme": "dark" }
    /// //   }
    /// // }
    ///
    /// // Access with get_path methods
    /// assert_eq!(store.get_path_as::<String>(path!("user.profile.name")).await?, "Alice");
    ///
    /// // Or navigate the nested structure manually from get_all()
    /// let all = store.get_all().await?;
    /// // all.get("user") returns a Doc, NOT all.get("user.profile.name")
    /// if let Some(Value::Doc(user)) = all.get("user") {
    ///     if let Some(Value::Doc(profile)) = user.get("profile") {
    ///         assert_eq!(profile.get("name"), Some(&Value::Text("Alice".to_string())));
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The path is empty
    /// - A non-final segment contains a non-node value that cannot be navigated through
    /// - The DocStore operation fails
    pub async fn set_path(&self, path: impl AsRef<Path>, value: impl Into<Value>) -> Result<()> {
        let value = value.into();

        // Get current data from the transaction, or create new if not existing
        let mut data = self
            .txn
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        // Use Doc's set method to handle the path logic
        data.set(&path, value);

        // Serialize and update the transaction
        let serialized = serde_json::to_string(&data)?;
        self.txn.update_subtree(&self.name, &serialized).await
    }

    /// Sets a value at the given path with string paths for runtime normalization
    pub async fn set_path_str(&self, path: &str, value: impl Into<Value>) -> Result<()> {
        let pathbuf = PathBuf::from_str(path).unwrap(); // Infallible
        self.set_path(&pathbuf, value).await
    }

    /// Modifies a value at a path in-place using a closure
    ///
    /// Similar to `modify()` but works with dot-notation paths for nested access.
    /// This method follows the DocStore staging model by checking local staged data
    /// first, then falling back to historical data from the backend.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The path doesn't exist (`SubtreeError::KeyNotFound`)
    /// - The value cannot be converted to type T (`CRDTError::TypeMismatch`)
    /// - Setting the path fails (`CRDTError::InvalidPath`)
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # use eidetica::store::DocStore;
    /// # use eidetica::crdt::doc::path;
    /// # async fn example(database: Database) -> eidetica::Result<()> {
    /// let txn = database.new_transaction().await?;
    /// let store = txn.get_store::<DocStore>("data").await?;
    ///
    /// store.set_path(path!("user.score"), 100).await?;
    ///
    /// store.modify_path::<i64, _>(path!("user.score"), |score| {
    ///     *score += 50;
    /// }).await?;
    ///
    /// assert_eq!(store.get_path_as::<i64>(path!("user.score")).await?, 150);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn modify_path<T, F>(&self, path: impl AsRef<Path>, f: F) -> Result<()>
    where
        T: for<'a> TryFrom<&'a Value, Error = CRDTError> + Into<Value>,
        F: FnOnce(&mut T),
    {
        // Try to get and convert the current value
        let mut value = self.get_path_as(path.as_ref()).await?;

        // Apply the modification
        f(&mut value);

        // Stage the modified value back
        self.set_path(path, value).await?;
        Ok(())
    }

    /// Modify a value at a path with string paths for runtime normalization
    pub async fn modify_path_str<T, F>(&self, path: &str, f: F) -> Result<()>
    where
        T: for<'a> TryFrom<&'a Value, Error = CRDTError> + Into<Value>,
        F: FnOnce(&mut T),
    {
        let pathbuf = PathBuf::from_str(path).unwrap(); // Infallible
        self.modify_path(&pathbuf, f).await
    }

    /// Stages the deletion of a key within the associated `Transaction`.
    ///
    /// This method removes the key-value pair from the `Map` data held within
    /// the `Transaction` for this `Doc` instance's subtree name. A tombstone is created,
    /// which will propagate the deletion when merged with other data. The change is **not**
    /// persisted to the backend until the `Transaction::commit()` method is called.
    ///
    /// When using the `get` method, deleted keys will return `Error::NotFound`. However,
    /// the deletion is still tracked internally as a tombstone, which ensures that the
    /// deletion propagates correctly when merging with other versions of the data.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # use eidetica::store::DocStore;
    /// # async fn example(database: Database) -> eidetica::Result<()> {
    /// let txn = database.new_transaction().await?;
    /// let store = txn.get_store::<DocStore>("my_data").await?;
    ///
    /// // First set a value
    /// store.set("user1", "Alice").await?;
    ///
    /// // Later delete the value
    /// store.delete("user1").await?;
    ///
    /// // Attempting to get the deleted key will return NotFound
    /// assert!(store.get("user1").await.is_err());
    ///
    /// // You can verify the tombstone exists by checking the full state
    /// let all_data = store.get_all().await?;
    /// assert!(all_data.is_tombstone("user1"));
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Arguments
    /// * `key` - The key to delete.
    ///
    /// # Returns
    /// - `Ok(true)` if the key existed and was deleted
    /// - `Ok(false)` if the key did not exist (no-op)
    /// - `Err` on serialization or staging errors
    pub async fn delete(&self, key: impl AsRef<str>) -> Result<bool> {
        let key_str = key.as_ref();

        // Check if key exists in full merged state
        let full_state = self.get_all().await?;
        if full_state.get(key_str).is_none() {
            return Ok(false); // Key doesn't exist, no-op
        }

        // Get current data from the transaction, or create new if not existing
        let mut data = self
            .txn
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        // Remove the key (creates a tombstone)
        data.remove(key_str);

        // Serialize and update the transaction
        let serialized = serde_json::to_string(&data)?;
        self.txn.update_subtree(&self.name, &serialized).await?;
        Ok(true)
    }

    /// Retrieves all key-value pairs as a Doc, merging staged and historical state.
    ///
    /// This method combines the data staged within the current `Transaction` with the
    /// fully merged historical state from the backend, providing a complete view
    /// of the document as it would appear if the transaction were committed.
    /// The staged data takes precedence in case of conflicts (overwrites).
    ///
    /// # Important: Understanding Nested Structure
    ///
    /// When using `set_path()` with dot-notation paths, the data is stored as **nested maps**.
    /// The returned Doc will contain the top-level keys, with nested structures as `Value::Doc` values.
    ///
    /// ## Example:
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # use eidetica::store::DocStore;
    /// # use eidetica::crdt::doc::path;
    /// # use eidetica::crdt::doc::Value;
    /// # async fn example(database: Database) -> eidetica::Result<()> {
    /// let txn = database.new_transaction().await?;
    /// let store = txn.get_store::<DocStore>("data").await?;
    ///
    /// // Using set_path creates nested structure
    /// store.set_path(path!("user.name"), "Alice").await?;
    /// store.set_path(path!("user.age"), 30).await?;
    /// store.set_path(path!("config.theme"), "dark").await?;
    ///
    /// let all_data = store.get_all().await?;
    ///
    /// // The top-level map has keys "user" and "config", NOT "user.name", "user.age", etc.
    /// assert_eq!(all_data.len(), 2); // Only 2 top-level keys
    ///
    /// // To access nested data from get_all():
    /// if let Some(Value::Doc(user_node)) = all_data.get("user") {
    ///     // user_node contains "name" and "age" as its children
    ///     assert_eq!(user_node.get("name"), Some(&Value::Text("Alice".to_string())));
    ///     assert_eq!(user_node.get("age"), Some(&Value::Text("30".to_string())));
    /// }
    ///
    /// // For direct access, use get_path() or get_path_as() instead:
    /// assert_eq!(store.get_path_as::<String>(path!("user.name")).await?, "Alice");
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Returns
    /// A `Result` containing the merged `Doc` data structure with nested maps for path-based data.
    pub async fn get_all(&self) -> Result<Doc> {
        // First get the local data directly from the transaction
        let local_data = self.txn.get_local_data::<Doc>(&self.name);

        // Get the full state from the backend
        let mut data = self.txn.get_full_state::<Doc>(&self.name).await?;

        // If there's also local data, merge it with the full state
        if let Ok(local) = local_data {
            data = data.merge(&local)?;
        }

        Ok(data)
    }

    /// Returns true if the DocStore contains the given key
    ///
    /// This method checks both local staged data and historical backend data
    /// following the DocStore staging model. A key is considered to exist if:
    /// - It exists in local staged data (and is not deleted)
    /// - It exists in backend data (and is not deleted)
    ///
    /// Deleted keys (tombstones) are treated as non-existent.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # use eidetica::store::DocStore;
    /// # async fn example(database: Database) -> eidetica::Result<()> {
    /// let txn = database.new_transaction().await?;
    /// let store = txn.get_store::<DocStore>("data").await?;
    ///
    /// assert!(!store.contains_key("missing").await); // Key doesn't exist
    ///
    /// store.set("name", "Alice").await?;
    /// assert!(store.contains_key("name").await); // Key exists in staging
    ///
    /// store.delete("name").await?;
    /// assert!(!store.contains_key("name").await); // Key deleted (tombstone)
    /// # Ok(())
    /// # }
    /// ```
    pub async fn contains_key(&self, key: impl AsRef<str>) -> bool {
        let key = key.as_ref();

        // Check local staged data first
        if let Ok(local_data) = self.txn.get_local_data::<Doc>(&self.name)
            && local_data.contains_key(key)
        {
            return true;
        }

        // Check backend data
        if let Ok(backend_data) = self.txn.get_full_state::<Doc>(&self.name).await {
            backend_data.contains_key(key)
        } else {
            false
        }
    }

    /// Returns true if the DocStore contains the given path
    ///
    /// This method checks both local staged data and historical backend data
    /// following the DocStore staging model. A path is considered to exist if:
    /// - The complete path exists and points to a non-deleted value
    /// - All intermediate segments are navigable (nodes or lists)
    ///
    /// # Path Syntax
    ///
    /// Uses the same dot notation as other path methods:
    /// - **Docs**: Navigate by key name (e.g., "user.profile.name")
    /// - **Lists**: Navigate by index (e.g., "items.0.title")
    /// - **Mixed**: Combine both (e.g., "users.0.tags.1")
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # use eidetica::store::DocStore;
    /// # use eidetica::crdt::doc::path;
    /// # async fn example(database: Database) -> eidetica::Result<()> {
    /// let txn = database.new_transaction().await?;
    /// let store = txn.get_store::<DocStore>("data").await?;
    ///
    /// assert!(!store.contains_path(path!("user.name")).await); // Path doesn't exist
    ///
    /// store.set_path(path!("user.profile.name"), "Alice").await?;
    /// assert!(store.contains_path(path!("user")).await); // Intermediate path exists
    /// assert!(store.contains_path(path!("user.profile")).await); // Intermediate path exists
    /// assert!(store.contains_path(path!("user.profile.name")).await); // Full path exists
    /// assert!(!store.contains_path(path!("user.profile.age")).await); // Path doesn't exist
    /// # Ok(())
    /// # }
    /// ```
    pub async fn contains_path(&self, path: impl AsRef<Path>) -> bool {
        // Check local staged data first
        if let Ok(local_data) = self.txn.get_local_data::<Doc>(&self.name)
            && local_data.get(&path).is_some()
        {
            return true;
        }

        // Check backend data
        if let Ok(backend_data) = self.txn.get_full_state::<Doc>(&self.name).await {
            backend_data.get(&path).is_some()
        } else {
            false
        }
    }

    /// Returns true if the DocStore contains the given path with string paths for runtime normalization
    pub async fn contains_path_str(&self, path: &str) -> bool {
        let pathbuf = PathBuf::from_str(path).unwrap(); // Infallible
        self.contains_path(&pathbuf).await
    }
}
