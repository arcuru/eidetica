use std::{process::ExitCode, str::FromStr};

use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use eidetica::Result;
use shistory::{DEFAULT_LIMIT, connect, finish, open_database, print_entries, query, setup, start};

#[derive(Parser)]
#[command(about = "Record zsh history in an Eidetica daemon")]
struct Cli {
    /// Passwordless Eidetica user already present in the daemon.
    #[arg(long, env = "SHISTORY_USER", default_value = "shistory")]
    user: String,

    /// Stable, unique label for this host.
    #[arg(long, env = "SHISTORY_HOST")]
    host: Option<String>,

    /// Disable history capture while leaving queries available.
    #[arg(long, env = "SHISTORY_CAPTURE_DISABLED", default_value_t = false)]
    capture_disabled: bool,

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
        command: String,
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
    if matches!(&cli.command, Command::Start { command, .. } if cli.capture_disabled || command.starts_with(' '))
    {
        return Ok(());
    }
    if matches!(&cli.command, Command::Finish { .. }) && cli.capture_disabled {
        return Ok(());
    }

    let (_instance, mut user) = connect(&cli.user).await?;
    match cli.command {
        Command::Setup => {
            let host = required_host(cli.host.as_deref())?;
            let database = setup(&mut user, host).await?;
            println!("history database: {}", database.root_id());
        }
        Command::Start {
            session,
            cwd,
            started_at,
            command,
        } => {
            let host = required_host(cli.host.as_deref())?;
            let database = open_database(&user).await?;
            if let Some(id) = start(
                &database,
                host,
                &session,
                &cwd,
                &command,
                parse_time(&started_at)?,
            )
            .await?
            {
                println!("{id}");
            }
        }
        Command::Finish {
            id,
            duration_ms,
            exit_status,
        } => {
            let host = required_host(cli.host.as_deref())?;
            let database = open_database(&user).await?;
            finish(&database, host, &id, duration_ms, exit_status).await?;
        }
        Command::List {
            host,
            all_hosts,
            limit,
        } => {
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
