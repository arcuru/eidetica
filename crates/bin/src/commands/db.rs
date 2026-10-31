//! Database management commands.

use eidetica::{Instance, backend::BackendImpl};

use crate::backend::create_backend;
use crate::cli::{Backend, BackendConfig, DbListArgs, DbRepairArgs, DbResetArgs};
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
    let (backend, save_path) = open_existing(&args.backend_config).await?;
    backend.reset_local_verification().await?;
    save_if_needed(&*backend, save_path.as_deref())?;
    println!("Local verification reset; restart and reverify all databases before trusting reads.");
    Ok(())
}

/// Repair never claims successful cryptographic verification or resets Failed decisions.
pub async fn repair_verification(args: &DbRepairArgs) -> Result<(), Box<dyn std::error::Error>> {
    if !args.confirm {
        return Err("Pass --confirm to repair local verification".into());
    }
    let tree = eidetica::entry::ID::parse(&args.tree)?;
    let (backend, save_path) = open_existing(&args.backend_config).await?;
    let root = backend.get(&tree).await?;
    if !root.is_root() {
        return Err("--tree must identify a stored root Entry".into());
    }
    let result = backend.repair_verification(&tree).await?;
    save_if_needed(&*backend, save_path.as_deref())?;
    println!(
        "Verification repaired: {} demoted to Unverified; {} verified tips. No entries were verified.",
        result.demoted,
        result.snapshot.len()
    );
    Ok(())
}

/// Maintenance must not use the general factory, which creates missing targets
/// and treats corrupt in-memory persistence as empty.
async fn open_existing(
    config: &BackendConfig,
) -> Result<(Box<dyn BackendImpl>, Option<std::path::PathBuf>), Box<dyn std::error::Error>> {
    let policy = crate::backend::cache_policy(config);
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
            Ok((Box::new(backend), Some(path)))
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
            Ok((
                Box::new(
                    eidetica::backend::database::Sqlite::open_with_cache_policy(&path, policy)
                        .await?,
                ),
                None,
            ))
        }
        Backend::Postgres => {
            let url = config
                .postgres_url
                .as_ref()
                .ok_or("Specify --postgres-url explicitly")?;
            Ok((
                Box::new(
                    eidetica::backend::database::Postgres::connect_with_cache_policy(url, policy)
                        .await?,
                ),
                None,
            ))
        }
    }
}

fn save_if_needed(
    backend: &dyn BackendImpl,
    path: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(path) = path {
        backend
            .as_any()
            .downcast_ref::<eidetica::backend::database::InMemory>()
            .expect("only InMemory needs explicit save")
            .save_to_file(path)?;
    }
    Ok(())
}
