use std::{future::Future, io::Read, process::ExitCode, str::FromStr, time::Duration};

use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use eidetica::{
    Instance, Result,
    service::{default_socket_path, default_socket_url},
    store::StoreError,
    sync::DatabaseTicket,
};
use shistory::{
    DEFAULT_LIMIT, HOST_DISPLAY_LIMIT, bounded_escaped, connect, finish, host, open_database,
    print_entries, print_summary, query, rename, setup, start, summarize, ticket,
};
use uuid::Uuid;

const CAPTURE_DEADLINE: Duration = Duration::from_secs(1);
const STATUS_VALUE_LIMIT: usize = 200;
const STATUS_ERROR_LIMIT: usize = 240;

#[derive(Parser)]
#[command(about = "Record zsh history in an Eidetica daemon")]
struct Cli {
    /// Passwordless Eidetica user already present in the daemon.
    #[arg(long, env = "SHISTORY_USER", default_value = "shistory")]
    user: String,

    /// Stable UUID printed by setup for this local host record.
    #[arg(long, env = "SHISTORY_HOST_ID")]
    host_id: Option<Uuid>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create the first history database or join one using an Eidetica ticket.
    Setup {
        /// Eidetica ticket from the first host; omit only when creating it.
        ticket: Option<DatabaseTicket>,
        /// Mutable display name for this host.
        #[arg(long, env = "SHISTORY_HOST_NAME")]
        name: String,
    },
    /// Change this host's display name without changing its UUID or Store.
    Rename { name: String },
    /// Print an Eidetica ticket for setting up another host.
    Ticket,
    /// Start a history record, printing its ID.
    Start {
        #[arg(long)]
        session: String,
        #[arg(long)]
        cwd: String,
        #[arg(long)]
        started_at: String,
    },
    /// Complete the history record with duration and exit status.
    Finish {
        id: String,
        #[arg(long)]
        duration_ms: i64,
        #[arg(long, allow_hyphen_values = true)]
        exit_status: i32,
    },
    /// List recent history.
    List {
        #[arg(long, conflicts_with = "all_hosts")]
        host_id: Option<Uuid>,
        #[arg(long)]
        all_hosts: bool,
        #[arg(long, default_value_t = DEFAULT_LIMIT)]
        limit: usize,
        /// Show every execution rather than one newest occurrence per command.
        #[arg(long)]
        duplicates: bool,
    },
    /// Search command text.
    Search {
        text: String,
        #[arg(long, conflicts_with = "all_hosts")]
        host_id: Option<Uuid>,
        #[arg(long)]
        all_hosts: bool,
        #[arg(long, default_value_t = DEFAULT_LIMIT)]
        limit: usize,
        /// Show every execution rather than one newest occurrence per command.
        #[arg(long)]
        duplicates: bool,
    },
    /// Summarize stored history across all hosts or one host UUID.
    Summary {
        #[arg(long)]
        host_id: Option<Uuid>,
    },
    /// Check local history-recording prerequisites without writing history or initiating sync.
    Status,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("shistory: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Setup { ticket, name } => {
            let (_instance, mut user) = connect(&cli.user).await?;
            let host = setup(&mut user, ticket.as_ref(), &name).await?;
            println!(
                "history database: {}",
                open_database(&user).await?.root_id()
            );
            println!("host id: {}", host.id);
        }
        Command::Rename { name } => {
            let host_id = required_host_id(cli.host_id)?;
            let (_instance, user) = connect(&cli.user).await?;
            rename(&open_database(&user).await?, host_id, &name).await?;
        }
        Command::Ticket => {
            let (_instance, mut user) = connect(&cli.user).await?;
            println!("{}", ticket(&mut user).await?);
        }
        Command::Start {
            session,
            cwd,
            started_at,
        } => {
            let host_id = required_host_id(cli.host_id)?;
            let command = read_command()?;
            if command.starts_with(' ') {
                return Ok(());
            }
            let id = capture_with_deadline(async {
                let (_instance, user) = connect(&cli.user).await?;
                start(
                    &open_database(&user).await?,
                    host_id,
                    &session,
                    &cwd,
                    &command,
                    parse_time(&started_at)?,
                )
                .await
            })
            .await?;
            if let Some(id) = id {
                println!("{id}");
            }
        }
        Command::Finish {
            id,
            duration_ms,
            exit_status,
        } => {
            let host_id = required_host_id(cli.host_id)?;
            capture_with_deadline(async {
                let (_instance, user) = connect(&cli.user).await?;
                finish(
                    &open_database(&user).await?,
                    host_id,
                    &id,
                    duration_ms,
                    exit_status,
                )
                .await
            })
            .await?;
        }
        Command::List {
            host_id,
            all_hosts,
            limit,
            duplicates,
        } => {
            let (_instance, user) = connect(&cli.user).await?;
            let database = open_database(&user).await?;
            let host_id = selected_host(host_id.or(cli.host_id), all_hosts)?;
            print_entries(
                std::io::stdout(),
                &query(&database, host_id, None, limit, duplicates).await?,
            )?;
        }
        Command::Search {
            text,
            host_id,
            all_hosts,
            limit,
            duplicates,
        } => {
            let (_instance, user) = connect(&cli.user).await?;
            let database = open_database(&user).await?;
            let host_id = selected_host(host_id.or(cli.host_id), all_hosts)?;
            print_entries(
                std::io::stdout(),
                &query(&database, host_id, Some(&text), limit, duplicates).await?,
            )?;
        }
        Command::Summary { host_id } => {
            let (_instance, user) = connect(&cli.user).await?;
            let database = open_database(&user).await?;
            print_summary(std::io::stdout(), &summarize(&database, host_id).await?)?;
        }
        Command::Status => status(&cli.user, cli.host_id).await?,
    }
    Ok(())
}

