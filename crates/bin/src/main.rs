use clap::Parser;

mod backend;
mod cli;
mod commands;
mod session;
mod templates;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Health(ref args)) => commands::health::run(args).await,
        Some(Commands::Serve(ref args)) => commands::serve::run(args).await,
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
