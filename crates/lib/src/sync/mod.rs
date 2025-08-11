//! Synchronization module for Eidetica database.
//!
//! The Sync module manages synchronization settings and state for the database,
//! storing its configuration in a dedicated tree within the database.

use crate::{Result, basedb::BaseDB, crdt::Map, subtree::Dict, tree::Tree};
use std::sync::Arc;

/// Private constant for the sync settings subtree name
const SETTINGS_SUBTREE: &str = "settings_map";

/// Synchronization manager for the database.
///
/// The Sync module maintains its own tree for storing synchronization settings
/// and managing the synchronization state of the database.
#[derive(Clone)]
pub struct Sync {
    /// The BaseDB instance this sync manager belongs to
    base_db: Arc<BaseDB>,
    /// The tree containing synchronization settings
    sync_tree: Tree,
}

impl Sync {
    /// Create a new Sync instance with a dedicated settings tree.
    ///
    /// # Arguments
    /// * `base_db` - The BaseDB instance to manage synchronization for
    /// * `signing_key_name` - The key name to use for authenticating sync tree operations
    ///
    /// # Returns
    /// A new Sync instance with its own settings tree.
    pub fn new(base_db: Arc<BaseDB>, signing_key_name: impl AsRef<str>) -> Result<Self> {
        let mut sync_settings = Map::new();
        sync_settings.set_string("name", "_sync");
        sync_settings.set_string("type", "sync_settings");

        let sync_tree = base_db.new_tree(sync_settings, signing_key_name)?;

        Ok(Self { base_db, sync_tree })
    }

    /// Load an existing Sync instance from a sync tree root ID.
    ///
    /// # Arguments
    /// * `base_db` - The BaseDB instance
    /// * `sync_tree_root_id` - The root ID of the existing sync tree
    ///
    /// # Returns
    /// A Sync instance loaded from the existing tree.
    pub fn load(base_db: Arc<BaseDB>, sync_tree_root_id: &crate::entry::ID) -> Result<Self> {
        let sync_tree = base_db.load_tree(sync_tree_root_id)?;

        Ok(Self { base_db, sync_tree })
    }

    /// Get the root ID of the sync settings tree.
    pub fn sync_tree_root_id(&self) -> &crate::entry::ID {
        self.sync_tree.root_id()
    }

    /// Store a setting in the sync_settings subtree.
    ///
    /// # Arguments
    /// * `key` - The setting key
    /// * `value` - The setting value
    /// * `signing_key_name` - The key name to use for authentication
    pub fn set_setting(
        &mut self,
        key: impl AsRef<str>,
        value: impl AsRef<str>,
        signing_key_name: impl AsRef<str>,
    ) -> Result<()> {
        let op = self
            .sync_tree
            .new_authenticated_operation(signing_key_name)?;
        let sync_settings = op.get_subtree::<Dict>(SETTINGS_SUBTREE)?;
        sync_settings.set_string(key.as_ref(), value.as_ref())?;
        op.commit()?;
        Ok(())
    }

    /// Retrieve a setting from the settings_map subtree.
    ///
    /// # Arguments
    /// * `key` - The setting key to retrieve
    ///
    /// # Returns
    /// The setting value if found, None otherwise.
    pub fn get_setting(&self, key: impl AsRef<str>) -> Result<Option<String>> {
        let sync_settings = self
            .sync_tree
            .get_subtree_viewer::<Dict>(SETTINGS_SUBTREE)?;
        match sync_settings.get_string(key) {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Get a reference to the underlying BaseDB.
    pub fn base_db(&self) -> &Arc<BaseDB> {
        &self.base_db
    }

    /// Get a reference to the sync settings tree.
    pub fn sync_tree(&self) -> &Tree {
        &self.sync_tree
    }
}
