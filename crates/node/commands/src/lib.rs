//! CLI commands for Vertex Swarm node.
//!
//! This crate provides the command-line interface:
//! - [`Cli`] - Top-level CLI parser
//! - [`Commands`] - Available subcommands
//! - [`NodeArgs`] - Combined arguments for the node command
//!
//! Configuration is loaded using Figment with the following priority
//! (highest wins):
//!
//! 1. CLI arguments
//! 2. Config file (TOML)
//! 3. Environment variables (`VERTEX_` prefix)
//! 4. Defaults

mod cli;
pub mod commands;
pub mod config;

pub use cli::{Cli, Commands, NodeArgs, SwarmNodeType};
pub use config::NodeConfig;

use clap::Parser;
use color_eyre::eyre;
use tracing::info;
use vertex_node_core::{logging, version};

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
        Commands::Node(args) => {
            commands::node::run(args).await?;
        }
    }

    Ok(())
}
