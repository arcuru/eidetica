#![cfg(unix)]

use std::{path::PathBuf, process::Command};

use chrono::{TimeZone, Utc};
use eidetica::{
    Instance, NewUser, Result, backend::database::InMemory, service::ServiceServer,
    sync::transports::http::HttpTransport,
};
use shistory::{
    HistoryEntry, connect_to, finish, host, open_database, print_entries, query, rename, setup,
    start,
};
use tempfile::TempDir;
use tokio::{sync::watch, task::JoinHandle};
use uuid::Uuid;

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
        instance.enable_sync().await.unwrap();
        let sync = instance.sync().unwrap();
        sync.register_transport("http", HttpTransport::builder().bind("127.0.0.1:0"))
            .await
            .unwrap();
        sync.accept_connections().await.unwrap();
        sync.get_server_address_for("http").await.unwrap();
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
        self.instance.sync().unwrap().stop_server().await.unwrap();
        drop(self.shutdown);
        self.service.await.unwrap().unwrap();
    }
}

async fn run_cli(daemon: &Daemon, args: &[&str]) -> std::process::Output {
    run_cli_with_stdin(daemon, args, b"").await
}

async fn run_cli_with_stdin(daemon: &Daemon, args: &[&str], stdin: &[u8]) -> std::process::Output {
    use std::io::Write;

    let socket = daemon.socket.clone();
    let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    let stdin = stdin.to_vec();
    tokio::task::spawn_blocking(move || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_shistory"))
            .env("EIDETICA_SOCKET", socket)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&stdin).unwrap();
        child.wait_with_output().unwrap()
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
async fn service_capture_query_and_incomplete_records() -> Result<()> {
    let daemon = Daemon::start("alice").await;
    let (_client, mut user) = connect_to(&daemon.socket_url, "alice").await?;
    let laptop = setup(&mut user, None, "laptop").await?;
    let database = open_database(&user).await?;

    let one = start(&database, laptop.id, "shell-a", "/one", "sleep 1", time(1))
        .await?
        .unwrap();
    let two = start(&database, laptop.id, "shell-b", "/two", "false", time(2))
        .await?
        .unwrap();
    let skipped = start(&database, laptop.id, "shell-a", "/", " secret", time(3)).await?;
    finish(&database, laptop.id, &two, 8, 1).await?;

    assert!(skipped.is_none());
    let entries = query(&database, Some(laptop.id), None, 10).await?;
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].command, "false");
    assert_eq!(entries[0].exit_status, Some(1));
    assert_eq!(entries[1].command, "sleep 1");
    assert_eq!(entries[1].exit_status, None);
    assert_ne!(one, two);

    daemon.stop().await;
    Ok(())
}

#[tokio::test]
async fn ticket_is_sufficient_to_setup_a_second_host() -> Result<()> {
    let first = Daemon::start("alice").await;
    let second = Daemon::start("bob").await;
    let (_first_client, mut first_user) = connect_to(&first.socket_url, "alice").await?;
    let laptop = setup(&mut first_user, None, "laptop").await?;
    let first_database = open_database(&first_user).await?;
    first_user.disable_sync(first_database.root_id()).await?;
    assert!(!first_user.is_sync_enabled(first_database.root_id()).await?);
    let ticket = shistory::ticket(&mut first_user).await?;
    assert_eq!(ticket.database_id(), first_database.root_id());
    assert!(!ticket.addresses().is_empty());
    assert!(first_user.is_sync_enabled(first_database.root_id()).await?);

    let (_second_client, mut second_user) = connect_to(&second.socket_url, "bob").await?;
    let server = setup(&mut second_user, Some(&ticket), "server").await?;
    let second_database = open_database(&second_user).await?;
    assert_eq!(second_database.root_id(), first_database.root_id());

    let record = start(
        &second_database,
        server.id,
        "server-shell",
        "/srv",
        "echo joined",
        time(2),
    )
    .await?
    .unwrap();
    finish(&second_database, server.id, &record, 4, 0).await?;
    second.instance.flush_sync().await?;
    first.instance.flush_sync().await?;

    let entries = query(&first_database, None, None, 10).await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "echo joined");
    assert_eq!(host(&first_database, laptop.id).await?.name, "laptop");
    assert_eq!(host(&first_database, server.id).await?.name, "server");

    first.stop().await;
    second.stop().await;
    Ok(())
}