fn read_command() -> Result<String> {
    let mut command = String::new();
    std::io::stdin().read_to_string(&mut command)?;
    Ok(command)
}

async fn capture_with_deadline<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(CAPTURE_DEADLINE, future)
        .await
        .map_err(|_| -> eidetica::Error {
            eidetica::store::StoreError::InvalidOperation {
                store: "shistory".to_owned(),
                operation: "capture".to_owned(),
                reason: "daemon request exceeded the 1 second capture deadline".to_owned(),
            }
            .into()
        })?
}

async fn status(username: &str, host_id: Option<Uuid>) -> Result<()> {
    let socket_path = default_socket_path();
    println!(
        "effective socket: {}",
        status_value(&socket_path.display().to_string())
    );
    println!("selected user: {}", status_value(username));

    let mut healthy = true;
    let host_id = match host_id {
        Some(host_id) => {
            println!("host configuration: {host_id}");
            Some(host_id)
        }
        None => {
            healthy = false;
            println!(
                "host configuration: failed — set SHISTORY_HOST_ID or --host-id to the UUID printed by setup"
            );
            None
        }
    };

    let instance = match status_with_deadline(
        "daemon connection",
        Instance::connect(default_socket_url()),
    )
    .await
    {
        Ok(instance) => {
            println!("daemon connectivity: ok");
            instance
        }
        Err(error) => {
            println!(
                "daemon connectivity: failed — {}; start the Eidetica daemon or set EIDETICA_SOCKET to its service socket",
                status_error_value(&error)
            );
            println!("passwordless user: skipped — daemon connection failed");
            println!("history database: skipped — daemon connection failed");
            println!("configured host: skipped — daemon connection failed");
            status_limitations();
            return status_result(false);
        }
    };

    let user = match status_with_deadline(
        "passwordless user login",
        instance.login_user(username, None),
    )
    .await
    {
        Ok(user) => {
            println!("passwordless user: ok");
            user
        }
        Err(error) => {
            println!(
                "passwordless user: failed — {}; create the configured passwordless daemon user or select it with SHISTORY_USER",
                status_error_value(&error)
            );
            println!("history database: skipped — passwordless user login failed");
            println!("configured host: skipped — passwordless user login failed");
            status_limitations();
            return status_result(false);
        }
    };

    let database = match status_with_deadline("history database lookup", open_database(&user)).await
    {
        Ok(database) => {
            println!(
                "history database: ok ({})",
                status_value(&database.root_id().to_string())
            );
            database
        }
        Err(error) => {
            println!(
                "history database: failed — {}; run shistory setup once for this user",
                status_error_value(&error)
            );
            println!("configured host: skipped — history database lookup failed");
            status_limitations();
            return status_result(false);
        }
    };

    match host_id {
        Some(host_id) => {
            match status_with_deadline("configured host lookup", host(&database, host_id)).await {
                Ok(host) => println!(
                    "configured host: ok ({host_id}, {})",
                    bounded_escaped(&host.name, HOST_DISPLAY_LIMIT)
                ),
                Err(error) => {
                    healthy = false;
                    println!(
                        "configured host: failed — {}; use the UUID printed by setup for this host",
                        status_error_value(&error)
                    );
                }
            }
        }
        None => println!("configured host: skipped — no host UUID is configured"),
    }

    status_limitations();
    status_result(healthy)
}

fn status_limitations() {
    println!("shell hooks: not checked — status cannot inspect a parent shell");
    println!("remote synchronization: not checked — status does not attempt or prove sync");
    println!(
        "write readiness: not checked — status does not write a probe; service login may idempotently bootstrap missing user-tree metadata"
    );
}

fn status_value(value: &str) -> String {
    bounded_escaped(value, STATUS_VALUE_LIMIT)
}

fn status_error_value(error: &impl std::fmt::Display) -> String {
    bounded_escaped(&error.to_string(), STATUS_ERROR_LIMIT)
}

async fn status_with_deadline<T>(
    operation: &str,
    future: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::time::timeout(CAPTURE_DEADLINE, future)
        .await
        .map_err(|_| {
            status_error(format!(
                "{operation} exceeded the 1 second diagnostic deadline"
            ))
        })?
}

fn status_result(healthy: bool) -> Result<()> {
    if healthy {
        Ok(())
    } else {
        Err(status_error(
            "diagnostics found local recording prerequisites that need attention".to_owned(),
        ))
    }
}

fn status_error(reason: String) -> eidetica::Error {
    StoreError::InvalidOperation {
        store: "shistory".to_owned(),
        operation: "status".to_owned(),
        reason,
    }
    .into()
}

fn required_host_id(host_id: Option<Uuid>) -> Result<Uuid> {
    host_id.ok_or_else(|| {
        eidetica::store::StoreError::InvalidConfiguration {
            store: "shistory".to_owned(),
            reason: "set --host-id or SHISTORY_HOST_ID to the UUID printed by setup".to_owned(),
        }
        .into()
    })
}

fn selected_host(host_id: Option<Uuid>, all_hosts: bool) -> Result<Option<Uuid>> {
    if all_hosts {
        Ok(None)
    } else {
        required_host_id(host_id).map(Some)
    }
}

fn parse_time(value: &str) -> Result<DateTime<Utc>> {
    DateTime::from_str(value).map_err(|error| {
        eidetica::store::StoreError::InvalidConfiguration {
            store: "shistory".to_owned(),
            reason: format!("invalid RFC 3339 timestamp '{value}': {error}"),
        }
        .into()
    })
}
