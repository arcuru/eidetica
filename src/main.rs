mod datastore;
mod utils;

use clap::Parser;
use datastore::data_handler::DataLocation;
use datastore::error::Result;
use datastore::store::DataStore;
use sqlx::postgres::PgPoolOptions;
use std::env;
use std::path::PathBuf;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct EideticaArgs {
    /// path to config file
    #[arg(short, long)]
    config: Option<PathBuf>,

    #[arg(short, long)]
    insert: Option<PathBuf>,

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

    if let Some(insertion) = args.insert {
        let id = store
            .store_data(
                DataLocation::LocalPath(insertion),
                serde_json::Value::Null,
                None,
            )
            .await?;
        println!("Successfully inserted, UUID: {}", id);
    } else if args.list {
        // List the raw data from all active entries
        let entries = store.get_active_entries().await?;
        for x in entries {
            println!("Entry: {:?}", x);
        }
    }

    Ok(())
}
