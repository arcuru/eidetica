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

    let server = ServiceServer::bind(instance, &socket_path).await?;
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

    let server = server.run(shutdown_rx);
    tokio::pin!(server);

    let mut sigterm = signal(SignalKind::terminate()).expect("failed to set up SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("failed to set up SIGINT handler");

    tokio::select! {
        result = &mut server => {
            result?;
            return Err("service server stopped unexpectedly".into());
        }
        _ = sigterm.recv() => tracing::info!("Received SIGTERM"),
        _ = sigint.recv() => tracing::info!("Received SIGINT"),
    }

    drop(shutdown_tx);
    server.await?;

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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use super::*;
    use crate::cli::Backend;

    fn daemon_args(data_dir: &std::path::Path, socket: &std::path::Path) -> DaemonArgs {
        DaemonArgs {
            command: None,
            socket: Some(socket.to_path_buf()),
            backend_config: BackendConfig {
                backend: Backend::Inmemory,
                data_dir: Some(data_dir.to_path_buf()),
                postgres_url: None,
            },
        }
    }

    #[tokio::test]
    async fn daemon_bind_error_returns_within_a_bound() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let socket_parent = dir.path().join("not-a-directory");
        let socket_path = socket_parent.join("daemon.sock");
        tokio::fs::write(&socket_parent, b"not a directory")
            .await
            .unwrap();
        let backend = create_backend(&BackendConfig {
            backend: Backend::Inmemory,
            data_dir: Some(data_dir.clone()),
            postgres_url: None,
        })
        .await
        .unwrap();
        Instance::create_backend(backend, NewUser::passwordless("admin"))
            .await
            .unwrap();

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run(&daemon_args(&data_dir, &socket_path)),
        )
        .await
        .expect("daemon startup failure must not hang");

        assert!(result.is_err());
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn concurrent_daemon_is_rejected_without_disturbing_owner() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let socket_parent = dir.path().join("runtime");
        tokio::fs::create_dir(&socket_parent).await.unwrap();
        tokio::fs::set_permissions(&socket_parent, std::fs::Permissions::from_mode(0o700))
            .await
            .unwrap();
        let socket_path = socket_parent.join("daemon.sock");
        let backend = create_backend(&BackendConfig {
            backend: Backend::Inmemory,
            data_dir: Some(data_dir.clone()),
            postgres_url: None,
        })
        .await
        .unwrap();
        let (instance, _admin) = Instance::create_backend(backend, NewUser::passwordless("admin"))
            .await
            .unwrap();
        let owner = ServiceServer::bind(instance.clone(), &socket_path)
            .await
            .unwrap();
        let expected_id = instance.id();
        let (shutdown, rx) = tokio::sync::watch::channel(());
        let owner_task = tokio::spawn(owner.run(rx));

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            run(&daemon_args(&data_dir, &socket_path)),
        )
        .await
        .expect("concurrent daemon startup must be bounded");

        assert!(result.is_err());
        let client = tokio::time::timeout(
            Duration::from_secs(1),
            Instance::connect(format!("unix://{}", socket_path.display())),
        )
        .await
        .expect("the original daemon must answer within a bound")
        .expect("the original daemon must still serve protocol requests");
        assert_eq!(client.id(), expected_id);
        drop(client);
        drop(shutdown);
        owner_task.await.unwrap().unwrap();
        assert!(!socket_path.exists());
    }
}
