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
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run the Eidetica server
    Serve(ServeArgs),
    /// Check health of a running Eidetica server
    Health(HealthArgs),
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
