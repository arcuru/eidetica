//! HTTP transport implementation for sync communication.
//!
//! This module provides HTTP-based sync communication using a single
//! framed endpoint (/api/v1) with axum for the server and reqwest for the client.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, State},
    response::Json,
    routing::post,
};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::{SyncTransport, TransportBuilder, TransportConfig, framing, shared::*};
use crate::{
    Result,
    crdt::Doc,
    store::Registered,
    sync::{
        error::{SyncError, TimeoutPhase},
        handler::SyncHandler,
        peer_types::Address,
        protocol::{SyncRequest, SyncResponse},
    },
};

/// Maximum time spent establishing a TCP connection to a peer.
///
/// A peer that is down or firewalled is detected within this bound rather than
/// at the operating system's connect timeout, which can run into minutes.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum time an outbound request may spend waiting for the peer to send
/// something, reset by every byte that arrives.
///
/// reqwest applies no timeout by default, so without this a peer that accepts
/// the connection and never answers holds the request open forever. Requests
/// are awaited inline by the background sync engine, so an unbounded one stalls
/// every other thing that engine does.
///
/// Framed transfers additionally have this absolute whole-exchange deadline;
/// successful reads and new pages never reset it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Build the outbound HTTP client with both deadlines applied.
fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| SyncError::Network(format!("Failed to build HTTP client: {e}")).into())
}

/// The timeout a failed request represents, or `None` if it failed some other
/// way.
///
/// Which deadline expired says what the peer proved about itself: one that
/// never completed the connection may not be there, while one that accepted it
/// and then went quiet is reachable. Both are timeouts, so both carry the phase
/// rather than being sorted into different variants.
fn timeout_error(error: &reqwest::Error, address: &str) -> Option<SyncError> {
    if !error.is_timeout() {
        return None;
    }

    let (phase, elapsed) = if error.is_connect() {
        (TimeoutPhase::Connect, CONNECT_TIMEOUT)
    } else {
        (TimeoutPhase::Request, REQUEST_TIMEOUT)
    };

    Some(SyncError::Timeout {
        address: address.to_string(),
        phase,
        elapsed,
    })
}

/// Persistable configuration for the HTTP transport.
///
/// Stores HTTP-specific configuration such as the bind address for listening.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HttpTransportConfig {
    /// Bind address for HTTP server (e.g., "127.0.0.1:8080").
    /// If None, HTTP server won't be started by accept_connections().
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind_address: Option<String>,
}

impl Registered for HttpTransportConfig {
    fn type_id() -> &'static str {
        "http:v0"
    }
}

impl TransportConfig for HttpTransportConfig {}

/// Builder for configuring HTTP transport.
///
/// # Example
///
/// ```ignore
/// use eidetica::sync::transports::http::HttpTransport;
///
/// // Create transport with specific bind address
/// let builder = HttpTransport::builder()
///     .bind("127.0.0.1:8080");
///
/// // Register with sync system
/// sync.register_transport("http-local", builder).await?;
/// ```
#[derive(Debug, Clone, Default)]
pub struct HttpTransportBuilder {
    bind_address: Option<String>,
}

impl HttpTransportBuilder {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the bind address for the HTTP server.
    ///
    /// # Arguments
    /// * `addr` - The address to bind to (e.g., "127.0.0.1:8080" or "0.0.0.0:80")
    ///
    /// # Example
    ///
    /// ```ignore
    /// let builder = HttpTransport::builder()
    ///     .bind("0.0.0.0:8080");
    /// ```
    pub fn bind(mut self, addr: impl Into<String>) -> Self {
        self.bind_address = Some(addr.into());
        self
    }

    /// Build the transport synchronously (for backwards compatibility).
    ///
    /// Note: The bind address is stored but the server is not started.
    /// Call `start_server()` on the transport to actually bind.
    pub fn build_sync(self) -> Result<HttpTransport> {
        Ok(HttpTransport {
            server_state: ServerState::new(),
            bind_address: self.bind_address,
            client: build_client()?,
        })
    }
}

#[async_trait]
impl TransportBuilder for HttpTransportBuilder {
    type Transport = HttpTransport;

    /// Build the HTTP transport.
    ///
    /// HTTP transport doesn't require persisted state for identity.
    async fn build(self, _persisted: Doc) -> Result<(Self::Transport, Option<Doc>)> {
        let transport = HttpTransport {
            server_state: ServerState::new(),
            bind_address: self.bind_address,
            client: build_client()?,
        };
        Ok((transport, None))
    }
}

/// HTTP transport implementation using axum and reqwest.
pub struct HttpTransport {
    /// Shared server state management.
    server_state: ServerState,
    /// Configured bind address (used when start_server is called with empty addr)
    bind_address: Option<String>,
    /// Outbound HTTP client, shared across requests so its connection pool
    /// survives between sends rather than being rebuilt per request.
    client: reqwest::Client,
}

impl HttpTransport {
    /// Transport type identifier for HTTP
    pub const TRANSPORT_TYPE: &'static str = "http";

