use std::sync::Arc;

use axum::{
    Router,
    extract::{Json as ExtractJson, State},
    routing::post,
};
use eidetica::{
    Instance,
    backend::database::InMemory,
    sync::{
        handler::SyncHandlerImpl,
        protocol::{SyncRequest, SyncResponse},
    },
};
use signal_hook::flag as signal_flag;
use tracing_subscriber::EnvFilter;

const DB_FILE: &str = "eidetica.json";
const PORT: u16 = 3000;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("eidetica=info".parse().unwrap()),
        )
        .init();

    // Load or create database instance
    let backend_box: Box<dyn eidetica::backend::BackendImpl> =
        match InMemory::load_from_file(DB_FILE) {
            Ok(backend) => {
                tracing::info!("Loaded database from {DB_FILE}");
                Box::new(backend)
            }
            Err(e) => {
                tracing::warn!("Failed to load database: {e:?}. Creating a new one.");
                Box::new(InMemory::new())
            }
        };

    // Initialize Instance
    let instance = Instance::open(backend_box)?;

    // Enable sync (creates sync tree and device key internally)
    instance.enable_sync()?;

    // Get sync object and create handler
    let sync = instance.sync().expect("Sync should be enabled");
    let sync_tree_id = sync.sync_tree_root_id().clone();
    let sync_handler: Arc<dyn eidetica::sync::handler::SyncHandler> =
        Arc::new(SyncHandlerImpl::new(instance.clone(), sync_tree_id));

    // Build router
    let app = Router::new()
        .route("/api/v0", post(handle_sync_request))
        .with_state(sync_handler);

    // Set up graceful shutdown signal handling
    let term_signal = Arc::new(std::sync::atomic::AtomicBool::new(false));
    for signal in signal_hook::consts::TERM_SIGNALS {
        let _ = signal_flag::register(*signal, Arc::clone(&term_signal));
    }

    // Bind server
    let addr = format!("0.0.0.0:{PORT}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let local_addr = listener.local_addr()?;

    // Print startup message
    println!(
        "Eidetica Server starting on http://localhost:{}",
        local_addr.port()
    );
    println!();
    println!("Available endpoints:");
    println!("  POST /api/v0       - Eidetica sync protocol endpoint");
    println!();
    println!("Press Ctrl+C to shutdown");

    // Clone instance and term_signal for shutdown handler
    let instance_for_shutdown = instance.clone();
    let term_signal_for_shutdown = term_signal.clone();

    // Start server with graceful shutdown
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            // Wait for shutdown signal
            while !term_signal_for_shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }

            tracing::info!("Shutdown signal received, saving database...");

            // Save database on shutdown
            if let Some(in_memory_backend) = instance_for_shutdown
                .backend()
                .as_any()
                .downcast_ref::<InMemory>()
            {
                match in_memory_backend.save_to_file(DB_FILE) {
                    Ok(_) => {
                        tracing::info!("Database saved successfully");
                        println!("\nDatabase saved successfully");
                    }
                    Err(e) => {
                        tracing::error!("Failed to save database: {e:?}");
                        eprintln!("Failed to save database: {e:?}");
                    }
                }
            }
        })
        .await?;

    println!("Server shut down");
    Ok(())
}

/// Handler for POST /api/v0 - Eidetica sync endpoint
async fn handle_sync_request(
    State(handler): State<Arc<dyn eidetica::sync::handler::SyncHandler>>,
    ExtractJson(request): ExtractJson<SyncRequest>,
) -> axum::Json<SyncResponse> {
    let response = handler.handle_request(&request).await;
    axum::Json(response)
}
