//! Instance info command - shows device ID, backend, user/database counts.

use eidetica::Instance;

use crate::backend::{backend_label, create_backend};
use crate::cli::InfoArgs;
use crate::output::OutputFormat;

/// Run the info command
pub async fn run(args: &InfoArgs, format: OutputFormat) -> Result<(), Box<dyn std::error::Error>> {
    let backend = create_backend(&args.backend_config).await?;
    let instance = Instance::open(backend).await?;

    let device_id = instance.device_id_string();
    let users = instance.list_users().await?;
    let all_roots = instance.backend().all_roots().await?;

    // Filter out system database roots to get user database count
    let system_db_count = if instance.sync().is_some() { 3 } else { 2 };
    let db_count = all_roots.len().saturating_sub(system_db_count);

    let backend_str = backend_label(&args.backend_config);

    match format {
        OutputFormat::Human => {
            println!("Device ID:   {device_id}");
            println!("Backend:     {backend_str}");
            println!("Users:       {}", users.len());
            println!("Databases:   {db_count}");
        }
        OutputFormat::Json => {
            let value = serde_json::json!({
                "device_id": device_id,
                "backend": backend_str,
                "users": users.len(),
                "databases": db_count,
            });
            println!("{}", serde_json::to_string(&value)?);
        }
    }

    Ok(())
}
