//! Daemon command - runs the Eidetica local service (Unix socket).
//!
//! Two entry points:
//!
//! - [`run_init`] — `eidetica daemon init` — initialises a fresh instance with
//!   an explicit initial admin user. Fails if the backend is already
//!   initialised.
//! - [`run`] — `eidetica daemon` (no subcommand) — runs the daemon against an
//!   already-initialised backend. Fails with a pointer to `daemon init` if
//!   the backend is empty.

use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::EnvFilter;

use eidetica::Instance;
use eidetica::NewUser;
use eidetica::instance::InstanceError;
use eidetica::service::ServiceServer;
use eidetica::service::default_socket_path;

use crate::backend::create_backend;
use crate::cli::{BackendConfig, DaemonArgs, DaemonInitArgs};

/// Run the Eidetica daemon against an already-initialised backend.
///
/// Errors with a pointer to `eidetica daemon init` if the backend hasn't been
/// initialised yet (i.e. `Instance::open_backend` returns
/// [`InstanceError::NotInitialized`]).
pub async fn run(args: &DaemonArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("eidetica=info".parse().unwrap()),
        )
        .init();

    // Create backend
    let backend = create_backend(&args.backend_config).await?;

    // Initialize Instance — load only. Map NotInitialized to a friendly
    // pointer at `daemon init`.
    let instance = match Instance::open_backend(backend).await {
        Ok(instance) => instance,
        Err(e) => {
            if let eidetica::Error::Instance(boxed) = &e
                && matches!(boxed.as_ref(), InstanceError::NotInitialized)
            {
                return Err(format!(
                    "Backend at {} is not initialised.\nRun `eidetica daemon init --username <NAME> [--password PASS | --passwordless]` first.",
                    crate::backend::backend_label(&args.backend_config)
                )
                .into());
            }
            return Err(Box::new(e));
        }
    };
    tracing::info!("Instance initialized (device ID: {})", instance.id());

    // Determine socket path
    let socket_path = args.socket.clone().unwrap_or_else(default_socket_path);

    // Create and start server
    let server = ServiceServer::new(instance, &socket_path);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(());

    println!("Eidetica daemon listening on {}", socket_path.display());
    println!(
        "  Backend: {}",
        crate::backend::backend_label(&args.backend_config)
    );
    println!();
    println!("Connect with:");
    println!("  Instance::connect(\"unix://{}\")", socket_path.display());
    println!();
    println!("Press Ctrl+C to shutdown");

    // Run server with signal-based shutdown
    let server_handle = tokio::spawn(async move { server.run(shutdown_rx).await });

    // Wait for shutdown signal
    let mut sigterm = signal(SignalKind::terminate()).expect("failed to set up SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("failed to set up SIGINT handler");

    tokio::select! {
        _ = sigterm.recv() => tracing::info!("Received SIGTERM"),
        _ = sigint.recv() => tracing::info!("Received SIGINT"),
    }

    // Signal shutdown
    drop(shutdown_tx);
    let _ = server_handle.await;

    println!("Daemon shut down");
    Ok(())
}

/// Initialise a fresh daemon instance with an initial admin user.
///
/// Builds the [`NewUser`] from `--username` + one of `--password` /
/// `--passwordless` / an interactive double-prompt, then calls
/// [`Instance::create_backend`]. Exits after initialisation; the operator runs
/// `eidetica daemon` separately to actually serve the socket.
pub async fn run_init(
    args: &DaemonInitArgs,
    backend_args: &BackendConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("eidetica=info".parse().unwrap()),
        )
        .init();

    // Resolve the password choice. `--passwordless` and explicit `--password`
    // are mutually exclusive (enforced by clap); otherwise prompt twice.
    let new_user = if args.passwordless {
        NewUser::passwordless(&args.username)
    } else if let Some(pw) = &args.password {
        NewUser::with_password(&args.username, pw)
    } else {
        let pw = rpassword::prompt_password(format!(
            "Password for new admin user '{}': ",
            args.username
        ))?;
        let confirm = rpassword::prompt_password("Re-enter password: ")?;
        if pw != confirm {
            return Err("Passwords did not match.".into());
        }
        if pw.is_empty() {
            return Err(
                "Empty password rejected. Pass `--passwordless` to opt in to no-password mode."
                    .into(),
            );
        }
        NewUser::with_password(&args.username, pw)
    };

    let backend = create_backend(backend_args).await?;
    let (instance, _user) = match Instance::create_backend(backend, new_user).await {
        Ok(pair) => pair,
        Err(e) => {
            if let eidetica::Error::Instance(boxed) = &e
                && matches!(boxed.as_ref(), InstanceError::InstanceAlreadyExists)
            {
                return Err(format!(
                    "Backend at {} is already initialised.\nUse `eidetica daemon` (no subcommand) to run the existing instance.",
                    crate::backend::backend_label(backend_args)
                )
                .into());
            }
            return Err(Box::new(e));
        }
    };

    println!(
        "Eidetica instance initialised on {}",
        crate::backend::backend_label(backend_args)
    );
    println!("  Device ID:    {}", instance.id());
    println!("  Initial user: {}", args.username);
    println!();
    println!("Start the daemon with: `eidetica daemon`");

    Ok(())
}
