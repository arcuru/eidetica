//! Index store for managing subtree registry and type metadata
//!
//! This module provides a high-level interface for managing the `_index` subtree,
//! which stores metadata about all subtrees in a database including their types
//! and configurations. It wraps DocStore to provide index-specific functionality

use serde::{Deserialize, Serialize};

use crate::{
    Result, Transaction,
    constants::INDEX,
    crdt::{Doc, doc},
    store::{DocStore, Store, StoreError},
};

/// Metadata for a single subtree stored in the index
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubtreeInfo {
    /// The Store type identifier (e.g., "docstore:v1", "table:v1")
    #[serde(rename = "type")]
    pub type_id: String,

    /// Store-specific configuration data as JSON string
    // TODO: Pick a better data type, perhaps RawData
    pub config: String,
}

/// An index-specific Store that wraps DocStore and provides specialized methods
/// for managing the subtree registry in the `_index` subtree.
///
/// IndexStore provides a clean abstraction over the `_index` subtree, offering
/// type-safe methods for registering and querying subtree metadata.
pub struct IndexStore {
    /// The underlying DocStore for the _index subtree
    inner: DocStore,
}

impl IndexStore {
    /// Create a new IndexStore from a Transaction
    ///
    /// This creates an IndexStore that operates on the `_index` subtree
    /// within the given transaction context.
    ///
    /// This is crate-private - users should use `Transaction::get_index_store()` instead.
    ///
    /// # Arguments
    /// * `transaction` - The transaction to operate within
    ///
    /// # Returns
    /// A Result containing the IndexStore or an error if creation fails
    pub(crate) fn new(transaction: &Transaction) -> Result<Self> {
        let inner = <DocStore as Store>::new(transaction, INDEX)?;
        Ok(Self { inner })
    }

    /// Get metadata for a specific subtree
    ///
    /// # Arguments
    /// * `subtree_name` - The name of the subtree to query
    ///
    /// # Returns
    /// The subtree metadata if found, or an error if not registered
    pub fn get_subtree_info(&self, subtree_name: impl AsRef<str>) -> Result<SubtreeInfo> {
        let subtree_name = subtree_name.as_ref();
        let value = self.inner.get(subtree_name)?;

        // The value should be a Doc (nested map) with "type" and "config" keys
        let doc = value
            .as_doc()
            .ok_or_else(|| StoreError::DeserializationFailed {
                store: INDEX.to_string(),
                reason: format!("Subtree '{subtree_name}' metadata is not a Doc"),
            })?;

        let type_id = doc
            .get("type")
            .and_then(|v: &doc::Value| v.as_text())
            .ok_or_else(|| StoreError::DeserializationFailed {
                store: INDEX.to_string(),
                reason: format!("Subtree '{subtree_name}' missing 'type' field"),
            })?
            .to_string();

        let config = doc
            .get("config")
            .and_then(|v: &doc::Value| v.as_text())
            .ok_or_else(|| StoreError::DeserializationFailed {
                store: INDEX.to_string(),
                reason: format!("Subtree '{subtree_name}' missing 'config' field"),
            })?
            .to_string();

        Ok(SubtreeInfo { type_id, config })
    }

    /// Check if a subtree is registered in the index
    ///
    /// # Arguments
    /// * `subtree_name` - The name of the subtree to check
    ///
    /// # Returns
    /// true if the subtree is registered, false otherwise
    pub fn contains_subtree(&self, subtree_name: impl AsRef<str>) -> bool {
        self.get_subtree_info(subtree_name).is_ok()
    }

    /// Register or update a subtree in the index
    ///
    /// This method updates the _index subtree with metadata for a specific subtree.
    /// The architectural constraint that _index modifications must be paired with
    /// the corresponding subtree is enforced at commit time by Transaction::commit().
    ///
    /// # Arguments
    /// * `subtree_name` - The name of the subtree to register/update
    /// * `type_id` - The Store type identifier (e.g., "docstore:v1")
    /// * `config` - Store-specific configuration as a JSON string
    ///
    /// # Returns
    /// Result indicating success or failure
    pub fn set_subtree_info(
        &self,
        subtree_name: impl AsRef<str>,
        type_id: impl Into<String>,
        config: impl Into<String>,
    ) -> Result<()> {
        let subtree_name = subtree_name.as_ref();
        let type_id = type_id.into();
        let config = config.into();

        // Create the nested structure for this subtree's metadata
        let mut metadata_doc = Doc::new();
        metadata_doc.set("type", doc::Value::Text(type_id));
        metadata_doc.set("config", doc::Value::Text(config));

        // Set the metadata in the _index subtree
        self.inner
            .set(subtree_name, doc::Value::Doc(metadata_doc))?;

        Ok(())
    }

    /// List all registered subtrees
    ///
    /// # Returns
    /// A vector of subtree names that are registered in the index
    pub fn list_subtrees(&self) -> Result<Vec<String>> {
        let full_state: Doc = self.inner.transaction().get_full_state("_index")?;

        // Get all top-level keys from the Doc and clone them to owned Strings
        let keys: Vec<String> = full_state.keys().cloned().collect();

        Ok(keys)
    }
}
