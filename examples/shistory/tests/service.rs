#![cfg(unix)]

use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use chrono::{TimeZone, Utc};
use eidetica::{
    AccountSession, Database, Error, ID, Instance, NewAccount, Result,
    account::SyncSettings,
    auth::{AuthKey, Permission},
    backend::database::InMemory,
    crdt::Doc,
    service::ServiceServer,
    store::Table,
    sync::{DatabaseTicket, SyncError, transports::http::HttpTransport},
};
use shistory::{
    COMMAND_DISPLAY_LIMIT, CWD_DISPLAY_LIMIT, DEFAULT_LIMIT, HOST_DISPLAY_LIMIT, HistoryEntry,
    Host, MAX_LIMIT, connect_to, finish, host, open_database, print_entries, print_summary, query,
    rename, setup, start, summarize,
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
        Self::start_backend(
            user,
            Box::new(InMemory::new()),
            tempfile::tempdir().unwrap(),
        )
        .await
    }

    async fn start_backend(
        user: &str,
        backend: Box<dyn eidetica::backend::BackendImpl>,
        dir: TempDir,
    ) -> Self {
        let socket = dir.path().join("service.sock");
        let (instance, _) = Instance::create_backend(backend, NewAccount::passwordless(user))
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
    run_cli_at_socket(&daemon.socket, args, stdin).await
}

async fn run_cli_at_socket(
    socket: &std::path::Path,
    args: &[&str],
    stdin: &[u8],
) -> std::process::Output {
    use std::io::Write;

    let socket = socket.to_owned();
    let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    let stdin = stdin.to_vec();
    tokio::task::spawn_blocking(move || {
        let mut child = Command::new(shistory_bin())
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

fn shistory_bin() -> String {
    // Nextest archives relocate binaries; keep Cargo's path for ordinary cargo test.
    std::env::var("NEXTEST_BIN_EXE_shistory")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_shistory").to_owned())
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
    let entries = query(&database, Some(laptop.id), None, 10, true).await?;
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].command, "false");
    assert_eq!(entries[0].exit_status, Some(1));
    assert_eq!(entries[1].command, "sleep 1");
    assert_eq!(entries[1].exit_status, None);
    assert_ne!(one, two);

    let before = database.snapshot().await?;
    assert!(
        start(&database, Uuid::new_v4(), "shell", "/", "true", time(4))
            .await
            .is_err()
    );
    assert_eq!(database.snapshot().await?, before);

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
    first_database.stop_sharing().await?;
    assert!(!first_database.is_shared().await?);
    let printed = run_cli(&first, &["--user", "alice", "ticket"]).await;
    assert!(
        printed.status.success(),
        "{}",
        String::from_utf8_lossy(&printed.stderr)
    );
    let ticket_text = String::from_utf8(printed.stdout).unwrap();
    let ticket = ticket_text.trim().parse::<DatabaseTicket>().unwrap();
    assert_eq!(ticket.database_id(), first_database.root_id());
    assert!(!ticket.addresses().is_empty());
    assert!(first_database.is_shared().await?);

    let joined = run_cli(
        &second,
        &[
            "--user",
            "bob",
            "setup",
            ticket_text.trim(),
            "--name",
            "server",
        ],
    )
    .await;
    assert!(
        joined.status.success(),
        "{}",
        String::from_utf8_lossy(&joined.stderr)
    );
    let output = String::from_utf8(joined.stdout).unwrap();
    let host_id = output
        .lines()
        .find_map(|line| line.strip_prefix("host id: "))
        .unwrap();
    let (_second_client, second_user) = connect_to(&second.socket_url, "bob").await?;
    let second_database = open_database(&second_user).await?;
    let server = host(&second_database, Uuid::parse_str(host_id).unwrap()).await?;
    assert_eq!(second_database.root_id(), first_database.root_id());
    assert!(_first_client.sync().is_none() && _second_client.sync().is_none());
    let settings = second_database.sync_settings().await?;
    assert!(settings.sync_enabled && settings.sync_on_commit);
    assert!(settings.interval_seconds.is_none());

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

    let entries = query(&first_database, None, None, 10, true).await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, "echo joined");
    assert_eq!(host(&first_database, laptop.id).await?.name, "laptop");
    assert_eq!(host(&first_database, server.id).await?.name, "server");

    first.stop().await;
    second.stop().await;
    Ok(())
}

// An operator-configured private peer, not Shistory's default global-Write database.
async fn private_history(daemon: &Daemon) -> Result<(AccountSession, Database, DatabaseTicket)> {
    let (_client, mut owner) = connect_to(&daemon.socket_url, "alice").await?;
    let key = owner.get_default_key()?;
    let mut settings = Doc::new();
    settings.set("name", "shistory");
    let database = owner.create_database(settings, &key).await?;
    let transaction = database.new_transaction().await?;
    transaction
        .get_settings()?
        .set_auth_key(
            &daemon.instance.id(),
            AuthKey::active(None, Permission::Admin(0)),
        )
        .await?;
    transaction.commit().await?;
    let ticket = shistory::ticket(&owner).await?;
    Ok((owner, database, ticket))
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_pending_setup_requires_explicit_retry_after_approval() -> Result<()> {
    let first = Daemon::start("alice").await;
    let (owner, _database, ticket) = private_history(&first).await?;
    let second = Daemon::start("bob").await;
    let (_client, user) = connect_to(&second.socket_url, "bob").await?;
    let user_before = user.account().database().snapshot().await?;
    let ticket_text = ticket.to_string();
    let args = ["--user", "bob", "setup", &ticket_text, "--name", "server"];

    for _ in 0..2 {
        let pending = run_cli(&second, &args).await;
        assert!(!pending.status.success());
        assert!(
            pending.stdout.is_empty(),
            "pending must not print a host UUID"
        );
        let error = String::from_utf8(pending.stderr).unwrap();
        assert!(error.contains("join pending approval"), "{error}");
        assert!(error.contains("same ticket and --name"));
        assert!(error.contains("no automatic completion"));
        assert_eq!(user.account().database().snapshot().await?, user_before);
        assert!(user.database(ticket.database_id()).await.is_err());
    }

    let sync = first.instance.sync().unwrap();
    let pending = sync.pending_bootstrap_requests().await?;
    assert_eq!(
        pending.len(),
        1,
        "retry must not duplicate the peer request"
    );
    assert_eq!(pending[0].1.requesting_pubkey, user.get_default_key()?);
    owner
        .approve_bootstrap_request(&sync, &pending[0].0, &owner.get_default_key()?)
        .await?;
    assert!(
        open_database(&user).await.is_err(),
        "approval is not completion"
    );
    assert_eq!(user.account().database().snapshot().await?, user_before);

    let joined = run_cli(&second, &args).await;
    assert!(
        joined.status.success(),
        "{}",
        String::from_utf8_lossy(&joined.stderr)
    );
    let output = String::from_utf8(joined.stdout).unwrap();
    let host_id = output
        .lines()
        .find_map(|line| line.strip_prefix("host id: "))
        .unwrap();
    let (_client, user) = connect_to(&second.socket_url, "bob").await?;
    let database = open_database(&user).await?;
    assert_eq!(database.root_id(), ticket.database_id());
    assert_eq!(
        host(&database, Uuid::parse_str(host_id).unwrap())
            .await?
            .name,
        "server"
    );
    assert_eq!(database.sync_settings().await?, SyncSettings::on_commit());
    assert!(
        database
            .get_settings()
            .await?
            .auth_snapshot()
            .await?
            .get_key_by_pubkey(&second.instance.id())
            .is_err(),
        "joining must not enroll the daemon device"
    );

    first.stop().await;
    second.stop().await;
    Ok(())
}

#[tokio::test]
async fn setup_rejection_and_held_database_access_failure_record_no_host() -> Result<()> {
    let first = Daemon::start("alice").await;
    let (owner, database, ticket) = private_history(&first).await?;
    owner
        .admin()
        .await?
        .create_account(NewAccount::passwordless("outsider"))
        .await?;
    let (_client, mut outsider) = connect_to(&first.socket_url, "outsider").await?;
    let before = outsider.account().database().snapshot().await?;
    let error = setup(&mut outsider, Some(&ticket), "outsider")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not permitted"), "{error}");
    assert_eq!(outsider.account().database().snapshot().await?, before);
    let sync = first.instance.sync().unwrap();
    assert!(sync.pending_bootstrap_requests().await?.is_empty());

    let second = Daemon::start("bob").await;
    let (_client, mut user) = connect_to(&second.socket_url, "bob").await?;
    let before = user.account().database().snapshot().await?;
    let error = setup(&mut user, Some(&ticket), "server").await.unwrap_err();
    assert!(matches!(error, Error::Sync(e) if matches!(*e, SyncError::BootstrapPending { .. })));
    let pending = sync.pending_bootstrap_requests().await?;
    owner
        .reject_bootstrap_request(&sync, &pending[0].0, &owner.get_default_key()?)
        .await?;
    let error = setup(&mut user, Some(&ticket), "server").await.unwrap_err();
    assert!(matches!(error, Error::Sync(e) if matches!(*e, SyncError::BootstrapRejected { .. })));
    assert_eq!(user.account().database().snapshot().await?, before);
    assert!(user.database(ticket.database_id()).await.is_err());
    assert!(
        database
            .get_store_viewer::<Table<Host>>("hosts")
            .await?
            .search(|_| true)
            .await?
            .is_empty()
    );

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
    let entries = query(&database, Some(host_record.id), None, 10, true).await?;
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
            .contains("start time\thost\texit status\tduration (ms)\tworking directory\tcommand\n2026-09-14T12:00:00+00:00\tlaptop\t0\t12\t/tmp\techo cli needle\\n\\n")
    );

    let summary = run_cli(&daemon, &["--user", "alice", "summary"]).await;
    assert!(
        summary.status.success(),
        "{}",
        String::from_utf8_lossy(&summary.stderr)
    );
    assert!(
        String::from_utf8(summary.stdout)
            .unwrap()
            .contains("total records: 1\n")
    );

    daemon.stop().await;
}

