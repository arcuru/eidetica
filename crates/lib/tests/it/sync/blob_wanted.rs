//! Registration-owned wanted blobs: durable intent, retention and fetching
//! from the registered Database's peers, through embedded and explicitly
//! connected daemon callers over real HTTP and Iroh transports.

use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{Router, http::StatusCode, routing::post};
use eidetica::{
    Database, Instance, NewAccount,
    account::AccountSession,
    backend::database::InMemory,
    blob::{WantedBlobState, WantedBlobStatus},
    crdt::Doc,
    entry::ID,
    instance::GcOptions,
    sync::{
        handler::{SyncHandler, SyncHandlerImpl},
        peer_types::Address,
        transports::http::HttpTransport,
    },
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

use super::blob_transfer::{Node, Transport, WAIT, fake_peer, link};
use crate::helpers::{LocalBackendTestExt, test_backend};

const COLLECT_ALL: GcOptions = GcOptions {
    max_total_bytes: None,
    min_age_ms: 0,
};

fn data(label: &str) -> Vec<u8> {
    let mut bytes = label.as_bytes().to_vec();
    bytes.extend((0..70_000u32).map(|i| (i % 241) as u8));
    bytes
}

async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    tokio::time::timeout(WAIT, async {
        while !check().await {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

async fn status(database: &Database, cid: &ID) -> WantedBlobStatus {
    database
        .blob_status(std::slice::from_ref(cid))
        .await
        .unwrap()
        .remove(0)
}

/// An explicitly connected daemon whose clients can disconnect and reconnect.
#[cfg(all(unix, feature = "service"))]
struct Daemon {
    socket: std::path::PathBuf,
    shutdown: tokio::sync::watch::Sender<()>,
    task: JoinHandle<eidetica::Result<()>>,
    _dir: tempfile::TempDir,
}

#[cfg(all(unix, feature = "service"))]
impl Daemon {
    async fn serve(instance: &Instance) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = dir.path().join("wanted.sock");
        let service = eidetica::service::ServiceServer::bind(instance.clone(), socket.clone())
            .await
            .unwrap();
        let (shutdown, rx) = tokio::sync::watch::channel(());
        let task = tokio::spawn(service.run(rx));
        Self {
            socket,
            shutdown,
            task,
            _dir: dir,
        }
    }
    async fn client(&self) -> Instance {
        Instance::connect(format!("unix://{}", self.socket.display()))
            .await
            .unwrap()
    }
    async fn stop(self) {
        self.shutdown.send(()).unwrap();
        tokio::time::timeout(WAIT, self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

/// The caller's view: an Instance, its AccountSession and the Database handle.
struct Caller {
    instance: Instance,
    account: AccountSession,
    database: Database,
}

async fn embedded_caller(node: &Node, database: &Database) -> Caller {
    Caller {
        instance: node.instance.clone(),
        account: node.instance.login_account("receiver", "").await.unwrap(),
        database: database.clone(),
    }
}

#[cfg(all(unix, feature = "service"))]
async fn daemon_caller(daemon: &Daemon, database: &ID) -> Caller {
    let instance = daemon.client().await;
    let account = instance.login_account("receiver", "").await.unwrap();
    let database = account.open_database(database).await.unwrap();
    Caller {
        instance,
        account,
        database,
    }
}

async fn wanted_scenario(transport: Transport, daemon: bool) {
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
    let bytes = data("wanted before presence");
    let cid = ID::from_bytes(&bytes);
    #[cfg(all(unix, feature = "service"))]
    let served = if daemon {
        Some(Daemon::serve(&receiver.instance).await)
    } else {
        None
    };
    #[cfg(not(all(unix, feature = "service")))]
    assert!(!daemon);
    let connect = async || -> Caller {
        #[cfg(all(unix, feature = "service"))]
        if let Some(served) = &served {
            return daemon_caller(served, database.root_id()).await;
        }
        embedded_caller(&receiver, &database).await
    };
    let caller = connect().await;

    // Want-before-presence: no Instance holds the bytes yet, and the
    // registration is not sharing. Success records intent only.
    caller
        .database
        .set_wanted_blobs([cid.clone(), cid.clone()])
        .await
        .unwrap();
    assert_eq!(
        caller.database.wanted_blobs().await.unwrap(),
        vec![cid.clone()]
    );
    assert_eq!(
        caller
            .account
            .wanted_blobs(database.root_id())
            .await
            .unwrap(),
        vec![cid.clone()]
    );
    assert_eq!(
        status(&caller.database, &cid).await.state,
        WantedBlobState::Pending { last_fetch: None }
    );

    // A paused registration does not fetch even once a provider holds X.
    holder.instance.put_blob(bytes.clone()).await.unwrap();
    caller.database.fetch_wanted_blobs().await.unwrap();
    assert_eq!(
        status(&caller.database, &cid).await.state,
        WantedBlobState::Pending { last_fetch: None },
        "paused registration must not fetch"
    );
    // Database data stays usable while the wanted blob is missing.
    let tx = caller.database.new_transaction().await.unwrap();
    tx.get_store::<eidetica::store::DocStore>("data")
        .await
        .unwrap()
        .set("key", "value")
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Reconnect: a new client session sees the same durable wanted set.
    drop(caller);
    let caller = connect().await;
    assert_eq!(
        caller.database.wanted_blobs().await.unwrap(),
        vec![cid.clone()]
    );

    // Resuming sharing resumes fetching without an explicit fetch call.
    caller.database.share().await.unwrap();
    let (db, wanted) = (&caller.database, &cid);
    eventually("resumed fetch to publish X", || async move {
        status(db, wanted).await.is_available()
    })
    .await;
    assert_eq!(
        caller.instance.get_blob(&cid).await.unwrap().as_deref(),
        Some(bytes.as_slice())
    );
    // The fixed-set wait returns at once for available bytes.
    tokio::time::timeout(
        WAIT,
        caller.database.wait_for_blobs(std::slice::from_ref(&cid)),
    )
    .await
    .unwrap()
    .unwrap();

    // Owned bytes survive GC, including while sharing is paused.
    caller.instance.gc_blobs(COLLECT_ALL).await.unwrap();
    assert!(caller.instance.get_blob(&cid).await.unwrap().is_some());
    caller.database.stop_sharing().await.unwrap();
    assert_eq!(
        caller.database.wanted_blobs().await.unwrap(),
        vec![cid.clone()]
    );
    caller.instance.gc_blobs(COLLECT_ALL).await.unwrap();
    assert!(caller.instance.get_blob(&cid).await.unwrap().is_some());

    // Untracking releases this registration; the bytes become collectible.
    let mut account = caller.account;
    account.untrack_database(database.root_id()).await.unwrap();
    assert!(
        account
            .wanted_blobs(database.root_id())
            .await
            .unwrap()
            .is_empty()
    );
    let report = caller.instance.gc_blobs(COLLECT_ALL).await.unwrap();
    assert_eq!(report.evicted_count, 1);
    assert!(caller.instance.get_blob(&cid).await.unwrap().is_none());
    println!(
        "BLOB_WANTED transport={transport:?} caller={} backend={} want_before_presence=true paused_no_fetch=true reconnect=true resumed_fetch=true retained_paused=true released_on_untrack=true",
        if daemon { "daemon" } else { "embedded" },
        std::env::var("TEST_BACKEND").unwrap_or_else(|_| "inmemory".into())
    );
    drop(caller.instance);
    #[cfg(all(unix, feature = "service"))]
    if let Some(served) = served {
        served.stop().await;
    }
    receiver.sync.stop_server().await.unwrap();
    holder.sync.stop_server().await.unwrap();
}

#[tokio::test]
async fn blob_wanted_http_embedded_owns_fetches_and_releases() {
    wanted_scenario(Transport::Http, false).await;
}
#[tokio::test]
async fn blob_wanted_iroh_embedded_owns_fetches_and_releases() {
    wanted_scenario(Transport::Iroh, false).await;
}
#[cfg(all(unix, feature = "service"))]
#[tokio::test]
async fn blob_wanted_http_daemon_owns_fetches_and_releases() {
    wanted_scenario(Transport::Http, true).await;
}
#[cfg(all(unix, feature = "service"))]
#[tokio::test]
async fn blob_wanted_iroh_daemon_owns_fetches_and_releases() {
    wanted_scenario(Transport::Iroh, true).await;
}

/// The wanted set and missing-byte intent survive an Instance restart, and
/// enabling sharing afterwards fetches from the Database's peer.
#[tokio::test]
async fn blob_wanted_set_survives_restart_before_presence() {
    let (instance, mut account) = Instance::create_backend(
        Box::new(InMemory::new()),
        NewAccount::passwordless("receiver"),
    )
    .await
    .unwrap();
    let key = account.get_default_key().unwrap();
    let database = account.create_database(Doc::new(), &key).await.unwrap();
    let root = database.root_id().clone();
    let bytes = data("wanted across restart");
    let cid = ID::from_bytes(&bytes);
    database.set_wanted_blobs([cid.clone()]).await.unwrap();

    let path = tempfile::tempdir().unwrap();
    let file = path.path().join("instance.json");
    instance
        .backend()
        .engine()
        .as_any()
        .downcast_ref::<InMemory>()
        .unwrap()
        .save_to_file(&file)
        .unwrap();
    drop((database, account, instance));

    let instance = Instance::open_backend(Box::new(InMemory::load_from_file(&file).await.unwrap()))
        .await
        .unwrap();
    let account = instance.login_account("receiver", "").await.unwrap();
    let database = account.open_database(&root).await.unwrap();
    assert_eq!(database.wanted_blobs().await.unwrap(), vec![cid.clone()]);
    assert!(!status(&database, &cid).await.is_available());

    let holder = Node::new("holder", Transport::Http).await;
    holder.instance.put_blob(bytes.clone()).await.unwrap();
    instance.enable_sync().await.unwrap();
    let sync = instance.sync().unwrap();
    sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await
        .unwrap();
    link(&sync, &holder.pubkey(), holder.address.clone(), Some(&root)).await;
    database.share().await.unwrap();
    tokio::time::timeout(WAIT, database.wait_for_blobs(std::slice::from_ref(&cid)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        instance.get_blob(&cid).await.unwrap().as_deref(),
        Some(bytes.as_slice())
    );
    instance.gc_blobs(COLLECT_ALL).await.unwrap();
    assert!(instance.get_blob(&cid).await.unwrap().is_some());
    holder.sync.stop_server().await.unwrap();
}

/// Only the registered Database's peers are asked. An unrelated Instance peer
/// that holds X is never queried until it becomes a peer of that Database.
#[tokio::test]
async fn blob_wanted_fetch_asks_only_database_peers() {
    let mut receiver = Node::new("receiver", Transport::Http).await;
    let database = receiver.database().await;
    database.share().await.unwrap();
    let holder = Node::new("holder", Transport::Http).await;
    let bytes = data("held by an unrelated peer");
    let cid = holder.instance.put_blob(bytes.clone()).await.unwrap();
    let encoded = SyncHandlerImpl::new(
        holder.instance.clone(),
        holder.sync.sync_tree_root_id().clone(),
    )
    .serve_blob_range(&cid, 0..u64::MAX)
    .await
    .unwrap()
    .unwrap();
    let unrelated = fake_peer(StatusCode::OK, encoded).await;
    let unrelated_key = eidetica::auth::crypto::generate_keypair().1;
    link(
        &receiver.sync,
        &unrelated_key,
        unrelated.address.clone(),
        None,
    )
    .await;

    database.set_wanted_blobs([cid.clone()]).await.unwrap();
    database.fetch_wanted_blobs().await.unwrap();
    match status(&database, &cid).await.state {
        WantedBlobState::Pending {
            last_fetch: Some(attempt),
        } => assert!(attempt.had_no_providers(), "{attempt:?}"),
        other => panic!("{other:?}"),
    }
    // A Database peer that answers "absent" is the only one asked.
    let absent = fake_peer(StatusCode::NOT_FOUND, Vec::new()).await;
    link(
        &receiver.sync,
        &eidetica::auth::crypto::generate_keypair().1,
        absent.address.clone(),
        Some(database.root_id()),
    )
    .await;
    database.fetch_wanted_blobs().await.unwrap();
    match status(&database, &cid).await.state {
        WantedBlobState::Pending {
            last_fetch: Some(attempt),
        } => assert!(attempt.absent_from_all_queried(), "{attempt:?}"),
        other => panic!("{other:?}"),
    }
    assert!(absent.hits.load(Ordering::SeqCst) >= 1);
    assert_eq!(
        unrelated.hits.load(Ordering::SeqCst),
        0,
        "an unrelated Instance peer must not be queried"
    );
    // Positive control: once associated, the same peer supplies verified X.
    receiver
        .sync
        .add_tree_sync(&unrelated_key, database.root_id())
        .await
        .unwrap();
    tokio::time::timeout(WAIT, database.wait_for_blobs(std::slice::from_ref(&cid)))
        .await
        .unwrap()
        .unwrap();
    assert!(unrelated.hits.load(Ordering::SeqCst) >= 1);
    assert_eq!(
        receiver.instance.get_blob(&cid).await.unwrap().as_deref(),
        Some(bytes.as_slice())
    );
    receiver.sync.stop_server().await.unwrap();
    holder.sync.stop_server().await.unwrap();
}

/// A provider that answers blob reads only after `gate` opens.
struct GatedPeer {
    address: Address,
    hits: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}
impl Drop for GatedPeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn gated_peer(body: Vec<u8>, gate: Arc<Notify>) -> GatedPeer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = Address::http(listener.local_addr().unwrap().to_string());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let handler = move || {
        let counter = counter.clone();
        let body = body.clone();
        let gate = gate.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            gate.notified().await;
            (StatusCode::OK, [("x-eidetica-blob", "0")], body)
        }
    };
    let router = Router::new().route("/api/v0/blob", post(handler));
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    GatedPeer {
        address,
        hits,
        task,
    }
}

/// Replacing `[X, Y]` with `[Y, Z]` keeps Y owned throughout, and a fetch of
/// X that completes after the replacement publishes only unowned cache bytes.
#[tokio::test]
async fn blob_wanted_replacement_and_late_fetch_do_not_restore_ownership() {
    let mut receiver = Node::new("receiver", Transport::Http).await;
    let database = receiver.database().await;
    database.share().await.unwrap();
    let holder = Node::new("holder", Transport::Http).await;
    let x_bytes = data("replaced X");
    let x = holder.instance.put_blob(x_bytes.clone()).await.unwrap();
    let encoded = SyncHandlerImpl::new(
        holder.instance.clone(),
        holder.sync.sync_tree_root_id().clone(),
    )
    .serve_blob_range(&x, 0..u64::MAX)
    .await
    .unwrap()
    .unwrap();
    let gate = Arc::new(Notify::new());
    let provider = gated_peer(encoded, gate.clone()).await;
    link(
        &receiver.sync,
        &eidetica::auth::crypto::generate_keypair().1,
        provider.address.clone(),
        Some(database.root_id()),
    )
    .await;
    let y = receiver
        .instance
        .put_blob(b"kept Y".to_vec())
        .await
        .unwrap();
    let z = ID::from_bytes(b"not yet anywhere Z");

    // Declaring the set starts a fetch of X that blocks at the provider.
    database
        .set_wanted_blobs([x.clone(), y.clone()])
        .await
        .unwrap();
    let hits = provider.hits.clone();
    eventually("the X fetch to reach the provider", || {
        let hits = hits.clone();
        async move { hits.load(Ordering::SeqCst) > 0 }
    })
    .await;
    database
        .set_wanted_blobs([y.clone(), z.clone()])
        .await
        .unwrap();
    let mut expected = vec![y.clone(), z.clone()];
    expected.sort();
    assert_eq!(database.wanted_blobs().await.unwrap(), expected);
    // Y was in both sets and stays owned.
    receiver.instance.gc_blobs(COLLECT_ALL).await.unwrap();
    assert!(receiver.instance.get_blob(&y).await.unwrap().is_some());

    // The late completion publishes verified X but cannot restore ownership.
    gate.notify_waiters();
    gate.notify_one();
    let (local, late) = (&receiver.instance, &x);
    eventually("the late fetch to publish X", || async move {
        local.get_blob(late).await.unwrap().is_some()
    })
    .await;
    assert_eq!(database.wanted_blobs().await.unwrap(), expected);
    let report = receiver.instance.gc_blobs(COLLECT_ALL).await.unwrap();
    assert_eq!(report.evicted_count, 1, "only unowned X is collected");
    assert!(receiver.instance.get_blob(&x).await.unwrap().is_none());
    assert!(receiver.instance.get_blob(&y).await.unwrap().is_some());
    receiver.sync.stop_server().await.unwrap();
    holder.sync.stop_server().await.unwrap();
}

/// Two registrations of different Accounts want X. Releasing or pausing one
/// keeps X while the other owns it; releasing the last makes it collectible.
#[tokio::test]
async fn blob_wanted_union_of_registrations_controls_gc() {
    let (instance, mut first) =
        Instance::create_backend(test_backend().await, NewAccount::passwordless("first"))
            .await
            .unwrap();
    first
        .admin()
        .await
        .unwrap()
        .create_account(NewAccount::passwordless("second"))
        .await
        .unwrap();
    let mut second = instance.login_account("second", "").await.unwrap();
    let first_key = first.get_default_key().unwrap();
    let second_key = second.get_default_key().unwrap();
    let first_db = first.create_database(Doc::new(), &first_key).await.unwrap();
    let other_db = first.create_database(Doc::new(), &first_key).await.unwrap();
    let second_db = second
        .create_database(Doc::new(), &second_key)
        .await
        .unwrap();
    let x = instance.put_blob(b"shared X".to_vec()).await.unwrap();
    let only_first = instance.put_blob(b"first only".to_vec()).await.unwrap();

    // Registration-level declaration, alongside the registration itself.
    first
        .set_wanted_blobs(first_db.root_id(), [x.clone(), only_first.clone()])
        .await
        .unwrap();
    other_db
        .set_wanted_blobs([only_first.clone()])
        .await
        .unwrap();
    second_db.set_wanted_blobs([x.clone()]).await.unwrap();
    // Another Account's registration is not visible through this one.
    assert_eq!(
        second.wanted_blobs(first_db.root_id()).await.unwrap(),
        vec![]
    );
    assert!(
        second
            .set_wanted_blobs(first_db.root_id(), [x.clone()])
            .await
            .is_err(),
        "an Account cannot declare blobs for a registration it does not hold"
    );

    first.untrack_database(first_db.root_id()).await.unwrap();
    assert!(
        first
            .wanted_blobs(first_db.root_id())
            .await
            .unwrap()
            .is_empty()
    );
    instance.gc_blobs(COLLECT_ALL).await.unwrap();
    assert!(
        instance.get_blob(&x).await.unwrap().is_some(),
        "second still owns X"
    );
    assert!(
        instance.get_blob(&only_first).await.unwrap().is_some(),
        "the same Account's other registration still owns it"
    );
    second_db.stop_sharing().await.unwrap();
    instance.gc_blobs(COLLECT_ALL).await.unwrap();
    assert!(
        instance.get_blob(&x).await.unwrap().is_some(),
        "pause keeps X"
    );

    second.untrack_database(second_db.root_id()).await.unwrap();
    other_db.set_wanted_blobs([]).await.unwrap();
    let report = instance.gc_blobs(COLLECT_ALL).await.unwrap();
    assert_eq!(report.evicted_count, 2);
    assert!(instance.get_blob(&x).await.unwrap().is_none());
    // Releasing ownership never changed Database data.
    assert!(!second_db.snapshot().await.unwrap().is_empty());
}
