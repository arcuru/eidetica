//! Actual network bytes through Database reads, including explicit daemon callers.
//! The backend factory is matrix-aware; daemon cases additionally connect over a
//! real Unix socket. TEST_BACKEND=service alone is not a daemon-transfer test.

use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use axum::{Router, http::StatusCode, routing::post};
use eidetica::{
    Database, Instance, NewAccount, Result,
    account::AccountSession,
    auth::crypto::{PublicKey, generate_keypair},
    backend::BackendError,
    crdt::Doc,
    entry::ID,
    store::DocStore,
    sync::{
        Sync, SyncError,
        handler::{SyncHandler, SyncHandlerImpl},
        peer_types::Address,
        protocol::{RequestContext, SyncRequest, SyncResponse},
        transports::{SyncTransport, http::HttpTransport, iroh::IrohTransport},
    },
};
use iroh::{RelayMode, endpoint::PortmapperConfig};
use tokio::{net::TcpListener, task::JoinHandle};

use crate::helpers::test_backend;

pub(super) const WAIT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug)]
pub(super) enum Transport {
    Http,
    Iroh,
}

impl Transport {
    fn build(self) -> Box<dyn SyncTransport> {
        match self {
            Self::Http => Box::new(
                HttpTransport::builder()
                    .bind("127.0.0.1:0")
                    .build_sync()
                    .unwrap(),
            ),
            Self::Iroh => Box::new(
                IrohTransport::builder()
                    .relay_mode(RelayMode::Disabled)
                    .portmapper_config(PortmapperConfig::Disabled)
                    .n0_dns(false)
                    .mdns(false)
                    .build()
                    .unwrap(),
            ),
        }
    }
    fn address(self, address: String) -> Address {
        match self {
            Self::Http => Address::http(address),
            Self::Iroh => Address::iroh(address),
        }
    }
}

pub(super) struct Node {
    pub(super) instance: Instance,
    pub(super) account: AccountSession,
    pub(super) sync: Arc<Sync>,
    pub(super) address: Address,
}

impl Node {
    pub(super) async fn new(name: &str, transport: Transport) -> Self {
        let (instance, account) =
            Instance::create_backend(test_backend().await, NewAccount::passwordless(name))
                .await
                .unwrap();
        instance.enable_sync().await.unwrap();
        let sync = instance.sync().unwrap();
        match transport {
            Transport::Http => sync
                .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
                .await
                .unwrap(),
            Transport::Iroh => sync
                .register_transport(
                    "iroh",
                    IrohTransport::builder()
                        .relay_mode(RelayMode::Disabled)
                        .portmapper_config(PortmapperConfig::Disabled)
                        .n0_dns(false)
                        .mdns(false),
                )
                .await
                .unwrap(),
        }
        sync.accept_connections().await.unwrap();
        let address = transport.address(sync.get_server_address().await.unwrap());
        Self {
            instance,
            account,
            sync,
            address,
        }
    }
    pub(super) async fn database(&mut self) -> Database {
        let key = self.account.get_default_key().unwrap();
        self.account
            .create_database(Doc::new(), &key)
            .await
            .unwrap()
    }
    pub(super) fn pubkey(&self) -> PublicKey {
        self.sync.get_device_pubkey().unwrap()
    }
}

pub(super) async fn link(sync: &Sync, peer: &PublicKey, address: Address, database: Option<&ID>) {
    sync.register_peer(peer, None).await.unwrap();
    sync.add_peer_address(peer, address).await.unwrap();
    if let Some(database) = database {
        sync.add_tree_sync(peer, database).await.unwrap();
    }
}

#[cfg(all(unix, feature = "service"))]
struct Daemon {
    client: Instance,
    shutdown: tokio::sync::watch::Sender<()>,
    task: JoinHandle<Result<()>>,
    _dir: tempfile::TempDir,
}

