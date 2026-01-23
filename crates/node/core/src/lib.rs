//! Vertex Swarm node library
//!
//! This crate provides the core node functionality for Vertex Swarm.
//! It is used by the `vertex` binary to run the node.

pub mod availability;
pub mod builder;
pub mod cli;
pub mod commands;
pub mod config;
pub mod constants;
pub mod dirs;
pub mod logging;
pub mod version;

use crate::cli::Cli;
use clap::Parser;
use color_eyre::eyre;
use tracing::info;

/// Run the Vertex node with the given CLI arguments.
///
/// This is the main entry point that should be called from the binary.
pub async fn run() -> eyre::Result<()> {
    // Setup error handling
    color_eyre::install()?;

    // Parse command line arguments
    let cli = Cli::parse();

    // Initialize logging
    logging::init_logging(&cli.logs)?;

    info!("Starting Vertex Swarm {}", version::VERSION);

    // Dispatch command
    match cli.command {
        cli::Commands::Node(args) => {
            commands::node::run(args).await?;
        }
    }

    Ok(())
}
