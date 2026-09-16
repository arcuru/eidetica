use std::{future::Future, io::Read, process::ExitCode, str::FromStr, time::Duration};

use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use eidetica::{Result, sync::DatabaseTicket};
use shistory::{
    DEFAULT_LIMIT, connect, finish, open_database, print_entries, query, rename, setup, start,
    ticket,
};
use uuid::Uuid;

const CAPTURE_DEADLINE: Duration = Duration::from_secs(1);

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
        } => {
            let (_instance, user) = connect(&cli.user).await?;
            let database = open_database(&user).await?;
            let host_id = selected_host(host_id.or(cli.host_id), all_hosts)?;
            print_entries(
                std::io::stdout(),
                &query(&database, host_id, None, limit).await?,
            )?;
        }
        Command::Search {
            text,
            host_id,
            all_hosts,
            limit,
        } => {
            let (_instance, user) = connect(&cli.user).await?;
            let database = open_database(&user).await?;
            let host_id = selected_host(host_id.or(cli.host_id), all_hosts)?;
            print_entries(
                std::io::stdout(),
                &query(&database, host_id, Some(&text), limit).await?,
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
