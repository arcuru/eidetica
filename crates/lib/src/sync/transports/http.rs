//! HTTP transport implementation for sync communication.
//!
//! This module provides HTTP-based sync communication using a single
//! JSON sync endpoint (`/api/v0`) and separate named-blob endpoints, using axum
//! for the server and a shared reqwest client for outbound requests.

use std::{net::SocketAddr, ops::Range, sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{
    Router,
    extract::{ConnectInfo, Json as ExtractJson, State},
    http::{StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Json, Response},
    routing::post,
};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::{SyncTransport, TransportBuilder, TransportConfig, shared::*};
use crate::{
    Result,
    crdt::Doc,
    entry::ID,
    store::Registered,
    sync::{
        error::{SyncError, TimeoutPhase},
        handler::SyncHandler,
        peer_types::Address,
        protocol::{
            BlobAvailabilityRequest, BlobAvailabilityResponse, BlobRangeRequest, RequestContext,
            SyncRequest, SyncResponse,
        },
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
/// run outside the background engine's command loop; the silence deadline
/// prevents an unresponsive peer from holding those tasks indefinitely.
///
/// This bounds silence rather than the whole exchange, because the two are not
/// the same request. A bootstrap response carries an entire database, so a
/// deadline on the exchange would fail a transfer that is making progress on a
/// slow link or against a large tree, while adding nothing against the peer
/// that has stopped sending.
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

/// Slack added to the requested range length when bounding the blob-range
/// response read: the bao stream adds a small size prefix plus parent hashes
/// (≈0.4% of the range + a per-request path overhead). 1 MiB dwarfs the path
/// overhead for any realistic blob, so a response larger than this is a
/// misbehaving peer and is rejected rather than buffered unbounded.
const BAO_STREAM_SLACK: u64 = 1024 * 1024;

// Distinguish a blob miss from an older peer's missing HTTP endpoint.
const BLOB_PROTOCOL_HEADER: &str = "x-eidetica-blob";

/// Most bytes a verified stream for `range` may occupy on the wire: the range
/// (capped at the blob limit) plus bao overhead. Shared by every transport so
/// none of them buffers an unbounded response.
pub(super) fn blob_stream_budget(range: &Range<u64>) -> u64 {
    range
        .end
        .saturating_sub(range.start)
        .min(crate::backend::DEFAULT_MAX_BLOB_BYTES as u64)
        .saturating_add(BAO_STREAM_SLACK)
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

    /// Create the axum router with single JSON endpoint and handler state.
    fn create_router(handler: Arc<dyn SyncHandler>) -> Router {
        Router::new()
            .route("/api/v0", post(handle_sync_request))
            .route("/api/v0/blob", post(handle_blob_request))
            .route("/api/v0/blob/has", post(handle_blob_availability))
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

        let router = Self::create_router(handler);

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

        let url = format!("http://{}/api/v0", address.address);

        let response = self
            .client
            .post(&url)
            .json(&request) // Send SyncRequest as JSON body
            .send()
            .await
            .map_err(|e| {
                timeout_error(&e, &address.address).unwrap_or_else(|| SyncError::ConnectionFailed {
                    address: address.address.clone(),
                    reason: e.to_string(),
                })
            })?;

        if !response.status().is_success() {
            return Err(SyncError::Network(format!(
                "Server returned error: {}",
                response.status()
            ))
            .into());
        }

        // The response body is read here rather than above, so a peer that
        // stops sending partway through one runs out of time here too. Without
        // this the stall would be reported as a malformed response, which is a
        // different fault with a different remedy.
        let sync_response: SyncResponse = response.json().await.map_err(|e| {
            timeout_error(&e, &address.address)
                .unwrap_or_else(|| SyncError::Network(format!("Failed to parse response: {e}")))
        })?;

        Ok(sync_response)
    }

    async fn fetch_blob_range(
        &self,
        address: &Address,
        cid: &ID,
        range: Range<u64>,
    ) -> Result<Option<Vec<u8>>> {
        crate::backend::validate_blob_cid(cid)?;
        if !self.can_handle_address(address) {
            return Err(SyncError::UnsupportedTransport {
                transport_type: address.transport_type.clone(),
            }
            .into());
        }

        let url = format!("http://{}/api/v0/blob", address.address);
        let req = BlobRangeRequest {
            cid: cid.clone(),
            start: range.start,
            end: range.end,
        };

        let mut response = self
            .client
            .post(&url)
            .json(&req)
            .send()
            .await
            .map_err(|e| {
                timeout_error(&e, &address.address).unwrap_or_else(|| SyncError::ConnectionFailed {
                    address: address.address.clone(),
                    reason: e.to_string(),
                })
            })?;

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return if response
                .headers()
                .get(BLOB_PROTOCOL_HEADER)
                .and_then(|v| v.to_str().ok())
                == Some("0")
            {
                Ok(None)
            } else {
                Err(SyncError::UnsupportedTransport {
                    transport_type: "http blob/v0".into(),
                }
                .into())
            };
        }
        if !response.status().is_success() {
            return Err(SyncError::Network(format!(
                "Server returned error: {}",
                response.status()
            ))
            .into());
        }

        // Bounded read: the verified stream for this range is at most the range
        // length (capped at the blob limit) plus bao overhead. A chunk-by-chunk
        // budget bounds memory even if the peer misbehaves. This is the
        // *only* peer blob-fetch path — whole-blob fetches ride it with a
        // `0..cap` range — so the receive is always bounded (no unbounded
        // whole-blob buffering, the memory-DoS that the removed JSON blob path
        // had).
        let budget = blob_stream_budget(&range);
        let mut buf: Vec<u8> = Vec::new();
        // A peer that stalls mid-stream hits the shared read timeout here and
        // is reported as a timeout, as for `send_request`.
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            timeout_error(&e, &address.address)
                .unwrap_or_else(|| SyncError::Network(format!("Failed to read blob stream: {e}")))
        })? {
            if buf.len() as u64 + chunk.len() as u64 > budget {
                return Err(crate::backend::errors::BackendError::BlobStreamInvalid {
                    cid: cid.clone(),
                }
                .into());
            }
            buf.extend_from_slice(&chunk);
        }

        // Decode + verify against the CID; tampered/wrong bytes are rejected.
        let bytes = crate::blob::bao::decode_range(cid, range, &buf)?;
        Ok(Some(bytes))
    }

    async fn query_blobs(&self, address: &Address, cids: &[ID]) -> Result<Vec<bool>> {
        if !self.can_handle_address(address) {
            return Err(SyncError::UnsupportedTransport {
                transport_type: address.transport_type.clone(),
            }
            .into());
        }
        crate::sync::handler::check_blob_availability_request(cids)?;
        let url = format!("http://{}/api/v0/blob/has", address.address);
        let mut response = self
            .client
            .post(&url)
            .json(&BlobAvailabilityRequest {
                cids: cids.to_vec(),
            })
            .send()
            .await
            .map_err(|e| {
                timeout_error(&e, &address.address).unwrap_or_else(|| SyncError::ConnectionFailed {
                    address: address.address.clone(),
                    reason: e.to_string(),
                })
            })?;
        if !response.status().is_success() {
            return Err(SyncError::Network(format!(
                "Server returned error: {}",
                response.status()
            ))
            .into());
        }
        let mut payload = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            timeout_error(&e, &address.address).unwrap_or_else(|| SyncError::Network(e.to_string()))
        })? {
            if payload.len().saturating_add(chunk.len()) > 64 * 1024 {
                return Err(SyncError::InvalidData(
                    "blob availability response exceeded its receive budget".into(),
                )
                .into());
            }
            payload.extend_from_slice(&chunk);
        }
        let answer: BlobAvailabilityResponse = serde_json::from_slice(&payload)
            .map_err(|e| SyncError::SerializationError(e.to_string()))?;
        check_availability_answer(cids, answer)
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

