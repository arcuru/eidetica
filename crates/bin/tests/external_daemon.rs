//! The service integration derivation sets EIDETICA_EXTERNAL_SOCKET to exercise
//! the *external* daemon binary; ordinary unit runs have no external daemon.

use axum::{Json, Router, routing::post};
use eidetica::{
    Instance, NewUser,
    auth::{AuthKey, Permission},
    backend::database::InMemory,
    crdt::Doc,
    store::DocStore,
    sync::{
        Address, DatabaseTicket,
        handler::{SyncHandler, SyncHandlerImpl},
        protocol::{RequestContext, SyncRequest, SyncResponse, SyncTreeRequest},
        transports::http::HttpTransport,
    },
    user::SyncSettings,
};
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, Semaphore};

#[tokio::test]
async fn socket_write_visible_to_dashboard() {
    let Ok(socket) = std::env::var("EIDETICA_EXTERNAL_SOCKET") else {
        return;
    };
    let instance = eidetica::Instance::connect(format!("unix://{socket}"))
        .await
        .expect("connect to real daemon");
    let mut admin = instance.login_user("admin", None).await.expect("login");
    let mut settings = eidetica::crdt::Doc::new();
    settings.set("name", "socket_written_database");
    let key = admin.get_default_key().expect("default key");
    let db = admin
        .create_database(settings, &key)
        .await
        .expect("write over socket");
    std::fs::write(
        std::env::var("EIDETICA_EXTERNAL_DB_ID_FILE").expect("result path"),
        db.root_id().to_string(),
    )
    .expect("record ID for dashboard assertion");
}

async fn public_database(instance: &Instance, username: &str) -> eidetica::Database {
    let mut user = instance.login_user(username, None).await.unwrap();
    let key = user.get_default_key().unwrap();
    let db = user.create_database(Doc::new(), &key).await.unwrap();
    let tx = db.new_transaction().await.unwrap();
    tx.get_settings()
        .unwrap()
        .set_global_auth_key(AuthKey::active(None, Permission::Read))
        .await
        .unwrap();
    tx.get_settings()
        .unwrap()
        .set_auth_key(
            &instance.id(),
            AuthKey::active(Some("device"), Permission::Admin(0)),
        )
        .await
        .unwrap();
    tx.get_store::<DocStore>("data")
        .await
        .unwrap()
        .set_string("marker", "daemon HTTP sync")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    db.share().await.unwrap();
    db
}

#[tokio::test]
async fn daemon_http_inbound_sync() {
    let Ok(socket) = std::env::var("EIDETICA_EXTERNAL_SOCKET") else {
        return;
    };
    let address = std::env::var("EIDETICA_EXTERNAL_HTTP_BIND").expect("HTTP bind for integration");
    tokio::time::timeout(Duration::from_secs(10), async {
        let daemon = Instance::connect(format!("unix://{socket}")).await.unwrap();
        let mut admin = daemon.login_user("admin", None).await.unwrap();
        let private = admin
            .create_database(Doc::new(), &admin.get_default_key().unwrap())
            .await
            .unwrap();
        private.share().await.unwrap();
        let denied = reqwest::Client::new()
            .post(format!("http://{address}/api/v0"))
            .json(&SyncRequest::SyncTree(SyncTreeRequest {
                tree_id: private.root_id().clone(),
                our_tips: Vec::new().into(),
                peer_pubkey: None,
                requesting_key: None,
                requesting_key_name: None,
                requested_permission: None,
                metadata: None,
                auth: None,
            }))
            .send()
            .await
            .unwrap()
            .json::<SyncResponse>()
            .await
            .unwrap();
        assert!(
            matches!(&denied, SyncResponse::Error(message) if message.starts_with("Authentication required:")),
            "private database must refuse anonymous HTTP sync: {denied:?}"
        );
        let db = public_database(&daemon, "admin").await;
        let (peer, _) =
            Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("peer"))
                .await
                .unwrap();
        peer.enable_sync().await.unwrap();
        let sync = peer.sync().unwrap();
        sync.register_transport("http", HttpTransport::builder())
            .await
            .unwrap();
        sync.sync_with_peer(&Address::http(address), Some(db.root_id()))
            .await
            .unwrap();
        let copied = eidetica::Database::open(&peer, db.root_id()).await.unwrap();
        assert_eq!(
            copied
                .get_store_viewer::<DocStore>("data")
                .await
                .unwrap()
                .get_string("marker")
                .await
                .unwrap(),
            "daemon HTTP sync"
        );
        assert_eq!(
            copied.snapshot().await.unwrap(),
            db.snapshot().await.unwrap()
        );
    })
    .await
    .expect("inbound daemon HTTP sync must complete within a bound");
}

