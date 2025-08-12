//! Transport abstractions for sync communication.
//!
//! This module defines the transport trait that different network
//! implementations must implement, allowing the sync module to
//! work over various protocols (HTTP, Iroh, Bluetooth, etc.).

use crate::Result;
use crate::entry::Entry;
use crate::sync::protocol::SyncResponse;
use async_trait::async_trait;

pub mod http;
pub mod iroh;
pub mod shared;

/// Trait for implementing sync communication over different transports.
///
/// Each transport implementation (HTTP, Iroh, etc.) must
/// implement this trait to provide server and client functionality.
#[async_trait]
pub trait SyncTransport: Send + Sync {
    /// Start a server listening on the specified address.
    ///
    /// # Arguments
    /// * `addr` - The address to bind the server to (use port 0 for automatic port assignment)
    ///
    /// # Returns
    /// A Result indicating success or failure of server startup.
    async fn start_server(&mut self, addr: &str) -> Result<()>;

    /// Stop the running server gracefully.
    ///
    /// # Returns
    /// A Result indicating success or failure of server shutdown.
    async fn stop_server(&mut self) -> Result<()>;

    /// Send a request to a sync peer and receive a response.
    ///
    /// # Arguments
    /// * `addr` - The address of the peer to connect to
    /// * `request` - The sync request to send (list of entries)
    ///
    /// # Returns
    /// The response from the peer, or an error if the request failed.
    async fn send_request(&self, addr: &str, request: &[Entry]) -> Result<SyncResponse>;

    /// Send entries to a sync peer and ensure they are acknowledged.
    ///
    /// This is a convenience method that wraps send_request and validates the response.
    ///
    /// # Arguments
    /// * `addr` - The address of the peer to connect to
    /// * `entries` - The entries to send
    ///
    /// # Returns
    /// A Result indicating whether the entries were successfully acknowledged.
    async fn send_entries(&self, addr: &str, entries: &[Entry]) -> Result<()> {
        let response = self.send_request(addr, entries).await?;
        match response {
            SyncResponse::Ack | SyncResponse::Count(_) => Ok(()),
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
