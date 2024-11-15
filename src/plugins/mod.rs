use anyhow::Result;
use clap::Subcommand;

use crate::datastore::data::DataTable;
use crate::datastore::metadata::MetadataTable;
use crate::datastore::store::DataStore;

pub mod file;

#[derive(Subcommand, Debug)]
pub enum PluginArgs {
    /// File plugin
    File(file::FileArgs),
}

pub async fn run<D, M>(args: PluginArgs, store: &mut DataStore<D, M>) -> Result<()>
where
    D: DataTable,
    M: MetadataTable,
{
    match args {
        PluginArgs::File(args) => file::run(args, store).await?,
    }
    Ok(())
}
