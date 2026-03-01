//! Daemon command - runs the Eidetica local service (Unix socket).

use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::EnvFilter;

use eidetica::Instance;
use eidetica::service::ServiceServer;
use eidetica::service::default_socket_path;

use crate::backend::create_backend;
use crate::cli::DaemonArgs;

/// Run the Eidetica daemon.
pub async fn run(args: &DaemonArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("eidetica=info".parse().unwrap()),
        )
        .init();

    // Create backend
    let backend = create_backend(&args.backend_config).await?;

    // Initialize Instance
    let instance = Instance::open(backend).await?;
    tracing::info!(
        "Instance initialized (device ID: {})",
        instance.device_id_string()
    );

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
    println!("  Instance::connect(\"{}\")", socket_path.display());
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