    /// Create a new HTTP transport instance.
    pub fn new() -> Result<Self> {
        Ok(Self {
            server_state: ServerState::new(),
            bind_address: None,
            client: build_client()?,
        })
    }

    /// Create a builder for configuring the transport.
    pub fn builder() -> HttpTransportBuilder {
        HttpTransportBuilder::new()
    }

    /// Build the peer-only v1 router, with a bounded rejection-only v0 route.
    /// Embedders use this same codec/handler seam rather than a legacy JSON RPC.
    pub fn router(handler: Arc<dyn SyncHandler>) -> Router {
        Router::new()
            .route("/api/v1", post(handle_sync_request))
            .route("/api/v0", post(reject_legacy_request))
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

    async fn start_server(&self, handler: Arc<dyn SyncHandler>) -> Result<()> {
        // No bind address configured = client-only transport, nothing to start
        let Some(effective_addr) = self.bind_address.as_deref() else {
            return Ok(());
        };

        let start = self.server_state.begin_start(effective_addr)?;

        let socket_addr: SocketAddr =
            effective_addr.parse().map_err(|e| SyncError::ServerBind {
                address: effective_addr.to_string(),
                reason: format!("Invalid address: {e}"),
            })?;

        let router = Self::router(handler);

        // Create server coordination channels
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        // Create a channel to get the actual bound address back
        let (addr_tx, addr_rx) = oneshot::channel::<SocketAddr>();

        let listener = match tokio::net::TcpListener::bind(socket_addr).await {
            Ok(listener) => listener,
            Err(error) => {
                return Err(SyncError::ServerBind {
                    address: effective_addr.to_string(),
                    reason: error.to_string(),
                }
                .into());
            }
        };
        let actual_addr = match listener.local_addr() {
            Ok(address) => address,
            Err(error) => {
                return Err(SyncError::ServerBind {
                    address: effective_addr.to_string(),
                    reason: error.to_string(),
                }
                .into());
            }
        };

        // Spawn server task
        tokio::spawn(async move {
            // Send the actual address back
            let _ = addr_tx.send(actual_addr);

            // Signal that server is ready
            let _ = ready_tx.send(());

            // Run server with graceful shutdown
            // Convert router to service with ConnectInfo support
            if let Err(error) = axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
            {
                tracing::error!(%error, "HTTP sync server failed");
            }
        });

        // Confirm the server task published the address it bound.
        let actual_addr = match addr_rx.await {
            Ok(address) => address,
            Err(_) => {
                return Err(SyncError::ServerBind {
                    address: effective_addr.to_string(),
                    reason: "Failed to get actual server address".to_string(),
                }
                .into());
            }
        };

        // Wait for server to be ready
        wait_for_ready(ready_rx, effective_addr).await?;

        // Start server state with address and shutdown sender
        start.complete(actual_addr.to_string(), shutdown_tx);

        Ok(())
    }

    async fn stop_server(&self) -> Result<()> {
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

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
        tokio::time::timeout_at(deadline, async {
            let mut upload_permit = None;
            let url = format!("http://{}/api/v1", address.address);
            let mut response = if matches!(request, SyncRequest::SendEntries(_)) {
                upload_permit = Some(framing::admit_outbound()?);
                // One bounded bridge lets an owned reqwest body be driven by
                // a borrowed input list, without a whole-transfer clone or a
                // detached producer task. Caller drop cancels both futures.
                let (reader, mut writer) = tokio::io::duplex(framing::MAX_FRAME_BYTES);
                let body = futures_util::stream::try_unfold(reader, |mut reader| async move {
                    let mut bytes = vec![0u8; 8192];
                    let n = reader.read(&mut bytes).await?;
                    bytes.truncate(n);
                    Ok::<_, std::io::Error>((n != 0).then_some((bytes, reader)))
                });
                let send = self
                    .client
                    .post(&url)
                    .header("content-type", "application/eidetica-peer-v1")
                    .body(reqwest::Body::wrap_stream(body))
                    .send();
                let write = async {
                    framing::write_request(&mut writer, request).await?;
                    writer.shutdown().await?;
                    Ok::<_, crate::Error>(())
                };
                let (_, response) = tokio::try_join!(write, async {
                    send.await.map_err(|e| {
                        timeout_error(&e, &address.address)
                            .unwrap_or_else(|| SyncError::ConnectionFailed {
                                address: address.address.clone(),
                                reason: e.to_string(),
                            })
                            .into()
                    })
                })?;
                response
            } else {
                // Control requests are one capped frame. In particular, a
                // silent address must not reserve a history collector slot
                // and starve the live winner in an existing address race.
                self.client
                    .post(&url)
                    .header("content-type", "application/eidetica-peer-v1")
                    .body(framing::encode(&framing::Frame::Request(request.clone()))?)
                    .send()
                    .await
                    .map_err(|e| {
                        timeout_error(&e, &address.address).unwrap_or_else(|| {
                            SyncError::ConnectionFailed {
                                address: address.address.clone(),
                                reason: e.to_string(),
                            }
                        })
                    })?
            };
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Err(SyncError::PeerProtocolUnsupported.into());
            }
            if !response.status().is_success() {
                return Err(SyncError::Network(format!(
                    "Server returned error: {}",
                    response.status()
                ))
                .into());
            }
            let _permit = match upload_permit {
                Some(permit) => permit,
                None => framing::admit_outbound()?,
            };
            let mut collector = framing::Collector::for_request(request, deadline)?;
            let mut decoder = framing::Decoder::default();
            while let Some(bytes) = response.chunk().await.map_err(|e| {
                timeout_error(&e, &address.address)
                    .unwrap_or_else(|| SyncError::Network(format!("Failed to read response: {e}")))
            })? {
                framing::receive_chunk(&mut decoder, &mut collector, &bytes)?;
            }
            collector.finish(decoder)
        })
        .await
        .map_err(|_| SyncError::Timeout {
            address: address.address.clone(),
            phase: TimeoutPhase::Request,
            elapsed: REQUEST_TIMEOUT,
        })?
    }

