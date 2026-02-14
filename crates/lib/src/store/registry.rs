//! Registry for managing typed entries with metadata
//!
//! This module provides a high-level interface for managing registry subtrees,
//! which store entries with type identifiers and configuration data.
//! Used for the `_index` system subtree (store metadata) and other subtrees
//! that need the same typed configuration pattern.

use serde::{Deserialize, Serialize};

use crate::height::HeightStrategy;

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

/// Common settings for any subtree type.
///
/// These settings can be configured per-subtree via the `_index` registry.
/// Settings not specified here inherit from database-level defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtreeSettings {
    /// Height strategy override for this subtree.
    ///
    /// - `None`: Inherit from database-level strategy (subtree height omitted in entries)
    /// - `Some(strategy)`: Use independent height calculation for this subtree
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height_strategy: Option<HeightStrategy>,
}

impl SubtreeSettings {
    /// Check if all settings are at their default values.
    ///
    /// Used for serde `skip_serializing_if` to avoid storing empty settings.
    pub fn is_default(&self) -> bool {
        self.height_strategy.is_none()
    }
}

/// Metadata for a registry entry
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryEntry {
    /// The type identifier (e.g., "docstore:v0", "iroh:v0")
    pub type_id: String,

    /// Type-specific configuration data as a [`Doc`]
    pub config: Doc,

    /// Common subtree settings
    pub settings: SubtreeSettings,
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
    pub(crate) async fn new(
        transaction: &Transaction,
        subtree_name: impl Into<String>,
    ) -> Result<Self> {
        let name = subtree_name.into();
        // Initialize subtree parents before creating the store
        // This mirrors what get_store() does but avoids the recursive
        // get_store -> get_index -> Registry::new -> get_store cycle
        transaction.init_subtree_parents(&name).await?;
        // Create DocStore directly instead of going through get_store
        let inner = DocStore::new(transaction, name).await?;
        Ok(Self { inner })
    }

    /// Get metadata for a named entry
    ///
    /// # Arguments
    /// * `name` - The name of the entry to query
    ///
    /// # Returns
    /// The entry metadata if found, or an error if not registered
    pub async fn get_entry(&self, name: impl AsRef<str>) -> Result<RegistryEntry> {
        let name = name.as_ref();
        let value = self.inner.get(name).await?;

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

        let config = match doc.get("config") {
            Some(doc::Value::Doc(d)) => d.clone(),
            Some(doc::Value::Text(s)) if s == "{}" => Doc::new(),
            Some(doc::Value::Text(_)) => {
                return Err(StoreError::DeserializationFailed {
                    store: self.inner.name().to_string(),
                    reason: format!("Entry '{name}' config is a non-empty Text, expected Doc"),
                }
                .into());
            }
            _ => Doc::new(),
        };

        // Parse settings if present, default to empty settings
        let settings = match doc.get("settings") {
            Some(settings_value) => {
                let settings_doc =
                    settings_value
                        .as_doc()
                        .ok_or_else(|| StoreError::DeserializationFailed {
                            store: self.inner.name().to_string(),
                            reason: format!("Entry '{name}' settings is not a Doc"),
                        })?;

                // Parse height_strategy if present
                let height_strategy = match settings_doc.get("height_strategy") {
                    Some(strategy_value) => {
                        let json = strategy_value.as_text().ok_or_else(|| {
                            StoreError::DeserializationFailed {
                                store: self.inner.name().to_string(),
                                reason: format!(
                                    "Entry '{name}' height_strategy is not a text value"
                                ),
                            }
                        })?;
                        Some(serde_json::from_str(json).map_err(|e| {
                            StoreError::DeserializationFailed {
                                store: self.inner.name().to_string(),
                                reason: format!(
                                    "Failed to parse height_strategy for '{name}': {e}"
                                ),
                            }
                        })?)
                    }
                    None => None,
                };

                SubtreeSettings { height_strategy }
            }
            None => SubtreeSettings::default(),
        };

        Ok(RegistryEntry {
            type_id,
            config,
            settings,
        })
    }

    /// Check if an entry is registered
    ///
    /// # Arguments
    /// * `name` - The name of the entry to check
    ///
    /// # Returns
    /// true if the entry is registered, false otherwise
    pub async fn contains(&self, name: impl AsRef<str>) -> bool {
        self.get_entry(name).await.is_ok()
    }

    /// Register or update an entry
    ///
    /// # Arguments
    /// * `name` - The name of the entry to register/update
    /// * `type_id` - The type identifier (e.g., "docstore:v0", "iroh:v0")
    /// * `config` - Type-specific configuration as a [`Doc`]
    ///
    /// # Returns
    /// Result indicating success or failure
    pub async fn set_entry(
        &self,
        name: impl AsRef<str>,
        type_id: impl Into<String>,
        config: Doc,
    ) -> Result<()> {
        let name = name.as_ref();
        let type_id = type_id.into();

        // Create the nested structure for this entry's metadata
        let mut metadata_doc = Doc::new();
        metadata_doc.set("type", doc::Value::Text(type_id));
        metadata_doc.set("config", doc::Value::Doc(config));

        // Set the metadata in the registry subtree
        self.inner.set(name, doc::Value::Doc(metadata_doc)).await?;

        Ok(())
    }

    /// List all registered entries
    ///
    /// # Returns
    /// A vector of entry names that are registered
    pub async fn list(&self) -> Result<Vec<String>> {
        let full_state: Doc = self
            .inner
            .transaction()
            .get_full_state(self.inner.name())
            .await?;

        // Get all top-level keys from the Doc and clone them to owned Strings
        let keys: Vec<String> = full_state.keys().cloned().collect();

        Ok(keys)
    }

    /// Get the settings for a subtree.
    ///
    /// Returns default settings if the subtree is not registered or has no settings.
    ///
    /// # Arguments
    /// * `name` - The name of the subtree
    ///
    /// # Returns
    /// The subtree settings (default if not found or not registered)
    pub async fn get_subtree_settings(&self, name: impl AsRef<str>) -> Result<SubtreeSettings> {
        match self.get_entry(name).await {
            Ok(entry) => Ok(entry.settings),
            Err(e) if e.is_not_found() => Ok(SubtreeSettings::default()),
            Err(e) => Err(e),
        }
    }

    /// Update the settings for a registered subtree.
    ///
    /// The subtree must already be registered (via `set_entry`). This method
    /// only updates the settings portion, preserving the type_id and config.
    ///
    /// # Arguments
    /// * `name` - The name of the subtree
    /// * `settings` - The new settings to apply
    ///
    /// # Returns
    /// Result indicating success or failure. Returns an error if the subtree
    /// is not registered.
    pub async fn set_subtree_settings(
        &self,
        name: impl AsRef<str>,
        settings: SubtreeSettings,
    ) -> Result<()> {
        let name = name.as_ref();

        // Get existing entry to preserve type_id and config
        let entry = self.get_entry(name).await?;

        // Create the nested structure with updated settings
        let mut metadata_doc = Doc::new();
        metadata_doc.set("type", doc::Value::Text(entry.type_id));
        metadata_doc.set("config", doc::Value::Doc(entry.config));

        // Only add settings if non-default
        if !settings.is_default() {
            let mut settings_doc = Doc::new();
            if let Some(strategy) = settings.height_strategy {
                // Serialize the strategy as a JSON string for storage
                let strategy_json = serde_json::to_string(&strategy).map_err(|e| {
                    StoreError::SerializationFailed {
                        store: self.inner.name().to_string(),
                        reason: format!("Failed to serialize height_strategy: {e}"),
                    }
                })?;
                settings_doc.set("height_strategy", doc::Value::Text(strategy_json));
            }
            metadata_doc.set("settings", doc::Value::Doc(settings_doc));
        }

        // Set the updated metadata
        self.inner.set(name, doc::Value::Doc(metadata_doc)).await?;

        Ok(())
    }
}