#[cfg(all(unix, feature = "service"))]
impl Daemon {
    async fn connect(node: &Node) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = dir.path().join("blob.sock");
        let service = eidetica::service::ServiceServer::bind(node.instance.clone(), socket.clone())
            .await
            .unwrap();
        let (shutdown, rx) = tokio::sync::watch::channel(());
        let task = tokio::spawn(service.run(rx));
        let client = Instance::connect(format!("unix://{}", socket.display()))
            .await
            .unwrap();
        Self {
            client,
            shutdown,
            task,
            _dir: dir,
        }
    }
    async fn stop(self) {
        drop(self.client);
        self.shutdown.send(()).unwrap();
        tokio::time::timeout(WAIT, self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

async fn transfer_scenario(transport: Transport, daemon: bool) {
    let holder = Node::new("holder", transport).await;
    let mut receiver = Node::new("receiver", transport).await;
    let database = receiver.database().await;
    link(
        &receiver.sync,
        &holder.pubkey(),
        holder.address.clone(),
        Some(database.root_id()),
    )
    .await;
    // The holder has no relationship to this Database: named-CID reads have no DB auth.
    let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let cid = ID::from_bytes(&data);
    #[cfg(all(unix, feature = "service"))]
    let connected = if daemon {
        Some(Daemon::connect(&receiver).await)
    } else {
        None
    };
    #[cfg(all(unix, feature = "service"))]
    let database = if let Some(remote) = &connected {
        remote
            .client
            .login_account("receiver", "")
            .await
            .unwrap()
            .open_database(database.root_id())
            .await
            .unwrap()
    } else {
        database
    };
    #[cfg(not(all(unix, feature = "service")))]
    assert!(!daemon);

    assert_eq!(database.get_blob_range(&cid, 0..10).await.unwrap(), None);
    holder.instance.put_blob(data.clone()).await.unwrap();
    let reversed = Range { start: 8, end: 4 };
    for range in [
        40_000..60_123,
        0..u64::MAX,
        100_000..100_200,
        3..3,
        reversed,
    ] {
        let start = range.start.min(data.len() as u64) as usize;
        let end = range.end.clamp(start as u64, data.len() as u64) as usize;
        assert_eq!(
            database
                .get_blob_range(&cid, range.clone())
                .await
                .unwrap()
                .as_deref(),
            Some(&data[start..end]),
            "{transport:?} daemon={daemon} range={range:?}"
        );
    }
    let empty = holder.instance.put_blob(Vec::new()).await.unwrap();
    assert_eq!(
        database.get_blob_range(&empty, 0..u64::MAX).await.unwrap(),
        Some(Vec::new())
    );
    // Neither whole nor partial explicit receive publishes or retains bytes.
    assert!(receiver.instance.get_blob(&cid).await.unwrap().is_none());
    #[cfg(all(unix, feature = "service"))]
    if let Some(remote) = &connected {
        assert!(remote.client.get_blob(&cid).await.unwrap().is_none());
    }
    assert_eq!(
        receiver
            .sync
            .query_blob_availability(
                &holder.address,
                &[ID::from_bytes(b"missing"), cid.clone(), cid.clone()]
            )
            .await
            .unwrap(),
        vec![false, true, true]
    );
    // Database reads remain usable independently of remote byte presence.
    let tx = database.new_transaction().await.unwrap();
    tx.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set("key", "value")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    println!(
        "BLOB_BYTES transport={transport:?} caller={} backend={} whole=100000 range=20123 empty=0 persisted=false",
        if daemon { "daemon" } else { "embedded" },
        std::env::var("TEST_BACKEND").unwrap_or_else(|_| "inmemory".into())
    );
    #[cfg(all(unix, feature = "service"))]
    if let Some(remote) = connected {
        remote.stop().await;
    }
    receiver.sync.stop_server().await.unwrap();
    holder.sync.stop_server().await.unwrap();
}

#[tokio::test]
async fn blob_http_embedded_whole_and_range() {
    transfer_scenario(Transport::Http, false).await;
}
#[tokio::test]
async fn blob_iroh_embedded_whole_and_range() {
    transfer_scenario(Transport::Iroh, false).await;
}
#[cfg(all(unix, feature = "service"))]
#[tokio::test]
async fn blob_http_daemon_whole_and_range() {
    transfer_scenario(Transport::Http, true).await;
}
#[cfg(all(unix, feature = "service"))]
#[tokio::test]
async fn blob_iroh_daemon_whole_and_range() {
    transfer_scenario(Transport::Iroh, true).await;
}

pub(super) struct FakePeer {
    pub(super) address: Address,
    pub(super) hits: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}
impl Drop for FakePeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(super) async fn fake_peer(status: StatusCode, body: Vec<u8>) -> FakePeer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = Address::http(listener.local_addr().unwrap().to_string());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let handler = move || {
        let counter = counter.clone();
        let body = body.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            (status, [("x-eidetica-blob", "0")], body)
        }
    };
    let router = Router::new()
        .route("/api/v0/blob", post(handler.clone()))
        .route("/api/v0/blob/has", post(handler));
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    FakePeer {
        address,
        hits,
        task,
    }
}
async fn unreachable() -> Address {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    Address::http(listener.local_addr().unwrap().to_string())
}
fn attempt(error: eidetica::Error) -> eidetica::blob::BlobFetchAttempt {
    match error {
        eidetica::Error::Sync(e) => match *e {
            SyncError::BlobUnavailable { attempt, .. } => attempt,
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn blob_database_peers_only_with_unrelated_positive_control_and_address_fallback() {
    let mut receiver = Node::new("receiver", Transport::Http).await;
    let database = receiver.database().await;
    let holder = Node::new("holder", Transport::Http).await;
    let bytes = b"held by an unrelated provider".to_vec();
    let cid = holder.instance.put_blob(bytes.clone()).await.unwrap();
    let handler = SyncHandlerImpl::new(
        holder.instance.clone(),
        holder.sync.sync_tree_root_id().clone(),
    );
    let encoded = handler
        .serve_blob_range(&cid, 0..u64::MAX)
        .await
        .unwrap()
        .unwrap();
    let unrelated = fake_peer(StatusCode::OK, encoded).await;
    let unrelated_key = generate_keypair().1;
    link(
        &receiver.sync,
        &unrelated_key,
        unrelated.address.clone(),
        None,
    )
    .await;
    let summary = attempt(
        database
            .get_blob_range(&cid, 0..u64::MAX)
            .await
            .unwrap_err(),
    );
    assert!(summary.had_no_providers());
    assert_eq!(unrelated.hits.load(Ordering::SeqCst), 0);
    // A Database peer actually says absent, even though the unrelated peer holds X.
    let absent = fake_peer(StatusCode::NOT_FOUND, Vec::new()).await;
    link(
        &receiver.sync,
        &generate_keypair().1,
        absent.address.clone(),
        Some(database.root_id()),
    )
    .await;
    assert!(
        database
            .get_blob_range(&cid, 0..u64::MAX)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        unrelated.hits.load(Ordering::SeqCst),
        0,
        "unrelated holder must not be queried"
    );
    // Positive control: after association, that same peer delivers real verified X.
    receiver
        .sync
        .add_tree_sync(&unrelated_key, database.root_id())
        .await
        .unwrap();
    assert_eq!(
        database.get_blob_range(&cid, 0..u64::MAX).await.unwrap(),
        Some(bytes)
    );
    assert_eq!(
        unrelated.hits.load(Ordering::SeqCst),
        1,
        "positive control must be queried"
    );
    // Same peer: unavailable transports must not consume the usable-route
    // budget. A refused usable address also falls back to the working route.
    let fallback_db = receiver.database().await;
    link(
        &receiver.sync,
        &holder.pubkey(),
        Address::new("unsupported", "opaque"),
        Some(fallback_db.root_id()),
    )
    .await;
    for i in 0..20 {
        receiver
            .sync
            .add_peer_address(
                &holder.pubkey(),
                Address::new("unsupported", format!("opaque-{i}")),
            )
            .await
            .unwrap();
    }
    receiver
        .sync
        .add_peer_address(&holder.pubkey(), unreachable().await)
        .await
        .unwrap();
    receiver
        .sync
        .add_peer_address(&holder.pubkey(), holder.address.clone())
        .await
        .unwrap();
    let bytes = b"address fallback reaches verified bytes".to_vec();
    let cid = holder.instance.put_blob(bytes.clone()).await.unwrap();
    assert_eq!(
        fallback_db.get_blob_range(&cid, 0..u64::MAX).await.unwrap(),
        Some(bytes)
    );
    receiver.sync.stop_server().await.unwrap();
    holder.sync.stop_server().await.unwrap();
}

async fn failure_outcomes(daemon: bool) {
    let mut receiver = Node::new("receiver", Transport::Http).await;
    let database = receiver.database().await;
    let absent = fake_peer(StatusCode::NOT_FOUND, Vec::new()).await;
    let corrupt = fake_peer(StatusCode::OK, vec![0xAB; 4096]).await;
    for address in [
        absent.address.clone(),
        corrupt.address.clone(),
        unreachable().await,
        Address::new("unknown", "x"),
    ] {
        link(
            &receiver.sync,
            &generate_keypair().1,
            address,
            Some(database.root_id()),
        )
        .await;
    }
    #[cfg(all(unix, feature = "service"))]
    let connected = if daemon {
        Some(Daemon::connect(&receiver).await)
    } else {
        None
    };
    #[cfg(all(unix, feature = "service"))]
    let database = if let Some(remote) = &connected {
        remote
            .client
            .login_account("receiver", "")
            .await
            .unwrap()
            .open_database(database.root_id())
            .await
            .unwrap()
    } else {
        database
    };
    #[cfg(not(all(unix, feature = "service")))]
    assert!(!daemon);
    let summary = attempt(
        database
            .get_blob_range(&ID::from_bytes(b"valid cid"), 0..20)
            .await
            .unwrap_err(),
    );
    assert_eq!(
        (
            summary.providers,
            summary.absent,
            summary.invalid,
            summary.unreachable,
            summary.unsupported
        ),
        (4, 1, 1, 1, 1)
    );
    assert!(!summary.absent_from_all_queried());
    #[cfg(all(unix, feature = "service"))]
    if let Some(remote) = connected {
        remote.stop().await;
    }
    receiver.sync.stop_server().await.unwrap();
}

#[tokio::test]
async fn blob_embedded_provider_failure_outcomes_are_not_absence() {
    failure_outcomes(false).await;
}
#[cfg(all(unix, feature = "service"))]
#[tokio::test]
async fn blob_daemon_provider_failure_outcomes_are_not_absence() {
    failure_outcomes(true).await;
}

struct CountingBlobHandler {
    inner: SyncHandlerImpl,
    hits: Arc<AtomicUsize>,
}

#[async_trait]
impl SyncHandler for CountingBlobHandler {
    async fn handle_request(
        &self,
        request: &SyncRequest,
        context: &RequestContext,
    ) -> SyncResponse {
        self.inner.handle_request(request, context).await
    }
    async fn serve_blob_range(&self, cid: &ID, range: Range<u64>) -> Result<Option<Vec<u8>>> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        self.inner.serve_blob_range(cid, range).await
    }
    async fn blob_availability(&self, cids: &[ID]) -> Result<Vec<bool>> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        self.inner.blob_availability(cids).await
    }
}

async fn named_only(transport: Transport) {
    let mut holder = Node::new("holder", transport).await;
    let asker = Node::new("asker", transport).await;
    let db = holder.database().await;
    let upstream = Node::new("upstream", transport).await;
    let bytes = b"held only upstream".to_vec();
    let elsewhere = upstream.instance.put_blob(bytes.clone()).await.unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = transport.build();
    counted
        .start_server(Arc::new(CountingBlobHandler {
            inner: SyncHandlerImpl::new(
                upstream.instance.clone(),
                upstream.sync.sync_tree_root_id().clone(),
            ),
            hits: hits.clone(),
        }))
        .await
        .unwrap();
    let upstream_address = transport.address(counted.get_server_address().unwrap());
    link(
        &holder.sync,
        &generate_keypair().1,
        upstream_address,
        Some(db.root_id()),
    )
    .await;
    let held = holder.instance.put_blob(b"held".to_vec()).await.unwrap();
    assert_eq!(
        asker
            .sync
            .query_blob_availability(&holder.address, &[elsewhere.clone(), held])
            .await
            .unwrap(),
        vec![false, true]
    );
    assert!(
        asker
            .sync
            .fetch_blob_range_from_peer(&holder.address, &elsewhere, 0..10)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0, "serving must not recurse");
    // Positive control over the same transport: this provider really can deliver
    // the missing CID, and the counter observes a Database-associated fetch.
    assert_eq!(
        db.get_blob_range(&elsewhere, 0..u64::MAX).await.unwrap(),
        Some(bytes)
    );
    assert!(hits.load(Ordering::SeqCst) > 0);
    assert!(
        holder
            .instance
            .get_blob(&elsewhere)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        asker
            .sync
            .query_blob_availability(&holder.address, &[])
            .await
            .unwrap(),
        Vec::<bool>::new()
    );
    let max = vec![elsewhere.clone(); eidetica::blob::MAX_BLOB_AVAILABILITY_BATCH];
    assert_eq!(
        asker
            .sync
            .query_blob_availability(&holder.address, &max)
            .await
            .unwrap(),
        vec![false; max.len()]
    );
    let mut excessive = max;
    excessive.push(elsewhere);
    assert!(
        asker
            .sync
            .query_blob_availability(&holder.address, &excessive)
            .await
            .is_err()
    );
    let valid = ID::from_bytes(b"valid");
    let digest = valid.as_cid().unwrap().hash().digest();
    for invalid in [
        ID::from_dagcbor_bytes(b"not raw"),
        ID::from(cid::Cid::new_v1(
            0x55,
            cid::multihash::Multihash::<64>::wrap(0x12, digest).unwrap(),
        )),
        ID::from(cid::Cid::new_v1(
            0x55,
            cid::multihash::Multihash::<64>::wrap(0x1e, &digest[..31]).unwrap(),
        )),
    ] {
        assert!(
            asker
                .sync
                .query_blob_availability(&holder.address, std::slice::from_ref(&invalid))
                .await
                .is_err()
        );
        assert!(
            asker
                .sync
                .fetch_blob_range_from_peer(&holder.address, &invalid, 0..10)
                .await
                .is_err()
        );
    }
    counted.stop_server().await.unwrap();
    upstream.sync.stop_server().await.unwrap();
    holder.sync.stop_server().await.unwrap();
    asker.sync.stop_server().await.unwrap();
}
#[tokio::test]
async fn blob_http_blind_named_bounded_no_recursion() {
    named_only(Transport::Http).await;
}
#[tokio::test]
async fn blob_iroh_blind_named_bounded_no_recursion() {
    named_only(Transport::Iroh).await;
}

struct BadHandler {
    stream: Vec<u8>,
    available: Vec<bool>,
}
#[async_trait]
impl SyncHandler for BadHandler {
    async fn handle_request(&self, _: &SyncRequest, _: &RequestContext) -> SyncResponse {
        SyncResponse::Ack
    }
    async fn serve_blob_range(&self, _: &ID, _: Range<u64>) -> Result<Option<Vec<u8>>> {
        Ok(Some(self.stream.clone()))
    }
    async fn blob_availability(&self, _: &[ID]) -> Result<Vec<bool>> {
        Ok(self.available.clone())
    }
}

async fn invalid_streams(transport: Transport) {
    let holder = Node::new("holder", Transport::Http).await;
    let data = vec![37u8; 100_000];
    let cid = holder.instance.put_blob(data).await.unwrap();
    let handler = SyncHandlerImpl::new(
        holder.instance.clone(),
        holder.sync.sync_tree_root_id().clone(),
    );
    let valid = handler
        .serve_blob_range(&cid, 40_000..40_100)
        .await
        .unwrap()
        .unwrap();
    let client = transport.build();
    client
        .start_server(Arc::new(BadHandler {
            stream: Vec::new(),
            available: Vec::new(),
        }))
        .await
        .unwrap();
    let good = transport.build();
    good.start_server(Arc::new(BadHandler {
        stream: valid.clone(),
        available: vec![true],
    }))
    .await
    .unwrap();
    let address = transport.address(good.get_server_address().unwrap());
    // The shared dial path still selects the JSON sync protocol, not blob ALPN.
    assert_eq!(
        client
            .send_request(&address, &SyncRequest::SendEntries(Vec::new()))
            .await
            .unwrap(),
        SyncResponse::Ack
    );
    assert_eq!(
        client
            .fetch_blob_range(&address, &cid, 40_000..40_100)
            .await
            .unwrap(),
        Some(vec![37; 100])
    );
    assert!(
        client
            .fetch_blob_range(
                &address,
                &ID::from_bytes(b"wrong logical CID"),
                40_000..40_100
            )
            .await
            .is_err()
    );
    good.stop_server().await.unwrap();
    let mut tampered = valid.clone();
    *tampered.last_mut().unwrap() ^= 1;
    let mut size = valid.clone();
    size[..8].copy_from_slice(&(u64::MAX).to_le_bytes());
    let truncated = valid[..valid.len() - 1].to_vec();
    // An unproved short size must not turn a requested nonempty range into
    // successful empty bytes. The declared size itself needs CID authentication.
    for stream in [
        0u64.to_le_bytes().to_vec(),
        1u64.to_le_bytes().to_vec(),
        tampered,
        size,
        truncated,
        vec![0; 1_048_677],
    ] {
        let server = transport.build();
        server
            .start_server(Arc::new(BadHandler {
                stream,
                available: vec![true, false],
            }))
            .await
            .unwrap();
        let address = transport.address(server.get_server_address().unwrap());
        let error = client
            .fetch_blob_range(&address, &cid, 40_000..40_100)
            .await
            .unwrap_err();
        assert!(
            matches!(error, eidetica::Error::Backend(ref e) if matches!(**e, BackendError::BlobStreamInvalid {..} | BackendError::BlobTooLarge {..})),
            "{transport:?}: {error:?}"
        );
        assert!(
            client
                .query_blobs(&address, std::slice::from_ref(&cid))
                .await
                .is_err(),
            "extra availability result must be refused"
        );
        server.stop_server().await.unwrap();
    }
    let oversized = transport.build();
    oversized
        .start_server(Arc::new(BadHandler {
            stream: Vec::new(),
            available: vec![true; 20_000],
        }))
        .await
        .unwrap();
    let address = transport.address(oversized.get_server_address().unwrap());
    assert!(
        client
            .query_blobs(&address, std::slice::from_ref(&cid))
            .await
            .is_err()
    );
    oversized.stop_server().await.unwrap();
    client.stop_server().await.unwrap();
    holder.sync.stop_server().await.unwrap();
}
#[tokio::test]
async fn blob_http_rejects_proof_size_truncation_receive_limit_and_extra_answers() {
    invalid_streams(Transport::Http).await;
}
#[tokio::test]
async fn blob_iroh_rejects_proof_size_truncation_receive_limit_and_extra_answers() {
    invalid_streams(Transport::Iroh).await;
}

#[tokio::test]
async fn blob_stalled_receive_leaves_database_sync_and_healthy_blob_work_running() {
    use tokio::io::AsyncReadExt;
    let mut holder = Node::new("holder", Transport::Http).await;
    let receiver = Node::new("receiver", Transport::Http).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = Address::http(listener.local_addr().unwrap().to_string());
    let (started, rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        assert!(socket.read(&mut buf).await.unwrap() > 0);
        started.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    let sync = receiver.sync.clone();
    let fetch = tokio::spawn(async move {
        sync.fetch_blob_range_from_peer(&address, &ID::from_bytes(b"stall"), 0..10)
            .await
    });
    tokio::time::timeout(WAIT, rx).await.unwrap().unwrap();
    let cid = holder.instance.put_blob(b"healthy".to_vec()).await.unwrap();
    let answer = tokio::time::timeout(
        Duration::from_secs(5),
        receiver
            .sync
            .query_blob_availability(&holder.address, &[cid]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(answer, vec![true]);
    // A real database bootstrap runs on the same background engine while the
    // range transfer remains stalled, not just another local helper call.
    let database = holder.database().await;
    database.share().await.unwrap();
    let key = holder.account.get_default_key().unwrap();
    let signing_key = holder.account.get_signing_key(&key).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        receiver.sync.sync_with_peer_for_bootstrap_with_key(
            &holder.address,
            database.root_id(),
            &signing_key,
            &key.to_string(),
            eidetica::auth::Permission::Read,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(receiver.instance.has_database(database.root_id()).await);
    assert!(!fetch.is_finished());
    fetch.abort();
    server.abort();
    receiver.sync.stop_server().await.unwrap();
    holder.sync.stop_server().await.unwrap();
}

#[tokio::test]
async fn blob_http_missing_endpoint_is_unsupported_and_server_checks_named_bounds() {
    let holder = Node::new("holder", Transport::Http).await;
    let client = reqwest::Client::new();
    let endpoint = format!("http://{}/api/v0/blob/has", holder.address.address);
    let excessive = vec![ID::from_bytes(b"named"); eidetica::blob::MAX_BLOB_AVAILABILITY_BATCH + 1];
    let response = client
        .post(&endpoint)
        .json(&serde_json::json!({ "cids": excessive }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = client
        .post(&endpoint)
        .json(&serde_json::json!({ "cids": [ID::from_dagcbor_bytes(b"not raw")] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        !client
            .post(&endpoint)
            .json(&serde_json::json!({ "prefix": "bafk" }))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let legacy = Address::http(listener.local_addr().unwrap().to_string());
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new()).await.unwrap();
    });
    let error = holder
        .sync
        .fetch_blob_range_from_peer(&legacy, &ID::from_bytes(b"not known"), 0..10)
        .await
        .unwrap_err();
    assert!(
        matches!(error, eidetica::Error::Sync(ref e) if matches!(**e, SyncError::UnsupportedTransport { .. }))
    );
    server.abort();
    holder.sync.stop_server().await.unwrap();
}

#[tokio::test]
async fn blob_outbound_task_capacity_is_bounded_without_blocking_engine_commands() {
    let receiver = Node::new("receiver", Transport::Http).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = Address::http(listener.local_addr().unwrap().to_string());
    let (started, mut rx) = tokio::sync::mpsc::channel(4);
    let release = Arc::new(tokio::sync::Notify::new());
    let notify = release.clone();
    let router = Router::new().route(
        "/api/v0/blob",
        post(move || {
            let started = started.clone();
            let notify = notify.clone();
            async move {
                started.send(()).await.unwrap();
                notify.notified().await;
                (StatusCode::NOT_FOUND, [("x-eidetica-blob", "0")])
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let sync = receiver.sync.clone();
        let address = address.clone();
        tasks.push(tokio::spawn(async move {
            sync.fetch_blob_range_from_peer(&address, &ID::from_bytes(b"stall"), 0..10)
                .await
        }));
    }
    for _ in 0..4 {
        tokio::time::timeout(WAIT, rx.recv())
            .await
            .unwrap()
            .unwrap();
    }
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        receiver
            .sync
            .fetch_blob_range_from_peer(&address, &ID::from_bytes(b"fifth"), 0..10),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.to_string().contains("capacity reached"), "{error}");
    // An engine command still completes while all blob slots are occupied.
    tokio::time::timeout(Duration::from_secs(5), receiver.sync.get_server_address())
        .await
        .unwrap()
        .unwrap();
    release.notify_waiters();
    for task in tasks {
        assert!(
            tokio::time::timeout(WAIT, task)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .is_none()
        );
    }
    // Permits are released on actual task completion, not on command launch.
    assert_eq!(
        receiver
            .sync
            .query_blob_availability(&receiver.address, &[])
            .await
            .unwrap(),
        Vec::<bool>::new()
    );
    server.abort();
    receiver.sync.stop_server().await.unwrap();
}

#[cfg(all(unix, feature = "service"))]
#[tokio::test]
async fn blob_daemon_peer_receive_still_refuses_oversized_json_frame_and_keeps_connection() {
    let holder = Node::new("holder", Transport::Http).await;
    let mut receiver = Node::new("receiver", Transport::Http).await;
    let database = receiver.database().await;
    link(
        &receiver.sync,
        &holder.pubkey(),
        holder.address.clone(),
        Some(database.root_id()),
    )
    .await;
    // 17 MiB is below the blob cap, but its JSON number array exceeds 64 MiB.
    let cid = holder
        .instance
        .put_blob(vec![255; 17 * 1024 * 1024])
        .await
        .unwrap();
    let connected = Daemon::connect(&receiver).await;
    let database = connected
        .client
        .login_account("receiver", "")
        .await
        .unwrap()
        .open_database(database.root_id())
        .await
        .unwrap();
    let error = tokio::time::timeout(WAIT, database.get_blob_range(&cid, 0..u64::MAX))
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(error, eidetica::Error::Backend(ref e) if matches!(**e, BackendError::BlobResponseTooLarge { .. })),
        "{error:?}"
    );
    assert_eq!(
        database.get_blob_range(&cid, 4096..4112).await.unwrap(),
        Some(vec![255; 16])
    );
    assert!(
        connected.client.get_blob(&cid).await.unwrap().is_none(),
        "explicit receive never publishes even after a frame refusal"
    );
    connected.stop().await;
    receiver.sync.stop_server().await.unwrap();
    holder.sync.stop_server().await.unwrap();
}
