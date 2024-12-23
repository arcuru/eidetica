use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::Value;
use std::path::PathBuf;
use std::time::SystemTime;
use std::{thread, time::Duration};
use tracing::debug;
use uuid::Uuid;
use walkdir::WalkDir;

use crate::datastore::data::DataTable;
use crate::datastore::data_handler::DataLocation;
use crate::datastore::metadata::MetadataTable;
use crate::datastore::settings::Setting;
use crate::datastore::store::DataStore;

#[derive(Parser, Debug)]
pub struct FileArgs {
    /// Initialize the db with the path to the root of the file system to index
    #[command(subcommand)]
    command: Option<FileCommand>,
}

#[derive(Subcommand, Debug)]
enum FileCommand {
    /// Scan the file system for new files and index them
    Scan(ScanArgs),

    /// Initialize with a directory to index
    Init(InitArgs),

    /// List entries
    List(ListArgs),

    /// Watch directory for changes, scanning periodically
    Watch(WatchArgs),
}

#[derive(Parser, Debug)]
struct InitArgs {
    /// Path to the root of the file system to index
    path: PathBuf,
}

#[derive(Parser, Debug)]
struct ScanArgs {
    /// Optional path that the scan will be restricted to.
    #[arg(short, long)]
    path: Option<PathBuf>,
}

#[derive(Parser, Debug)]
struct ListArgs {
    /// Optional path that the list will be restricted to.
    #[arg(short, long)]
    path: Option<PathBuf>,

    /// Set this to list the full raw Metadata info
    #[arg(long)]
    raw: bool,
}

#[derive(Parser, Debug)]
struct WatchArgs {
    /// Optional path that the watch will be restricted to.
    #[arg(short, long)]
    path: Option<PathBuf>,

    /// Interval between scans in seconds (default: 300)
    #[arg(short, long, default_value = "300")]
    interval: u64,
}

pub async fn run<D, M>(args: FileArgs, store: &mut DataStore<D, M>) -> Result<()>
where
    D: DataTable,
    M: MetadataTable,
{
    match args.command {
        Some(FileCommand::Scan(args)) => scan(args, store).await?,
        Some(FileCommand::Init(args)) => init(args, store).await?,
        Some(FileCommand::List(args)) => list(args, store).await?,
        Some(FileCommand::Watch(args)) => watch(args, store).await?,
        None => unimplemented!(),
    }
    Ok(())
}

/// Scan a single file given its current path.
async fn scan_file<D, M>(path: PathBuf, store: &mut DataStore<D, M>) -> Result<Uuid>
where
    D: DataTable,
    M: MetadataTable,
{
    let path = if path.is_absolute() {
        path
    } else {
        path.canonicalize()
            .context("Failed to convert path to absolute.")?
    };

    if !path.exists() {
        bail!("File does not exist on the filesystem")
    }
    if !path.is_file() {
        bail!("Path is not a file")
    }

    let base_path = base_path(store).await?;

    // Get path relative to base_path
    let relative_path = path
        .strip_prefix(base_path)
        .context("Could not get relative path")?;

    let file_metadata = std::fs::metadata(&path).context("Failed to get file metadata")?;
    let file_modified = file_metadata
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_secs();

    let conditions = serde_json::json!({
        "path": relative_path.to_str().unwrap()
    });
    let entry = store
        .get_entries_by_metadata_conditions(conditions)
        .await
        .context("Failed while looking up existing metadata entry.")?;

    let mut parent_id = None;

    let metadata = {
        if entry.len() > 1 {
            // TODO: Handle this case, which we should only hit if this file was changed from multiple sources.
            bail!("Multiple active entries exist for this path")
        } else if entry.is_empty() {
            serde_json::json!({
                "path": relative_path.to_str().unwrap(),
                "size": file_metadata.len(),
                "modified": file_modified,
            })
        } else {
            // Check the filesystem modification time to save on re-hashing
            let parent = &entry[0];
            let parent_modified_time = parent
                .metadata
                .get("modified")
                .and_then(|v| v.as_u64())
                .context("Failed to get modified time from parent metadata")?;

            if file_modified == parent_modified_time {
                // No change, so do nothing
                return Ok(parent.id);
            }

            parent_id = Some(parent.id);
            let mut metadata = parent.metadata.clone();
            metadata["size"] = Value::from(file_metadata.len());
            metadata["modified"] = Value::from(file_modified);
            metadata
        }
    };

    // TODO: Check to see if the same data exists elsewhere?
    // It might be useful to record it as a copy/move,
    // Many use cases may not care about recording the change, so this should be a setting

    store
        .store_data(DataLocation::LocalPath(path), metadata, parent_id)
        .await
}