/// Handler for `/api/v0/blob` — serves a verified bao stream of a blob range as
/// `application/octet-stream`. `404` when the blob isn't held; `500` on error.
/// Blob reads are global and deliberately unauthorized.
async fn handle_blob_request(
    State(handler): State<Arc<dyn SyncHandler>>,
    ExtractJson(req): ExtractJson<BlobRangeRequest>,
) -> Response {
    match handler.serve_blob_range(&req.cid, req.start..req.end).await {
        Ok(Some(stream)) => ([(CONTENT_TYPE, "application/octet-stream")], stream).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, [(BLOB_PROTOCOL_HEADER, "0")]).into_response(),
        Err(e) => {
            tracing::warn!(cid = %req.cid, error = %e, "Failed to serve blob range");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Handler for `/api/v0/blob/has` — a blind availability answer covering
/// exactly the requested CIDs. `400` for an oversized or non-blob query.
async fn handle_blob_availability(
    State(handler): State<Arc<dyn SyncHandler>>,
    ExtractJson(req): ExtractJson<BlobAvailabilityRequest>,
) -> Response {
    if crate::sync::handler::check_blob_availability_request(&req.cids).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match handler.blob_availability(&req.cids).await {
        Ok(available) => Json(BlobAvailabilityResponse { available }).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to answer blob availability");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Accept an availability answer only if it covers exactly the request.
pub(super) fn check_availability_answer(
    cids: &[ID],
    answer: BlobAvailabilityResponse,
) -> Result<Vec<bool>> {
    if answer.available.len() != cids.len() {
        return Err(SyncError::UnexpectedResponse {
            expected: "one availability answer per requested CID",
            actual: format!("{} answers for {} CIDs", answer.available.len(), cids.len()),
        }
        .into());
    }
    Ok(answer.available)
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

    /// A blob-range fetch rides the transport's shared client, so a peer that
    /// starts a blob stream and then goes quiet is reported as a request-phase
    /// timeout rather than as a malformed stream.
    #[tokio::test]
    async fn a_peer_that_stalls_mid_blob_stream_times_out_in_the_request_phase() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();

        // Answer with headers and a partial body, then hold the connection.
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 1024\r\n\r\npartial")
                .await
                .unwrap();
            pending::<()>().await;
        });

        let transport = HttpTransport {
            server_state: ServerState::new(),
            bind_address: None,
            client: reqwest::Client::builder()
                .read_timeout(Duration::from_millis(200))
                .build()
                .unwrap(),
        };
        let cid = ID::from_bytes(b"blob");
        let error = transport
            .fetch_blob_range(&Address::http(address), &cid, 0..4)
            .await
            .expect_err("a stalled blob stream cannot complete");

        match error {
            crate::Error::Sync(sync) => match *sync {
                SyncError::Timeout { phase, .. } => assert_eq!(phase, TimeoutPhase::Request),
                other => panic!("expected a request-phase timeout, got {other:?}"),
            },
            other => panic!("expected a sync error, got {other:?}"),
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
mod blob_budget_tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn blob_range_with_unbounded_end_still_has_a_receive_cap() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = Address::http(listener.local_addr().unwrap().to_string());
        // An intentionally invalid stream whose zero size would decode as
        // empty. The receive budget must reject it before Bao is consulted.
        let body =
            vec![0u8; crate::backend::DEFAULT_MAX_BLOB_BYTES + BAO_STREAM_SLACK as usize + 1];
        let router = Router::new().route("/api/v0/blob", post(move || async move { body }));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let transport = HttpTransport {
            server_state: ServerState::new(),
            bind_address: None,
            client: build_client().unwrap(),
        };
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            transport.fetch_blob_range(&address, &ID::from_bytes(b"blob"), 0..u64::MAX),
        )
        .await;
        server.abort();
        let error = result
            .expect("bounded receive finishes")
            .expect_err("over-cap body must be refused");
        assert!(
            matches!(error, crate::Error::Backend(ref e) if matches!(**e, crate::backend::errors::BackendError::BlobStreamInvalid { .. })),
            "{error:?}"
        );
    }
}
