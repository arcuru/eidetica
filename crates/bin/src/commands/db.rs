//! Database management commands.

use eidetica::{Instance, backend::BackendImpl};

use crate::backend::create_backend;
use crate::cli::{Backend, DbListArgs, DbResetArgs};
use crate::output::{OutputFormat, print_table};

/// Run the `db list` command
pub async fn list(
    args: &DbListArgs,
    format: OutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let backend = create_backend(&args.backend_config).await?;
    let instance = Instance::open_backend(backend).await?;

    let all_roots = instance
        .backend()
        .local_engine()
        .expect("db list opens a local backend directly")
        .all_roots()
        .await?;

    // Collect system DB root IDs to filter them out
    let metadata = instance.backend().get_instance_metadata().await?;
    let system_ids: Vec<_> = if let Some(ref meta) = metadata {
        let mut ids = vec![meta.users_db.clone(), meta.databases_db.clone()];
        if let Some(ref sync_id) = meta.sync_db {
            ids.push(sync_id.clone());
        }
        ids
    } else {
        vec![]
    };

    let user_roots: Vec<_> = all_roots
        .into_iter()
        .filter(|id| !system_ids.contains(id))
        .collect();

    match format {
        OutputFormat::Human => {
            if user_roots.is_empty() {
                println!("No databases found.");
                return Ok(());
            }

            let mut rows = Vec::with_capacity(user_roots.len());
            for root in &user_roots {
                let tips = instance.backend().snapshot(root).await?;
                rows.push(vec![root.to_string(), tips.len().to_string()]);
            }
            print_table(&["ROOT ID", "TIPS"], &rows);
        }
        OutputFormat::Json => {
            let mut entries = Vec::with_capacity(user_roots.len());
            for root in &user_roots {
                let tips = instance.backend().snapshot(root).await?;
                entries.push(serde_json::json!({
                    "id": root.to_string(),
                    "tips": tips.len(),
                }));
            }
            println!("{}", serde_json::to_string(&entries)?);
        }
    }

    Ok(())
}

/// Reset only local trust state. This must not use the general backend factory:
/// that factory creates directories and treats corrupt in-memory files as empty.
pub async fn reset_local_verification(
    args: &DbResetArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    if !args.confirm {
        return Err("Pass --confirm to reset all local verification statuses".into());
    }
    let config = &args.backend_config;
    match config.backend {
        Backend::Inmemory => {
            let dir = config
                .data_dir
                .as_ref()
                .ok_or("Specify --data-dir explicitly")?;
            let path = dir.join("eidetica.json");
            let backend = eidetica::backend::database::InMemory::try_load_from_file(&path)
                .await?
                .ok_or("In-memory persistence file does not exist")?;
            backend.reset_local_verification().await?;
            backend.save_to_file(&path)?;
        }
        Backend::Sqlite => {
            let dir = config
                .data_dir
                .as_ref()
                .ok_or("Specify --data-dir explicitly")?;
            let path = dir.join("eidetica.db");
            if !path.is_file() {
                return Err("SQLite database file does not exist".into());
            }
            eidetica::backend::database::Sqlite::open(&path)
                .await?
                .reset_local_verification()
                .await?;
        }
        Backend::Postgres => {
            let url = config
                .postgres_url
                .as_ref()
                .ok_or("Specify --postgres-url explicitly")?;
            eidetica::backend::database::Postgres::connect(url)
                .await?
                .reset_local_verification()
                .await?;
        }
    }
    println!("Local verification reset; restart and reverify all databases before trusting reads.");
    Ok(())
}
