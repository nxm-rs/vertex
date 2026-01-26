//! Generic CLI infrastructure for Vertex nodes.
//!
//! This crate provides the generic command-line interface infrastructure:
//! - [`LaunchContext`] - Generic infrastructure context (executor, dirs)
//! - [`run_cli`] - Generic CLI runner that handles logging, error handling, etc.
//!
//! Protocol-specific CLIs (like Swarm) build on this foundation using [`run_cli`].
//!
//! # For Protocol Implementors
//!
//! Protocol crates define their own CLI struct with logging args and use [`run_cli`]:
//!
//! ```ignore
//! use vertex_node_commands::{run_cli, LogArgs, HasLogs};
//! use clap::Parser;
//!
//! #[derive(Parser)]
//! struct MyCli {
//!     #[command(flatten)]
//!     logs: LogArgs,
//!     // ... protocol-specific args
//! }
//!
//! impl HasLogs for MyCli {
//!     fn logs(&self) -> &LogArgs { &self.logs }
//! }
//!
//! async fn main() -> eyre::Result<()> {
//!     run_cli(|cli: MyCli| async move {
//!         // Build your context and run your node
//!         Ok(())
//!     }).await
//! }
//! ```
//!
//! See `vertex-swarm-node` for a full example.

// Re-export LaunchContext from node-builder
pub use vertex_node_builder::LaunchContext;

// Re-export LogArgs and InfraArgs for protocol CLI definitions
pub use vertex_node_core::args::{InfraArgs, LogArgs};

use std::future::Future;

use clap::Parser;
use color_eyre::eyre;
use tracing::info;
use vertex_node_core::{logging, version};

/// Trait for CLI types that have logging configuration.
pub trait HasLogs {
    /// Get the logging configuration.
    fn logs(&self) -> &LogArgs;
}

/// Run a CLI with generic error handling and logging setup.
///
/// This handles the boilerplate that all node binaries need:
/// - Error handling setup (color_eyre)
/// - Logging initialization
/// - Version banner
///
/// The closure receives the parsed CLI and can build protocol-specific
/// contexts and run the node.
///
/// # Example
///
/// ```ignore
/// use vertex_node_commands::{run_cli, LogArgs, HasLogs};
/// use clap::Parser;
///
/// #[derive(Parser)]
/// struct MyCli {
///     #[command(flatten)]
///     logs: LogArgs,
///     // ... other args
/// }
///
/// impl HasLogs for MyCli {
///     fn logs(&self) -> &LogArgs { &self.logs }
/// }
///
/// #[tokio::main]
/// async fn main() -> eyre::Result<()> {
///     run_cli(|cli: MyCli| async move {
///         // Your node logic here
///         Ok(())
///     }).await
/// }
/// ```
pub async fn run_cli<C, F, Fut>(runner: F) -> eyre::Result<()>
where
    C: Parser + HasLogs,
    F: FnOnce(C) -> Fut,
    Fut: Future<Output = eyre::Result<()>>,
{
    // Setup error handling
    color_eyre::install()?;

    // Parse command line arguments
    let cli = C::parse();

    // Initialize logging
    logging::init_logging(cli.logs())?;

    info!("Starting Vertex {}", version::VERSION);

    // Call user's runner with parsed CLI
    runner(cli).await
}
