//! HTTP transport implementation for sync communication.
//!
//! This module provides HTTP-based sync communication using a single
//! binary endpoint ([`SYNC_PATH`]) with axum for the server and reqwest for the
//! client. Bodies carry the common message envelope and are streamed in both
//! directions, so neither side collects a whole message.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::oneshot,
};

use super::{SyncTransport, TransportBuilder, TransportConfig, shared::*};
use crate::{
    Result,
    crdt::Doc,
    message_io::{
        ProgressMonitor, ReadOptions, TransferBudget, TransferError, into_body_stream,
        prepare_message, read_message, reader_for_stream,
    },
    store::Registered,
    sync::{
        error::{SyncError, TimeoutPhase},
        handler::SyncHandler,
        peer_types::Address,
        protocol::{RequestContext, SyncRequest, SyncResponse},
    },
};

/// The sync endpoint. Its version is the wire version: a peer on another version
/// has no such route, which is a compatibility failure and never a cue to resend
/// an operation some other way.
pub const SYNC_PATH: &str = "/api/v1";

/// Media type of a message on this endpoint.
pub const SYNC_MEDIA_TYPE: &str = "application/vnd.eidetica.cbor;version=1";

/// How much of a refused body the server discards, and for how long, so the
/// refusal is not lost to a connection reset while the client is still sending.
/// Bounded: refusing a body must never mean reading all of it.
const REFUSED_BODY_DISCARD_BYTES: usize = 8 * 1024 * 1024;
const REFUSED_BODY_DISCARD_TIME: Duration = Duration::from_secs(1);

/// Maximum time spent establishing a TCP connection to a peer.
///
/// A peer that is down or firewalled is detected within this bound rather than
/// at the operating system's connect timeout, which can run into minutes.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Build the outbound HTTP client.
///
/// Time after connecting belongs to the message progress policy: reqwest's read
/// timeout would also fire during a healthy upload longer than itself. Redirects
/// are off so a request is never replayed to another address, and content
/// encodings stay off because they would hide the bytes admission accounts for.
fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| SyncError::Network(format!("Failed to build HTTP client: {e}")).into())
}

/// The timeout a failed request represents, or `None` if it failed some other
/// way.
///
/// The only deadline reqwest itself enforces here is the connection's. A peer
/// that never completed the connection may not be there, which is a different
/// statement from one that accepted it and then went quiet.
fn timeout_error(error: &reqwest::Error, address: &str) -> Option<SyncError> {
    (error.is_timeout() && error.is_connect()).then(|| SyncError::Timeout {
        address: address.to_string(),
        phase: TimeoutPhase::Connect,
        elapsed: CONNECT_TIMEOUT,
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
    budget: TransferBudget,
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

    /// Set the transfer budget, shared by this transport's sends and serving.
    ///
    /// Give several transports a clone of one budget to share its limits.
    pub fn transfer_budget(mut self, budget: TransferBudget) -> Self {
        self.budget = budget;
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
            budget: self.budget,
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
            budget: self.budget,
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
    /// Admission and limits shared by every message on this transport.
    budget: TransferBudget,
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
            budget: TransferBudget::default(),
        })
    }

    /// Create a builder for configuring the transport.
    pub fn builder() -> HttpTransportBuilder {
        HttpTransportBuilder::new()
    }

    /// Create the axum router with its single endpoint and handler state.
    fn create_router(handler: Arc<dyn SyncHandler>, budget: TransferBudget) -> Router {
        Router::new()
            .route(SYNC_PATH, post(handle_sync_request))
            .with_state((handler, budget))
    }

    /// Stream one request to `url` and read its response.
    async fn exchange(
        &self,
        address: &str,
        request: SyncRequest,
    ) -> std::result::Result<SyncResponse, SyncError> {
        let budget = &self.budget;
        let failed = |error: TransferError| error.into_sync_error(address);

        let message = prepare_message(request, budget).await.map_err(failed)?;
        let stream_bytes = message.stream_bytes();
        let monitor = Arc::new(ProgressMonitor::new(budget.policy()));
        let body = reqwest::Body::wrap_stream(into_body_stream(message, monitor.clone(), true));

        let send = self
            .client
            .post(format!("http://{address}{SYNC_PATH}"))
            .header(header::CONTENT_TYPE, SYNC_MEDIA_TYPE)
            .header(header::ACCEPT, SYNC_MEDIA_TYPE)
            .header(header::CONTENT_LENGTH, stream_bytes)
            .body(body)
            .send();
        let response = tokio::select! {
            response = send => response.map_err(|e| {
                timeout_error(&e, address).unwrap_or_else(|| SyncError::ConnectionFailed {
                    address: address.to_string(),
                    reason: e.to_string(),
                })
            })?,
            failure = monitor.expired() => return Err(failed(failure)),
        };
        monitor.response_started();

        match response.status() {
            status if status.is_success() => {}
            StatusCode::PAYLOAD_TOO_LARGE => {
                // The status is sufficient. An untrusted error body must not
                // bypass message admission by being collected as text.
                return Err(SyncError::ResourceLimit(
                    "peer refused the request under its resource policy".to_string(),
                ));
            }
            status => {
                return Err(SyncError::Network(format!(
                    "Server returned error: {status}"
                )));
            }
        }
        if !monitor.is_body_complete() {
            return Err(SyncError::Network(
                "peer answered before the request body finished".to_string(),
            ));
        }
        if !is_sync_media(response.headers()) {
            return Err(SyncError::Network(
                "Response is not a sync message".to_string(),
            ));
        }

        let declared_stream_bytes = response.content_length();
        let chunks = stream::unfold(response, |mut response| async move {
            match response.chunk().await {
                Ok(Some(chunk)) => Some((Ok(chunk), response)),
                Ok(None) => None,
                Err(error) => Some((Err(error), response)),
            }
        });
        let mut reader = reader_for_stream(chunks.boxed());
        let options = ReadOptions {
            require_end: true,
            declared_stream_bytes,
            first_byte_timeout: true,
            ..Default::default()
        };
        let inbound = read_message::<_, SyncResponse>(&mut reader, budget, options)
            .await
            .map_err(failed)?
            .ok_or_else(|| failed(TransferError::Truncated))?;
        Ok(inbound.value)
    }
}

