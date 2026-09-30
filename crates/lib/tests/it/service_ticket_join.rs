//! `User::join`: joining a database from a ticket, embedded and through a
//! service daemon, against a real HTTP peer.

#![cfg(all(unix, feature = "service"))]

use std::{os::unix::fs::PermissionsExt, path::PathBuf};

use eidetica::{
    Error, Instance, NewUser,
    auth::{
        AuthKey, Permission,
        crypto::{create_challenge_response, generate_keypair},
    },
    backend::database::InMemory,
    crdt::Doc,
    service::{
        ServiceServer,
        protocol::{
            Handshake, HandshakeAck, PROTOCOL_VERSION, ServerFrame, ServiceRequest,
            ServiceResponse, TicketBootstrapOutcome, TicketBootstrapRequest, read_frame,
            write_frame,
        },
    },
    store::DocStore,
    sync::{
        DatabaseTicket, SyncError, peer_types::Address, protocol::SyncRequestAuth,
        transports::http::HttpTransport,
    },
    user::{UserError, types::SyncSettings},
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::UnixStream,
    sync::watch,
};

use crate::sync::helpers::{
    setup_global_wildcard_server, setup_sync_enabled_client, start_sync_server,
};

/// A sharing peer on the system clock, like the daemon and its clients, so
/// request proofs signed by a connected client are fresh to it.
struct Peer {
    instance: Instance,
    user: eidetica::user::User,
    admin_key: eidetica::auth::crypto::PublicKey,
    db: eidetica::Database,
    sync: std::sync::Arc<eidetica::sync::Sync>,
    ticket: DatabaseTicket,
}

impl Peer {
    /// `open` grants every key Write; otherwise requests need approval.
    async fn start(open: bool) -> Self {
        let (instance, mut user) =
            Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("owner"))
                .await
                .unwrap();
        instance.enable_sync().await.unwrap();
        let admin_key = user.get_default_key().unwrap();
        let mut settings = Doc::new();
        settings.set("name", "shared");
        let db = user.create_database(settings, &admin_key).await.unwrap();
        let device = instance.id();
        db.with_transaction(|tx| async move {
            let settings = tx.get_settings()?;
            settings
                .set_auth_key(
                    &device,
                    AuthKey::active(Some("device"), Permission::Admin(0)),
                )
                .await?;
            if open {
                settings
                    .set_global_auth_key(AuthKey::active(None, Permission::Write(0)))
                    .await?;
            }
            Ok(())
        })
        .await
        .unwrap();
        let tree = db.root_id().clone();
        user.track_database(tree.clone(), &admin_key, SyncSettings::enabled())
            .await
            .unwrap();
        let sync = instance.sync().unwrap();
        let address = start_sync_server(&sync).await;
        let ticket = DatabaseTicket::with_addresses(tree, vec![address]);
        Self {
            instance,
            user,
            admin_key,
            db,
            sync,
            ticket,
        }
    }

    fn tree(&self) -> eidetica::entry::ID {
        self.ticket.database_id().clone()
    }
}

/// A daemon serving an in-memory instance whose only user is `alice`.
struct Daemon {
    instance: Instance,
    socket: PathBuf,
    _shutdown: watch::Sender<()>,
    _dir: TempDir,
}

impl Daemon {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = dir.path().join("daemon.sock");
        let (instance, _alice) =
            Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("alice"))
                .await
                .unwrap();
        instance.enable_sync().await.unwrap();
        instance
            .sync()
            .unwrap()
            .register_transport("http", HttpTransport::builder())
            .await
            .unwrap();
        let (shutdown, rx) = watch::channel(());
        let server = ServiceServer::bind(instance.clone(), &socket)
            .await
            .unwrap();
        tokio::spawn(server.run(rx));
        Self {
            instance,
            socket,
            _shutdown: shutdown,
            _dir: dir,
        }
    }

    async fn connect(&self) -> Instance {
        Instance::connect(format!("unix://{}", self.socket.display()))
            .await
            .unwrap()
    }
}

fn is_pending(error: &Error) -> bool {
    matches!(error, Error::Sync(e) if matches!(**e, SyncError::BootstrapPending { .. }))
}

async fn read_string(db: &eidetica::Database, key: &str) -> eidetica::Result<String> {
    db.get_store_viewer::<DocStore>("messages")
        .await?
        .get_string(key)
        .await
}

