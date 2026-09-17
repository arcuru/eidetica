#![cfg(unix)]

use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use chrono::{TimeZone, Utc};
use eidetica::{
    Instance, NewUser, Result, backend::database::InMemory, service::ServiceServer, store::Table,
    sync::transports::http::HttpTransport,
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
    let entries = query(&database, Some(laptop.id), None, 10, true).await?;
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

    let entries = query(&first_database, None, None, 10, true).await?;
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
    let user_before = user.user_database().snapshot().await?;
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
    assert_eq!(user.user_database().snapshot().await?, user_before);
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
