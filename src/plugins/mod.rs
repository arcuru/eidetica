use anyhow::Result;
use clap::Subcommand;

use crate::datastore::data::DataTable;
use crate::datastore::metadata::MetadataTable;
use crate::datastore::store::DataStore;
use crate::datastore::stream_table::StreamTable;

pub mod file;

#[derive(Subcommand, Debug)]
pub enum PluginArgs {
    /// File plugin
    File(file::FileArgs),
}

pub async fn run<D, M, S>(args: PluginArgs, store: &mut DataStore<D, M, S>) -> Result<()>
where
    D: DataTable,
    M: MetadataTable,
    S: StreamTable,
{
    match args {
        PluginArgs::File(args) => file::run(args, store).await?,
    }
    Ok(())
}
