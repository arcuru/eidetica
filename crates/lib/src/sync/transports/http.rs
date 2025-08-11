//! HTTP transport implementation for sync communication.
//!
//! This module provides HTTP-based sync communication using a single
//! JSON endpoint (/api/v0) with axum for the server and reqwest for the client.

use super::SyncTransport;
use crate::Result;
use crate::sync::error::SyncError;
use crate::sync::handler::handle_request;
use crate::sync::protocol::{SyncRequest, SyncResponse};
use async_trait::async_trait;
use axum::{Router, extract::Json as ExtractJson, response::Json, routing::post};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

/// HTTP transport implementation using axum and reqwest.
pub struct HttpTransport {
    /// Handle to the running server, if any.
    server_handle: Arc<RwLock<Option<ServerHandle>>>,
}

/// Handle to a running HTTP server.
struct ServerHandle {
    /// Address the server is listening on.
    _addr: SocketAddr,
    /// Shutdown signal sender.
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
}

impl HttpTransport {
    /// Create a new HTTP transport instance.
    pub fn new() -> Result<Self> {
        Ok(Self {
            server_handle: Arc::new(RwLock::new(None)),
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
        if self.is_server_running() {
            return Err(SyncError::ServerAlreadyRunning {
                address: addr.to_string(),
            }
            .into());
        }

        let addr: SocketAddr = addr.parse().map_err(|e| SyncError::ServerBind {
            address: addr.to_string(),
            reason: format!("Invalid address: {e}"),
        })?;

        let router = Self::create_router();
        let server_handle_clone = Arc::clone(&self.server_handle);

        // Create shutdown and ready channels
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();

        // Spawn server task
        tokio::spawn(async move {
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .expect("Failed to bind address");

            let actual_addr = listener.local_addr().expect("Failed to get local address");

            // Store server handle
            {
                let mut handle = server_handle_clone.write().await;
                *handle = Some(ServerHandle {
                    _addr: actual_addr,
                    shutdown_tx,
                });
            }

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

        // Wait for server to be ready
        ready_rx.await.map_err(|_| SyncError::ServerBind {
            address: addr.to_string(),
            reason: "Server startup failed".to_string(),
        })?;

        Ok(())
    }

    async fn stop_server(&mut self) -> Result<()> {
        let mut handle = self.server_handle.write().await;
        if let Some(server) = handle.take() {
            // Send shutdown signal
            let _ = server.shutdown_tx.send(());
            Ok(())
        } else {
            Err(SyncError::ServerNotRunning.into())
        }
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
        // Use try_read to avoid needing async context
        if let Ok(handle) = self.server_handle.try_read() {
            handle.is_some()
        } else {
            // If we can't get the lock, assume it's being modified (likely running)
            true
        }
    }

    fn get_server_address(&self) -> Result<String> {
        if let Ok(handle) = self.server_handle.try_read() {
            if let Some(server) = handle.as_ref() {
                Ok(server._addr.to_string())
            } else {
                Err(SyncError::ServerNotRunning.into())
            }
        } else {
            Err(SyncError::ServerNotRunning.into())
        }
    }
}

/// Handler for the /api/v0 endpoint - accepts JSON SyncRequest and returns JSON SyncResponse.
async fn handle_sync_request(ExtractJson(request): ExtractJson<SyncRequest>) -> Json<SyncResponse> {
    let response = handle_request(request).await;
    Json(response)
}
