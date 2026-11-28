use clap::Parser;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    eidetica_bin::dashboard::run(eidetica_bin::dashboard::Args::parse()).await
}