fn is_sync_media(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .is_some_and(|value| value == SYNC_MEDIA_TYPE)
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

        let router = Self::create_router(handler, self.budget.clone());

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

    /// Compatibility adapter. It must clone `request`, so its memory cost is
    /// the caller's choice; prefer [`SyncTransport::send_request_owned`].
    async fn send_request(&self, address: &Address, request: &SyncRequest) -> Result<SyncResponse> {
        self.send_request_owned(address, request.clone()).await
    }

    async fn send_request_owned(
        &self,
        address: &Address,
        request: SyncRequest,
    ) -> Result<SyncResponse> {
        if !self.can_handle_address(address) {
            return Err(SyncError::UnsupportedTransport {
                transport_type: address.transport_type.clone(),
            }
            .into());
        }

        let exchange = self.exchange(&address.address, request);
        let response = match self.budget.policy().overall_timeout {
            None => exchange.await?,
            Some(elapsed) => tokio::time::timeout(elapsed, exchange)
                .await
                .map_err(|_| {
                    TransferError::OverallTimeout { elapsed }.into_sync_error(&address.address)
                })??,
        };
        Ok(response)
    }

    fn is_server_running(&self) -> bool {
        self.server_state.is_running()
    }

    fn get_server_address(&self) -> Result<String> {
        self.server_state.get_address().map_err(|e| e.into())
    }
}

/// Handler for the sync endpoint on this transport's own server.
async fn handle_sync_request(
    State((handler, budget)): State<(Arc<dyn SyncHandler>, TransferBudget)>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    serve_sync_request(handler.as_ref(), &budget, addr, request).await
}

