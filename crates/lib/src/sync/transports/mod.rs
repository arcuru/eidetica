//! Transport abstractions for sync communication.
//!
//! This module defines the transport trait that different network
//! implementations must implement, allowing the sync module to
//! work over various protocols (HTTP, Iroh, Bluetooth, etc.).

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    Entry, Result,
    store::Registered,
    sync::{
        handler::SyncHandler,
        peer_types::Address,
        protocol::{SyncRequest, SyncResponse},
    },
};

pub mod http;
pub mod iroh;
pub mod shared;

/// Configuration that can be persisted for a transport.
///
/// Each transport implementation can define its own configuration type
/// that implements this trait. The configuration is stored in the `_sync`
/// database's `transports` subtree and loaded when the transport is enabled.
///
/// Transport configs must also implement [`Registered`] to provide their type identifier.
///
/// # Example
///
/// ```ignore
/// use serde::{Serialize, Deserialize};
/// use eidetica::store::Registered;
/// use eidetica::sync::transports::TransportConfig;
///
/// #[derive(Clone, Serialize, Deserialize, Default)]
/// pub struct MyTransportConfig {
///     pub some_setting: String,
/// }
///
/// impl Registered for MyTransportConfig {
///     fn type_id() -> &'static str {
///         "my-transport:v0"
///     }
/// }
///
/// impl TransportConfig for MyTransportConfig {}
/// ```
pub trait TransportConfig:
    Registered + Serialize + DeserializeOwned + Default + Clone + Send + Sync
{
}

/// Trait for implementing sync communication over different transports.
///
/// Each transport implementation (HTTP, Iroh, etc.) must
/// implement this trait to provide server and client functionality.
#[async_trait]
pub trait SyncTransport: Send + Sync {
    /// Get the transport type identifier.
    ///
    /// This should return a unique string identifying the transport type
    /// (e.g., "http", "iroh"). Used for routing and configuration lookup.
    fn transport_type(&self) -> &'static str;

    /// Check if this transport can handle the given address
    ///
    /// # Arguments
    /// * `address` - The address to check
    ///
    /// # Returns
    /// True if this transport can handle the address, false otherwise.
    fn can_handle_address(&self, address: &Address) -> bool;

    /// Start a server listening on the specified address with a sync handler.
    ///
    /// # Arguments
    /// * `addr` - The address to bind the server to (use port 0 for automatic port assignment)
    /// * `handler` - The sync handler to process incoming requests
    ///
    /// # Returns
    /// A Result indicating success or failure of server startup.
    async fn start_server(&mut self, addr: &str, handler: Arc<dyn SyncHandler>) -> Result<()>;

    /// Stop the running server gracefully.
    ///
    /// # Returns
    /// A Result indicating success or failure of server shutdown.
    async fn stop_server(&mut self) -> Result<()>;

    /// Send a sync request to a peer and receive a response.
    ///
    /// # Arguments
    /// * `address` - The address of the peer to connect to
    /// * `request` - The sync request to send
    ///
    /// # Returns
    /// The response from the peer, or an error if the request failed.
    async fn send_request(&self, address: &Address, request: &SyncRequest) -> Result<SyncResponse>;

    /// Send entries to a sync peer and ensure they are acknowledged.
    ///
    /// This is a convenience method that wraps send_request and validates the response.
    ///
    /// # Arguments
    /// * `address` - The address of the peer to connect to
    /// * `entries` - The entries to send
    ///
    /// # Returns
    /// A Result indicating whether the entries were successfully acknowledged.
    async fn send_entries(&self, address: &Address, entries: &[Entry]) -> Result<()> {
        let request = SyncRequest::SendEntries(entries.to_vec());
        let response = self.send_request(address, &request).await?;
        match response {
            SyncResponse::Ack | SyncResponse::Count(_) => Ok(()),
            SyncResponse::Error(msg) => Err(crate::sync::SyncError::Network(msg).into()),
            _ => Err(crate::sync::SyncError::UnexpectedResponse {
                expected: "Ack or Count",
                actual: format!("{response:?}"),
            }
            .into()),
        }
    }

    /// Check if the server is currently running.
    ///
    /// # Returns
    /// True if the server is running, false otherwise.
    fn is_server_running(&self) -> bool;

    /// Get the address the server is currently bound to.
    ///
    /// # Returns
    /// The server address if running, or an error if no server is running.
    /// Useful when the server was started with port 0 for dynamic port assignment.
    fn get_server_address(&self) -> Result<String>;
}