struct DaemonChild {
    process: Child,
    socket: PathBuf,
    log_path: PathBuf,
    _dir: tempfile::TempDir,
}

impl DaemonChild {
    fn spawn(binary: &str, ticket: &DatabaseTicket, args: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let socket = dir.path().join("service.sock");
        assert!(
            Command::new(binary)
                .args(["daemon", "--data-dir"])
                .arg(&data)
                .args(["init", "--username", "admin", "--passwordless"])
                .status()
                .unwrap()
                .success()
        );
        let log_path = dir.path().join("daemon.log");
        let log = std::fs::File::create(&log_path).unwrap();
        let process = Command::new(binary)
            .args(["daemon", "--data-dir"])
            .arg(&data)
            .arg("--socket")
            .arg(&socket)
            .arg("--sync-ticket")
            .arg(ticket.to_string())
            .args(args)
            .stdout(log.try_clone().unwrap())
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        Self {
            process,
            socket,
            log_path,
            _dir: dir,
        }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log_path).unwrap()
    }

    fn assert_running(&mut self) {
        if let Some(status) = self.process.try_wait().unwrap() {
            panic!("daemon exited {status}: {}", self.log());
        }
    }

    async fn connect(&mut self) -> Instance {
        loop {
            self.assert_running();
            if let Ok(client) = Instance::connect(format!("unix://{}", self.socket.display())).await
            {
                return client;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn terminate(&self) {
        assert!(
            Command::new("kill")
                .args(["-TERM", &self.process.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
    }

    async fn wait(&mut self) {
        loop {
            if let Some(status) = self.process.try_wait().unwrap() {
                assert!(status.success(), "{}", self.log());
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for DaemonChild {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[tokio::test]
async fn daemon_http_outbound_without_listener() {
    let Ok(binary) = std::env::var("EIDETICA_BIN") else {
        return;
    };
    tokio::time::timeout(Duration::from_secs(15), async {
        let (peer, _) =
            Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("peer"))
                .await
                .unwrap();
        peer.enable_sync().await.unwrap();
        let db = public_database(&peer, "peer").await;
        let sync = peer.sync().unwrap();
        sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
            .await
            .unwrap();
        sync.accept_connections_on("http").await.unwrap();
        let ticket = DatabaseTicket::with_addresses(
            db.root_id().clone(),
            vec![Address::http(
                sync.get_server_address_for("http").await.unwrap(),
            )],
        );
        let mut daemon = DaemonChild::spawn(&binary, &ticket, &[]);
        let client = daemon.connect().await;
        let admin = client.login_user("admin", None).await.unwrap();
        let copied = eidetica::Database::open(&client, db.root_id())
            .await
            .unwrap();
        // Read the transferred immutable entries directly: rebuilding a cold
        // client-side Store cache would require Write on this Read-only DB.
        for entry in db.get_all_entries().await.unwrap() {
            assert_eq!(
                client.backend().get(&entry.id()).await.unwrap().id(),
                entry.id()
            );
        }
        assert_eq!(
            copied.snapshot().await.unwrap(),
            db.snapshot().await.unwrap()
        );
        assert!(!daemon.log().contains("HTTP sync listening"));
        drop(copied);
        drop(admin);
        drop(client);
        daemon.terminate();
        daemon.wait().await;
        assert!(!daemon.socket.exists());
        sync.stop_server().await.unwrap();
    })
    .await
    .expect("outbound daemon HTTP bootstrap and shutdown must be bounded");
}

#[tokio::test]
async fn daemon_shared_http_listener_survives_final_flush() {
    let Ok(binary) = std::env::var("EIDETICA_BIN") else {
        return;
    };
    tokio::time::timeout(Duration::from_secs(20), async {
        let (peer, mut user) =
            Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless("peer"))
                .await
                .unwrap();
        peer.enable_sync().await.unwrap();
        let db = user
            .create_database(Doc::new(), &user.get_default_key().unwrap())
            .await
            .unwrap();
        let tx = db.new_transaction().await.unwrap();
        tx.get_settings()
            .unwrap()
            .set_global_auth_key(AuthKey::active(None, Permission::Admin(0)))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        db.share().await.unwrap();
        let handler = Arc::new(SyncHandlerImpl::new(
            peer.clone(),
            peer.sync().unwrap().sync_tree_root_id().clone(),
        ));
        let paused = Arc::new(AtomicBool::new(false));
        let sending = Arc::new(Notify::new());
        let release = Arc::new(Semaphore::new(0));
        let app = {
            let paused = paused.clone();
            let sending = sending.clone();
            let release = release.clone();
            Router::new().route(
                "/api/v0",
                post(move |Json(request): Json<SyncRequest>| {
                    let handler = handler.clone();
                    let paused = paused.clone();
                    let sending = sending.clone();
                    let release = release.clone();
                    async move {
                        if matches!(&request, SyncRequest::SendEntries(_))
                            && paused.load(Ordering::SeqCst)
                        {
                            sending.notify_one();
                            release.acquire().await.unwrap().forget();
                        }
                        Json(
                            handler
                                .handle_request(&request, &RequestContext::default())
                                .await,
                        )
                    }
                }),
            )
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ticket = DatabaseTicket::with_addresses(
            db.root_id().clone(),
            vec![Address::http(listener.local_addr().unwrap().to_string())],
        );
        let (peer_shutdown, rx) = tokio::sync::oneshot::channel::<()>();
        let peer_task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
                .unwrap();
        });
        let mut daemon = DaemonChild::spawn(
            &binary,
            &ticket,
            &[
                "--dashboard",
                "--dashboard-port",
                "0",
                "--http-sync-bind",
                "127.0.0.1:0",
            ],
        );
        let client = daemon.connect().await;
        let url = daemon
            .log()
            .lines()
            .find_map(|line| line.strip_prefix("Dashboard listening on "))
            .expect("shared dashboard address")
            .to_string();
        let mut admin = client.login_user("admin", None).await.unwrap();
        let key = admin.get_default_key().unwrap();
        admin
            .track_database(db.root_id().clone(), &key, SyncSettings::on_commit())
            .await
            .unwrap();
        let local_db = admin.open_database(db.root_id()).await.unwrap();
        paused.store(true, Ordering::SeqCst);
        let tx = local_db.new_transaction().await.unwrap();
        tx.get_store::<DocStore>("data")
            .await
            .unwrap()
            .set_string("final", "flushed before close")
            .await
            .unwrap();
        let entry = tx.commit().await.unwrap();
        sending.notified().await;
        drop(local_db);
        drop(admin);
        drop(client);
        daemon.terminate();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        loop {
            daemon.assert_running();
            if let Ok(response) = http
                .post(format!("{url}/login"))
                .header("Origin", &url)
                .form(&[("username", "admin")])
                .send()
                .await
                && response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE
                && !daemon.socket.exists()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let response = http
            .post(format!("{url}/api/v0"))
            .json(&SyncRequest::SendEntries(Vec::new()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(matches!(
            response.json::<SyncResponse>().await.unwrap(),
            SyncResponse::Ack
        ));
        paused.store(false, Ordering::SeqCst);
        release.add_permits(1);
        daemon.wait().await;
        assert_eq!(peer.backend().get(&entry).await.unwrap().id(), entry);
        assert!(
            http.post(format!("{url}/api/v0"))
                .json(&SyncRequest::SendEntries(Vec::new()))
                .send()
                .await
                .is_err()
        );
        peer_shutdown.send(()).unwrap();
        peer_task.await.unwrap();
    })
    .await
    .expect("shared listener final flush must be bounded");
}