/// A connected client joins, opens, writes, and receives peer writes, while
/// the daemon owns every network exchange and the client runs no sync engine.
#[tokio::test]
async fn connected_join_opens_database_and_daemon_keeps_it_in_sync() {
    let peer = Peer::start(true).await;
    let (tree, ticket, peer_db) = (peer.tree(), peer.ticket.clone(), peer.db.clone());
    peer_db
        .with_transaction(|tx| async move {
            tx.get_store::<DocStore>("messages")
                .await?
                .set_string("from_peer", "hello")
                .await
        })
        .await
        .unwrap();

    let daemon = Daemon::start().await;
    let service = daemon.connect().await;
    let mut user = service.login_user("alice", None).await.unwrap();
    // A key other than the login key must be proven to the daemon first.
    let key = user.add_private_key(Some("joiner")).await.unwrap();

    let db = user
        .join(
            &ticket,
            &key,
            Permission::Write(5),
            SyncSettings::on_commit().with_interval(19),
            None,
        )
        .await
        .expect("an authorized ticket join should open the database");

    assert_eq!(db.root_id(), &tree);
    assert_eq!(read_string(&db, "from_peer").await.unwrap(), "hello");
    let tracked = user.database(&tree).await.unwrap();
    assert_eq!(tracked.key_id, key);
    assert!(tracked.sync_settings.sync_enabled);
    assert_eq!(tracked.sync_settings.interval_seconds, Some(19));
    assert!(db.is_shared().await.unwrap());
    assert!(
        service.sync().is_none(),
        "a connected client must not start its own sync engine"
    );
    assert!(
        user.open_database(&tree).await.is_ok(),
        "the recorded mapping must make the database openable again"
    );

    // Client write -> daemon -> peer.
    db.with_transaction(|tx| async move {
        tx.get_store::<DocStore>("messages")
            .await?
            .set_string("from_client", "world")
            .await
    })
    .await
    .unwrap();
    daemon.instance.flush_sync().await.unwrap();
    assert_eq!(read_string(&peer_db, "from_client").await.unwrap(), "world");

    // Peer write -> daemon, pulled by the daemon's own sync engine.
    peer_db
        .with_transaction(|tx| async move {
            tx.get_store::<DocStore>("messages")
                .await?
                .set_string("later", "again")
                .await
        })
        .await
        .unwrap();
    daemon
        .instance
        .sync()
        .unwrap()
        .sync_tree_with_peer(&peer.instance.id(), &tree)
        .await
        .unwrap();
    assert_eq!(read_string(&db, "later").await.unwrap(), "again");

    peer.sync.stop_server().await.unwrap();
}

/// A pending request records a provisional mapping, keeps its approver
/// metadata, and a retry after approval opens the database.
#[tokio::test]
async fn connected_join_pending_then_approved_retry_opens_database() {
    let peer = Peer::start(false).await;
    let (tree, ticket) = (peer.tree(), peer.ticket.clone());

    let daemon = Daemon::start().await;
    let service = daemon.connect().await;
    let mut user = service.login_user("alice", None).await.unwrap();
    let key = user.get_default_key().unwrap();
    let mut metadata = Doc::new();
    metadata.set("device", "laptop");
    let settings = SyncSettings::on_commit().with_interval(29);

    let error = user
        .join(
            &ticket,
            &key,
            Permission::Write(5),
            settings.clone(),
            Some(metadata),
        )
        .await
        .expect_err("a manual-approval peer should leave the join pending");
    assert!(
        is_pending(&error),
        "expected BootstrapPending, got {error:?}"
    );
    assert_eq!(
        user.database(&tree)
            .await
            .unwrap()
            .sync_settings
            .interval_seconds,
        Some(29),
        "pending join must keep the requested preferences"
    );
    let open_error = user.open_database(&tree).await.unwrap_err();
    assert!(
        matches!(&open_error, Error::User(e) if matches!(**e, UserError::DatabaseAccessPending { .. })),
        "expected DatabaseAccessPending, got {open_error:?}"
    );

    // Retrying while still pending resumes the same request.
    let retry = user
        .join(&ticket, &key, Permission::Write(5), settings.clone(), None)
        .await
        .unwrap_err();
    assert!(
        is_pending(&retry),
        "expected BootstrapPending, got {retry:?}"
    );
    let pending = peer.sync.pending_bootstrap_requests().await.unwrap();
    assert_eq!(pending.len(), 1, "a retry must not duplicate the request");
    let (request_id, request) = &pending[0];
    assert_eq!(request.requesting_pubkey, key);
    assert_eq!(
        request
            .metadata
            .as_ref()
            .and_then(|m| m.get_as::<String>("device")),
        Some("laptop".to_string()),
        "approver metadata must cross the socket"
    );

    peer.user
        .approve_bootstrap_request(&peer.sync, request_id, &peer.admin_key)
        .await
        .unwrap();
    let db = user
        .join(&ticket, &key, Permission::Write(5), settings, None)
        .await
        .expect("join should succeed once approved");
    db.with_transaction(|tx| async move {
        tx.get_store::<DocStore>("messages")
            .await?
            .set_string("approved", "yes")
            .await
    })
    .await
    .expect("the approved key should be able to write");

    peer.sync.stop_server().await.unwrap();
}

