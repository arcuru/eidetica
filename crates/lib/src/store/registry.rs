//! Registry for managing typed entries with metadata
//!
//! This module provides a high-level interface for managing registry subtrees,
//! which store entries with type identifiers and configuration data.
//! Used for the `_index` system subtree (store metadata) and other subtrees
//! that need the same typed configuration pattern.

use serde::{Deserialize, Serialize};

/// Trait for types that can be registered in a Registry.
///
/// Provides a unique type identifier for runtime type checking. This trait
/// is implemented by both [`Store`](super::Store) types and
/// [`TransportConfig`](crate::sync::transports::TransportConfig) types.
///
/// # Example
///
/// ```
/// use eidetica::Registered;
///
/// struct MyType;
///
/// impl Registered for MyType {
///     fn type_id() -> &'static str {
///         "mytype:v0"
///     }
/// }
///
/// assert_eq!(MyType::type_id(), "mytype:v0");
/// assert!(MyType::supports_type_id("mytype:v0"));
/// assert!(!MyType::supports_type_id("other:v0"));
/// ```
pub trait Registered {
    /// Returns a unique identifier for this type.
    ///
    /// The format is typically `"name:version"` (e.g., `"docstore:v0"`, `"iroh:v0"`).
    fn type_id() -> &'static str;

    /// Check if this type supports loading from a stored type_id.
    ///
    /// Override this method to support version migration, allowing newer
    /// implementations to read data stored by older versions.
    ///
    /// # Example
    ///
    /// ```
    /// use eidetica::Registered;
    ///
    /// struct MyTypeV1;
    ///
    /// impl Registered for MyTypeV1 {
    ///     fn type_id() -> &'static str {
    ///         "mytype:v1"
    ///     }
    ///
    ///     fn supports_type_id(type_id: &str) -> bool {
    ///         // v1 can read both v0 and v1 data
    ///         type_id == "mytype:v0" || type_id == "mytype:v1"
    ///     }
    /// }
    ///
    /// assert_eq!(MyTypeV1::type_id(), "mytype:v1");
    /// assert!(MyTypeV1::supports_type_id("mytype:v0")); // Can read v0
    /// assert!(MyTypeV1::supports_type_id("mytype:v1")); // Can read v1
    /// assert!(!MyTypeV1::supports_type_id("other:v0")); // Cannot read other types
    /// ```
    fn supports_type_id(type_id: &str) -> bool {
        type_id == Self::type_id()
    }
}

use crate::{
    Result, Transaction,
    crdt::{Doc, doc},
    store::{DocStore, Store, StoreError},
};

/// Metadata for a registry entry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegistryEntry {
    /// The type identifier (e.g., "docstore:v0", "iroh:v0")
    #[serde(rename = "type")]
    pub type_id: String,

    /// Type-specific configuration data as JSON string
    pub config: String,
}

/// A registry that wraps a DocStore and provides specialized methods
/// for managing typed entries with metadata.
///
/// Registry provides a clean abstraction for storing entries with
/// `{ type, config }` structure. Used for:
/// - `_index` subtree: Store type metadata in databases
/// - `transports` subtree: Transport configuration in sync
pub struct Registry {
    /// The underlying DocStore
    inner: DocStore,
}

impl Registry {
    /// Create a new Registry for a specific subtree
    ///
    /// # Arguments
    /// * `transaction` - The transaction to operate within
    /// * `subtree_name` - The name of the subtree this registry manages
    ///
    /// # Returns
    /// A Result containing the Registry or an error if creation fails
    pub(crate) fn new(transaction: &Transaction, subtree_name: impl Into<String>) -> Result<Self> {
        let inner = transaction.get_store::<DocStore>(subtree_name)?;
        Ok(Self { inner })
    }

    /// Get metadata for a named entry
    ///
    /// # Arguments
    /// * `name` - The name of the entry to query
    ///
    /// # Returns
    /// The entry metadata if found, or an error if not registered
    pub fn get_entry(&self, name: impl AsRef<str>) -> Result<RegistryEntry> {
        let name = name.as_ref();
        let value = self.inner.get(name)?;

        // The value should be a Doc (nested map) with "type" and "config" keys
        let doc = value
            .as_doc()
            .ok_or_else(|| StoreError::DeserializationFailed {
                store: self.inner.name().to_string(),
                reason: format!("Entry '{name}' metadata is not a Doc"),
            })?;

        let type_id = doc
            .get("type")
            .and_then(|v: &doc::Value| v.as_text())
            .ok_or_else(|| StoreError::DeserializationFailed {
                store: self.inner.name().to_string(),
                reason: format!("Entry '{name}' missing 'type' field"),
            })?
            .to_string();

        let config = doc
            .get("config")
            .and_then(|v: &doc::Value| v.as_text())
            .ok_or_else(|| StoreError::DeserializationFailed {
                store: self.inner.name().to_string(),
                reason: format!("Entry '{name}' missing 'config' field"),
            })?
            .to_string();

        Ok(RegistryEntry { type_id, config })
    }

    /// Check if an entry is registered
    ///
    /// # Arguments
    /// * `name` - The name of the entry to check
    ///
    /// # Returns
    /// true if the entry is registered, false otherwise
    pub fn contains(&self, name: impl AsRef<str>) -> bool {
        self.get_entry(name).is_ok()
    }

    /// Register or update an entry
    ///
    /// # Arguments
    /// * `name` - The name of the entry to register/update
    /// * `type_id` - The type identifier (e.g., "docstore:v0", "iroh:v0")
    /// * `config` - Type-specific configuration as a JSON string
    ///
    /// # Returns
    /// Result indicating success or failure
    pub fn set_entry(
        &self,
        name: impl AsRef<str>,
        type_id: impl Into<String>,
        config: impl Into<String>,
    ) -> Result<()> {
        let name = name.as_ref();
        let type_id = type_id.into();
        let config = config.into();

        // Create the nested structure for this entry's metadata
        let mut metadata_doc = Doc::new();
        metadata_doc.set("type", doc::Value::Text(type_id));
        metadata_doc.set("config", doc::Value::Text(config));

        // Set the metadata in the registry subtree
        self.inner.set(name, doc::Value::Doc(metadata_doc))?;

        Ok(())
    }

    /// List all registered entries
    ///
    /// # Returns
    /// A vector of entry names that are registered
    pub fn list(&self) -> Result<Vec<String>> {
        let full_state: Doc = self.inner.transaction().get_full_state(self.inner.name())?;

        // Get all top-level keys from the Doc and clone them to owned Strings
        let keys: Vec<String> = full_state.keys().cloned().collect();

        Ok(keys)
    }
}
