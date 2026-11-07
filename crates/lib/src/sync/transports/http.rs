//! HTTP transport implementation for sync communication.
//!
//! This module provides HTTP-based sync communication using a single
//! framed endpoint (/api/v1) with axum for the server and reqwest for the client.

use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

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

impl HttpTransport {
    async fn send_framed(
        &self,
        address: &Address,
        request: &SyncRequest,
        source: Option<&framing::UploadSource<'_>>,
    ) -> Result<SyncResponse> {
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
            let mut response = if source.is_some() || matches!(request, SyncRequest::SendEntries(_))
            {
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
                    match source {
                        Some(source) => {
                            framing::write_upload(&mut writer, source, deadline).await?
                        }
                        None => framing::write_request(&mut writer, request).await?,
                    }
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
        self.send_framed(address, request, None).await
    }

    async fn send_upload(
        &self,
        address: &Address,
        source: &framing::UploadSource<'_>,
    ) -> Result<SyncResponse> {
        self.send_framed(address, &SyncRequest::SendEntries(vec![]), Some(source))
            .await
    }

    async fn send_entries(&self, address: &Address, entries: &[crate::Entry]) -> Result<()> {
        framing::acknowledged(
            self.send_upload(address, &framing::UploadSource::Entries(entries))
                .await?,
        )
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

/// The body owns its driver, including while Hyper stops polling for socket
/// backpressure. At most one encoded frame is queued and one is being produced
/// or sent; the existing absolute deadline covers both production and sends.
struct HttpResponse {
    receiver: tokio::sync::mpsc::Receiver<Result<Vec<u8>>>,
    producer: Option<tokio::task::JoinHandle<Result<()>>>,
}

impl futures_util::Stream for HttpResponse {
    type Item = Result<Vec<u8>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.receiver.poll_recv(cx) {
            Poll::Ready(None) => {}
            result => return result,
        }
        let Some(producer) = self.producer.as_mut() else {
            return Poll::Ready(None);
        };
        let result = match Pin::new(producer).poll(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(result) => result,
        };
        self.producer = None;
        // A timeout or panic is not clean EOF, even if End was already queued.
        Poll::Ready(match result {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(Err(error)),
            Err(error) => Some(Err(SyncError::Network(format!(
                "HTTP producer failed: {error}"
            ))
            .into())),
        })
    }
}

impl Drop for HttpResponse {
    fn drop(&mut self) {
        if let Some(producer) = &self.producer {
            producer.abort();
        }
    }
}

fn response_body(
    mut response: impl futures_util::Stream<Item = Result<Vec<u8>>> + Send + Unpin + 'static,
    deadline: tokio::time::Instant,
) -> Body {
    use futures_util::StreamExt;
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let producer = tokio::spawn(async move {
        tokio::time::timeout_at(deadline, async move {
            while let Some(bytes) = response.next().await {
                if sender.send(bytes).await.is_err() {
                    break;
                }
            }
        })
        .await
        .map_err(|_| SyncError::Network("peer exchange deadline".into()).into())
    });
    Body::from_stream(HttpResponse {
        receiver,
        producer: Some(producer),
    })
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
        Ok(request) => response_body(
            framing::response_bytes(handler, request, Address::http(addr.to_string()), deadline)
                .await,
            deadline,
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
mod lifetime_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures_util::StreamExt;
    use tokio::sync::Notify;

    use super::*;
    use crate::{
        Instance, NewAccount, Snapshot,
        account::types::SyncSettings,
        auth::{AuthKey, Permission},
        backend::database::InMemory,
        sync::{
            handler::SyncHandlerImpl,
            protocol::{RequestContext, SyncTreeRequest},
        },
    };

    struct ProducerDrop(Arc<Notify>);
    impl Drop for ProducerDrop {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }

    // Observe a real HistoryProducer and its admission permit, not a stand-in
    // timer. Exercise both a pending native producer and a blocked frame send.
    struct ObservedHistory {
        inner: Arc<SyncHandlerImpl>,
        dropped: Arc<Notify>,
        produced: Arc<AtomicUsize>,
        stall: bool,
    }

    #[async_trait]
    impl SyncHandler for ObservedHistory {
        async fn handle_request(&self, _: &SyncRequest, _: &RequestContext) -> SyncResponse {
            panic!("framed HTTP must not use the legacy handler")
        }

        async fn response_frames(
            self: Arc<Self>,
            request: SyncRequest,
            context: RequestContext,
            deadline: tokio::time::Instant,
        ) -> Result<framing::FrameStream> {
            let frames = self
                .inner
                .clone()
                .response_frames(request, context, deadline)
                .await?;
            let guard = ProducerDrop(self.dropped.clone());
            Ok(Box::pin(futures_util::stream::unfold(
                (frames, guard),
                move |(mut frames, guard)| {
                    let observer = self.clone();
                    async move {
                        if observer.stall && observer.produced.load(Ordering::SeqCst) == 1 {
                            std::future::pending::<()>().await;
                        }
                        let frame = frames.next().await?;
                        observer.produced.fetch_add(1, Ordering::SeqCst);
                        Some((frame, (frames, guard)))
                    }
                },
            )))
        }
    }

    async fn history_body(stall: bool) -> (Instance, SyncRequest, Arc<ObservedHistory>, Body) {
        let (instance, mut owner) = Instance::create_backend(
            Box::new(InMemory::new()),
            NewAccount::passwordless("http-lifetime"),
        )
        .await
        .unwrap();
        let key = owner.add_private_key(Some("owner")).await.unwrap();
        let database = owner.create_database(Doc::new(), &key).await.unwrap();
        let tree = database.root_id().clone();
        let txn = database.new_transaction().await.unwrap();
        txn.get_settings()
            .unwrap()
            .set_global_auth_key(AuthKey::active(None, Permission::Read))
            .await
            .unwrap();
        txn.commit().await.unwrap();
        owner
            .track_database(tree.clone(), &key, SyncSettings::enabled())
            .await
            .unwrap();
        instance.enable_sync().await.unwrap();
        let handler = Arc::new(ObservedHistory {
            inner: Arc::new(SyncHandlerImpl::new(
                instance.clone(),
                instance.sync().unwrap().sync_tree_root_id().clone(),
            )),
            dropped: Arc::new(Notify::new()),
            produced: Arc::new(AtomicUsize::new(0)),
            stall,
        });
        let request = SyncRequest::SyncTree(SyncTreeRequest {
            tree_id: tree,
            our_tips: Snapshot::EMPTY,
            peer_pubkey: None,
            requesting_key: None,
            requesting_key_name: None,
            requested_permission: None,
            metadata: None,
            auth: None,
        });
        let mut bytes = Vec::new();
        framing::write_request(&mut bytes, &request).await.unwrap();
        let body = handle_sync_request(
            State(handler.clone()),
            ConnectInfo("127.0.0.1:1234".parse().unwrap()),
            Body::from(bytes),
        )
        .await;
        (instance, request, handler, body)
    }

    #[tokio::test(start_paused = true)]
    async fn peer_paging_http_unpolled_body_expires_and_releases_native_producer() {
        let _other_permits: Vec<_> = (1
            ..crate::backend::history_page::MAX_CONCURRENT_HISTORY_PAGES)
            .map(|_| framing::admit_operation().unwrap())
            .collect();
        for stall in [false, true] {
            let (_instance, request, handler, body) = history_body(stall).await;
            assert!(
                framing::admit_operation().is_err(),
                "real producer must hold the last permit"
            );
            // Let the driver park at production/send without ever polling Body.
            tokio::task::yield_now().await;
            tokio::time::advance(REQUEST_TIMEOUT).await;
            tokio::time::timeout(Duration::from_secs(1), handler.dropped.notified())
                .await
                .expect("unpolled body must drop its actual producer at the absolute deadline");
            drop(
                framing::admit_operation()
                    .expect("deadline must release the native producer permit"),
            );
            assert_eq!(
                handler.produced.load(Ordering::SeqCst),
                if stall { 1 } else { 2 },
                "only one queued frame and one current frame may be retained"
            );
            // Use a fresh collector deadline so rejection proves an incomplete
            // prefix/unclean EOF, not merely the collector's own elapsed clock.
            let mut collector =
                framing::Collector::new(request, tokio::time::Instant::now() + REQUEST_TIMEOUT)
                    .unwrap();
            let mut decoder = framing::Decoder::default();
            let mut body = body.into_data_stream();
            let mut failed = false;
            while let Some(bytes) = body.next().await {
                match bytes {
                    Ok(bytes) => {
                        framing::receive_chunk(&mut decoder, &mut collector, &bytes).unwrap()
                    }
                    Err(_) => failed = true,
                }
            }
            assert!(failed, "deadline must not manufacture clean EOF");
            assert!(
                collector.finish(decoder).is_err(),
                "no complete prefix may escape expiry"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn peer_paging_http_body_drop_aborts_native_production_and_blocked_sends() {
        let _other_permits: Vec<_> = (1
            ..crate::backend::history_page::MAX_CONCURRENT_HISTORY_PAGES)
            .map(|_| framing::admit_operation().unwrap())
            .collect();
        for stall in [false, true] {
            let (_instance, _, handler, body) = history_body(stall).await;
            assert!(framing::admit_operation().is_err());
            tokio::task::yield_now().await;
            let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
            drop(body);
            tokio::time::timeout(Duration::from_secs(1), handler.dropped.notified())
                .await
                .expect(
                    "body drop must abort the actual producer without waiting for its deadline",
                );
            assert!(tokio::time::Instant::now() < deadline);
            drop(
                framing::admit_operation()
                    .expect("body drop must release the native producer permit"),
            );
            assert_eq!(
                handler.produced.load(Ordering::SeqCst),
                if stall { 1 } else { 2 }
            );
        }
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
    #[tokio::test]
    async fn peer_paging_http_partial_upload_and_oversized_root_are_not_ingested() {
        let (instance, _) = crate::Instance::create_backend(
            Box::new(crate::backend::database::InMemory::new()),
            crate::NewAccount::passwordless("partial-upload"),
        )
        .await
        .unwrap();
        instance.enable_sync().await.unwrap();
        let handler = Arc::new(crate::sync::handler::SyncHandlerImpl::new(
            instance.clone(),
            instance.sync().unwrap().sync_tree_root_id().clone(),
        ));
        let server = HttpTransport::builder()
            .bind("127.0.0.1:0")
            .build_sync()
            .unwrap();
        let client = HttpTransport::new().unwrap();
        server.start_server(handler).await.unwrap();
        let address = Address::http(server.get_server_address().unwrap());
        let entry = crate::Entry::root_builder()
            .set_metadata(b"incomplete upload must stay absent".to_vec())
            .build()
            .unwrap();
        let id = entry.id();
        let mut bytes = Vec::new();
        framing::write_request(&mut bytes, &SyncRequest::SendEntries(vec![entry]))
            .await
            .unwrap();
        let mut input = bytes.as_slice();
        let mut decoder = framing::Decoder::default();
        let mut end_bytes = 0;
        while !input.is_empty() {
            if let Some(frame) = decoder.next(&mut input).unwrap()
                && matches!(frame, framing::Frame::UploadEnd { .. })
            {
                end_bytes = framing::encode(&frame).unwrap().len();
            }
        }
        assert!(end_bytes > 0);
        bytes.truncate(bytes.len() - end_bytes);
        let response = build_client()
            .unwrap()
            .post(format!("http://{}/api/v1", address.address))
            .body(bytes)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert!(response.len() <= framing::MAX_FRAME_BYTES);
        let mut collector = framing::Collector::for_request(
            &SyncRequest::SendEntries(vec![]),
            tokio::time::Instant::now() + REQUEST_TIMEOUT,
        )
        .unwrap();
        assert!(
            framing::receive_chunk(&mut framing::Decoder::default(), &mut collector, &response)
                .is_err()
        );
        assert!(
            !instance.has_entry(&id).await,
            "transport ingested an upload without End"
        );
        let huge = crate::Entry::root_builder()
            .set_subtree_data("large", vec![255u8; framing::MAX_FRAME_BYTES])
            .build()
            .unwrap();
        let huge_id = huge.id();
        let error = client.send_entries(&address, &[huge]).await.unwrap_err();
        assert!(
            matches!(error, crate::Error::Sync(e) if matches!(*e, SyncError::EntryTooLarge { .. }))
        );
        assert!(!instance.has_entry(&huge_id).await);
        server.stop_server().await.unwrap();
    }
}
