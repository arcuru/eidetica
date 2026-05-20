//! Instance info command - shows device ID, backend, and database count.

use eidetica::Instance;
use eidetica::instance::InstanceError;

use crate::backend::{backend_label, create_backend};
use crate::cli::InfoArgs;
use crate::output::OutputFormat;

/// Run the info command
pub async fn run(args: &InfoArgs, format: OutputFormat) -> Result<(), Box<dyn std::error::Error>> {
    let backend = create_backend(&args.backend_config).await?;
    let instance = match Instance::open(backend).await {
        Ok(instance) => instance,
        Err(e) => {
            if let eidetica::Error::Instance(boxed) = &e
                && matches!(boxed.as_ref(), InstanceError::NotInitialized)
            {
                return Err(format!(
                    "Backend at {} is not initialised.\nRun `eidetica daemon init --username <NAME> [--password PASS | --passwordless]` first.",
                    backend_label(&args.backend_config)
                )
                .into());
            }
            return Err(Box::new(e));
        }
    };

    let device_id = instance.id();
    let all_roots = instance.backend().all_roots().await?;

    // Filter out system database roots to get user database count
    let system_db_count = if instance.sync().is_some() { 3 } else { 2 };
    let db_count = all_roots.len().saturating_sub(system_db_count);

    let backend_str = backend_label(&args.backend_config);

    // User count is no longer reported here: listing users is an admin
    // operation and `info` doesn't currently accept admin credentials.
    // (A future enhancement could add `--username` + interactive password
    // prompt to surface the user count when the operator provides them.)
    match format {
        OutputFormat::Human => {
            println!("Device ID:   {device_id}");
            println!("Backend:     {backend_str}");
            println!("Databases:   {db_count}");
        }
        OutputFormat::Json => {
            let value = serde_json::json!({
                "device_id": device_id,
                "backend": backend_str,
                "databases": db_count,
            });
            println!("{}", serde_json::to_string(&value)?);
        }
    }

    Ok(())
}
