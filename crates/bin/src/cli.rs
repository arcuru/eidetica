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
    /// Run the Eidetica local service daemon (Unix socket)
    Daemon(DaemonArgs),
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
    #[arg(short, long, env = "EIDETICA_DATA_DIR")]
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
    /// URL of the server to check (appends /health if no path)
    #[arg(default_value = "http://127.0.0.1:3000")]
    pub url: String,

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

/// Arguments for the daemon command
///
/// With no subcommand: runs the daemon (load-only — fails if the backend is
/// not initialised). With `init`: initialises a fresh instance with an
/// initial admin user and exits.
#[derive(clap::Args, Debug)]
pub struct DaemonArgs {
    /// Optional sub-action. With no value, runs the daemon.
    #[command(subcommand)]
    pub command: Option<DaemonCommand>,

    /// Unix socket path (default: $XDG_RUNTIME_DIR/eidetica/service.sock).
    /// Only used when running the daemon — ignored by `daemon init`.
    #[arg(short, long, env = "EIDETICA_SOCKET", global = true)]
    pub socket: Option<PathBuf>,

    #[command(flatten)]
    pub backend_config: BackendConfig,
}

/// Daemon sub-actions
#[derive(Subcommand, Debug)]
pub enum DaemonCommand {
    /// Initialise a fresh instance with an initial admin user.
    ///
    /// The first user created on an instance is automatically granted Admin
    /// on the system databases. Fails if the backend is already initialised.
    Init(DaemonInitArgs),
}

/// Arguments for `daemon init`.
#[derive(clap::Args, Debug)]
pub struct DaemonInitArgs {
    /// Username for the initial admin user. Required — there is no default
    /// to eliminate static-credential foot-guns.
    #[arg(long)]
    pub username: String,

    /// Password for the initial admin user. If neither this nor
    /// `--passwordless` is given, prompt interactively (hidden input,
    /// twice).
    #[arg(long, env = "EIDETICA_ADMIN_PASSWORD", conflicts_with = "passwordless")]
    pub password: Option<String>,

    /// Create the initial admin user without a password. Suitable for
    /// embedded / single-user development; not recommended for production.
    #[arg(long, conflicts_with = "password")]
    pub passwordless: bool,
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