/// Get the base path from the settings
///
/// This is local to this device, and refers to the user accessible location for the files.
async fn base_path<D, M>(store: &DataStore<D, M>) -> Result<PathBuf>
where
    D: DataTable,
    M: MetadataTable,
{
    let path = store
        .get_setting("base_path")
        .await?
        .context("Base path not found")?;
    Ok(path
        .value
        .as_str()
        .context("Base path is not a string")?
        .into())
}

/// List all stored entries
async fn list<D, M>(_args: ListArgs, store: &mut DataStore<D, M>) -> Result<()>
where
    D: DataTable,
    M: MetadataTable,
{
    let entries = store.get_active_entries().await?;
    for entry in entries.iter() {
        println!(
            "{}: {} - {}",
            entry.metadata.get("path").context("No path in metadata")?,
            entry.id,
            entry.data_hash,
        );
    }
    Ok(())
}

/// Scan a full directory for new/changed files.
///
/// Will only check files that are under the directory, but will not remove files.
async fn scan_directory<D, M>(scan_path: PathBuf, store: &mut DataStore<D, M>) -> Result<()>
where
    D: DataTable,
    M: MetadataTable,
{
    for entry in WalkDir::new(&scan_path)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            scan_file(entry.path().to_path_buf(), store).await?;
        }
    }
    Ok(())
}

/// Do a full scan of the stored file system.
async fn scan<D, M>(args: ScanArgs, store: &mut DataStore<D, M>) -> Result<()>
where
    D: DataTable,
    M: MetadataTable,
{
    // We'll either scan everything or scan a specific path if the user has given it
    let base_path = base_path(store).await?;
    let scan_path = args.path.unwrap_or(base_path.clone());

    // Convert scan_path to an absolute path
    let scan_path = scan_path
        .canonicalize()
        .context("Failed to make the scan path absolute")?;

    // Verify scan_path is within base_path
    if !scan_path.starts_with(&base_path) {
        bail!("Not a valid path, must be under {:?}", base_path)
    }

    if !scan_path.exists() {
        bail!("Path does not exist {:?}", scan_path)
    }

    if scan_path.is_file() {
        // If it's a single file, just scan it
        scan_file(scan_path, store).await?;
        return Ok(());
    }

    if !scan_path.is_dir() {
        bail!("Path is not a file or a directory {:?}", scan_path)
    }

    // It's a directory, so let's scan it
    scan_directory(scan_path.clone(), store).await?;

    // Now we should look at all the active entries that are under this relative path, and see if any have been deleted.
    let relative_path = scan_path
        .strip_prefix(base_path.clone())
        .context(format!("Could not get relative path for {:?}", scan_path))?;

    let conditions = serde_json::json!({
        "path": {
            "$regex": format!("^{}.*", relative_path.to_str().unwrap())
        }
    });

    let entries = store.get_entries_by_metadata_conditions(conditions).await?;
    for entry in entries.iter() {
        if !entry.metadata["path"]
            .as_str()
            .context("Stored path is not a string")?
            .starts_with(relative_path.to_str().unwrap())
        {
            debug!("Scanned entry does not match the regex");
            continue;
        }

        let file_path: PathBuf = entry.metadata["path"]
            .as_str()
            .context("Stored path is not a string")?
            .into();
        // Append file_path to base_path
        let file_path = base_path.join(file_path);
        if !file_path.exists() {
            store
                .archive(entry.id)
                .await
                .context(format!("Failed to archive {:?}", file_path))?;
        }
    }

    Ok(())
}

/// Watch a directory for changes by scanning periodically
async fn watch<D, M>(args: WatchArgs, store: &mut DataStore<D, M>) -> Result<()>
where
    D: DataTable,
    M: MetadataTable,
{
    println!("Starting file watch with {}s interval...", args.interval);
    loop {
        let scan_args = ScanArgs {
            path: args.path.clone(),
        };
        scan(scan_args, store).await?;
        println!("Scan complete.");

        thread::sleep(Duration::from_secs(args.interval));
    }
}

/// Initialize the file plugin to a base directory
///
/// The file plugin needs a base path to index all the files.
async fn init<D, M>(args: InitArgs, store: &mut DataStore<D, M>) -> Result<()>
where
    D: DataTable,
    M: MetadataTable,
{
    let path = args
        .path
        .canonicalize()
        .context("Failed to get absolute path")?;

    let mut path_setting = store.get_setting("base_path").await?.unwrap_or(Setting {
        key: "base_path".to_string(),
        value: serde_json::Value::Null,
        description: None,
    });

    if path_setting.value != serde_json::Value::Null
        && path_setting.value != serde_json::to_value(path.to_str())?
    {
        bail!("Base Path already set to {}", path_setting.value)
    }

    if !path.exists() {
        bail!("Base Path does not exist")
    }
    if !path.is_dir() {
        bail!("Base Path must be a directory")
    }

    path_setting.value = serde_json::to_value(path.to_str().unwrap())?;
    store.set_setting(path_setting).await
}