/// Serve one streamed sync request.
///
/// Other HTTP servers embedding the sync endpoint at [`SYNC_PATH`] use this too,
/// so every server applies one admission and streaming behavior.
pub async fn serve_sync_request(
    handler: &dyn SyncHandler,
    budget: &TransferBudget,
    addr: SocketAddr,
    request: Request,
) -> Response {
    if !is_sync_media(request.headers()) {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let declared_stream_bytes = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok());

    let mut reader = reader_for_stream(request.into_body().into_data_stream());
    let options = ReadOptions {
        require_end: true,
        declared_stream_bytes,
        first_byte_timeout: true,
        ..Default::default()
    };
    let inbound = match read_message::<_, SyncRequest>(&mut reader, budget, options).await {
        Ok(Some(inbound)) => inbound,
        Ok(None) => return StatusCode::BAD_REQUEST.into_response(),
        Err(refusal) if refusal.is_resource() => {
            tokio::spawn(discard_refused_body(reader));
            return transfer_failure(refusal);
        }
        Err(error) => return transfer_failure(error),
    };
    drop(reader);
    let request = inbound.value;

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

    let response = handler.handle_request(&request, &context).await;
    drop(request);
    drop(inbound.lease);

    let message = match prepare_message(response, budget).await {
        Ok(message) => message,
        Err(error) => return transfer_failure(error),
    };
    let stream_bytes = message.stream_bytes();
    let monitor = Arc::new(ProgressMonitor::new(budget.policy()));
    let body = Body::from_stream(into_body_stream(message, monitor, false));
    (
        [
            (header::CONTENT_TYPE, SYNC_MEDIA_TYPE.to_string()),
            (header::CONTENT_LENGTH, stream_bytes.to_string()),
        ],
        body,
    )
        .into_response()
}

/// Read and discard part of a refused body while its refusal is sent.
async fn discard_refused_body(mut reader: impl AsyncRead + Unpin) {
    let mut sink = vec![0u8; 16 * 1024];
    let mut left = REFUSED_BODY_DISCARD_BYTES;
    let discard = async {
        while left > 0 {
            match reader.read(&mut sink).await {
                Ok(read) if read > 0 => left = left.saturating_sub(read),
                _ => break,
            }
        }
    };
    let _ = tokio::time::timeout(REFUSED_BODY_DISCARD_TIME, discard).await;
}

/// A failed message as a small response. A refusal stays explicit so the peer
/// can tell it from a fault.
fn transfer_failure(error: TransferError) -> Response {
    let status = match error {
        TransferError::Resource { .. } => StatusCode::PAYLOAD_TOO_LARGE,
        TransferError::Malformed(_) | TransferError::Truncated => StatusCode::BAD_REQUEST,
        TransferError::Stalled { .. }
        | TransferError::InsufficientProgress { .. }
        | TransferError::ResponseTimeout { .. } => StatusCode::REQUEST_TIMEOUT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, error.to_string()).into_response()
}

#[cfg(test)]
mod tests {
    use std::future::pending;

    use tokio::net::TcpListener;

    use super::*;
    use crate::message_io::TransferPolicy;

    /// A peer that accepts the connection and then says nothing is reported as
    /// a timeout, in the phase that records it was reachable. This is the
    /// classification the tree walk and every log reader act on.
    #[tokio::test]
    async fn a_peer_that_accepts_and_says_nothing_times_out_in_the_request_phase() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();

