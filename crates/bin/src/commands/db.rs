//! Database management commands.

use eidetica::Instance;

use crate::backend::create_backend;
use crate::cli::DbListArgs;
use crate::output::{OutputFormat, print_table};

/// Run the `db list` command
pub async fn list(
    args: &DbListArgs,
    format: OutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let backend = create_backend(&args.backend_config).await?;
    let instance = Instance::open(backend).await?;

    let all_roots = instance.backend().all_roots().await?;

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
                let tips = instance.backend().get_tips(root).await?;
                rows.push(vec![root.to_string(), tips.len().to_string()]);
            }
            print_table(&["ROOT ID", "TIPS"], &rows);
        }
        OutputFormat::Json => {
            let mut entries = Vec::with_capacity(user_roots.len());
            for root in &user_roots {
                let tips = instance.backend().get_tips(root).await?;
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