#[tokio::test]
async fn connected_join_reports_rejection() {
    let peer = Peer::start(false).await;
    let ticket = peer.ticket.clone();

    let daemon = Daemon::start().await;
    let service = daemon.connect().await;
    let mut user = service.login_user("alice", None).await.unwrap();
    let key = user.get_default_key().unwrap();

    let error = user
        .join(
            &ticket,
            &key,
            Permission::Write(5),
            SyncSettings::on_commit(),
            None,
        )
        .await
        .unwrap_err();
    assert!(is_pending(&error));
    let pending = peer.sync.pending_bootstrap_requests().await.unwrap();
    peer.user
        .reject_bootstrap_request(&peer.sync, &pending[0].0, &peer.admin_key)
        .await
        .unwrap();

    let error = user
        .join(
            &ticket,
            &key,
            Permission::Write(5),
            SyncSettings::on_commit(),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&error, Error::Sync(e) if matches!(**e, SyncError::BootstrapRejected { .. })),
        "expected BootstrapRejected, got {error:?}"
    );

    peer.sync.stop_server().await.unwrap();
}

#[tokio::test]
async fn connected_join_with_unreachable_ticket_records_nothing() {
    let daemon = Daemon::start().await;
    let service = daemon.connect().await;
    let mut user = service.login_user("alice", None).await.unwrap();
    let key = user.get_default_key().unwrap();
    // Bind and drop a listener so the port is closed.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = Address::http(closed.local_addr().unwrap().to_string());
    drop(closed);
    let tree = eidetica::entry::ID::from_bytes("absent-database");
    let ticket = DatabaseTicket::with_addresses(tree.clone(), vec![address]);

    user.join(
        &ticket,
        &key,
        Permission::Read,
        SyncSettings::on_commit(),
        None,
    )
    .await
    .expect_err("an unreachable ticket cannot be joined");
    assert!(
        user.database(&tree).await.is_err(),
        "a failed join must not track the database"
    );
}

/// Read the next response frame, failing on notifications.
async fn read_response<R: AsyncRead + Unpin>(reader: &mut R) -> ServiceResponse {
    match read_frame::<_, ServerFrame>(reader).await.unwrap().unwrap() {
        ServerFrame::Response(response) => *response,
        ServerFrame::Notification(n) => panic!("unexpected notification: {n:?}"),
    }
}

async fn request<R, W>(reader: &mut R, writer: &mut W, request: ServiceRequest) -> ServiceResponse
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    write_frame(writer, &request).await.unwrap();
    read_response(reader).await
}