#[tokio::test]
async fn query_limits_unique_commands_and_summary_scans_all_history() -> Result<()> {
    let daemon = Daemon::start("alice").await;
    let (_client, mut user) = connect_to(&daemon.socket_url, "alice").await?;
    let laptop = setup(&mut user, None, "laptop").await?;
    let database = open_database(&user).await?;

    let transaction = database.new_transaction().await?;
    let history = transaction
        .get_store::<Table<HistoryEntry>>(&format!("history-{}", laptop.id))
        .await?;
    for second in 0..=MAX_LIMIT {
        history
            .insert(HistoryEntry {
                command: if second == MAX_LIMIT {
                    "rare".to_owned()
                } else {
                    "common".to_owned()
                },
                started_at: time(0) + chrono::Duration::seconds(second as i64),
                cwd: "/tmp".to_owned(),
                duration_ms: Some(second as i64),
                exit_status: Some(0),
                host_id: laptop.id,
                host_name: String::new(),
                entry_id: String::new(),
                session: "shell".to_owned(),
            })
            .await?;
    }
    transaction.commit().await?;

    let entries = query(&database, Some(laptop.id), None, DEFAULT_LIMIT, false).await?;
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].command, "rare");

    let summary = summarize(&database, None).await?;
    assert_eq!(summary.total, MAX_LIMIT + 1);
    assert_eq!(
        summary.most_common_command,
        Some(("common".to_owned(), MAX_LIMIT))
    );
    assert_eq!(summary.longest_runtime.unwrap().command, "rare");
    assert_eq!(summary.successes, MAX_LIMIT + 1);

    daemon.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_status_reports_read_only_recording_prerequisites() -> Result<()> {
    let daemon = Daemon::start("alice").await;
    let (_client, mut user) = connect_to(&daemon.socket_url, "alice").await?;
    let configured = setup(
        &mut user,
        None,
        &format!("laptop\ninjected\t{}", "x".repeat(100)),
    )
    .await?;
    let database = open_database(&user).await?;
    let host_id = configured.id.to_string();
    let user_before = user.account().database().snapshot().await?;
    let history_before = database.snapshot().await?;

    let healthy = run_cli(
        &daemon,
        &["--user", "alice", "--host-id", &host_id, "status"],
    )
    .await;
    assert!(
        healthy.status.success(),
        "{}",
        String::from_utf8_lossy(&healthy.stderr)
    );
    let healthy = String::from_utf8(healthy.stdout).unwrap();
    assert!(healthy.contains("effective socket: "));
    assert!(healthy.contains("daemon connectivity: ok"));
    assert!(healthy.contains("passwordless user: ok"));
    assert!(healthy.contains("history database: ok ("));
    assert!(healthy.contains("configured host: ok ("));
    assert!(healthy.contains(r"laptop\ninjected\t"));
    assert!(!healthy.lines().any(|line| line.contains('\t')));
    assert!(healthy.lines().all(|line| line.chars().count() < 500));
    assert!(healthy.contains("shell hooks: not checked"));
    assert!(healthy.contains("remote synchronization: not checked"));
    assert!(healthy.contains("write readiness: not checked"));
    assert_eq!(user.account().database().snapshot().await?, user_before);
    assert_eq!(database.snapshot().await?, history_before);
    assert_eq!(
        summarize(&database, None).await?.total,
        0,
        "status made no history record"
    );

    let unconfigured = run_cli(&daemon, &["--user", "alice", "status"]).await;
    assert!(!unconfigured.status.success());
    let unconfigured = String::from_utf8(unconfigured.stdout).unwrap();
    assert!(unconfigured.contains("host configuration: failed"));
    assert!(unconfigured.contains("configured host: skipped — no host UUID is configured"));
    assert_eq!(
        summarize(&database, None).await?.total,
        0,
        "unconfigured status made no history record"
    );

    let invalid = run_cli(
        &daemon,
        &[
            "--user",
            "alice",
            "--host-id",
            "00000000-0000-0000-0000-000000000000",
            "status",
        ],
    )
    .await;
    assert!(!invalid.status.success());
    let invalid = String::from_utf8(invalid.stdout).unwrap();
    assert!(invalid.contains("configured host: failed"));
    assert!(invalid.contains("use the UUID printed by setup"));
    assert_eq!(
        summarize(&database, None).await?.total,
        0,
        "failed status made no history record"
    );

    let hostile_user = format!("missing\ninjected\t{}", "x".repeat(300));
    let missing_user = run_cli(
        &daemon,
        &["--user", &hostile_user, "--host-id", &host_id, "status"],
    )
    .await;
    assert!(!missing_user.status.success());
    let missing_user = String::from_utf8(missing_user.stdout).unwrap();
    assert!(missing_user.contains("passwordless user: failed"));
    assert!(missing_user.contains("history database: skipped"));
    assert!(missing_user.contains("configured host: skipped"));
    assert!(missing_user.contains(r"missing\ninjected\t"));
    assert!(!missing_user.lines().any(|line| line.contains('\t')));
    assert!(missing_user.lines().all(|line| line.chars().count() < 500));

    daemon.stop().await;

    let no_database = Daemon::start("bob").await;
    let missing_database = run_cli(
        &no_database,
        &[
            "--user",
            "bob",
            "--host-id",
            "00000000-0000-0000-0000-000000000000",
            "status",
        ],
    )
    .await;
    assert!(!missing_database.status.success());
    let missing_database = String::from_utf8(missing_database.stdout).unwrap();
    assert!(missing_database.contains("passwordless user: ok"));
    assert!(missing_database.contains("history database: failed"));
    assert!(missing_database.contains("run shistory setup once"));
    assert!(missing_database.contains("configured host: skipped"));
    no_database.stop().await;

    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn cli_status_reports_unavailable_and_unresponsive_daemons_within_deadline() {
    use std::{os::unix::net::UnixListener, sync::mpsc, thread};

    let missing_dir = tempfile::tempdir().unwrap();
    let missing =
        run_cli_at_socket(&missing_dir.path().join("missing.sock"), &["status"], b"").await;
    assert!(!missing.status.success());
    let missing = String::from_utf8(missing.stdout).unwrap();
    assert!(missing.contains("daemon connectivity: failed"));
    assert!(missing.contains("start the Eidetica daemon"));
    assert!(missing.contains("passwordless user: skipped"));
    assert!(missing.contains("shell hooks: not checked"));
    assert!(missing.contains("remote synchronization: not checked"));

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

    let started = Instant::now();
    let stalled = run_cli_at_socket(&socket, &["status"], b"").await;
    assert!(started.elapsed() < Duration::from_secs(2));
    accepted_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(!stalled.status.success());
    let stalled = String::from_utf8(stalled.stdout).unwrap();
    assert!(stalled.contains("daemon connection exceeded the 1 second diagnostic deadline"));
    release_tx.send(()).unwrap();
    server.join().unwrap();
}

#[tokio::test]
async fn query_deduplicates_full_commands_after_filtering_and_before_limiting() -> Result<()> {
    let daemon = Daemon::start("alice").await;
    let (_client, mut user) = connect_to(&daemon.socket_url, "alice").await?;
    let first = setup(&mut user, None, "first").await?;
    let database = open_database(&user).await?;
    let second = Host {
        id: Uuid::new_v4(),
        name: "second".to_owned(),
    };
    let transaction = database.new_transaction().await?;
    transaction
        .get_store::<Table<Host>>("hosts")
        .await?
        .set(&second.id.to_string(), second.clone())
        .await?;
    let first_history = transaction
        .get_store::<Table<HistoryEntry>>(&format!("history-{}", first.id))
        .await?;
    let second_history = transaction
        .get_store::<Table<HistoryEntry>>(&format!("history-{}", second.id))
        .await?;
    for second_offset in 0..=DEFAULT_LIMIT {
        first_history
            .insert(HistoryEntry {
                command: "repeat needle".to_owned(),
                started_at: time(10) + chrono::Duration::seconds(second_offset as i64),
                cwd: format!("/repeat/{second_offset}"),
                duration_ms: Some(1),
                exit_status: Some(0),
                host_id: first.id,
                host_name: String::new(),
                entry_id: String::new(),
                session: "shell".to_owned(),
            })
            .await?;
    }
    for (command, cwd) in [
        (format!("{}a", "x".repeat(COMMAND_DISPLAY_LIMIT)), "/a"),
        (format!("{}b", "x".repeat(COMMAND_DISPLAY_LIMIT)), "/b"),
    ] {
        second_history
            .insert(HistoryEntry {
                command,
                started_at: time(50),
                cwd: cwd.to_owned(),
                duration_ms: Some(1),
                exit_status: Some(0),
                host_id: second.id,
                host_name: String::new(),
                entry_id: String::new(),
                session: "shell".to_owned(),
            })
            .await?;
    }
    second_history
        .insert(HistoryEntry {
            command: "repeat needle".to_owned(),
            started_at: time(5),
            cwd: "/second-host".to_owned(),
            duration_ms: Some(1),
            exit_status: Some(0),
            host_id: second.id,
            host_name: String::new(),
            entry_id: String::new(),
            session: "shell".to_owned(),
        })
        .await?;
    first_history
        .insert(HistoryEntry {
            command: "same time".to_owned(),
            started_at: time(55),
            cwd: "/a".to_owned(),
            duration_ms: Some(1),
            exit_status: Some(0),
            host_id: first.id,
            host_name: String::new(),
            entry_id: String::new(),
            session: "shell".to_owned(),
        })
        .await?;
    first_history
        .insert(HistoryEntry {
            command: "same time".to_owned(),
            started_at: time(55),
            cwd: "/b".to_owned(),
            duration_ms: Some(1),
            exit_status: Some(0),
            host_id: first.id,
            host_name: String::new(),
            entry_id: String::new(),
            session: "shell".to_owned(),
        })
        .await?;
    transaction.commit().await?;

    let unique = query(&database, None, Some("needle"), 2, false).await?;
    assert_eq!(unique.len(), 1, "deduplication happens before the limit");
    assert_eq!(unique[0].cwd, format!("/repeat/{DEFAULT_LIMIT}"));

    let all = query(&database, None, Some("needle"), 2, true).await?;
    assert_eq!(all.len(), 2, "--duplicates preserves executions");
    assert_eq!(all[0].cwd, format!("/repeat/{DEFAULT_LIMIT}"));
    assert_eq!(all[1].cwd, format!("/repeat/{}", DEFAULT_LIMIT - 1));

    let cli_unique = run_cli(
        &daemon,
        &[
            "--user",
            "alice",
            "search",
            "needle",
            "--all-hosts",
            "--limit",
            "2",
        ],
    )
    .await;
    assert!(
        cli_unique.status.success(),
        "{}",
        String::from_utf8_lossy(&cli_unique.stderr)
    );
    assert_eq!(
        String::from_utf8(cli_unique.stdout)
            .unwrap()
            .lines()
            .count(),
        2
    );
    let cli_duplicates = run_cli(
        &daemon,
        &[
            "--user",
            "alice",
            "search",
            "needle",
            "--all-hosts",
            "--limit",
            "2",
            "--duplicates",
        ],
    )
    .await;
    assert!(
        cli_duplicates.status.success(),
        "{}",
        String::from_utf8_lossy(&cli_duplicates.stderr)
    );
    assert_eq!(
        String::from_utf8(cli_duplicates.stdout)
            .unwrap()
            .lines()
            .count(),
        3
    );

    let truncated_alike = query(
        &database,
        None,
        Some(&"x".repeat(COMMAND_DISPLAY_LIMIT)),
        2,
        false,
    )
    .await?;
    assert_eq!(
        truncated_alike.len(),
        2,
        "full commands, not display text, deduplicate"
    );
    assert_ne!(truncated_alike[0].command, truncated_alike[1].command);

    let tied = query(&database, Some(first.id), Some("same time"), 1, false).await?;
    assert_eq!(tied[0].cwd, "/a", "equal timestamps have a stable winner");
    assert!(
        !tied[0].entry_id.is_empty(),
        "entry identity breaks full ties"
    );

    let scoped = query(&database, Some(first.id), Some("repeat"), 2, false).await?;
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].host_id, first.id);
    let second_scoped = query(&database, Some(second.id), Some("repeat"), 2, false).await?;
    assert_eq!(second_scoped.len(), 1);
    assert_eq!(second_scoped[0].cwd, "/second-host");

    let summary = summarize(&database, None).await?;
    assert_eq!(
        summary.total,
        DEFAULT_LIMIT + 6,
        "summary counts executions"
    );

    daemon.stop().await;
    Ok(())
}

