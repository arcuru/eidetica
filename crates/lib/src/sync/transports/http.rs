//! HTTP transport implementation for sync communication.
//!
//! This module provides HTTP-based sync communication using a single
//! JSON endpoint (/api/v0) with axum for the server and reqwest for the client.

use std::{net::SocketAddr, sync::Arc};

use async_trait::async_trait;
use axum::{
    Router,
    extract::{ConnectInfo, Json as ExtractJson, State},
    response::Json,
    routing::post,
};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::{SyncTransport, TransportConfig, shared::*};
use crate::{
    Result,
    store::Registered,
    sync::{
        error::SyncError,
        handler::SyncHandler,
        peer_types::Address,
        protocol::{RequestContext, SyncRequest, SyncResponse},
    },
};

/// Persistable configuration for the HTTP transport.
///
/// Currently empty, but provides an extension point for future
/// HTTP-specific configuration options (e.g., TLS settings,
/// timeouts, authentication).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HttpTransportConfig {
    // Reserved for future configuration options
}

impl Registered for HttpTransportConfig {
    fn type_id() -> &'static str {
        "http:v0"
    }
}

impl TransportConfig for HttpTransportConfig {}

/// HTTP transport implementation using axum and reqwest.
pub struct HttpTransport {
    /// Shared server state management.
    server_state: ServerState,
}

impl HttpTransport {
    /// Transport type identifier for HTTP
    pub const TRANSPORT_TYPE: &'static str = "http";

    /// Create a new HTTP transport instance.
    pub fn new() -> Result<Self> {
        Ok(Self {
            server_state: ServerState::new(),
        })
    }

    /// Create the axum router with single JSON endpoint and handler state.
    fn create_router(handler: Arc<dyn SyncHandler>) -> Router {
        Router::new()
            .route("/api/v0", post(handle_sync_request))
            .with_state(handler)
    }
}

#[async_trait]
impl SyncTransport for HttpTransport {
    fn transport_type(&self) -> &'static str {
        Self::TRANSPORT_TYPE
    }

    fn can_handle_address(&self, address: &Address) -> bool {
        address.transport_type == Self::TRANSPORT_TYPE
    }

    async fn start_server(&mut self, addr: &str, handler: Arc<dyn SyncHandler>) -> Result<()> {
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

        let router = Self::create_router(handler);

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
            // Convert router to service with ConnectInfo support
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
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

    async fn send_request(&self, address: &Address, request: &SyncRequest) -> Result<SyncResponse> {
        if !self.can_handle_address(address) {
            return Err(SyncError::UnsupportedTransport {
                transport_type: address.transport_type.clone(),
            }
            .into());
        }

        let client = reqwest::Client::new();
        let url = format!("http://{}/api/v0", address.address);

        let response = client
            .post(&url)
            .json(&request) // Send SyncRequest as JSON body
            .send()
            .await
            .map_err(|e| SyncError::ConnectionFailed {
                address: address.address.clone(),
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
async fn handle_sync_request(
    State(handler): State<Arc<dyn SyncHandler>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    ExtractJson(request): ExtractJson<SyncRequest>,
) -> Json<SyncResponse> {
    // Extract peer_pubkey from SyncTreeRequest if present
    let peer_pubkey = match &request {
        SyncRequest::SyncTree(sync_tree_request) => sync_tree_request.peer_pubkey.clone(),
        _ => None,
    };

    // Create request context with remote address and peer pubkey
    let context = RequestContext {
        remote_address: Some(Address {
            transport_type: HttpTransport::TRANSPORT_TYPE.to_string(),
            address: addr.to_string(),
        }),
        peer_pubkey,
    };

    // Call handler directly (Transaction is now Send since it uses Arc<Mutex>)
    let response = handler.handle_request(&request, &context).await;

    Json(response)
}
