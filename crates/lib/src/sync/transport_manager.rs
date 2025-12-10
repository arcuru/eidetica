//! Transport manager for handling multiple sync transports.
//!
//! This module provides the `TransportManager` struct that manages multiple
//! transport implementations, routing requests to the appropriate transport
//! based on address type.

use std::sync::Arc;

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
/// The `TransportManager` holds a collection of transports and provides
/// methods to route requests to the correct transport based on address type.
/// It implements a "first match wins" policy for address routing.
#[allow(dead_code)]
pub struct TransportManager {
    transports: Vec<Box<dyn SyncTransport>>,
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
            transports: Vec::new(),
        }
    }

    /// Create a transport manager with an initial transport.
    pub fn with_transport(transport: Box<dyn SyncTransport>) -> Self {
        Self {
            transports: vec![transport],
        }
    }

    /// Add a transport to the manager.
    ///
    /// Transports are checked in order of addition for address routing.
    pub fn add(&mut self, transport: Box<dyn SyncTransport>) {
        self.transports.push(transport);
    }

    /// Check if any transports are registered.
    pub fn is_empty(&self) -> bool {
        self.transports.is_empty()
    }

    /// Get the number of registered transports.
    pub fn len(&self) -> usize {
        self.transports.len()
    }

    /// Get the transport that can handle the given address.
    ///
    /// Returns the first transport (in order of addition) that can handle
    /// the address, or `None` if no transport can handle it.
    pub fn get_for_address(&self, address: &Address) -> Option<&dyn SyncTransport> {
        self.transports
            .iter()
            .find(|t| t.can_handle_address(address))
            .map(|t| t.as_ref())
    }

    /// Get a mutable reference to the transport that can handle the given address.
    pub fn get_for_address_mut(
        &mut self,
        address: &Address,
    ) -> Option<&mut Box<dyn SyncTransport>> {
        self.transports
            .iter_mut()
            .find(|t| t.can_handle_address(address))
    }

    /// Get a transport by its type identifier.
    pub fn get_by_type(&self, transport_type: &str) -> Option<&dyn SyncTransport> {
        self.transports
            .iter()
            .find(|t| t.transport_type() == transport_type)
            .map(|t| t.as_ref())
    }

    /// Get a mutable reference to a transport by its type identifier.
    pub fn get_by_type_mut(&mut self, transport_type: &str) -> Option<&mut Box<dyn SyncTransport>> {
        self.transports
            .iter_mut()
            .find(|t| t.transport_type() == transport_type)
    }

    /// Iterate over all transports.
    pub fn iter(&self) -> impl Iterator<Item = &dyn SyncTransport> {
        self.transports.iter().map(|t| t.as_ref())
    }

    /// Iterate mutably over all transports.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn SyncTransport>> {
        self.transports.iter_mut()
    }

    /// Get all transport type identifiers.
    pub fn transport_types(&self) -> Vec<&str> {
        self.transports.iter().map(|t| t.transport_type()).collect()
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
    /// Each transport is started with its own server on the given address.
    /// If any transport fails to start, previously started transports are stopped.
    pub async fn start_all_servers(
        &mut self,
        addr: &str,
        handler: Arc<dyn SyncHandler>,
    ) -> Result<()> {
        for (started_count, i) in (0..self.transports.len()).enumerate() {
            if let Err(e) = self.transports[i].start_server(addr, handler.clone()).await {
                // Rollback: stop all previously started transports
                for j in 0..started_count {
                    let _ = self.transports[j].stop_server().await;
                }
                return Err(e);
            }
        }

        Ok(())
    }

    /// Start a server on a specific transport.
    pub async fn start_server(
        &mut self,
        transport_type: &str,
        addr: &str,
        handler: Arc<dyn SyncHandler>,
    ) -> Result<()> {
        let transport =
            self.get_by_type_mut(transport_type)
                .ok_or_else(|| SyncError::TransportNotFound {
                    transport_type: transport_type.to_string(),
                })?;

        transport.start_server(addr, handler).await
    }

    /// Stop servers on all transports.
    ///
    /// Attempts to stop all running servers, collecting any errors.
    pub async fn stop_all_servers(&mut self) -> Result<()> {
        let mut errors = Vec::new();

        for transport in &mut self.transports {
            if transport.is_server_running()
                && let Err(e) = transport.stop_server().await
            {
                errors.push(format!("{}: {}", transport.transport_type(), e));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(SyncError::MultipleTransportErrors { errors }.into())
        }
    }

    /// Stop a server on a specific transport.
    pub async fn stop_server(&mut self, transport_type: &str) -> Result<()> {
        let transport =
            self.get_by_type_mut(transport_type)
                .ok_or_else(|| SyncError::TransportNotFound {
                    transport_type: transport_type.to_string(),
                })?;

        transport.stop_server().await
    }

    /// Get the server address for a specific transport.
    pub fn get_server_address(&self, transport_type: &str) -> Result<String> {
        let transport =
            self.get_by_type(transport_type)
                .ok_or_else(|| SyncError::TransportNotFound {
                    transport_type: transport_type.to_string(),
                })?;

        transport.get_server_address()
    }

    /// Get all server addresses (transport_type, address) for running servers.
    pub fn get_all_server_addresses(&self) -> Vec<(String, String)> {
        self.transports
            .iter()
            .filter(|t| t.is_server_running())
            .filter_map(|t| {
                t.get_server_address()
                    .ok()
                    .map(|addr| (t.transport_type().to_string(), addr))
            })
            .collect()
    }

    /// Check if any server is running.
    pub fn is_any_server_running(&self) -> bool {
        self.transports.iter().any(|t| t.is_server_running())
    }

    /// Check if a specific transport's server is running.
    pub fn is_server_running(&self, transport_type: &str) -> bool {
        self.get_by_type(transport_type)
            .is_some_and(|t| t.is_server_running())
    }
}

impl std::fmt::Debug for TransportManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportManager")
            .field("transport_count", &self.transports.len())
            .field("transport_types", &self.transport_types())
            .finish()
    }
}