#[tokio::test]
async fn summary_keeps_duplicate_names_distinct_and_filters_by_uuid() -> Result<()> {
    let daemon = Daemon::start("alice").await;
    let (_client, mut user) = connect_to(&daemon.socket_url, "alice").await?;
    let first = setup(&mut user, None, "same\tname").await?;
    let database = open_database(&user).await?;
    let second = Host {
        id: Uuid::new_v4(),
        name: "same\tname".to_owned(),
    };
    let transaction = database.new_transaction().await?;
    transaction
        .get_store::<Table<Host>>("hosts")
        .await?
        .set(&second.id.to_string(), second.clone())
        .await?;
    transaction.commit().await?;

    let one = start(&database, first.id, "shell", "/one", "git status", time(1))
        .await?
        .unwrap();
    let two = start(&database, second.id, "shell", "/two", "cargo test", time(2))
        .await?
        .unwrap();
    let incomplete = start(&database, first.id, "shell", "/one", "git diff", time(3))
        .await?
        .unwrap();
    let tied = start(
        &database,
        second.id,
        "shell",
        "/two",
        "cargo build",
        time(4),
    )
    .await?
    .unwrap();
    finish(&database, first.id, &one, 3, 1).await?;
    finish(&database, second.id, &two, 8, 0).await?;
    finish(&database, second.id, &tied, 8, 0).await?;

    let summary = summarize(&database, None).await?;
    assert_eq!(summary.machine_counts.len(), 2);
    assert!(
        summary
            .machine_counts
            .iter()
            .any(|(id, _, count)| *id == first.id && *count == 2)
    );
    assert!(
        summary
            .machine_counts
            .iter()
            .any(|(id, _, count)| *id == second.id && *count == 2)
    );
    assert_eq!(summary.failures, 1);
    assert_eq!(summary.successes, 2);
    assert_eq!(summary.incomplete, 1);
    assert_eq!(summary.most_common_command, Some(("cargo".to_owned(), 2)));
    assert_eq!(
        summary
            .longest_runtime
            .as_ref()
            .map(|entry| entry.command.as_str()),
        Some("cargo build")
    );
    assert!(!incomplete.is_empty());

    let filtered = summarize(&database, Some(first.id)).await?;
    assert_eq!(filtered.total, 2);
    assert_eq!(filtered.machine_counts, vec![(first.id, first.name, 2)]);

    daemon.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_summary_ignores_global_host_default_but_accepts_explicit_filter() {
    let daemon = Daemon::start("alice").await;
    let (_client, mut user) = connect_to(&daemon.socket_url, "alice").await.unwrap();
    let first = setup(&mut user, None, "first").await.unwrap();
    let database = open_database(&user).await.unwrap();
    let second = Host {
        id: Uuid::new_v4(),
        name: "second".to_owned(),
    };
    let transaction = database.new_transaction().await.unwrap();
    transaction
        .get_store::<Table<Host>>("hosts")
        .await
        .unwrap()
        .set(&second.id.to_string(), second.clone())
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    start(&database, first.id, "shell", "/one", "one", time(1))
        .await
        .unwrap();
    start(&database, second.id, "shell", "/two", "two", time(2))
        .await
        .unwrap();
    let first_id = first.id.to_string();
    let second_id = second.id.to_string();

    let all = run_cli(
        &daemon,
        &["--user", "alice", "--host-id", &first_id, "summary"],
    )
    .await;
    assert!(
        all.status.success(),
        "{}",
        String::from_utf8_lossy(&all.stderr)
    );
    assert!(
        String::from_utf8(all.stdout)
            .unwrap()
            .contains("total records: 2\n")
    );

    let filtered = run_cli(
        &daemon,
        &["--user", "alice", "summary", "--host-id", &second_id],
    )
    .await;
    assert!(
        filtered.status.success(),
        "{}",
        String::from_utf8_lossy(&filtered.stderr)
    );
    let filtered = String::from_utf8(filtered.stdout).unwrap();
    assert!(filtered.contains("total records: 1\n"));
    assert!(filtered.contains(&second_id));
    assert!(!filtered.contains(&first_id));

    daemon.stop().await;
}

#[test]
fn displayed_entries_bound_escape_and_preserve_unicode() {
    let entry = HistoryEntry {
        command: format!("界{}\u{1b}[31m", "x".repeat(COMMAND_DISPLAY_LIMIT)),
        started_at: time(0),
        cwd: format!("📁{}\n", "x".repeat(CWD_DISPLAY_LIMIT)),
        duration_ms: None,
        exit_status: None,
        host_id: Uuid::nil(),
        host_name: format!("🚀{}\t", "x".repeat(HOST_DISPLAY_LIMIT)),
        entry_id: String::new(),
        session: String::new(),
    };
    let mut output = Vec::new();

    print_entries(&mut output, &[entry]).unwrap();

    let row = String::from_utf8(output)
        .unwrap()
        .lines()
        .nth(1)
        .unwrap()
        .to_owned();
    let fields = row.split('\t').collect::<Vec<_>>();
    assert_eq!(fields.len(), 6);
    assert_eq!(fields[2], "incomplete");
    assert_eq!(fields[3], "-");
    assert!(fields[1].ends_with('…') && fields[1].chars().count() <= HOST_DISPLAY_LIMIT);
    assert!(fields[4].ends_with('…') && fields[4].chars().count() <= CWD_DISPLAY_LIMIT);
    assert!(fields[5].ends_with('…') && fields[5].chars().count() <= COMMAND_DISPLAY_LIMIT);
    assert!(fields[1].contains('🚀') && fields[4].contains('📁') && fields[5].contains('界'));
    assert!(!row.contains('\u{1b}') && !row.contains('\n'));
}

#[test]
fn empty_summary_is_bounded_and_printable() {
    let mut output = Vec::new();
    print_summary(
        &mut output,
        &shistory::HistorySummary {
            total: 0,
            first_started_at: None,
            last_started_at: None,
            successes: 0,
            failures: 0,
            incomplete: 0,
            most_common_command: None,
            longest_runtime: None,
            machine_counts: Vec::new(),
        },
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(output).unwrap(),
        "total records: 0\nstatus: 0 success, 0 failure, 0 incomplete\nrecords by machine:\n"
    );
}

#[test]
fn displayed_summary_bounds_and_escapes_untrusted_labels() {
    let host_id = Uuid::nil();
    let command = format!("界\n{}", "x".repeat(COMMAND_DISPLAY_LIMIT));
    let host = format!("🚀\t{}", "x".repeat(HOST_DISPLAY_LIMIT));
    let entry = HistoryEntry {
        command: command.clone(),
        started_at: time(0),
        cwd: String::new(),
        duration_ms: Some(7),
        exit_status: Some(0),
        host_id,
        host_name: host.clone(),
        entry_id: String::new(),
        session: String::new(),
    };
    let mut output = Vec::new();

    print_summary(
        &mut output,
        &shistory::HistorySummary {
            total: 1,
            first_started_at: Some(time(0)),
            last_started_at: Some(time(0)),
            successes: 1,
            failures: 0,
            incomplete: 0,
            most_common_command: Some((command, 1)),
            longest_runtime: Some(entry),
            machine_counts: vec![(host_id, host, 1)],
        },
    )
    .unwrap();

    let output = String::from_utf8(output).unwrap();
    let common = output
        .lines()
        .find_map(|line| line.strip_prefix("most common command: "))
        .unwrap()
        .strip_suffix(" (1)")
        .unwrap();
    assert_eq!(common.chars().count(), COMMAND_DISPLAY_LIMIT);
    assert!(common.starts_with("界\\n") && common.ends_with('…'));
    let longest = output
        .lines()
        .find_map(|line| line.strip_prefix("longest runtime: 7 ms\t"))
        .unwrap()
        .split('\t')
        .collect::<Vec<_>>();
    assert_eq!(longest[0].chars().count(), HOST_DISPLAY_LIMIT);
    assert_eq!(longest[1].chars().count(), COMMAND_DISPLAY_LIMIT);
    assert!(longest[0].starts_with("🚀\\t"));
    assert!(longest[1].starts_with("界\\n"));
    assert!(longest.iter().all(|field| field.ends_with('…')));

    let machine = output
        .lines()
        .find(|line| line.contains(&host_id.to_string()))
        .unwrap()
        .split('\t')
        .collect::<Vec<_>>();
    assert_eq!(machine.len(), 3);
    assert_eq!(machine[0].chars().count(), HOST_DISPLAY_LIMIT);
    assert!(machine[0].starts_with("🚀\\t") && machine[0].ends_with('…'));
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
    let mut child = Command::new(shistory_bin())
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
        entry_id: String::new(),
        session: "fixture-session".to_owned(),
    };
    let mut output = Vec::new();

    print_entries(&mut output, &[entry]).unwrap();

    assert_eq!(
        String::from_utf8(output).unwrap(),
        "start time\thost\texit status\tduration (ms)\tworking directory\tcommand\n2026-09-14T12:00:00+00:00\tfixture\\thost\\nname\t0\t1\t/tmp/with\\ttab\\nand-newline\tprint 'one\\ttwo'\\nprint \\u{1b}[31mthree\\r\n"
    );
}

