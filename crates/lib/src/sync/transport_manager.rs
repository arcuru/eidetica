//! Transport manager for handling multiple sync transports.
//!
//! This module provides the `TransportManager` struct that manages multiple
//! transport implementations, routing requests to the appropriate transport.
//! Transports are stored by name, allowing multiple instances of the same
//! transport type with different configurations.

use std::{collections::HashMap, sync::Arc};

use crate::{
    Result,
    sync::{
        error::SyncError,
        handler::SyncHandler,
        peer_types::Address,
        protocol::{SyncRequest, SyncResponse},
        transports::SyncTransport,
    },
};

/// Manages multiple sync transports and routes requests appropriately.
///
/// The `TransportManager` holds a collection of named transports and provides
/// methods to route requests to the correct transport based on address type.
/// It implements a "first match wins" policy for address routing.
///
/// # Named Instances
///
/// Transports are stored by name (e.g., "http-local", "p2p-staging"), allowing
/// multiple instances of the same transport type with different configurations.
/// Each named instance has its own persisted state in the sync database.
#[allow(dead_code)]
pub struct TransportManager {
    transports: HashMap<String, Box<dyn SyncTransport>>,
}

impl Default for TransportManager {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl TransportManager {
    /// Create a new empty transport manager.
    pub fn new() -> Self {
        Self {
            transports: HashMap::new(),
        }
    }

    /// Add a named transport to the manager.
    ///
    /// If a transport with the same name already exists, it will be replaced.
    pub fn add(&mut self, name: impl Into<String>, transport: Box<dyn SyncTransport>) {
        self.transports.insert(name.into(), transport);
    }

    /// Remove a transport by name.
    ///
    /// Returns the removed transport if it existed.
    pub fn remove(&mut self, name: &str) -> Option<Box<dyn SyncTransport>> {
        self.transports.remove(name)
    }

    /// Check if any transports are registered.
    pub fn is_empty(&self) -> bool {
        self.transports.is_empty()
    }

    /// Get the number of registered transports.
    pub fn len(&self) -> usize {
        self.transports.len()
    }

    /// Check if a transport with the given name exists.
    pub fn contains(&self, name: &str) -> bool {
        self.transports.contains_key(name)
    }

    /// Get the transport that can handle the given address.
    ///
    /// Returns the first transport that can handle the address,
    /// or `None` if no transport can handle it.
    pub fn get_for_address(&self, address: &Address) -> Option<&dyn SyncTransport> {
        self.transports
            .values()
            .find(|t| t.can_handle_address(address))
            .map(|t| t.as_ref())
    }

    /// Get a mutable reference to the transport that can handle the given address.
    pub fn get_for_address_mut(
        &mut self,
        address: &Address,
    ) -> Option<&mut Box<dyn SyncTransport>> {
        self.transports
            .values_mut()
            .find(|t| t.can_handle_address(address))
    }

    /// Get a transport by name.
    pub fn get(&self, name: &str) -> Option<&dyn SyncTransport> {
        self.transports.get(name).map(|t| t.as_ref())
    }

    /// Get a mutable reference to a transport by name.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut Box<dyn SyncTransport>> {
        self.transports.get_mut(name)
    }

    /// Get a transport by its type identifier.
    ///
    /// Returns the first transport of the given type. If multiple transports
    /// of the same type exist, use `get()` with the specific name instead.
    pub fn get_by_type(&self, transport_type: &str) -> Option<&dyn SyncTransport> {
        self.transports
            .values()
            .find(|t| t.transport_type() == transport_type)
            .map(|t| t.as_ref())
    }

    /// Get a mutable reference to a transport by its type identifier.
    ///
    /// Returns the first transport of the given type.
    pub fn get_by_type_mut(&mut self, transport_type: &str) -> Option<&mut Box<dyn SyncTransport>> {
        self.transports
            .values_mut()
            .find(|t| t.transport_type() == transport_type)
    }

