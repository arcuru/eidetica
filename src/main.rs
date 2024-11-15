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

#[tokio::main]
async fn main() -> Result<()> {
    // tracing_subscriber::fmt::init();

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

    // Device ids aren't real yet, so add a dummy
    let device_id = uuid::Builder::nil().into_uuid();

    let mut store = DataStore::from_pool(pool, "cmdfiles", device_id).await?;

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
        println!("Location: {:?}", &location);

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
