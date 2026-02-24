//! CLI argument definitions for the Eidetica binary.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// Storage backend type
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Backend {
    /// SQLite database (default, production-ready)
    Sqlite,
    /// PostgreSQL database (for distributed deployments)
    Postgres,
    /// In-memory with JSON persistence (for development and ephemeral deployments)
    Inmemory,
}

/// Eidetica decentralized database server
#[derive(Parser, Debug)]
#[command(name = "eidetica")]
#[command(about = "Eidetica: Remember Everything - Decentralized Database Server")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Output in JSON format instead of human-readable text
    #[arg(long, global = true)]
    pub json: bool,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run the Eidetica server
    Serve(ServeArgs),
    /// Check health of a running Eidetica server
    Health(HealthArgs),
    /// Show instance information (device ID, user count, database count)
    Info(InfoArgs),
    /// Database management commands
    Db {
        #[command(subcommand)]
        command: DbCommands,
    },
}

/// Shared backend configuration for commands that access storage directly
#[derive(clap::Args, Debug)]
pub struct BackendConfig {
    /// Storage backend to use
    #[arg(short, long, default_value = "sqlite", env = "EIDETICA_BACKEND")]
    pub backend: Backend,

    /// Data directory for storage files.
    /// For SQLite: stores eidetica.db
    /// For InMemory: stores eidetica.json
    #[arg(short = 'D', long, env = "EIDETICA_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    /// PostgreSQL connection URL (required when backend=postgres)
    #[arg(long, env = "EIDETICA_POSTGRES_URL")]
    pub postgres_url: Option<String>,
}

/// Arguments for the serve command
#[derive(clap::Args, Debug)]
pub struct ServeArgs {
    /// Port to listen on
    #[arg(short, long, default_value_t = 3000, env = "EIDETICA_PORT")]
    pub port: u16,

    /// Bind address
    #[arg(long, default_value = "0.0.0.0", env = "EIDETICA_HOST")]
    pub host: String,

    #[command(flatten)]
    pub backend_config: BackendConfig,
}

/// Arguments for the health command
#[derive(clap::Args, Debug)]
pub struct HealthArgs {
    /// Port of the server to check
    #[arg(short, long, default_value_t = 3000, env = "EIDETICA_PORT")]
    pub port: u16,

    /// Host of the server to check
    #[arg(long, default_value = "127.0.0.1", env = "EIDETICA_HOST")]
    pub host: String,

    /// Timeout in seconds
    #[arg(short, long, default_value_t = 5)]
    pub timeout: u64,
}

/// Arguments for the info command
#[derive(clap::Args, Debug)]
pub struct InfoArgs {
    #[command(flatten)]
    pub backend_config: BackendConfig,
}

/// Database subcommands
#[derive(Subcommand, Debug)]
pub enum DbCommands {
    /// List all databases with their root IDs and tip counts
    List(DbListArgs),
}

/// Arguments for db list
#[derive(clap::Args, Debug)]
pub struct DbListArgs {
    #[command(flatten)]
    pub backend_config: BackendConfig,
}