    /// Iterate over all transports as (name, transport) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &dyn SyncTransport)> {
        self.transports
            .iter()
            .map(|(name, t)| (name.as_str(), t.as_ref()))
    }

    /// Iterate mutably over all transports as (name, transport) pairs.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&str, &mut Box<dyn SyncTransport>)> {
        self.transports
            .iter_mut()
            .map(|(name, t)| (name.as_str(), t))
    }

    /// Get all transport names.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.transports.keys().map(|s| s.as_str())
    }

    /// Get all transport type identifiers.
    pub fn transport_types(&self) -> Vec<&str> {
        self.transports
            .values()
            .map(|t| t.transport_type())
            .collect()
    }

    /// Send a request to a peer using the appropriate transport.
    ///
    /// Routes to the transport that can handle the address.
    pub async fn send_request(
        &self,
        address: &Address,
        request: &SyncRequest,
    ) -> Result<SyncResponse> {
        let transport =
            self.get_for_address(address)
                .ok_or_else(|| SyncError::NoTransportForAddress {
                    address: address.clone(),
                })?;

        transport.send_request(address, request).await
    }

    /// Start servers on all transports.
    ///
    /// Each transport is started with the given handler. Transports that fail
    /// to start are logged and skipped. Successfully started transports continue
    /// running even if others fail.
    ///
    /// Returns an error only if ALL transports fail to start.
    ///
    /// # Arguments
    /// * `default_addr` - Default address to use for transports that don't have their own configured.
    ///   Pass an empty string to require each transport to have its bind address pre-configured.
    /// * `handler` - The sync handler to use for all transports.
    pub async fn start_all_servers(
        &mut self,
        default_addr: &str,
        handler: Arc<dyn SyncHandler>,
    ) -> Result<()> {
        let names: Vec<String> = self.transports.keys().cloned().collect();
        let mut errors: Vec<String> = Vec::new();
        let mut started = 0;

        for name in &names {
            if let Some(transport) = self.transports.get_mut(name) {
                if let Err(e) = transport.start_server(default_addr, handler.clone()).await {
                    tracing::warn!(transport = %name, error = %e, "Failed to start transport server");
                    errors.push(format!("{}: {}", name, e));
                } else {
                    started += 1;
                }
            }
        }

        if started > 0 || names.is_empty() {
            Ok(())
        } else {
            Err(SyncError::MultipleTransportErrors { errors }.into())
        }
    }

    /// Start a server on a specific named transport.
    pub async fn start_server(
        &mut self,
        name: &str,
        addr: &str,
        handler: Arc<dyn SyncHandler>,
    ) -> Result<()> {
        let transport = self
            .get_mut(name)
            .ok_or_else(|| SyncError::TransportNotFound {
                name: name.to_string(),
            })?;

        transport.start_server(addr, handler).await
    }

    /// Stop servers on all transports.
    ///
    /// Attempts to stop all running servers, collecting any errors.
    pub async fn stop_all_servers(&mut self) -> Result<()> {
        let mut errors = Vec::new();

        for (name, transport) in &mut self.transports {
            if transport.is_server_running()
                && let Err(e) = transport.stop_server().await
            {
                errors.push(format!("{}: {}", name, e));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(SyncError::MultipleTransportErrors { errors }.into())
        }
    }

    /// Stop a server on a specific named transport.
    pub async fn stop_server(&mut self, name: &str) -> Result<()> {
        let transport = self
            .get_mut(name)
            .ok_or_else(|| SyncError::TransportNotFound {
                name: name.to_string(),
            })?;

        transport.stop_server().await
    }

    /// Get the server address for a specific named transport.
    pub fn get_server_address(&self, name: &str) -> Result<String> {
        let transport = self.get(name).ok_or_else(|| SyncError::TransportNotFound {
            name: name.to_string(),
        })?;

        transport.get_server_address()
    }

    /// Get all server addresses (name, address) for running servers.
    pub fn get_all_server_addresses(&self) -> Vec<(String, String)> {
        self.transports
            .iter()
            .filter(|(_, t)| t.is_server_running())
            .filter_map(|(name, t)| t.get_server_address().ok().map(|addr| (name.clone(), addr)))
            .collect()
    }

    /// Check if any server is running.
    pub fn is_any_server_running(&self) -> bool {
        self.transports.values().any(|t| t.is_server_running())
    }

    /// Check if a specific named transport's server is running.
    pub fn is_server_running(&self, name: &str) -> bool {
        self.get(name).is_some_and(|t| t.is_server_running())
    }
}

impl std::fmt::Debug for TransportManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportManager")
            .field("transport_count", &self.transports.len())
            .field("transport_names", &self.names().collect::<Vec<_>>())
            .finish()
    }
}
