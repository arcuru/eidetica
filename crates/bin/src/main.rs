use clap::Parser;

mod backend;
mod cli;
mod commands;
mod output;
mod session;
mod templates;

use cli::{Cli, Commands, DbCommands};
use output::OutputFormat;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let format = if cli.json {
        OutputFormat::Json
    } else {
        OutputFormat::Human
    };

    match cli.command {
        Some(Commands::Health(ref args)) => commands::health::run(args).await,
        Some(Commands::Serve(ref args)) => commands::serve::run(args).await,
        Some(Commands::Info(ref args)) => commands::info::run(args, format).await,
        Some(Commands::Db {
            command: DbCommands::List(ref args),
        }) => commands::db::list(args, format).await,
        None => {
            // Default to serve with default args for backward compatibility
            let serve_cli = Cli::parse_from(["eidetica", "serve"]);
            if let Some(Commands::Serve(ref args)) = serve_cli.command {
                commands::serve::run(args).await
            } else {
                unreachable!()
            }
        }
    }
}
