use std::{future::Future, io::Read, process::ExitCode, str::FromStr, time::Duration};

use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use eidetica::Result;
use shistory::{DEFAULT_LIMIT, connect, finish, open_database, print_entries, query, setup, start};

const CAPTURE_DEADLINE: Duration = Duration::from_secs(1);

#[derive(Parser)]
#[command(about = "Record zsh history in an Eidetica daemon")]
struct Cli {
    /// Passwordless Eidetica user already present in the daemon.
    #[arg(long, env = "SHISTORY_USER", default_value = "shistory")]
    user: String,

    /// Stable, unique label for this host.
    #[arg(long, env = "SHISTORY_HOST")]
    host: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create or open the history database and register this host.
    Setup,
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
        host: Option<String>,
        #[arg(long)]
        all_hosts: bool,
        #[arg(long, default_value_t = DEFAULT_LIMIT)]
        limit: usize,
    },
    /// Search command text.
    Search {
        text: String,
        #[arg(long, conflicts_with = "all_hosts")]
        host: Option<String>,
        #[arg(long)]
        all_hosts: bool,
        #[arg(long, default_value_t = DEFAULT_LIMIT)]
        limit: usize,
    },
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
        Command::Setup => {
            let host = required_host(cli.host.as_deref())?;
            let (_instance, mut user) = connect(&cli.user).await?;
            let database = setup(&mut user, host).await?;
            println!("history database: {}", database.root_id());
        }
        Command::Start {
            session,
            cwd,
            started_at,
        } => {
            let host = required_host(cli.host.as_deref())?;
            let command = read_command()?;
            if command.starts_with(' ') {
                return Ok(());
            }
            let id = capture_with_deadline(async {
                let (_instance, user) = connect(&cli.user).await?;
                let database = open_database(&user).await?;
                start(
                    &database,
                    host,
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
            let host = required_host(cli.host.as_deref())?;
            capture_with_deadline(async {
                let (_instance, user) = connect(&cli.user).await?;
                let database = open_database(&user).await?;
                finish(&database, host, &id, duration_ms, exit_status).await
            })
            .await?;
        }
        Command::List {
            host,
            all_hosts,
            limit,
        } => {
            let (_instance, user) = connect(&cli.user).await?;
            let database = open_database(&user).await?;
            let host = selected_host(host.as_deref().or(cli.host.as_deref()), all_hosts)?;
            print_entries(
                std::io::stdout(),
                &query(&database, host, None, limit).await?,
            )?;
        }
        Command::Search {
            text,
            host,
            all_hosts,
            limit,
        } => {
            let (_instance, user) = connect(&cli.user).await?;
            let database = open_database(&user).await?;
            let host = selected_host(host.as_deref().or(cli.host.as_deref()), all_hosts)?;
            print_entries(
                std::io::stdout(),
                &query(&database, host, Some(&text), limit).await?,
            )?;
        }
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

fn required_host(host: Option<&str>) -> Result<&str> {
    host.ok_or_else(|| {
        eidetica::store::StoreError::InvalidConfiguration {
            store: "shistory".to_owned(),
            reason: "set --host or SHISTORY_HOST to a stable, unique host label".to_owned(),
        }
        .into()
    })
}

fn selected_host(host: Option<&str>, all_hosts: bool) -> Result<Option<&str>> {
    if all_hosts {
        Ok(None)
    } else {
        required_host(host).map(Some)
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
