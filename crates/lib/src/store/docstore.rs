use crate::Result;
use crate::Store;
use crate::Transaction;
use crate::crdt::doc::{List, Node, Value};
use crate::crdt::doc::{Path, PathBuf, PathError};
use crate::crdt::{CRDT, Doc};
use crate::store::errors::StoreError;
use std::str::FromStr;

/// A document-oriented Store providing ergonomic access to Doc CRDT data.
///
/// It assumes that the Store data is a Doc CRDT, which allows for nested document structures.
/// This implementation supports string values, as well as deletions via tombstones.
/// For more complex data structures, consider using the nested capabilities of Doc directly.
pub struct DocStore {
    name: String,
    atomic_op: Transaction,
}

impl Store for DocStore {
    fn new(op: &Transaction, subtree_name: impl Into<String>) -> Result<Self> {
        Ok(Self {
            name: subtree_name.into(),
            atomic_op: op.clone(),
        })
    }

    fn name(&self) -> &str {
        &self.name
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
    pub fn get(&self, key: impl AsRef<str>) -> Result<Value> {
        let key = key.as_ref();
        // First check if there's any data in the atomic op itself
        let local_data: Result<Doc> = self.atomic_op.get_local_data(&self.name);

        // If there's data in the operation and it contains the key, return that
        if let Ok(data) = local_data
            && let Some(value) = data.get(key)
        {
            return Ok(value.clone());
        }

        // Otherwise, get the full state from the backend
        let data: Doc = self.atomic_op.get_full_state(&self.name)?;

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
    pub fn get_string(&self, key: impl AsRef<str>) -> Result<String> {
        let key_ref = key.as_ref();
        match self.get(key_ref)? {
            Value::Text(value) => Ok(value),
            Value::Node(_) => Err(StoreError::TypeMismatch {
                store: self.name.clone(),
                expected: "String".to_string(),
                actual: "Node".to_string(),
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
    /// Calling this method on a `Doc` obtained via `Database::get_store_viewer` is possible
    /// but the changes will be ephemeral and discarded, as the viewer's underlying `Transaction`
    /// is not intended to be committed.
    ///
    /// # Arguments
    /// * `key` - The key to set.
    /// * `value` - The value to associate with the key (can be &str, String, MapValue, etc.)
    ///
    /// # Returns
    /// A `Result<()>` indicating success or an error during serialization or staging.
    pub fn set(&self, key: impl Into<String>, value: impl Into<Value>) -> Result<()> {
        // Get current data from the atomic op, or create new if not existing
        let mut data = self
            .atomic_op
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        // Update the data
        data.as_hashmap_mut().insert(key.into(), value.into());

        // Serialize and update the atomic op
        let serialized = serde_json::to_string(&data)?;
        self.atomic_op.update_subtree(&self.name, &serialized)
    }

    /// Convenience method to set a string value.
    pub fn set_string(&self, key: impl Into<String>, value: impl Into<String>) -> Result<()> {
        self.set(key, Value::Text(value.into()))
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
    pub fn get_list(&self, key: impl AsRef<str>) -> Result<List> {
        match self.get(key)? {
            Value::List(list) => Ok(list),
            _ => Err(StoreError::TypeMismatch {
                store: self.name.clone(),
                expected: "list".to_string(),
                actual: "Other".to_string(),
            }
            .into()),
        }
    }

    /// Convenience method to get a Node value.
    pub fn get_node(&self, key: impl AsRef<str>) -> Result<Doc> {
        match self.get(key)? {
            Value::Node(node) => Ok(node.into()),
            _ => Err(StoreError::TypeMismatch {
                store: self.name.clone(),
                expected: "Node".to_string(),
                actual: "Other".to_string(),
            }
            .into()),
        }
    }

    /// Convenience method to set a list value.
    pub fn set_list(&self, key: impl Into<String>, list: impl Into<List>) -> Result<()> {
        self.set(key, Value::List(list.into()))
    }

    /// Convenience method to set a node value.
    pub fn set_node(&self, key: impl Into<String>, node: impl Into<Doc>) -> Result<()> {
        self.set(key, Value::Node(node.into().into()))
    }

    /// Legacy method for backward compatibility - now just an alias to set
    pub fn set_value(&self, key: impl Into<String>, value: impl Into<Value>) -> Result<()> {
        self.set(key, value.into())
    }

    /// Legacy method for backward compatibility - now just an alias to get
    pub fn get_value(&self, key: impl AsRef<str>) -> Result<Value> {
        self.get(key)
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
    /// - **Nodes**: Navigate by key name (e.g., "user.profile.name")
    /// - **Lists**: Navigate by index (e.g., "items.0.title")
    /// - **Mixed**: Combine both (e.g., "users.0.tags.1")
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # use eidetica::store::DocStore;
    /// # use eidetica::crdt::doc::path;
    /// # let database: Database = unimplemented!();
    /// let op = database.new_transaction()?;
    /// let store = op.get_store::<DocStore>("data")?;
    ///
    /// store.set_path(path!("user.profile.name"), "Alice")?;
    ///
    /// // Navigate nested structure
    /// let name = store.get_path(path!("user.profile.name"))?;
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Any segment of the path doesn't exist
    /// - A non-final segment has the wrong type (not a node or list)
    /// - The DocStore operation fails
    pub fn get_path(&self, path: impl AsRef<Path>) -> Result<Value> {
        // First check if there's any local staged data
        let local_data: Result<Doc> = self.atomic_op.get_local_data(&self.name);

        // If there's local data, try to get the path from it
        if let Ok(data) = local_data
            && let Some(value) = data.get_path(&path)
        {
            return Ok(value.clone());
        }

        // Otherwise, get the full state from the backend
        let data: Doc = self.atomic_op.get_full_state(&self.name)?;

        // Get the path from the full state
        match data.get_path(&path) {
            Some(value) => Ok(value.clone()),
            None => Err(StoreError::KeyNotFound {
                store: self.name.clone(),
                key: path.as_ref().as_str().to_string(),
            }
            .into()),
        }
    }
}

impl From<PathError> for crate::Error {
    fn from(err: PathError) -> Self {
        // Convert PathError to CRDTError first, then to main Error
        crate::Error::CRDT(err.into())
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
    /// # let database: Database = unimplemented!();
    /// let op = database.new_transaction()?;
    /// let store = op.get_store::<DocStore>("data")?;
    ///
    /// store.set("name", "Alice")?;
    /// store.set("age", 30)?;
    ///
    /// // Type inference makes this clean
    /// let name: String = store.get_as("name")?;
    /// let age: i64 = store.get_as("age")?;
    ///
    /// assert_eq!(name, "Alice");
    /// assert_eq!(age, 30);
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn get_as<T>(&self, key: impl AsRef<str>) -> Result<T>
    where
        T: for<'a> TryFrom<&'a Value, Error = crate::crdt::CRDTError>,
    {
        let value = self.get(key)?;
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
    /// # let database: Database = unimplemented!();
    /// let op = database.new_transaction()?;
    /// let store = op.get_store::<DocStore>("data")?;
    ///
    /// // Assuming nested structure exists
    /// // Type inference with path access
    /// let name: String = store.get_path_as(path!("user.profile.name"))?;
    /// let age: i64 = store.get_path_as(path!("user.profile.age"))?;
    ///
    /// assert_eq!(name, "Alice");
    /// assert_eq!(age, 30);
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The path doesn't exist (`SubtreeError::KeyNotFound`)
    /// - The value cannot be converted to type T (`CRDTError::TypeMismatch`)
    /// - The DocStore operation fails
    pub fn get_path_as<T>(&self, path: impl AsRef<Path>) -> Result<T>
    where
        T: for<'a> TryFrom<&'a Value, Error = crate::crdt::CRDTError>,
    {
        let value = self.get_path(path)?;
        T::try_from(&value).map_err(Into::into)
    }

    /// Gets a value by path with automatic type conversion using string paths for runtime validation
    pub fn get_path_as_str<T>(&self, path: &str) -> Result<T>
    where
        T: for<'a> TryFrom<&'a Value, Error = crate::crdt::CRDTError>,
    {
        let pathbuf = PathBuf::from_str(path)?;
        let value = self.get_path(&pathbuf)?;
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
    /// # let database: Database = unimplemented!();
    /// let op = database.new_transaction()?;
    /// let store = op.get_store::<DocStore>("data")?;
    ///
    /// // Key doesn't exist - will set default
    /// let count1: i64 = store.get_or_insert("counter", 0)?;
    /// assert_eq!(count1, 0);
    ///
    /// // Key exists - will return existing value
    /// store.set("counter", 5)?;
    /// let count2: i64 = store.get_or_insert("counter", 100)?;
    /// assert_eq!(count2, 5);
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn get_or_insert<T>(&self, key: impl AsRef<str>, default: T) -> Result<T>
    where
        T: Into<Value> + for<'a> TryFrom<&'a Value, Error = crate::crdt::CRDTError> + Clone,
    {
        let key_str = key.as_ref();

        // Try to get existing value first
        match self.get_as::<T>(key_str) {
            Ok(existing) => Ok(existing),
            Err(_) => {
                // Key doesn't exist or wrong type - set default and return it
                self.set(key_str, default.clone())?;
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
    /// # let database: Database = unimplemented!();
    /// let op = database.new_transaction()?;
    /// let store = op.get_store::<DocStore>("data")?;
    ///
    /// store.set("count", 5)?;
    /// store.set("text", "hello")?;
    ///
    /// // Modify counter
    /// store.modify::<i64, _>("count", |count| {
    ///     *count += 10;
    /// })?;
    /// assert_eq!(store.get_as::<i64>("count")?, 15);
    ///
    /// // Modify string
    /// store.modify::<String, _>("text", |text| {
    ///     text.push_str(" world");
    /// })?;
    /// assert_eq!(store.get_as::<String>("text")?, "hello world");
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn modify<T, F>(&self, key: impl AsRef<str>, f: F) -> Result<()>
    where
        T: for<'a> TryFrom<&'a Value, Error = crate::crdt::CRDTError> + Into<Value>,
        F: FnOnce(&mut T),
    {
        let key = key.as_ref();

        // Try to get and convert the current value
        let mut value = self.get_as::<T>(key)?;

        // Apply the modification
        f(&mut value);

        // Stage the modified value back
        self.set(key, value)?;
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
    /// # let database: Database = unimplemented!();
    /// let op = database.new_transaction()?;
    /// let store = op.get_store::<DocStore>("data")?;
    ///
    /// // Key doesn't exist - will create with default then modify
    /// store.modify_or_insert::<i64, _>("counter", 0, |count| {
    ///     *count += 5;
    /// })?;
    /// assert_eq!(store.get_as::<i64>("counter")?, 5);
    ///
    /// // Key exists - will just modify
    /// store.modify_or_insert::<i64, _>("counter", 100, |count| {
    ///     *count *= 2;
    /// })?;
    /// assert_eq!(store.get_as::<i64>("counter")?, 10);
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn modify_or_insert<T, F>(&self, key: impl AsRef<str>, default: T, f: F) -> Result<()>
    where
        T: Into<Value> + for<'a> TryFrom<&'a Value, Error = crate::crdt::CRDTError> + Clone,
        F: FnOnce(&mut T),
    {
        let key = key.as_ref();

        // Get existing value or insert default
        let mut value = self.get_or_insert(key, default)?;

        // Apply the modification
        f(&mut value);

        // Stage the modified value back
        self.set(key, value)?;

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
    /// # let database: Database = unimplemented!();
    /// let op = database.new_transaction()?;
    /// let store = op.get_store::<DocStore>("data")?;
    ///
    /// // Path doesn't exist - will create structure and set default
    /// let count1: i64 = store.get_or_insert_path(path!("user.stats.score"), 0)?;
    /// assert_eq!(count1, 0);
    ///
    /// // Path exists - will return existing value
    /// store.set_path(path!("user.stats.score"), 42)?;
    /// let count2: i64 = store.get_or_insert_path(path!("user.stats.score"), 100)?;
    /// assert_eq!(count2, 42);
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn get_or_insert_path<T>(&self, path: impl AsRef<Path>, default: T) -> Result<T>
    where
        T: Into<Value> + for<'a> TryFrom<&'a Value, Error = crate::crdt::CRDTError> + Clone,
    {
        // Try to get existing value first
        match self.get_path_as(path.as_ref()) {
            Ok(existing) => Ok(existing),
            Err(_) => {
                // Path doesn't exist or wrong type - set default and return it
                self.set_path(path, default.clone())?;
                Ok(default)
            }
        }
    }

    /// Get or insert a value at a path with string paths for runtime validation
    pub fn get_or_insert_path_str<T>(&self, path: &str, default: T) -> Result<T>
    where
        T: Into<Value> + for<'a> TryFrom<&'a Value, Error = crate::crdt::CRDTError> + Clone,
    {
        let pathbuf = PathBuf::from_str(path)?;
        self.get_or_insert_path(&pathbuf, default)
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
    /// # let database: Database = unimplemented!();
    /// let op = database.new_transaction()?;
    /// let store = op.get_store::<DocStore>("data")?;
    ///
    /// // Path doesn't exist - will create structure with default then modify
    /// store.modify_or_insert_path::<i64, _>(path!("user.stats.score"), 0, |score| {
    ///     *score += 10;
    /// })?;
    /// assert_eq!(store.get_path_as::<i64>(path!("user.stats.score"))?, 10);
    ///
    /// // Path exists - will just modify
    /// store.modify_or_insert_path::<i64, _>(path!("user.stats.score"), 100, |score| {
    ///     *score *= 2;
    /// })?;
    /// assert_eq!(store.get_path_as::<i64>(path!("user.stats.score"))?, 20);
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn modify_or_insert_path<T, F>(
        &self,
        path: impl AsRef<Path>,
        default: T,
        f: F,
    ) -> Result<()>
    where
        T: Into<Value> + for<'a> TryFrom<&'a Value, Error = crate::crdt::CRDTError> + Clone,
        F: FnOnce(&mut T),
    {
        // Get existing value or insert default
        let mut value = self.get_or_insert_path(path.as_ref(), default)?;

        // Apply the modification
        f(&mut value);

        // Stage the modified value back
        self.set_path(path, value)?;

        Ok(())
    }

    /// Modify a value or insert a default with string paths for runtime validation
    pub fn modify_or_insert_path_str<T, F>(&self, path: &str, default: T, f: F) -> Result<()>
    where
        T: Into<Value> + for<'a> TryFrom<&'a Value, Error = crate::crdt::CRDTError> + Clone,
        F: FnOnce(&mut T),
    {
        let pathbuf = PathBuf::from_str(path)?;
        self.modify_or_insert_path(&pathbuf, default, f)
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
    /// - **Nodes**: Navigate by key name (e.g., "user.profile.name")
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
    /// # let database: Database = unimplemented!();
    /// let op = database.new_transaction()?;
    /// let store = op.get_store::<DocStore>("data")?;
    ///
    /// // Set nested values, creating structure as needed
    /// store.set_path(path!("user.profile.name"), "Alice")?;
    /// store.set_path(path!("user.profile.age"), 30)?;
    /// store.set_path(path!("user.settings.theme"), "dark")?;
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
    /// assert_eq!(store.get_path_as::<String>(path!("user.profile.name"))?, "Alice");
    ///
    /// // Or navigate the nested structure manually from get_all()
    /// let all = store.get_all()?;
    /// // all.get("user") returns a Node, NOT all.get("user.profile.name")
    /// if let Some(Value::Node(user)) = all.get("user") {
    ///     if let Some(Value::Node(profile)) = user.get("profile") {
    ///         assert_eq!(profile.get("name"), Some(&Value::Text("Alice".to_string())));
    ///     }
    /// }
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The path is empty
    /// - A non-final segment contains a non-node value that cannot be navigated through
    /// - The DocStore operation fails
    pub fn set_path(&self, path: impl AsRef<Path>, value: impl Into<Value>) -> Result<()> {
        let value = value.into();

        // Get current data from the atomic op, or create new if not existing
        let mut data = self
            .atomic_op
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        // Use Doc's set_path method to handle the path logic
        data.set_path(&path, value)
            .map_err(|e| -> crate::Error { e.into() })?;

        // Serialize and update the atomic op
        let serialized = serde_json::to_string(&data)?;
        self.atomic_op.update_subtree(&self.name, &serialized)
    }

    /// Sets a value at the given path with string paths for runtime validation
    pub fn set_path_str(&self, path: &str, value: impl Into<Value>) -> Result<()> {
        let pathbuf = PathBuf::from_str(path)?;
        self.set_path(&pathbuf, value)
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
    /// # let database: Database = unimplemented!();
    /// let op = database.new_transaction()?;
    /// let store = op.get_store::<DocStore>("data")?;
    ///
    /// store.set_path(path!("user.score"), 100)?;
    ///
    /// store.modify_path::<i64, _>(path!("user.score"), |score| {
    ///     *score += 50;
    /// })?;
    ///
    /// assert_eq!(store.get_path_as::<i64>(path!("user.score"))?, 150);
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn modify_path<T, F>(&self, path: impl AsRef<Path>, f: F) -> Result<()>
    where
        T: for<'a> TryFrom<&'a Value, Error = crate::crdt::CRDTError> + Into<Value>,
        F: FnOnce(&mut T),
    {
        // Try to get and convert the current value
        let mut value = self.get_path_as(path.as_ref())?;

        // Apply the modification
        f(&mut value);

        // Stage the modified value back
        self.set_path(path, value)?;
        Ok(())
    }

    /// Modify a value at a path with string paths for runtime validation
    pub fn modify_path_str<T, F>(&self, path: &str, f: F) -> Result<()>
    where
        T: for<'a> TryFrom<&'a Value, Error = crate::crdt::CRDTError> + Into<Value>,
        F: FnOnce(&mut T),
    {
        let pathbuf = PathBuf::from_str(path)?;
        self.modify_path(&pathbuf, f)
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
    /// # let database: Database = unimplemented!();
    /// let op = database.new_transaction().unwrap();
    /// let store = op.get_store::<DocStore>("my_data").unwrap();
    ///
    /// // First set a value
    /// store.set("user1", "Alice").unwrap();
    ///
    /// // Later delete the value
    /// store.delete("user1").unwrap();
    ///
    /// // Attempting to get the deleted key will return NotFound
    /// assert!(store.get("user1").is_err());
    ///
    /// // You can verify the tombstone exists by checking the full state
    /// let all_data = store.get_all().unwrap();
    /// assert!(all_data.as_hashmap().contains_key("user1"));
    /// ```
    ///
    /// # Arguments
    /// * `key` - The key to delete.
    ///
    /// # Returns
    /// A `Result<()>` indicating success or an error during serialization or staging.
    pub fn delete(&self, key: impl AsRef<str>) -> Result<()> {
        // Get current data from the atomic op, or create new if not existing
        let mut data = self
            .atomic_op
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        // Remove the key (creates a tombstone)
        data.remove(key.as_ref());

        // Serialize and update the atomic op
        let serialized = serde_json::to_string(&data)?;
        self.atomic_op.update_subtree(&self.name, &serialized)
    }

    /// Retrieves all key-value pairs as a Doc, merging staged and historical state.
    ///
    /// This method combines the data staged within the current `Transaction` with the
    /// fully merged historical state from the backend, providing a complete view
    /// of the document as it would appear if the operation were committed.
    /// The staged data takes precedence in case of conflicts (overwrites).
    ///
    /// # Important: Understanding Nested Structure
    ///
    /// When using `set_path()` with dot-notation paths, the data is stored as **nested maps**.
    /// The returned Doc will contain the top-level keys, with nested structures as `Value::Node` values.
    ///
    /// ## Example:
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # use eidetica::store::DocStore;
    /// # use eidetica::crdt::doc::path;
    /// # use eidetica::crdt::doc::Value;
    /// # let database: Database = unimplemented!();
    /// let op = database.new_transaction()?;
    /// let store = op.get_store::<DocStore>("data")?;
    ///
    /// // Using set_path creates nested structure
    /// store.set_path(path!("user.name"), "Alice")?;
    /// store.set_path(path!("user.age"), 30)?;
    /// store.set_path(path!("config.theme"), "dark")?;
    ///
    /// let all_data = store.get_all()?;
    ///
    /// // The top-level map has keys "user" and "config", NOT "user.name", "user.age", etc.
    /// assert_eq!(all_data.as_hashmap().len(), 2); // Only 2 top-level keys
    ///
    /// // To access nested data from get_all():
    /// if let Some(Value::Node(user_node)) = all_data.get("user") {
    ///     // user_node contains "name" and "age" as its children
    ///     assert_eq!(user_node.get("name"), Some(&Value::Text("Alice".to_string())));
    ///     assert_eq!(user_node.get("age"), Some(&Value::Text("30".to_string())));
    /// }
    ///
    /// // For direct access, use get_path() or get_path_as() instead:
    /// assert_eq!(store.get_path_as::<String>(path!("user.name"))?, "Alice");
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    ///
    /// # Returns
    /// A `Result` containing the merged `Doc` data structure with nested maps for path-based data.
    pub fn get_all(&self) -> Result<Doc> {
        // First get the local data directly from the atomic op
        let local_data = self.atomic_op.get_local_data::<Doc>(&self.name);

        // Get the full state from the backend
        let mut data = self.atomic_op.get_full_state::<Doc>(&self.name)?;

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
    /// # let database: Database = unimplemented!();
    /// let op = database.new_transaction()?;
    /// let store = op.get_store::<DocStore>("data")?;
    ///
    /// assert!(!store.contains_key("missing")); // Key doesn't exist
    ///
    /// store.set("name", "Alice")?;
    /// assert!(store.contains_key("name")); // Key exists in staging
    ///
    /// store.delete("name")?;
    /// assert!(!store.contains_key("name")); // Key deleted (tombstone)
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn contains_key(&self, key: impl AsRef<str>) -> bool {
        let key = key.as_ref();

        // Check local staged data first
        if let Ok(local_data) = self.atomic_op.get_local_data::<Doc>(&self.name)
            && local_data.contains_key(key)
        {
            return true;
        }

        // Check backend data
        if let Ok(backend_data) = self.atomic_op.get_full_state::<Doc>(&self.name) {
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
    /// - **Nodes**: Navigate by key name (e.g., "user.profile.name")
    /// - **Lists**: Navigate by index (e.g., "items.0.title")
    /// - **Mixed**: Combine both (e.g., "users.0.tags.1")
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use eidetica::Database;
    /// # use eidetica::store::DocStore;
    /// # use eidetica::crdt::doc::path;
    /// # let database: Database = unimplemented!();
    /// let op = database.new_transaction()?;
    /// let store = op.get_store::<DocStore>("data")?;
    ///
    /// assert!(!store.contains_path(path!("user.name"))); // Path doesn't exist
    ///
    /// store.set_path(path!("user.profile.name"), "Alice")?;
    /// assert!(store.contains_path(path!("user"))); // Intermediate path exists
    /// assert!(store.contains_path(path!("user.profile"))); // Intermediate path exists
    /// assert!(store.contains_path(path!("user.profile.name"))); // Full path exists
    /// assert!(!store.contains_path(path!("user.profile.age"))); // Path doesn't exist
    /// # Ok::<(), eidetica::Error>(())
    /// ```
    pub fn contains_path(&self, path: impl AsRef<Path>) -> bool {
        // Check local staged data first
        if let Ok(local_data) = self.atomic_op.get_local_data::<Doc>(&self.name)
            && local_data.get_path(&path).is_some()
        {
            return true;
        }

        // Check backend data
        if let Ok(backend_data) = self.atomic_op.get_full_state::<Doc>(&self.name) {
            backend_data.get_path(&path).is_some()
        } else {
            false
        }
    }

    /// Returns true if the DocStore contains the given path with string paths for runtime validation
    pub fn contains_path_str(&self, path: &str) -> bool {
        if let Ok(pathbuf) = PathBuf::from_str(path) {
            self.contains_path(&pathbuf)
        } else {
            false
        }
    }

    /// Gets a mutable editor for a value associated with the given key.
    ///
    /// If the key does not exist, the editor will be initialized with an empty map,
    /// allowing immediate use of map-modifying methods. The type can be changed
    /// later using `ValueEditor::set()`.
    ///
    /// Changes made via the `ValueEditor` are staged in the `Transaction` by its `set` method
    /// and must be committed via `Transaction::commit()` to be persisted to the `Doc`'s backend.
    pub fn get_value_mut(&self, key: impl Into<String>) -> ValueEditor<'_> {
        ValueEditor::new(self, vec![key.into()])
    }

    /// Gets a mutable editor for the root of this Doc's subtree.
    ///
    /// Changes made via the `ValueEditor` are staged in the `Transaction` by its `set` method
    /// and must be committed via `Transaction::commit()` to be persisted to the `Doc`'s backend.
    pub fn get_root_mut(&self) -> ValueEditor<'_> {
        ValueEditor::new(self, Vec::new())
    }

    /// Retrieves a `Value` from the Doc using a specified path.
    ///
    /// The path is a slice of strings, where each string is a key in the
    /// nested map structure. If the path is empty, it retrieves the entire
    /// content of this Doc's named subtree as a `Value::Node`.
    ///
    /// This method operates on the fully merged view of the Doc's data,
    /// including any local changes from the current `Transaction` layered on top
    /// of the backend state.
    ///
    /// # Arguments
    ///
    /// * `path`: A slice of `String` representing the path to the desired value.
    ///
    /// # Errors
    ///
    /// * `Error::NotFound` if any segment of the path does not exist (for non-empty paths),
    ///   or if the final value or an intermediate value is a `Value::Deleted` (tombstone).
    /// * `Error::Io` with `ErrorKind::InvalidData` if a non-map value is
    ///   encountered during path traversal where a map was expected.
    pub fn get_at_path<S, P>(&self, path: P) -> Result<Value>
    where
        S: AsRef<str>,
        P: AsRef<[S]>,
    {
        let path_slice = path.as_ref();
        if path_slice.is_empty() {
            // Requesting the root of this Doc's named subtree
            return Ok(Value::Node(self.get_all()?.into()));
        }

        let mut current_value_view = Value::Node(self.get_all()?.into());

        for key_segment_s in path_slice.iter() {
            match current_value_view {
                Value::Node(node) => match node.get(key_segment_s.as_ref()) {
                    Some(next_value) => {
                        current_value_view = next_value.clone();
                    }
                    None => {
                        return Err(StoreError::KeyNotFound {
                            store: self.name.clone(),
                            key: path_slice
                                .iter()
                                .map(|s| s.as_ref())
                                .collect::<Vec<_>>()
                                .join("."),
                        }
                        .into());
                    }
                },
                Value::Deleted => {
                    // A tombstone encountered in the path means the path doesn't lead to a value.
                    return Err(StoreError::KeyNotFound {
                        store: self.name.clone(),
                        key: path_slice
                            .iter()
                            .map(|s| s.as_ref())
                            .collect::<Vec<_>>()
                            .join("."),
                    }
                    .into());
                }
                _ => {
                    // Expected a node to continue traversal, but found something else.
                    return Err(StoreError::TypeMismatch {
                        store: self.name.clone(),
                        expected: "Node".to_string(),
                        actual: "non-node value".to_string(),
                    }
                    .into());
                }
            }
        }

        // Check if the final resolved value is a tombstone.
        match current_value_view {
            Value::Deleted => Err(StoreError::KeyNotFound {
                store: self.name.clone(),
                key: path_slice
                    .iter()
                    .map(|s| s.as_ref())
                    .collect::<Vec<_>>()
                    .join("."),
            }
            .into()),
            _ => Ok(current_value_view),
        }
    }

    /// Sets a `Value` at a specified path within the `Doc`'s `Transaction`.
    ///
    /// The path is a slice of strings, where each string is a key in the
    /// nested map structure.
    ///
    /// This method modifies the local data associated with the `Transaction`. The changes
    /// are not persisted to the backend until `Transaction::commit()` is called.
    /// If the path does not exist, it will be created. Intermediate non-map values
    /// in the path will be overwritten by maps as needed to complete the path.
    ///
    /// # Arguments
    ///
    /// * `path`: A slice of `String` representing the path where the value should be set.
    /// * `value`: The `Value` to set at the specified path.
    ///
    /// # Errors
    ///
    /// * `Error::InvalidOperation` if the `path` is empty and `value` is not a `Value::Node`.
    /// * `Error::Serialize` if the updated subtree data cannot be serialized to JSON.
    /// * Potentially other errors from `Transaction::update_subtree`.
    pub fn set_at_path<S, P>(&self, path: P, value: Value) -> Result<()>
    where
        S: Into<String> + Clone,
        P: AsRef<[S]>,
    {
        let path_slice = path.as_ref();
        if path_slice.is_empty() {
            // Setting the root of this Doc's named subtree.
            // The value must be a node.
            if let Value::Node(node) = value {
                let serialized_data = serde_json::to_string(&node)?;
                return self.atomic_op.update_subtree(&self.name, &serialized_data);
            } else {
                return Err(StoreError::TypeMismatch {
                    store: self.name.clone(),
                    expected: "Node".to_string(),
                    actual: "non-node value".to_string(),
                }
                .into());
            }
        }

        let mut subtree_data = self
            .atomic_op
            .get_local_data::<Doc>(&self.name)
            .unwrap_or_default();

        let mut current_map_mut = subtree_data.as_node_mut();

        // Traverse or create path segments up to the parent of the target key.
        for key_segment_s in path_slice.iter().take(path_slice.len() - 1) {
            let key_segment_string = key_segment_s.clone().into();
            let entry = current_map_mut.as_hashmap_mut().entry(key_segment_string);
            current_map_mut = match entry.or_insert_with(|| Value::Node(Node::default())) {
                Value::Node(node) => node,
                non_map_val => {
                    // If a non-map value exists at an intermediate path segment,
                    // overwrite it with a map to continue.
                    *non_map_val = Value::Node(Node::default());
                    match non_map_val {
                        Value::Node(node) => node,
                        _ => unreachable!("Just assigned a map"),
                    }
                }
            };
        }

        // Set the value at the final key in the path.
        if let Some(last_key_s) = path_slice.last() {
            let map_value = value;
            current_map_mut
                .as_hashmap_mut()
                .insert(last_key_s.clone().into(), map_value);
        } else {
            // This case should be prevented by the initial path.is_empty() check.
            // Given the check, this is technically unreachable if path is not empty.
            return Err(StoreError::InvalidOperation {
                store: self.name.clone(),
                operation: "set_at_path".to_string(),
                reason: "Path became empty unexpectedly".to_string(),
            }
            .into());
        }

        let serialized_data = serde_json::to_string(&subtree_data)?;
        self.atomic_op.update_subtree(&self.name, &serialized_data)
    }
}

/// An editor for a `Value` obtained from a `DocStore`.
///
/// This provides a mutable lens into a value, allowing modifications
/// to be staged and then saved back to the DocStore.
pub struct ValueEditor<'a> {
    kv_store: &'a DocStore,
    keys: Vec<String>,
}

impl<'a> ValueEditor<'a> {
    pub fn new<K>(kv_store: &'a DocStore, keys: K) -> Self
    where
        K: Into<Vec<String>>,
    {
        Self {
            kv_store,
            keys: keys.into(),
        }
    }

    /// Uses the stored keys to traverse the nested data structure and retrieve the value.
    ///
    /// This method starts from the fully merged view of the DocStore's subtree (local
    /// Transaction changes layered on top of backend state) and navigates using the path
    /// specified by `self.keys`. If `self.keys` is empty, it retrieves the root
    /// of the DocStore's subtree.
    ///
    /// Returns `Error::NotFound` if any part of the path does not exist, or if the
    /// final value is a tombstone (`Value::Deleted`).
    /// Returns `Error::Io` with `ErrorKind::InvalidData` if a non-map value is encountered
    /// during path traversal where a map was expected.
    pub fn get(&self) -> Result<Value> {
        self.kv_store.get_at_path(&self.keys)
    }

    /// Sets a `Value` at the path specified by `self.keys` within the `DocStore`'s `Transaction`.
    ///
    /// This method modifies the local data associated with the `Transaction`. The changes
    /// are not persisted to the backend until `Transaction::commit()` is called.
    /// If the path specified by `self.keys` does not exist, it will be created.
    /// Intermediate non-map values in the path will be overwritten by maps as needed.
    /// If `self.keys` is empty (editor points to root), the provided `value` must
    /// be a `Value::Node`.
    ///
    /// Returns `Error::InvalidOperation` if setting the root and `value` is not a node.
    pub fn set(&self, value: Value) -> Result<()> {
        self.kv_store.set_at_path(&self.keys, value)
    }

    /// Returns a nested value by appending `key` to the current editor's path.
    ///
    /// This is a convenience method that uses `self.get()` to find the map at the current
    /// editor's path, and then retrieves `key` from that map.
    pub fn get_value(&self, key: impl AsRef<str>) -> Result<Value> {
        let key = key.as_ref();
        if self.keys.is_empty() {
            // If the base path is empty, trying to get a sub-key implies trying to get a top-level key.
            return self.kv_store.get_at_path([key]);
        }

        let mut path_to_value = self.keys.clone();
        path_to_value.push(key.to_string());
        self.kv_store.get_at_path(&path_to_value)
    }

    /// Constructs a new `ValueEditor` for a path one level deeper.
    ///
    /// The new editor's path will be `self.keys` with `key` appended.
    pub fn get_value_mut(&self, key: impl Into<String>) -> ValueEditor<'a> {
        let mut new_keys = self.keys.clone();
        new_keys.push(key.into());
        ValueEditor::new(self.kv_store, new_keys)
    }

    /// Marks the value at the editor's current path as deleted.
    /// This is achieved by setting its value to `Value::Deleted`.
    /// The change is staged in the `Transaction` and needs to be committed.
    pub fn delete_self(&self) -> Result<()> {
        self.set(Value::Deleted)
    }

    /// Marks the value at the specified child `key` (relative to the editor's current path) as deleted.
    /// This is achieved by setting its value to `Value::Deleted`.
    /// The change is staged in the `Transaction` and needs to be committed.
    ///
    /// If the editor points to the root (empty path), this will delete the top-level `key`.
    pub fn delete_child(&self, key: impl Into<String>) -> Result<()> {
        let mut path_to_delete = self.keys.clone();
        path_to_delete.push(key.into());
        self.kv_store.set_at_path(&path_to_delete, Value::Deleted)
    }
}