#[tokio::test]
async fn display_name_change_keeps_uuid_store_and_history_ownership() -> Result<()> {
    let daemon = Daemon::start("alice").await;
    let (_client, mut user) = connect_to(&daemon.socket_url, "alice").await?;
    let host_record = setup(&mut user, None, "before").await?;
    let database = open_database(&user).await?;
    let record = start(
        &database,
        host_record.id,
        "shell",
        "/tmp",
        "echo before",
        time(1),
    )
    .await?
    .unwrap();
    finish(&database, host_record.id, &record, 1, 0).await?;

    rename(&database, host_record.id, "after").await?;
    let second = start(
        &database,
        host_record.id,
        "shell",
        "/tmp",
        "echo after",
        time(2),
    )
    .await?
    .unwrap();
    finish(&database, host_record.id, &second, 1, 0).await?;

    assert_eq!(host(&database, host_record.id).await?.name, "after");
    let entries = query(&database, Some(host_record.id), None, 10).await?;
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].host_id, host_record.id);
    assert_eq!(entries[1].host_id, host_record.id);
    assert_eq!(entries[0].host_name, "after");
    assert_eq!(entries[1].host_name, "after");
    assert_eq!(entries[1].command, "echo before");

    daemon.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_captures_and_queries_through_configured_socket() {
    let daemon = Daemon::start("alice").await;
    let setup = run_cli(&daemon, &["--user", "alice", "setup", "--name", "laptop"]).await;
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let setup_output = String::from_utf8(setup.stdout).unwrap();
    let host_id = setup_output
        .lines()
        .find_map(|line| line.strip_prefix("host id: "))
        .unwrap();
    Uuid::parse_str(host_id).unwrap();

    let start = run_cli_with_stdin(
        &daemon,
        &[
            "--user",
            "alice",
            "--host-id",
            host_id,
            "start",
            "--session",
            "cli-shell",
            "--cwd",
            "/tmp",
            "--started-at",
            "2026-09-14T12:00:00Z",
        ],
        b"echo cli needle\n\n",
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
            "--host-id",
            host_id,
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
    assert!(
        String::from_utf8(search.stdout)
            .unwrap()
            .contains("\tlaptop\t0\t12\t/tmp\techo cli needle\\n\\n")
    );

    daemon.stop().await;
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

#[cfg(target_os = "linux")]
#[test]
fn cli_command_text_is_not_exposed_in_argv() {
    use std::{
        fs, os::unix::net::UnixListener, process::Stdio, sync::mpsc, thread, time::Duration,
    };

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("stalled.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        accepted_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });

    let secret = "echo synthetic-secret-argv";
    let mut child = Command::new(env!("CARGO_BIN_EXE_shistory"))
        .env("EIDETICA_SOCKET", &socket)
        .args([
            "--user",
            "alice",
            "--host-id",
            "4cc330d8-b6af-44e5-a46b-eb700df805c5",
            "start",
            "--session",
            "cli-shell",
            "--cwd",
            "/tmp",
            "--started-at",
            "2026-09-14T12:00:00Z",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(secret.as_bytes())
        .unwrap();
    accepted_rx.recv_timeout(Duration::from_secs(1)).unwrap();

    let command_line = fs::read(format!("/proc/{}/cmdline", child.id())).unwrap();
    assert!(child.try_wait().unwrap().is_none());
    child.kill().unwrap();
    child.wait().unwrap();
    release_tx.send(()).unwrap();
    server.join().unwrap();

    assert!(
        !command_line
            .windows(secret.len())
            .any(|window| window == secret.as_bytes())
    );
}

#[test]
fn displayed_entries_escape_control_characters_into_one_row() {
    let entry = HistoryEntry {
        command: "print 'one\ttwo'\nprint \u{1b}[31mthree\r".to_owned(),
        started_at: time(0),
        cwd: "/tmp/with\ttab\nand-newline".to_owned(),
        duration_ms: Some(1),
        exit_status: Some(0),
        host_id: Uuid::nil(),
        host_name: "fixture\thost\nname".to_owned(),
        session: "fixture-session".to_owned(),
    };
    let mut output = Vec::new();

    print_entries(&mut output, &[entry]).unwrap();

    assert_eq!(
        String::from_utf8(output).unwrap(),
        "2026-09-14T12:00:00+00:00\tfixture\\thost\\nname\t0\t1\t/tmp/with\\ttab\\nand-newline\tprint 'one\\ttwo'\\nprint \\u{1b}[31mthree\\r\n"
    );
}