/// The daemon acts only for a key the connection has proven, and only with a
/// proof that covers the route it selected; a refused request leaves nothing
/// pending on the peer. The wire request carries a proof, never a private key.
#[tokio::test]
async fn daemon_refuses_unproven_keys_and_mismatched_proofs() {
    let peer = Peer::start(false).await;
    let (tree, ticket) = (peer.tree(), peer.ticket.clone());

    let daemon = Daemon::start().await;
    let alice = daemon.instance.login_user("alice", None).await.unwrap();
    let login_key = alice
        .get_signing_key(&alice.get_default_key().unwrap())
        .unwrap();

    let stream = UnixStream::connect(&daemon.socket).await.unwrap();
    let (mut reader, mut writer) = tokio::io::split(stream);
    write_frame(
        &mut writer,
        &Handshake {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await
    .unwrap();
    let _: HandshakeAck = read_frame(&mut reader).await.unwrap().unwrap();

    // Before login, even route selection is refused.
    let response = request(
        &mut reader,
        &mut writer,
        ServiceRequest::TicketBootstrapPrepare {
            ticket: ticket.clone(),
        },
    )
    .await;
    assert!(
        matches!(response, ServiceResponse::Error(_)),
        "{response:?}"
    );

    let ServiceResponse::TrustedLoginChallenge { challenge, .. } = request(
        &mut reader,
        &mut writer,
        ServiceRequest::TrustedLoginUser {
            username: "alice".to_string(),
        },
    )
    .await
    else {
        panic!("expected a login challenge");
    };
    let response = request(
        &mut reader,
        &mut writer,
        ServiceRequest::TrustedLoginProve {
            signature: create_challenge_response(&challenge, &login_key),
        },
    )
    .await;
    assert!(matches!(response, ServiceResponse::TrustedLoginOk));

    let ServiceResponse::TicketBootstrapRoute {
        address,
        peer: route_peer,
        tips,
    } = request(
        &mut reader,
        &mut writer,
        ServiceRequest::TicketBootstrapPrepare {
            ticket: ticket.clone(),
        },
    )
    .await
    else {
        panic!("expected a ticket route");
    };
    assert!(tips.is_empty(), "the daemon does not hold the database yet");
    let now = eidetica::Clock::now_millis(&eidetica::SystemClock);
    let bootstrap = |auth: SyncRequestAuth| {
        ServiceRequest::TicketBootstrap(Box::new(TicketBootstrapRequest {
            database_id: tree.clone(),
            address: address.clone(),
            peer: route_peer.clone(),
            tips: tips.clone(),
            requesting_key_name: "device".to_string(),
            requested_permission: Permission::Write(5),
            metadata: None,
            auth,
        }))
    };

    // A key this connection never proved.
    let (unproven, _) = generate_keypair();
    let response = request(
        &mut reader,
        &mut writer,
        bootstrap(SyncRequestAuth::sign(
            &unproven,
            &route_peer,
            &tree,
            &tips,
            now,
        )),
    )
    .await;
    let ServiceResponse::Error(error) = response else {
        panic!("an unproven key must be refused, got {response:?}");
    };
    assert!(error.message.contains("session keyset"), "{error:?}");

    // A proven key whose proof names another database.
    let other_tree = eidetica::entry::ID::from_bytes("another-database");
    let response = request(
        &mut reader,
        &mut writer,
        bootstrap(SyncRequestAuth::sign(
            &login_key,
            &route_peer,
            &other_tree,
            &tips,
            now,
        )),
    )
    .await;
    assert!(
        matches!(response, ServiceResponse::Error(_)),
        "a proof for another database must be refused, got {response:?}"
    );
    assert!(
        peer.sync
            .pending_bootstrap_requests()
            .await
            .unwrap()
            .is_empty(),
        "refused requests must never reach the peer"
    );

    // The correctly bound proof reaches the peer and comes back pending.
    let response = request(
        &mut reader,
        &mut writer,
        bootstrap(SyncRequestAuth::sign(
            &login_key,
            &route_peer,
            &tree,
            &tips,
            now,
        )),
    )
    .await;
    assert!(
        matches!(
            response,
            ServiceResponse::TicketBootstrapOutcome(TicketBootstrapOutcome::Pending { .. })
        ),
        "{response:?}"
    );
    let pending = peer.sync.pending_bootstrap_requests().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].1.requesting_pubkey, login_key.public_key());

    peer.sync.stop_server().await.unwrap();
}

/// The same call joins through an embedded instance's own sync engine.
#[tokio::test]
async fn embedded_join_uses_local_sync() {
    let (_peer, _peer_user, _peer_key, _peer_db, peer_sync, tree) =
        setup_global_wildcard_server().await;
    let peer_addr = start_sync_server(&peer_sync).await;
    let ticket = DatabaseTicket::with_addresses(tree.clone(), vec![peer_addr]);

    let (_instance, mut user, key, sync) = setup_sync_enabled_client("client", "client_key").await;
    sync.register_transport("http", HttpTransport::builder())
        .await
        .unwrap();
    let db = user
        .join(
            &ticket,
            &key,
            Permission::Write(5),
            SyncSettings::on_commit(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(db.root_id(), &tree);
    assert!(user.is_sync_enabled(&tree).await.unwrap());

    peer_sync.stop_server().await.unwrap();
}

#[tokio::test]
async fn embedded_join_without_sync_is_refused() {
    let (_instance, mut user) = crate::helpers::test_local_instance_with_user("client").await;
    let key = user.get_default_key().unwrap();
    let ticket = DatabaseTicket::with_addresses(
        eidetica::entry::ID::from_bytes("absent-database"),
        vec![Address::http("127.0.0.1:9")],
    );
    let error = user
        .join(
            &ticket,
            &key,
            Permission::Read,
            SyncSettings::on_commit(),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&error, Error::Sync(e) if matches!(**e, SyncError::SyncNotEnabled)),
        "{error:?}"
    );
}
