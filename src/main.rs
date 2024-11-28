mod datastore;
mod plugins;
mod utils;

use anyhow::Result;
use clap::Parser;
use datastore::data_handler::DataLocation;
use datastore::store::DataStore;
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use std::env;
use std::path::PathBuf;
use std::str::FromStr;
use tracing::info;
use utils::generate_key_deterministic;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct EideticaArgs {
    /// Plugin
    #[command(subcommand)]
    plugin: Option<plugins::PluginArgs>,

    /// path to config file
    #[arg(short, long)]
    config: Option<PathBuf>,

    #[arg(short, long)]
    insert: Option<String>,

    #[arg(long)]
    meta: Option<String>,

    #[arg(short, long)]
    list: bool,
}

/// Setup logging with tracing
fn setup_logging() {
    use tracing_subscriber::{fmt::format::FmtSpan, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_span_events(FmtSpan::CLOSE)
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    setup_logging();

    // Read in the config file
    let args = EideticaArgs::parse();

    // Retrieve DATABASE_URL from environment variables
    let database_url =
        env::var("DATABASE_URL").expect("DATABASE_URL must be set in the environment");

    // Create a connection pool
    let pool: sqlx::PgPool = PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("Error creating PostgreSQL connection pool");

    // Device IDs are sort-of real
    // FIXME: Stop generating this deterministically
    let signing_key = generate_key_deterministic();
    let device_id = signing_key.verifying_key().to_bytes();
    let store_key = generate_key_deterministic();
    let store_id = store_key.verifying_key().to_bytes();

    // Attempt to create store, initializing if needed
    let mut store = match DataStore::from_pool(pool.clone(), "cmdfiles", store_id, device_id).await
    {
        Ok(store) => store,
        Err(_) => {
            // If it fails lets just try to initialize
            let local_path = PathBuf::from(
                env::var("EIDETICA_DATA_DIR").unwrap_or_else(|_| "/tmp/eidetica".to_string()),
            );
            std::fs::create_dir_all(&local_path)?;

            DataStore::init(pool, "cmdfiles", store_id, device_id, local_path).await?
        }
    };

    let metadata: Option<Value> = match args.meta {
        Some(meta) =>
        // Parse the metadata string into a serde_json::Value
        {
            Some(
                serde_json::from_str(&meta)
                    .map_err(|e| anyhow::anyhow!("Invalid metadata JSON: {}", e))?,
            )
        }
        None => None,
    };

    // Send out to the plugin
    if let Some(plugin_args) = args.plugin {
        return plugins::run(plugin_args, &mut store).await;
    }

    if let Some(insertion) = args.insert {
        let location = string_to_datalocation(&insertion)?;
        info!("Location: {:?}", &location);

        let id = store
            .store_data(location, metadata.unwrap_or_default(), None)
            .await?;
        println!("Successfully inserted, UUID: {}", id);
    } else if args.list {
        // List the raw data from all active entries
        let entries = match metadata {
            Some(conditions) => store.get_entries_by_metadata_conditions(conditions).await?,
            None => store.get_active_entries().await?,
        };
        for x in entries {
            println!("Entry: {:?}", x);
        }
    }

    Ok(())
}

/// Attempts to interpret user input as some kind of data location
///
/// Either a path or a url
fn string_to_datalocation(something: &str) -> Result<DataLocation> {
    let pb = PathBuf::from_str(something).unwrap();
    if pb.exists() {
        Ok(DataLocation::LocalPath(pb))
    } else {
        Ok(DataLocation::Url(something.to_string()))
    }
}