async fn remembered(session: &AccountSession) -> Result<Option<ID>> {
    session
        .reference(shistory::REFERENCE_NAMESPACE, shistory::REFERENCE_NAME)
        .await
}

/// Raw Account writes simulate older or damaged installations; Shistory itself
/// reaches its reference only through session operations.
async fn write_raw_reference(session: &AccountSession, value: Option<eidetica::crdt::doc::Value>) {
    let tx = session
        .account()
        .database()
        .new_transaction()
        .await
        .unwrap();
    // Load past the store-type check, as raw corruption would.
    let store = <eidetica::store::DocStore as eidetica::Store>::load(
        &tx,
        format!("refs:{}", shistory::REFERENCE_NAMESPACE),
    )
    .await
    .unwrap();
    match value {
        Some(value) => {
            store.set(shistory::REFERENCE_NAME, value).await.unwrap();
        }
        None => {
            store.delete(shistory::REFERENCE_NAME).await.unwrap();
        }
    }
    tx.commit().await.unwrap();
}

/// A history database as created by Shistory releases before Account references.
async fn legacy_history(session: &mut AccountSession, name: &str) -> Result<(Database, Host)> {
    let key = session.get_default_key()?;
    let mut settings = Doc::new();
    settings.set("name", "shistory");
    let database = session.create_database(settings, &key).await?;
    let transaction = database.new_transaction().await?;
    transaction
        .get_settings()?
        .set_global_auth_key(AuthKey::active(None, Permission::Write(10)))
        .await?;
    transaction.commit().await?;
    session
        .track_database(database.root_id().clone(), &key, SyncSettings::on_commit())
        .await?;
    let host = Host {
        id: Uuid::new_v4(),
        name: name.to_owned(),
    };
    let transaction = database.new_transaction().await?;
    transaction
        .get_store::<Table<Host>>("hosts")
        .await?
        .set(&host.id.to_string(), host.clone())
        .await?;
    transaction.commit().await?;
    Ok((database, host))
}

