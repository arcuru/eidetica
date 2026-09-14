#![cfg(unix)]

use std::{path::PathBuf, process::Command};

use chrono::{TimeZone, Utc};
use eidetica::{
    Instance, NewUser, Result,
    auth::{AuthKey, Permission},
    backend::database::InMemory,
    service::ServiceServer,
    sync::{peer_types::Address, transports::http::HttpTransport},
    user::types::SyncSettings,
};
use shistory::{HistoryEntry, connect_to, finish, open_database, query, setup, start};
use tempfile::TempDir;
use tokio::{sync::watch, task::JoinHandle};

struct Daemon {
    instance: Instance,
    socket: PathBuf,
    socket_url: String,
    shutdown: watch::Sender<()>,
    service: JoinHandle<Result<()>>,
    _dir: TempDir,
}

impl Daemon {
    async fn start(user: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("service.sock");
        let (instance, _) =
            Instance::create_backend(Box::new(InMemory::new()), NewUser::passwordless(user))
                .await
                .unwrap();
        let service = ServiceServer::bind(instance.clone(), &socket)
            .await
            .unwrap();
        let (shutdown, rx) = watch::channel(());
        let handle = tokio::spawn(service.run(rx));
        Self {
            instance,
            socket: socket.clone(),
            socket_url: format!("unix://{}", socket.display()),
            shutdown,
            service: handle,
            _dir: dir,
        }
    }

    async fn stop(self) {
        drop(self.shutdown);
        self.service.await.unwrap().unwrap();
    }
}

async fn run_cli(daemon: &Daemon, args: &[&str]) -> std::process::Output {
    let socket = daemon.socket.clone();
    let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_shistory"))
            .env("EIDETICA_SOCKET", socket)
            .args(args)
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}

fn time(second: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 14, 12, 0, second)
        .single()
        .unwrap()
}

#[tokio::test]
async fn service_capture_query_concurrency_and_incomplete_records() -> Result<()> {
    let daemon = Daemon::start("alice").await;
    let (_client, mut user) = connect_to(&daemon.socket_url, "alice").await?;
    let database = setup(&mut user, "laptop").await?;
    setup(&mut user, "server").await?;

    let one = start(&database, "laptop", "shell-a", "/one", "sleep 1", time(1))
        .await?
        .unwrap();
    let two = start(&database, "laptop", "shell-b", "/two", "false", time(2))
        .await?
        .unwrap();
    let skipped = start(&database, "laptop", "shell-a", "/", " secret", time(3)).await?;
    let remote = start(
        &database,
        "server",
        "shell-c",
        "/srv",
        "echo needle",
        time(4),
    )
    .await?
    .unwrap();

    finish(&database, "laptop", &two, 8, 1).await?;
    finish(&database, "server", &remote, 3, 0).await?;

    assert!(skipped.is_none());
    let laptop = query(&database, Some("laptop"), None, 10).await?;
    assert_eq!(laptop.len(), 2);
    assert_eq!(laptop[0].command, "false");
    assert_eq!(laptop[0].exit_status, Some(1));
    assert_eq!(laptop[1].command, "sleep 1");
    assert_eq!(laptop[1].exit_status, None);
    assert_ne!(one, two);

    let all = query(&database, None, Some("needle"), 10).await?;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].host, "server");
    assert_eq!(query(&database, None, None, 2).await?.len(), 2);

    daemon.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_captures_and_queries_through_configured_socket() {
    let daemon = Daemon::start("alice").await;
    let setup = run_cli(&daemon, &["--user", "alice", "--host", "laptop", "setup"]).await;
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );

    let start = run_cli(
        &daemon,
        &[
            "--user",
            "alice",
            "--host",
            "laptop",
            "start",
            "--session",
            "cli-shell",
            "--cwd",
            "/tmp",
            "--started-at",
            "2026-09-14T12:00:00Z",
            "echo cli needle",
        ],
    )
    .await;
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    let id = String::from_utf8(start.stdout).unwrap();
    let finish = run_cli(
        &daemon,
        &[
            "--user",
            "alice",
            "--host",
            "laptop",
            "finish",
            id.trim(),
            "--duration-ms",
            "12",
            "--exit-status",
            "0",
        ],
    )
    .await;
    assert!(
        finish.status.success(),
        "{}",
        String::from_utf8_lossy(&finish.stderr)
    );

    let search = run_cli(
        &daemon,
        &[
            "--user",
            "alice",
            "search",
            "needle",
            "--all-hosts",
            "--limit",
            "1",
        ],
    )
    .await;
    assert!(
        search.status.success(),
        "{}",
        String::from_utf8_lossy(&search.stderr)
    );
    let output = String::from_utf8(search.stdout).unwrap();
    assert!(output.contains("\tlaptop\t0\t12\t/tmp\techo cli needle"));

    daemon.stop().await;
}

