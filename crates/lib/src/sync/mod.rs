//! Synchronization module for Eidetica database.
//!
//! The Sync module manages synchronization settings and state for the database,
//! storing its configuration in a dedicated tree within the database.

use crate::{Result, crdt::Map, entry::Entry, subtree::Dict, tree::Tree};
use std::sync::Arc;

pub mod error;
pub mod handler;
pub mod protocol;
pub mod transports;

pub use error::SyncError;

use transports::{SyncTransport, http::HttpTransport, iroh::IrohTransport};

/// Private constant for the sync settings subtree name
const SETTINGS_SUBTREE: &str = "settings_map";

/// Synchronization manager for the database.
///
/// The Sync module maintains its own tree for storing synchronization settings
/// and managing the synchronization state of the database.
pub struct Sync {
    /// The backend for tree operations
    backend: Arc<dyn crate::backend::Database>,
    /// The tree containing synchronization settings
    sync_tree: Tree,
    /// Optional network transport for sync communication
    // Uses simple Box ownership rather than Arc<RwLock<>> because:
    // 1. Each Sync instance exclusively owns its transport (1:1 relationship)
    // 2. All transport operations require &mut self, ensuring exclusive access
    // 3. No sharing between Sync instances - each BaseDB has exactly one Sync
    // 4. Simpler ownership model without unnecessary concurrency overhead
    transport: Option<Box<dyn SyncTransport>>,
}

impl Sync {
    /// Create a new Sync instance with a dedicated settings tree.
    ///
    /// # Arguments
    /// * `backend` - The database backend for tree operations
    /// * `signing_key_name` - The key name to use for authenticating sync tree operations
    ///
    /// # Returns
    /// A new Sync instance with its own settings tree.
    pub fn new(
        backend: Arc<dyn crate::backend::Database>,
        signing_key_name: impl AsRef<str>,
    ) -> Result<Self> {
        let mut sync_settings = Map::new();
        sync_settings.set_string("name", "_sync");
        sync_settings.set_string("type", "sync_settings");

        let sync_tree =
            crate::tree::Tree::new(sync_settings, Arc::clone(&backend), signing_key_name)?;

        Ok(Self {
            backend,
            sync_tree,
            transport: None,
        })
    }

    /// Load an existing Sync instance from a sync tree root ID.
    ///
    /// # Arguments
    /// * `backend` - The database backend
    /// * `sync_tree_root_id` - The root ID of the existing sync tree
    ///
    /// # Returns
    /// A Sync instance loaded from the existing tree.
    pub fn load(
        backend: Arc<dyn crate::backend::Database>,
        sync_tree_root_id: &crate::entry::ID,
    ) -> Result<Self> {
        let sync_tree =
            crate::tree::Tree::new_from_id(sync_tree_root_id.clone(), Arc::clone(&backend))?;

        Ok(Self {
            backend,
            sync_tree,
            transport: None,
        })
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

    /// Get a reference to the underlying backend.
    pub fn backend(&self) -> &Arc<dyn crate::backend::Database> {
        &self.backend
    }

    /// Get a reference to the sync settings tree.
    pub fn sync_tree(&self) -> &Tree {
        &self.sync_tree
    }

    // === Network Transport Methods ===

    /// Start a sync server on the specified address (async version).
    ///
    /// # Arguments
    /// * `addr` - The address to bind the server to (e.g., "127.0.0.1:8080")
    ///
    /// # Returns
    /// A Result indicating success or failure of server startup.
    pub async fn start_server_async(&mut self, addr: &str) -> Result<()> {
        if let Some(transport) = &mut self.transport {
            transport.start_server(addr).await
        } else {
            Err(SyncError::NoTransportEnabled.into())
        }
    }

    /// Stop the running sync server (async version).
    ///
    /// # Returns
    /// A Result indicating success or failure of server shutdown.
    pub async fn stop_server_async(&mut self) -> Result<()> {
        if let Some(transport) = &mut self.transport {
            transport.stop_server().await
        } else {
            Err(SyncError::NoTransportEnabled.into())
        }
    }

    /// Enable HTTP transport for network communication.
    ///
    /// This initializes the HTTP transport layer, allowing the sync module
    /// to communicate over HTTP/REST APIs.
    pub fn enable_http_transport(&mut self) -> Result<()> {
        let transport = HttpTransport::new()?;
        self.transport = Some(Box::new(transport));
        Ok(())
    }

    /// Enable Iroh transport for peer-to-peer network communication.
    ///
    /// This initializes the Iroh transport layer, allowing the sync module
    /// to communicate over QUIC-based peer-to-peer networking with hole punching.
    pub fn enable_iroh_transport(&mut self) -> Result<()> {
        let transport = IrohTransport::new()?;
        self.transport = Some(Box::new(transport));
        Ok(())
    }

    /// Get the server address if the transport is running a server.
    ///
    /// # Returns
    /// The address the server is bound to, or an error if no server is running.
    pub fn get_server_address(&self) -> Result<String> {
        if let Some(transport) = &self.transport {
            transport.get_server_address()
        } else {
            Err(SyncError::NoTransportEnabled.into())
        }
    }

    /// Start a sync server on the specified address.
    ///
    /// # Arguments
    /// * `addr` - The address to bind the server to (e.g., "127.0.0.1:8080")
    ///
    /// # Returns
    /// A Result indicating success or failure of server startup.
    pub fn start_server(&mut self, addr: &str) -> Result<()> {
        // Try to use existing async context, or create runtime if needed
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.block_on(self.start_server_async(addr))
        } else {
            let runtime = tokio::runtime::Runtime::new()
                .map_err(|e| SyncError::RuntimeCreation(e.to_string()))?;

            runtime.block_on(self.start_server_async(addr))
        }
    }

    /// Stop the running sync server.
    ///
    /// # Returns
    /// A Result indicating success or failure of server shutdown.
    pub fn stop_server(&mut self) -> Result<()> {
        // Try to use existing async context, or create runtime if needed
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.block_on(self.stop_server_async())
        } else {
            let runtime = tokio::runtime::Runtime::new()
                .map_err(|e| SyncError::RuntimeCreation(e.to_string()))?;

            runtime.block_on(self.stop_server_async())
        }
    }

    /// Send a batch of entries to a sync peer (async version).
    ///
    /// # Arguments
    /// * `entries` - The entries to send
    /// * `addr` - The address of the peer to send to
    ///
    /// # Returns
    /// A Result indicating whether the entries were successfully acknowledged.
    pub async fn send_entries_async(&self, entries: impl AsRef<[Entry]>, addr: &str) -> Result<()> {
        if let Some(transport) = &self.transport {
            transport.send_entries(addr, entries.as_ref()).await
        } else {
            Err(SyncError::NoTransportEnabled.into())
        }
    }

    /// Send a batch of entries to a sync peer.
    ///
    /// # Arguments
    /// * `entries` - The entries to send
    /// * `addr` - The address of the peer to send to
    ///
    /// # Returns
    /// A Result indicating whether the entries were successfully acknowledged.
    pub fn send_entries(&self, entries: impl AsRef<[Entry]>, addr: &str) -> Result<()> {
        // Try to use existing async context, or create runtime if needed
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.block_on(self.send_entries_async(entries, addr))
        } else {
            let entries_ref = entries.as_ref();
            let runtime = tokio::runtime::Runtime::new()
                .map_err(|e| SyncError::RuntimeCreation(e.to_string()))?;

            runtime.block_on(self.send_entries_async(entries_ref, addr))
        }
    }
}