async fn unrelated_databases(session: &mut AccountSession, count: usize) -> Result<Vec<ID>> {
    let key = session.get_default_key()?;
    let mut ids = Vec::new();
    for index in 0..count {
        let mut settings = Doc::new();
        settings.set("name", format!("unrelated-{index}"));
        ids.push(
            session
                .create_database(settings, &key)
                .await?
                .root_id()
                .clone(),
        );
    }
    Ok(ids)
}

fn capture_args<'a>(user: &'a str, host_id: &'a str, operation: &'a [&'a str]) -> Vec<&'a str> {
    let mut args = vec!["--user", user, "--host-id", host_id];
    args.extend_from_slice(operation);
    args
}

const START: &[&str] = &[
    "start",
    "--session",
    "reference",
    "--cwd",
    "/",
    "--started-at",
    "2026-09-14T12:00:00Z",
];

fn finish_args(id: &str) -> Vec<&str> {
    vec!["finish", id, "--duration-ms", "2", "--exit-status", "0"]
}

#[tokio::test]
async fn setup_remembers_root_across_reconnect_and_database_rename() -> Result<()> {
    let first = Daemon::start("alice").await;
    let second = Daemon::start("bob").await;
    let (client, mut alice) = connect_to(&first.socket_url, "alice").await?;
    let local = setup(&mut alice, None, "local").await?;
    let database = open_database(&alice).await?;
    assert_eq!(remembered(&alice).await?.as_ref(), Some(database.root_id()));

    let ticket = shistory::ticket(&alice).await?;
    let (_bob_client, mut bob) = connect_to(&second.socket_url, "bob").await?;
    assert_eq!(remembered(&bob).await?, None);
    setup(&mut bob, Some(&ticket), "server").await?;
    assert_eq!(remembered(&bob).await?.as_ref(), Some(database.root_id()));

    let tx = database.new_transaction().await?;
    tx.get_settings()?.set_name("renamed history").await?;
    tx.commit().await?;
    drop(alice);
    drop(client);
    let (_client, mut alice) = connect_to(&first.socket_url, "alice").await?;
    let database = open_database(&alice).await?;
    assert_eq!(remembered(&alice).await?.as_ref(), Some(database.root_id()));
    assert_eq!(database.get_name().await?, "renamed history");
    let replacement = setup(&mut alice, None, "replacement").await.unwrap_err();
    assert!(
        replacement
            .to_string()
            .contains(&database.root_id().to_string()),
        "{replacement}"
    );
    let tracked = alice.databases().await?.len();
    let host_id = local.id.to_string();
    let output = run_cli_with_stdin(&first, &capture_args("alice", &host_id, START), b"true").await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let id = String::from_utf8(output.stdout).unwrap();
    let output = run_cli(
        &first,
        &capture_args("alice", &host_id, &finish_args(id.trim())),
    )
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(alice.databases().await?.len(), tracked);
    assert_eq!(
        query(&database, Some(local.id), None, 10, true).await?[0].exit_status,
        Some(0)
    );

    first.stop().await;
    second.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_capture_migrates_but_read_only_commands_do_not() -> Result<()> {
    let daemon = Daemon::start("alice").await;
    let (_client, mut alice) = connect_to(&daemon.socket_url, "alice").await?;
    let (database, local) = legacy_history(&mut alice, "local").await?;
    let account_before = alice.account().database().snapshot().await?;
    let host_id = local.id.to_string();

    assert_eq!(open_database(&alice).await?.root_id(), database.root_id());
    for read_only in [
        vec!["--user", "alice", "--host-id", &host_id, "list"],
        vec!["--user", "alice", "--host-id", &host_id, "search", "x"],
        vec!["--user", "alice", "summary"],
        vec!["--user", "alice", "--host-id", &host_id, "status"],
    ] {
        let output = run_cli(&daemon, &read_only).await;
        assert!(
            output.status.success(),
            "{read_only:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(remembered(&alice).await?, None);
    assert_eq!(alice.account().database().snapshot().await?, account_before);

    // An unknown host must not migrate the legacy selection.
    let unknown = Uuid::new_v4().to_string();
    let output =
        run_cli_with_stdin(&daemon, &capture_args("alice", &unknown, START), b"true").await;
    assert!(!output.status.success());
    assert_eq!(remembered(&alice).await?, None);

    let output =
        run_cli_with_stdin(&daemon, &capture_args("alice", &host_id, START), b"true").await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(remembered(&alice).await?.as_ref(), Some(database.root_id()));

    // Upgrading between start and finish also migrates.
    write_raw_reference(&alice, None).await;
    assert_eq!(remembered(&alice).await?, None);
    let id = String::from_utf8(output.stdout).unwrap();
    let output = run_cli(
        &daemon,
        &capture_args("alice", &host_id, &finish_args(id.trim())),
    )
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(remembered(&alice).await?.as_ref(), Some(database.root_id()));
    let entries = query(&database, Some(local.id), None, 10, true).await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].exit_status, Some(0));
    assert_eq!(host(&database, local.id).await?, local);

    daemon.stop().await;
    Ok(())
}

#[tokio::test]
async fn legacy_ambiguous_or_missing_selection_does_not_migrate_or_create() -> Result<()> {
    let daemon = Daemon::start("alice").await;
    let (_client, mut alice) = connect_to(&daemon.socket_url, "alice").await?;
    let missing = shistory::open_capture_database(&alice, Uuid::new_v4())
        .await
        .unwrap_err();
    assert!(missing.is_not_found(), "{missing}");
    assert!(alice.databases().await?.is_empty());

    let (history, local) = legacy_history(&mut alice, "local").await?;
    let (_duplicate, _) = legacy_history(&mut alice, "other").await?;
    let before = history.snapshot().await?;
    let error = shistory::open_capture_database(&alice, local.id)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("found 2 tracked databases"),
        "{error}"
    );
    assert_eq!(remembered(&alice).await?, None);
    assert_eq!(history.snapshot().await?, before);
    let error = setup(&mut alice, None, "new").await.unwrap_err();
    assert!(
        error.to_string().contains(&history.root_id().to_string()),
        "{error}"
    );
    assert_eq!(alice.databases().await?.len(), 2);

    daemon.stop().await;
    Ok(())
}

#[tokio::test]
async fn bad_reference_fails_closed_without_capture_name_scan_or_replacement() -> Result<()> {
    let daemon = Daemon::start("alice").await;
    let (_client, mut alice) = connect_to(&daemon.socket_url, "alice").await?;
    let local = setup(&mut alice, None, "local").await?;
    let history = open_database(&alice).await?;
    let history_before = history.snapshot().await?;
    let key = alice.get_default_key()?;
    let mut settings = Doc::new();
    settings.set("name", "other");
    let other = alice.create_database(settings, &key).await?;
    let host_id = local.id.to_string();

    let capture_fails = |label: &'static str| {
        let daemon = &daemon;
        let host_id = host_id.clone();
        async move {
            let output = run_cli_with_stdin(
                daemon,
                &capture_args("alice", &host_id, START),
                b"synthetic private command",
            )
            .await;
            assert!(!output.status.success(), "{label}: start must fail");
            assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic private command"));
            let output = run_cli(
                daemon,
                &capture_args("alice", &host_id, &finish_args("missing")),
            )
            .await;
            assert!(!output.status.success(), "{label}: finish must fail");
        }
    };

    // Wrong target: tracked and openable, but without this host.
    alice
        .set_reference(
            shistory::REFERENCE_NAMESPACE,
            shistory::REFERENCE_NAME,
            other.root_id(),
        )
        .await?;
    let other_before = other.snapshot().await?;
    capture_fails("wrong target").await;
    assert_eq!(other.snapshot().await?, other_before);

    // Untracked target.
    alice.untrack_database(other.root_id()).await?;
    assert!(open_database(&alice).await.is_err());
    capture_fails("untracked").await;

    // Stale root that this daemon does not hold.
    let stale = ID::from_bytes("nonexistent shistory root");
    alice
        .set_reference(
            shistory::REFERENCE_NAMESPACE,
            shistory::REFERENCE_NAME,
            &stale,
        )
        .await?;
    assert!(open_database(&alice).await.is_err());
    capture_fails("stale").await;

    // Malformed reference data is an error, never absence.
    for value in [
        eidetica::crdt::doc::Value::Text("not a database id".to_owned()),
        eidetica::crdt::doc::Value::Int(42),
    ] {
        write_raw_reference(&alice, Some(value)).await;
        assert!(remembered(&alice).await.is_err());
        assert!(open_database(&alice).await.is_err());
        assert!(shistory::ticket(&alice).await.is_err());
        capture_fails("malformed").await;
        assert!(setup(&mut alice, None, "replacement").await.is_err());
    }

    // Nothing fell back to the real history tree by name, or created another.
    assert_eq!(history.snapshot().await?, history_before);
    assert_eq!(alice.databases().await?.len(), 1);
    assert!(
        query(&history, Some(local.id), None, 10, true)
            .await?
            .is_empty()
    );

    daemon.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_or_legacy_setup_is_repaired_with_existing_root() -> Result<()> {
    let daemon = Daemon::start("alice").await;
    let (_client, mut alice) = connect_to(&daemon.socket_url, "alice").await?;
    // Created and tracked, but neither remembered nor given a host.
    let (database, _) = legacy_history(&mut alice, "unused").await?;
    let root = database.root_id().to_string();
    let error = setup(&mut alice, None, "local").await.unwrap_err();
    assert!(error.to_string().contains(&root), "{error}");

    let output = run_cli(
        &daemon,
        &[
            "--user",
            "alice",
            "setup",
            "--database",
            &root,
            "--name",
            "local",
        ],
    )
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.contains(&format!("history database: {root}")),
        "{output}"
    );
    let host_id = output
        .lines()
        .find_map(|line| line.strip_prefix("host id: "))
        .unwrap();
    assert_eq!(remembered(&alice).await?.as_ref(), Some(database.root_id()));
    assert_eq!(
        host(&database, Uuid::parse_str(host_id).unwrap())
            .await?
            .name,
        "local"
    );
    assert_eq!(alice.databases().await?.len(), 1);

    // Remembered but a host write was interrupted: retry the same root.
    let again = shistory::setup_existing(&alice, database.root_id(), "second").await?;
    assert_eq!(host(&database, again.id).await?.name, "second");

    // A different root is never selected over the remembered one.
    let key = alice.get_default_key()?;
    let other = alice.create_database(Doc::new(), &key).await?;
    alice
        .track_database(other.root_id().clone(), &key, SyncSettings::on_commit())
        .await?;
    assert!(
        shistory::setup_existing(&alice, other.root_id(), "x")
            .await
            .is_err()
    );
    assert_eq!(remembered(&alice).await?.as_ref(), Some(database.root_id()));

    let conflict = run_cli(
        &daemon,
        &[
            "--user",
            "alice",
            "setup",
            "ticket-text",
            "--database",
            &root,
            "--name",
            "x",
        ],
    )
    .await;
    assert!(!conflict.status.success());

    daemon.stop().await;
    Ok(())
}

