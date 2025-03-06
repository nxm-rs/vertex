
//! Vertex Swarm node executable
//!
//! This is the main entry point for the Vertex Swarm node.

mod cli;
mod commands;
mod config;
mod constants;
mod dirs;
mod logging;
mod node;
mod utils;
mod version;

use crate::cli::Cli;
use clap::Parser;
use color_eyre::eyre;
use tracing::info;

/// Main entry point for the Vertex Swarm node.
#[tokio::main]
async fn main() -> eyre::Result<()> {
    // Setup error handling
    color_eyre::install()?;

    // Parse command line arguments
    let cli = Cli::parse();

    // Initialize logging
    let _guard = logging::init_logging(&cli.log_args)?;

    // Print version information
    if cli.version {
        println!("{}", version::LONG_VERSION);
        return Ok(());
    }

    info!("Starting Vertex Swarm {}", version::VERSION);

    // Dispatch command
    match cli.command {
        cli::Commands::Node(args) => {
            commands::node::run(args).await?;
        }
        cli::Commands::Dev(args) => {
            commands::dev::run(args).await?;
        }
        cli::Commands::Info(args) => {
            commands::info::run(args).await?;
        }
        cli::Commands::Config(args) => {
            commands::config::run(args).await?;
        }
    }

    Ok(())
}