#[tokio::test]
async fn no_peer_write_converges_between_two_service_daemons() -> Result<()> {
    let first = Daemon::start("alice").await;
    let second = Daemon::start("alice").await;
    first.instance.enable_sync().await?;
    second.instance.enable_sync().await?;
    let first_sync = first.instance.sync().unwrap();
    let second_sync = second.instance.sync().unwrap();
    first_sync
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;
    second_sync
        .register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
        .await?;
    first_sync.accept_connections().await?;
    second_sync.accept_connections().await?;
    let first_address = Address::http(first_sync.get_server_address_for("http").await?);
    let second_address = Address::http(second_sync.get_server_address_for("http").await?);

    let (_first_client, mut first_user) = connect_to(&first.socket_url, "alice").await?;
    let first_database = setup(&mut first_user, "laptop").await?;
    let database_id = first_database.root_id().clone();
    let transaction = first_database.new_transaction().await?;
    transaction
        .get_settings()?
        .set_global_auth_key(AuthKey::active(None, Permission::Admin(0)))
        .await?;
    transaction.commit().await?;
    first_user
        .track_database(
            database_id.clone(),
            &first_user.get_default_key()?,
            SyncSettings::on_commit(),
        )
        .await?;

    let offline_id = start(
        &first_database,
        "laptop",
        "offline-shell",
        "/tmp",
        "echo offline",
        time(1),
    )
    .await?
    .unwrap();
    finish(&first_database, "laptop", &offline_id, 7, 0).await?;

    second_sync
        .sync_with_peer(&first_address, Some(&database_id))
        .await?;
    let (_second_client, mut second_user) = connect_to(&second.socket_url, "alice").await?;
    second_user
        .track_database(
            database_id.clone(),
            &second_user.get_default_key()?,
            SyncSettings::on_commit(),
        )
        .await?;
    setup(&mut second_user, "server").await?;
    first_sync
        .add_peer_address(&second.instance.id(), second_address)
        .await?;
    second_sync
        .add_peer_address(&first.instance.id(), first_address)
        .await?;
    first_sync
        .add_tree_sync(&second.instance.id(), &database_id)
        .await?;
    second_sync
        .add_tree_sync(&first.instance.id(), &database_id)
        .await?;

    let second_database = open_database(&second_user).await?;
    let server_id = start(
        &second_database,
        "server",
        "server-shell",
        "/srv",
        "echo server",
        time(2),
    )
    .await?
    .unwrap();
    finish(&second_database, "server", &server_id, 4, 0).await?;
    second.instance.flush_sync().await?;
    first.instance.flush_sync().await?;

    let entries = query(&first_database, None, None, 10).await?;
    assert_eq!(commands(entries), ["echo server", "echo offline"]);

    first_sync.stop_server().await?;
    second_sync.stop_server().await?;
    first.stop().await;
    second.stop().await;
    Ok(())
}

fn commands(entries: Vec<HistoryEntry>) -> Vec<String> {
    entries.into_iter().map(|entry| entry.command).collect()
}

#[test]
fn daemon_unavailable_is_a_clear_error() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let error = runtime
        .block_on(connect_to(
            format!("unix://{}", dir.path().join("missing.sock").display()),
            "alice",
        ))
        .unwrap_err();
    assert!(error.to_string().contains("missing.sock"));
}