#[tokio::test]
async fn ticket_join_retry_repairs_missing_reference_without_new_root() -> Result<()> {
    let first = Daemon::start("alice").await;
    let second = Daemon::start("bob").await;
    let (_client, mut alice) = connect_to(&first.socket_url, "alice").await?;
    setup(&mut alice, None, "local").await?;
    let ticket = shistory::ticket(&alice).await?;
    let (_client, mut bob) = connect_to(&second.socket_url, "bob").await?;
    setup(&mut bob, Some(&ticket), "server").await?;
    // Simulate a join whose reference write was lost.
    write_raw_reference(&bob, None).await;
    let retry = setup(&mut bob, Some(&ticket), "server").await?;
    assert_eq!(remembered(&bob).await?.as_ref(), Some(ticket.database_id()));
    let database = open_database(&bob).await?;
    assert_eq!(host(&database, retry.id).await?.name, "server");
    assert_eq!(bob.databases().await?.len(), 1);

    // A ticket for another root never replaces the remembered one.
    let key = alice.get_default_key()?;
    let other = alice.create_database(Doc::new(), &key).await?;
    let tx = other.new_transaction().await?;
    tx.get_settings()?
        .set_global_auth_key(AuthKey::active(None, Permission::Write(10)))
        .await?;
    tx.commit().await?;
    other.share().await?;
    let other_ticket = other.ticket().await?;
    assert!(setup(&mut bob, Some(&other_ticket), "x").await.is_err());
    assert_eq!(remembered(&bob).await?.as_ref(), Some(ticket.database_id()));
    assert!(bob.database(other.root_id()).await.is_err());

    first.stop().await;
    second.stop().await;
    Ok(())
}

