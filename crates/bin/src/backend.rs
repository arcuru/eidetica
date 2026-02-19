//! Backend creation and utility functions.

use std::path::PathBuf;

use eidetica::backend::{
    BackendImpl,
    database::{InMemory, Postgres, Sqlite},
};

use crate::cli::{Backend, ServeArgs};

/// Redact credentials from a PostgreSQL connection URL for safe logging
pub fn redact_postgres_url(url: &str) -> String {
    if let Ok(parsed) = url::Url::parse(url) {
        let mut redacted = parsed.clone();
        if !parsed.username().is_empty() {
            let _ = redacted.set_username("***");
        }
        if parsed.password().is_some() {
            let _ = redacted.set_password(Some("***"));
        }
        redacted.to_string()
    } else {
        "postgres://***@<unparsable-url>".to_string()
    }
}

/// Create the appropriate backend based on configuration
pub async fn create_backend(
    args: &ServeArgs,
) -> Result<Box<dyn BackendImpl>, Box<dyn std::error::Error>> {
    let data_dir = args.data_dir.clone().unwrap_or_else(|| PathBuf::from("."));

    // Ensure data directory exists
    tokio::fs::create_dir_all(&data_dir).await?;

    match args.backend {
        Backend::Sqlite => {
            let db_path = data_dir.join("eidetica.db");
            tracing::info!("Using SQLite backend at {}", db_path.display());
            Ok(Box::new(Sqlite::open(&db_path).await?))
        }
        Backend::Postgres => {
            let url = args
                .postgres_url
                .as_ref()
                .ok_or("PostgreSQL backend requires --postgres-url or EIDETICA_POSTGRES_URL")?;

            let display_url = redact_postgres_url(url);
            tracing::info!("Connecting to PostgreSQL backend at {}", display_url);

            match Postgres::connect(url).await {
                Ok(backend) => {
                    tracing::info!("Connected to PostgreSQL successfully");
                    Ok(Box::new(backend))
                }
                Err(e) => {
                    Err(format!("Failed to connect to PostgreSQL at {}: {}", display_url, e).into())
                }
            }
        }
        Backend::Inmemory => {
            let json_path = data_dir.join("eidetica.json");
            tracing::info!(
                "Using in-memory backend with persistence at {}",
                json_path.display()
            );
            match InMemory::load_from_file(&json_path).await {
                Ok(backend) => {
                    tracing::info!("Loaded existing data from {}", json_path.display());
                    Ok(Box::new(backend))
                }
                Err(_) => {
                    tracing::info!("Starting with fresh database");
                    Ok(Box::new(InMemory::new()))
                }
            }
        }
    }
}
