//! HTTP transport implementation for sync communication.
//!
//! This module provides HTTP-based sync communication using a single
//! JSON endpoint (/api/v0) with axum for the server and reqwest for the client.

use super::{SyncTransport, shared::*};
use crate::Result;
use crate::sync::error::SyncError;
use crate::sync::handler::handle_request;
use crate::sync::protocol::{SyncRequest, SyncResponse};
use async_trait::async_trait;
use axum::{Router, extract::Json as ExtractJson, response::Json, routing::post};
use std::net::SocketAddr;
use tokio::sync::oneshot;

/// HTTP transport implementation using axum and reqwest.
pub struct HttpTransport {
    /// Shared server state management.
    server_state: ServerState,
}

impl HttpTransport {
    /// Create a new HTTP transport instance.
    pub fn new() -> Result<Self> {
        Ok(Self {
            server_state: ServerState::new(),
        })
    }

    /// Create the axum router with single JSON endpoint.
    fn create_router() -> Router {
        Router::new().route("/api/v0", post(handle_sync_request))
    }
}

#[async_trait]
impl SyncTransport for HttpTransport {
    async fn start_server(&mut self, addr: &str) -> Result<()> {
        // Check if server is already running
        if self.server_state.is_running() {
            return Err(SyncError::ServerAlreadyRunning {
                address: addr.to_string(),
            }
            .into());
        }

        let socket_addr: SocketAddr = addr.parse().map_err(|e| SyncError::ServerBind {
            address: addr.to_string(),
            reason: format!("Invalid address: {e}"),
        })?;

        let router = Self::create_router();

        // Create server coordination channels
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        // Create a channel to get the actual bound address back
        let (addr_tx, addr_rx) = oneshot::channel::<SocketAddr>();

        // Spawn server task
        tokio::spawn(async move {
            let listener = tokio::net::TcpListener::bind(socket_addr)
                .await
                .expect("Failed to bind address");

            // Get the actual bound address (important for port 0)
            let actual_addr = listener.local_addr().expect("Failed to get local address");

            // Send the actual address back
            let _ = addr_tx.send(actual_addr);

            // Signal that server is ready
            let _ = ready_tx.send(());

            // Run server with graceful shutdown
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("Server failed");
        });

        // Get the actual bound address
        let actual_addr = addr_rx.await.map_err(|_| SyncError::ServerBind {
            address: addr.to_string(),
            reason: "Failed to get actual server address".to_string(),
        })?;

        // Wait for server to be ready
        wait_for_ready(ready_rx, addr).await?;

        // Start server state with address and shutdown sender
        self.server_state
            .server_started(actual_addr.to_string(), shutdown_tx);

        Ok(())
    }

    async fn stop_server(&mut self) -> Result<()> {
        if !self.server_state.is_running() {
            return Err(SyncError::ServerNotRunning.into());
        }

        // Stop server using combined method
        self.server_state.stop_server();

        Ok(())
    }

    async fn send_request(&self, addr: &str, request: SyncRequest) -> Result<SyncResponse> {
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/api/v0");

        let response = client
            .post(&url)
            .json(&request) // Send request as JSON body
            .send()
            .await
            .map_err(|e| SyncError::ConnectionFailed {
                address: addr.to_string(),
                reason: e.to_string(),
            })?;

        if !response.status().is_success() {
            return Err(SyncError::Network(format!(
                "Server returned error: {}",
                response.status()
            ))
            .into());
        }

        let sync_response: SyncResponse = response
            .json()
            .await
            .map_err(|e| SyncError::Network(format!("Failed to parse response: {e}")))?;

        Ok(sync_response)
    }

    fn is_server_running(&self) -> bool {
        self.server_state.is_running()
    }

    fn get_server_address(&self) -> Result<String> {
        self.server_state.get_address().map_err(|e| e.into())
    }
}

/// Handler for the /api/v0 endpoint - accepts JSON SyncRequest and returns JSON SyncResponse.
async fn handle_sync_request(ExtractJson(request): ExtractJson<SyncRequest>) -> Json<SyncResponse> {
    let response = handle_request(request).await;
    Json(response)
}