/// Storage requests one CLI process sent before its first history write.
struct CaptureTrace {
    output: std::process::Output,
    roots: Vec<ID>,
    history_snapshots: usize,
    pre_submit: Option<Duration>,
    total: Duration,
}

impl CaptureTrace {
    fn touched(&self, ids: &[ID]) -> usize {
        self.roots.iter().filter(|root| ids.contains(root)).count()
    }
}

/// Run `bin` through a recording proxy. Records request roots, never payloads.
async fn traced_cli(
    daemon: &Daemon,
    bin: &str,
    args: &[&str],
    stdin: &[u8],
    history: &ID,
) -> CaptureTrace {
    use eidetica::service::protocol::{
        DatabaseOp, Handshake, ServiceRequest, read_frame, write_frame,
    };
    use tokio::{
        io::AsyncWriteExt,
        net::{UnixListener, UnixStream},
    };

    let path = daemon
        ._dir
        .path()
        .join(format!("trace-{}.sock", Uuid::new_v4().simple()));
    let listener = UnixListener::bind(&path).unwrap();
    let target = daemon.socket.clone();
    let history = history.clone();
    let proxy = tokio::spawn(async move {
        let (client, _) = listener.accept().await.unwrap();
        let began = Instant::now();
        let server = UnixStream::connect(target).await.unwrap();
        let (mut client_read, mut client_write) = client.into_split();
        let (mut server_read, mut server_write) = server.into_split();
        let mut roots = Vec::new();
        let mut history_snapshots = 0;
        let mut pre_submit = None;
        let requests = async {
            let handshake: Handshake = read_frame(&mut client_read).await.unwrap().unwrap();
            write_frame(&mut server_write, &handshake).await.unwrap();
            while let Ok(Some(request)) = read_frame::<_, ServiceRequest>(&mut client_read).await {
                if let ServiceRequest::AuthenticatedDb(db) = &request
                    && pre_submit.is_none()
                {
                    if db.root_id == history && matches!(db.op, DatabaseOp::GetVerifiedTips) {
                        history_snapshots += 1;
                    }
                    if db.root_id == history
                        && matches!(db.op, DatabaseOp::SubmitSignedEntry { .. })
                    {
                        pre_submit = Some(began.elapsed());
                    } else {
                        roots.push(db.root_id.clone());
                    }
                }
                write_frame(&mut server_write, &request).await.unwrap();
            }
            let _ = server_write.shutdown().await;
        };
        let responses = tokio::io::copy(&mut server_read, &mut client_write);
        tokio::select! { () = requests => {}, _ = responses => {} }
        (roots, history_snapshots, pre_submit)
    });
    let bin = bin.to_owned();
    let args = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    let stdin = stdin.to_vec();
    let socket = path.clone();
    let began = Instant::now();
    let output = tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let mut child = Command::new(bin)
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
    .unwrap();
    let total = began.elapsed();
    let (roots, history_snapshots, pre_submit) = proxy.await.unwrap();
    std::fs::remove_file(path).unwrap();
    CaptureTrace {
        output,
        roots,
        history_snapshots,
        pre_submit,
        total,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_queries_pin_host_and_history_reads_once() -> Result<()> {
    let daemon = Daemon::start("alice").await;
    let (_client, mut alice) = connect_to(&daemon.socket_url, "alice").await?;
    let first = setup(&mut alice, None, "first").await?;
    let database = open_database(&alice).await?;
    let second = shistory::setup_existing(&alice, database.root_id(), "second").await?;
    for (host, second) in [(first.id, 1), (second.id, 2)] {
        start(&database, host, "shell", "/", "probe", time(second)).await?;
    }
    let before = database.snapshot().await?;
    let host_id = first.id.to_string();
    for args in [
        vec!["--user", "alice", "list", "--all-hosts", "--duplicates"],
        vec!["--user", "alice", "search", "probe", "--all-hosts"],
        vec!["--user", "alice", "summary"],
        vec!["--user", "alice", "list", "--host-id", &host_id],
    ] {
        let trace = traced_cli(&daemon, &shistory_bin(), &args, b"", database.root_id()).await;
        assert!(
            trace.output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&trace.output.stderr)
        );
        assert_eq!(
            trace.history_snapshots, 1,
            "{args:?}: pin one history boundary"
        );
        assert!(
            trace.pre_submit.is_none(),
            "queries must not submit history writes"
        );
        assert!(String::from_utf8_lossy(&trace.output.stdout).contains("probe"));
    }
    assert_eq!(database.snapshot().await?, before);
    daemon.stop().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn remembered_capture_opens_no_unrelated_databases_as_they_grow() -> Result<()> {
    let daemon = Daemon::start("alice").await;
    let admin = daemon.instance.login_account("alice", None).await?;
    admin
        .admin()
        .await?
        .create_account(NewAccount::passwordless("legacy"))
        .await?;
    let (_client, mut alice) = connect_to(&daemon.socket_url, "alice").await?;
    let (_client, mut legacy) = connect_to(&daemon.socket_url, "legacy").await?;
    let local = setup(&mut alice, None, "local").await?;
    let history = open_database(&alice).await?;
    let (legacy_db, legacy_host) = legacy_history(&mut legacy, "legacy").await?;
    let bin = shistory_bin();
    let bin = bin.as_str();
    let alice_host = local.id.to_string();
    let legacy_host = legacy_host.id.to_string();
    let bin_start = |user: &'static str, host: String, history: ID| {
        let daemon = &daemon;
        async move {
            let trace = traced_cli(
                daemon,
                bin,
                &capture_args(user, &host, START),
                b"true",
                &history,
            )
            .await;
            assert!(
                trace.output.status.success(),
                "{}",
                String::from_utf8_lossy(&trace.output.stderr)
            );
            assert!(
                trace.pre_submit.is_some(),
                "capture must submit to the selected history"
            );
            trace
        }
    };

    let small = bin_start("alice", alice_host.clone(), history.root_id().clone()).await;
    let alice_unrelated = unrelated_databases(&mut alice, 20).await?;
    let large = bin_start("alice", alice_host.clone(), history.root_id().clone()).await;
    // Only the Account, the selected history and instance-level requests are
    // visited. Request counts follow Account/history tip state, so compare the
    // set of databases, not an exact count.
    let allowed = [
        ID::default(),
        alice.account().id().clone(),
        history.root_id().clone(),
    ];
    for trace in [&small, &large] {
        assert_eq!(
            trace.touched(&alice_unrelated),
            0,
            "reference lookup opened unrelated databases"
        );
        assert_eq!(
            trace.touched(&allowed),
            trace.roots.len(),
            "{:?}",
            trace.roots
        );
    }

    // Negative control: before migration, the legacy name lookup opens every tracked database.
    let legacy_unrelated = unrelated_databases(&mut legacy, 20).await?;
    let scan = bin_start("legacy", legacy_host.clone(), legacy_db.root_id().clone()).await;
    assert_eq!(
        scan.touched(&legacy_unrelated),
        20 * scan.touched(&legacy_unrelated[..1])
    );
    assert!(
        scan.touched(&legacy_unrelated[..1]) > 0,
        "legacy lookup must visibly scan"
    );
    // The same capture is migrated, so the next one reads only its reference.
    assert_eq!(
        remembered(&legacy).await?.as_ref(),
        Some(legacy_db.root_id())
    );
    let migrated = bin_start("legacy", legacy_host, legacy_db.root_id().clone()).await;
    let allowed = [
        ID::default(),
        legacy.account().id().clone(),
        legacy_db.root_id().clone(),
    ];
    assert_eq!(migrated.touched(&legacy_unrelated), 0);
    assert_eq!(migrated.touched(&allowed), migrated.roots.len());
    println!(
        "TRACE pre_submit_requests remembered_small={} remembered_large={} legacy_scan={} migrated={}",
        small.roots.len(),
        large.roots.len(),
        scan.roots.len(),
        migrated.roots.len()
    );
    for trace in [&small, &large, &migrated] {
        assert_eq!(
            trace.history_snapshots, 2,
            "capture needs one host-selection boundary and one history-write boundary"
        );
    }

    daemon.stop().await;
    Ok(())
}

/// Compare capture latency against a baseline binary on a populated SQLite daemon.
///
/// Run explicitly with SHISTORY_BASELINE_BIN naming a binary built with the same
/// Cargo profile from the baseline commit. Reports storage requests and time
/// before the history write is submitted; never logs command text.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a separately built baseline binary"]
async fn populated_sql_capture_profile() -> Result<()> {
    let baseline = std::env::var("SHISTORY_BASELINE_BIN").expect("set SHISTORY_BASELINE_BIN");
    let dir = tempfile::tempdir()?;
    let backend =
        eidetica::backend::database::Sqlite::open(dir.path().join("profile.sqlite")).await?;
    let daemon = Daemon::start_backend("alice", Box::new(backend), dir).await;
    let (_client, mut alice) = connect_to(&daemon.socket_url, "alice").await?;
    // Place history among unrelated tracked databases, as on a populated Account.
    unrelated_databases(&mut alice, 10).await?;
    let local = setup(&mut alice, None, "profile-host").await?;
    let database = open_database(&alice).await?;
    unrelated_databases(&mut alice, 10).await?;
    for _ in 0..100 {
        let id = start(&database, local.id, "seed", "/", "true", time(0))
            .await?
            .unwrap();
        finish(&database, local.id, &id, 1, 0).await?;
    }
    let host_id = local.id.to_string();
    let root = database.root_id().clone();
    let candidate = shistory_bin();
    for pair in 0..21 {
        // Alternate order to reduce drift; pair 0 warms both binaries.
        let legs = if pair % 2 == 0 {
            ["baseline", "candidate"]
        } else {
            ["candidate", "baseline"]
        };
        for leg in legs {
            let bin = if leg == "baseline" {
                baseline.as_str()
            } else {
                candidate.as_str()
            };
            let start = traced_cli(
                &daemon,
                bin,
                &capture_args("alice", &host_id, START),
                b"true",
                &root,
            )
            .await;
            let mut traces = vec![("start", start)];
            if traces[0].1.output.status.success() {
                let id = String::from_utf8(traces[0].1.output.stdout.clone()).unwrap();
                let finish = traced_cli(
                    &daemon,
                    bin,
                    &capture_args("alice", &host_id, &finish_args(id.trim())),
                    b"",
                    &root,
                )
                .await;
                traces.push(("finish", finish));
            }
            for (operation, trace) in &traces {
                let ok = trace.output.status.success();
                // The baseline may exceed the capture deadline; after warm-up the
                // candidate must not.
                assert!(
                    ok || leg == "baseline" || pair == 0,
                    "{}",
                    String::from_utf8_lossy(&trace.output.stderr)
                );
                println!(
                    "PROFILE pair={pair} leg={leg} operation={operation} ok={ok} pre_submit_rpcs={} pre_submit_ms={} total_ms={:.3}",
                    trace.roots.len(),
                    trace.pre_submit.map_or_else(
                        || "-".to_owned(),
                        |t| format!("{:.3}", t.as_secs_f64() * 1000.)
                    ),
                    trace.total.as_secs_f64() * 1000.
                );
            }
        }
    }
    daemon.stop().await;
    Ok(())
}