    fn is_server_running(&self) -> bool {
        self.server_state.is_running()
    }

    fn get_server_address(&self) -> Result<String> {
        self.server_state.get_address().map_err(|e| e.into())
    }
}

/// Reject legacy peers without producing or ingesting an unpaged history.
async fn reject_legacy_request(body: Body) -> Json<SyncResponse> {
    let _ = tokio::time::timeout(
        REQUEST_TIMEOUT,
        axum::body::to_bytes(body, framing::MAX_FRAME_BYTES),
    )
    .await;
    Json(SyncResponse::Error(
        "Unsupported peer protocol v0; required v1".into(),
    ))
}

async fn handle_sync_request(
    State(handler): State<Arc<dyn SyncHandler>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    body: Body,
) -> Body {
    use futures_util::StreamExt;
    let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
    let request = tokio::time::timeout_at(deadline, async {
        let _permit = framing::admit_operation()?;
        let mut body = body.into_data_stream();
        let mut decoder = framing::Decoder::default();
        let mut collector = framing::RequestCollector::new(deadline);
        while let Some(bytes) = body.next().await {
            let bytes = bytes.map_err(|e| SyncError::Network(e.to_string()))?;
            framing::receive_request_chunk(&mut decoder, &mut collector, &bytes)?;
        }
        collector.finish(decoder)
    })
    .await
    .unwrap_or_else(|_| Err(SyncError::Network("peer request deadline".into()).into()));
    match request {
        Ok(request) => Body::from_stream(
            framing::response_bytes(handler, request, Address::http(addr.to_string()), deadline)
                .await,
        ),
        Err(error) => {
            Body::from(framing::encode(&framing::error_frame(error)).expect("bounded error frame"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;

    use tokio::net::TcpListener;

    use super::*;

    /// A peer that accepts the connection and then says nothing is reported as
    /// a timeout, in the phase that records it was reachable. This is the
    /// classification the tree walk and every log reader act on, and it rests
    /// on how the HTTP client reports a deadline, so it is pinned here rather
    /// than assumed.
    #[tokio::test]
    async fn a_peer_that_accepts_and_says_nothing_times_out_in_the_request_phase() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();

        // Accept the connection and hold it open without answering.
        tokio::spawn(async move {
            let _accepted = listener.accept().await;
            pending::<()>().await;
        });

        let client = reqwest::Client::builder()
            .read_timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        let error = client
            .post(format!("http://{address}/api/v0"))
            .send()
            .await
            .expect_err("a peer that never answers cannot produce a response");

        match timeout_error(&error, &address) {
            Some(SyncError::Timeout { phase, .. }) => assert_eq!(phase, TimeoutPhase::Request),
            other => panic!("expected a request-phase timeout, got {other:?}"),
        }
    }

    /// A refusal is not a timeout: nothing is listening, so the connection is
    /// rejected rather than left waiting, and a caller must be able to tell the
    /// two apart without reading the message.
    #[tokio::test]
    async fn a_refused_connection_is_not_reported_as_a_timeout() {
        // Bind and drop, so the port is known to have no listener on it.
        let address = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().to_string()
        };

        let client = build_client().unwrap();
        let error = client
            .post(format!("http://{address}/api/v0"))
            .send()
            .await
            .expect_err("nothing is listening on that port");

        assert!(timeout_error(&error, &address).is_none());
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    #[tokio::test]
    async fn peer_paging_http_v0_only_peer_is_explicitly_unsupported() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = Address::http(listener.local_addr().unwrap().to_string());
        let (shutdown, stop) = oneshot::channel();
        let server = tokio::spawn(async move {
            let router = Router::new().route("/api/v0", post(|| async { Json(SyncResponse::Ack) }));
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = stop.await;
                })
                .await
                .unwrap();
        });
        let error = HttpTransport::new()
            .unwrap()
            .send_request(&address, &SyncRequest::SendEntries(vec![]))
            .await
            .unwrap_err();
        assert!(
            matches!(error, crate::Error::Sync(error) if matches!(*error, SyncError::PeerProtocolUnsupported))
        );
        shutdown.send(()).unwrap();
        tokio::time::timeout(REQUEST_TIMEOUT, server)
            .await
            .unwrap()
            .unwrap();
    }
}
