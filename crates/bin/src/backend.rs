//! Backend creation and utility functions.

use std::path::PathBuf;

use eidetica::backend::{
    BackendImpl, DerivedCachePolicy,
    database::{InMemory, Postgres, SqlxBackend},
};

use crate::cli::{Backend, BackendConfig};

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

/// Select host-owned live-cache bounds before any persistent backend opens.
pub fn cache_policy(config: &BackendConfig) -> DerivedCachePolicy {
    let mut policy = if matches!(config.backend, Backend::Inmemory) {
        DerivedCachePolicy::default()
    } else {
        DerivedCachePolicy::persistent()
    };
    policy.max_namespaces = config.cache_namespaces;
    if let Some(bytes) = config.cache_bytes {
        policy.max_bytes = bytes;
    }
    policy
}

/// Resolve environment only at the host boundary, never in the library.
fn native_cache_dir(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let root = env("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let root = env("HOME").map(|home| PathBuf::from(home).join("Library/Caches"));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let root = env("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".cache")));
    root.map(|root| root.join("eidetica"))
}

/// Create the appropriate backend based on configuration
pub async fn create_backend(
    config: &BackendConfig,
) -> Result<Box<dyn BackendImpl>, Box<dyn std::error::Error>> {
    let data_dir = config
        .data_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));

    // Ensure data directory exists
    tokio::fs::create_dir_all(&data_dir).await?;

    let policy = cache_policy(config);

    match config.backend {
        Backend::Sqlite => {
            let db_path = data_dir.join("eidetica.db");
            tracing::info!("Using SQLite backend at {}", db_path.display());
            Ok(Box::new(open_sqlite(config, &db_path).await?))
        }
        Backend::Postgres => {
            let url = config
                .postgres_url
                .as_ref()
                .ok_or("PostgreSQL backend requires --postgres-url or EIDETICA_POSTGRES_URL")?;

            let display_url = redact_postgres_url(url);
            tracing::info!("Connecting to PostgreSQL backend at {}", display_url);

            match Postgres::connect_with_cache_policy(url, policy).await {
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
            match InMemory::load_from_file_with_cache_policy(&json_path, policy).await {
                Ok(backend) => {
                    tracing::info!("Loaded existing data from {}", json_path.display());
                    Ok(Box::new(backend))
                }
                Err(_) => {
                    tracing::info!("Starting with fresh database");
                    Ok(Box::new(InMemory::new_with_cache_policy(policy)))
                }
            }
        }
    }
}

/// Shared by ordinary opens and offline maintenance after its existing-file check.
pub async fn open_sqlite(
    config: &BackendConfig,
    path: &std::path::Path,
) -> Result<SqlxBackend, Box<dyn std::error::Error>> {
    let cache_dir = config
        .cache_dir
        .clone()
        .or_else(|| native_cache_dir(|name| std::env::var_os(name)))
        .ok_or("No platform cache location; set --cache-dir or EIDETICA_CACHE_DIR")?;
    // Create only the designated application directory. Never chmod/chown
    // supplied directories or clean the shared platform cache root.
    tokio::fs::create_dir_all(&cache_dir).await?;
    tracing::info!("Using SQLite cache directory {}", cache_dir.display());
    Ok(SqlxBackend::open_sqlite_with_cache_dir(path, &cache_dir, cache_policy(config)).await?)
}

/// Return a human-readable label for the backend type and its path/URL
pub fn backend_label(config: &BackendConfig) -> String {
    let data_dir = config
        .data_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));
    match config.backend {
        Backend::Sqlite => {
            let db_path = data_dir.join("eidetica.db");
            format!("sqlite ({})", db_path.display())
        }
        Backend::Postgres => {
            if let Some(ref url) = config.postgres_url {
                format!("postgres ({})", redact_postgres_url(url))
            } else {
                "postgres".to_string()
            }
        }
        Backend::Inmemory => {
            let json_path = data_dir.join("eidetica.json");
            format!("inmemory ({})", json_path.display())
        }
    }
}

#[cfg(test)]
mod cache_policy_tests {
    use super::*;
    use crate::cli::{Cli, Commands};
    use clap::Parser;

    fn parse(args: &[&str]) -> BackendConfig {
        let cli = Cli::try_parse_from(args).unwrap();
        let Some(Commands::Daemon(daemon)) = cli.command else {
            panic!("expected daemon");
        };
        daemon.backend_config
    }

    #[test]
    fn cache_directory_override_and_platform_default() {
        let config = parse(&["eidetica", "daemon", "--cache-dir", "/custom/cache"]);
        assert_eq!(config.cache_dir, Some(PathBuf::from("/custom/cache")));
        let env = |key: &str| match key {
            "HOME" => Some("/home/test".into()),
            "XDG_CACHE_HOME" => Some("/scratch/cache".into()),
            "LOCALAPPDATA" => Some("C:/Users/test/AppData/Local".into()),
            _ => None,
        };
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            assert_eq!(
                native_cache_dir(env),
                Some("/scratch/cache/eidetica".into())
            );
            assert_eq!(
                native_cache_dir(|key| (key == "HOME").then(|| "/home/test".into())),
                Some("/home/test/.cache/eidetica".into())
            );
        }
        #[cfg(target_os = "macos")]
        assert_eq!(
            native_cache_dir(env),
            Some("/home/test/Library/Caches/eidetica".into())
        );
        #[cfg(target_os = "windows")]
        assert_eq!(
            native_cache_dir(env),
            Some("C:/Users/test/AppData/Local/eidetica".into())
        );
        assert!(native_cache_dir(|_| None).is_none());
    }

    #[test]
    fn backend_defaults_and_explicit_live_bounds() {
        let config = parse(&["eidetica", "daemon", "--backend", "inmemory"]);
        assert_eq!(
            cache_policy(&config),
            DerivedCachePolicy {
                max_namespaces: 1024,
                max_bytes: 268435456
            }
        );
        for backend in ["sqlite", "postgres"] {
            let config = parse(&["eidetica", "daemon", "--backend", backend]);
            assert_eq!(
                cache_policy(&config),
                DerivedCachePolicy {
                    max_namespaces: 1024,
                    max_bytes: 4294967296
                }
            );
        }
        let config = parse(&[
            "eidetica",
            "daemon",
            "--cache-bytes",
            "0",
            "--cache-namespaces",
            "7",
        ]);
        assert_eq!(
            cache_policy(&config),
            DerivedCachePolicy {
                max_namespaces: 7,
                max_bytes: 0
            }
        );
        assert!(Cli::try_parse_from(["eidetica", "daemon", "--cache-bytes", "-1"]).is_err());
    }
}