        // Accept the connection, read the request and never answer.
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut sink = [0u8; 1024];
            while tokio::io::AsyncReadExt::read(&mut stream, &mut sink)
                .await
                .is_ok_and(|read| read > 0)
            {}
            pending::<()>().await;
        });

        let policy = TransferPolicy {
            first_response_timeout: Duration::from_millis(200),
            ..Default::default()
        };
        let transport = HttpTransport::builder()
            .transfer_budget(TransferBudget::new(policy).unwrap())
            .build_sync()
            .unwrap();
        let error = transport
            .send_request_owned(&Address::http(&address), SyncRequest::SendEntries(vec![]))
            .await
            .expect_err("a peer that never answers cannot produce a response");

        match error {
            crate::Error::Sync(error) => match *error {
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
            .post(format!("http://{address}{SYNC_PATH}"))
            .send()
            .await
            .expect_err("nothing is listening on that port");

        assert!(timeout_error(&error, &address).is_none());
    }
    /// A refusal must be observed without collecting the peer's arbitrary
    /// status-body (which need not end, even after the upload is refused).
    #[tokio::test]
    async fn early_resource_refusal_does_not_read_an_unbounded_error_body() {
        use tokio::io::AsyncWriteExt;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream
                .write_all(b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 999999999\r\n\r\n")
                .await
                .unwrap();
            pending::<()>().await;
        });
        let policy = TransferPolicy {
            overall_timeout: Some(Duration::from_millis(500)),
            ..Default::default()
        };
        let transport = HttpTransport::builder()
            .transfer_budget(TransferBudget::new(policy).unwrap())
            .build_sync()
            .unwrap();
        let refused = transport
            .send_request_owned(&Address::http(address), SyncRequest::SendEntries(vec![]))
            .await;
        peer.abort();
        let _ = peer.await;
        assert!(
            refused.as_ref().is_err_and(crate::Error::is_resource_limit),
            "{refused:?}"
        );
    }

    /// A client that stops reading a response stops hyper polling the body, so
    /// only the monitor can see it. The worker must not hold admission forever.
    #[tokio::test]
    async fn a_client_that_stops_reading_a_response_frees_server_admission() {
        use tokio::io::AsyncWriteExt;

        struct LargeAnswer;
        #[async_trait]
        impl SyncHandler for LargeAnswer {
            async fn handle_request(&self, _: &SyncRequest, _: &RequestContext) -> SyncResponse {
                SyncResponse::Error("x".repeat(64 * 1024 * 1024))
            }
        }

        let policy = TransferPolicy {
            stall_timeout: Duration::from_millis(200),
            ..Default::default()
        };
        let budget = TransferBudget::new(policy).unwrap();
        let server = HttpTransport::builder()
            .bind("127.0.0.1:0")
            .transfer_budget(budget.clone())
            .build_sync()
            .unwrap();
        server.start_server(Arc::new(LargeAnswer)).await.unwrap();
        let address = server.get_server_address().unwrap();

        let body = cbor4ii::serde::to_vec(Vec::new(), &SyncRequest::SendEntries(vec![])).unwrap();
        let mut client = tokio::net::TcpStream::connect(&address).await.unwrap();
        let head = format!(
            "POST {SYNC_PATH} HTTP/1.1\r\nHost: {address}\r\nContent-Type: {SYNC_MEDIA_TYPE}\r\n\
             Content-Length: {}\r\n\r\n",
            body.len() + 8
        );
        client.write_all(head.as_bytes()).await.unwrap();
        client
            .write_all(&(body.len() as u64).to_be_bytes())
            .await
            .unwrap();
        client.write_all(&body).await.unwrap();

        let saw_response = tokio::time::timeout(Duration::from_secs(5), async {
            while budget.usage().outbound_messages == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
        saw_response.await.expect("the response began");
        tokio::time::timeout(Duration::from_secs(10), async {
            while budget.usage().outbound_messages != 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("a stalled response must release its worker's admission");
        assert_eq!(budget.usage().reserved_bytes, 0);
        drop(client);
        server.stop_server().await.unwrap();
    }

    #[tokio::test]
    async fn truncated_ack_must_not_succeed() {
        use tokio::io::AsyncWriteExt;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let peer = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(socket.read_u8().await.unwrap());
                assert!(headers.len() < 8192);
            }
            let request = read_message::<_, SyncRequest>(
                &mut socket,
                &TransferBudget::default(),
                ReadOptions::default(),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(matches!(request.value, SyncRequest::SendEntries(_)));
            let malformed = [0xa1, 0x63, b'A', b'c', b'k'];
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {SYNC_MEDIA_TYPE}\r\nContent-Length: 13\r\nConnection: close\r\n\r\n"
            );
            socket.write_all(header.as_bytes()).await.unwrap();
            socket.write_all(&5u64.to_be_bytes()).await.unwrap();
            socket.write_all(&malformed).await.unwrap();
        };
        let transport = HttpTransport::new().unwrap();
        let address = Address::http(address);
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(transport.send_owned_entries(&address, vec![]), peer)
        })
        .await
        .unwrap();
        assert!(
            result.is_err(),
            "malformed acknowledgement returned success: {result:?}"
        );
    }

    #[tokio::test]
    async fn early_success_cannot_excuse_an_incomplete_upload() {
        use tokio::io::AsyncWriteExt;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let body = cbor4ii::serde::to_vec(Vec::new(), &SyncResponse::Ack).unwrap();
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {SYNC_MEDIA_TYPE}\r\nContent-Length: {}\r\n\r\n",
                body.len() + 8
            );
            stream.write_all(header.as_bytes()).await.unwrap();
            stream
                .write_all(&(body.len() as u64).to_be_bytes())
                .await
                .unwrap();
            stream.write_all(&body).await.unwrap();
            pending::<()>().await;
        });
        let entry = crate::Entry::root_builder()
            .set_subtree_data("data", "x".repeat(32 * 1024 * 1024))
            .build()
            .unwrap();
        let transport = HttpTransport::new().unwrap();
        let result = transport
            .send_owned_entries(&Address::http(address), vec![entry])
            .await;
        peer.abort();
        let _ = peer.await;
        assert!(result.is_err(), "an incomplete upload cannot succeed");
    }
}
