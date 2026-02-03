//! Generic CLI infrastructure for Vertex nodes.

pub use vertex_node_builder::LaunchContext;
pub use vertex_node_core::args::{InfraArgs, LogArgs};

use std::future::Future;
use std::time::Duration;

use clap::Parser;
use color_eyre::eyre;
use tracing::{error, info, warn};
use vertex_node_core::{logging, version};
use vertex_tasks::{TaskExecutor, TaskManager};

/// Timeout for graceful shutdown before forcing exit.
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// CLI types with logging configuration.
pub trait HasLogs {
    fn logs(&self) -> &LogArgs;
}

/// Run a CLI with error handling, logging, task management, and graceful shutdown.
pub async fn run_cli<C, F, Fut>(runner: F) -> eyre::Result<()>
where
    C: Parser + HasLogs,
    F: FnOnce(C) -> Fut,
    Fut: Future<Output = eyre::Result<()>>,
{
    color_eyre::install()?;
    let cli = C::parse();
    logging::init_logging(cli.logs())?;
    info!("Starting Vertex {}", version::VERSION);

    // TaskManager must stay alive - dropping it fires shutdown signal to all tasks.
    // Awaiting it in select! keeps it alive and notifies us if a critical task panics.
    let task_manager = TaskManager::current();
    let executor = TaskExecutor::current();

    tokio::select! {
        result = task_manager => {
            match result {
                Ok(()) => {
                    info!("Shutdown complete");
                    Ok(())
                }
                Err(e) => {
                    error!("Critical task panicked: {}", e);
                    Err(eyre::eyre!("Critical task panicked: {}", e))
                }
            }
        }
        result = runner(cli) => {
            result
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Received Ctrl+C, initiating graceful shutdown...");
            match executor.initiate_graceful_shutdown() {
                Ok(graceful_shutdown) => {
                    // Guard must be held until shutdown completes
                    match tokio::time::timeout(GRACEFUL_SHUTDOWN_TIMEOUT, graceful_shutdown).await {
                        Ok(_guard) => info!("Graceful shutdown complete"),
                        Err(_) => warn!(
                            "Graceful shutdown timed out after {:?}, forcing exit",
                            GRACEFUL_SHUTDOWN_TIMEOUT
                        ),
                    }
                }
                Err(e) => warn!("Failed to initiate graceful shutdown: {}", e),
            }
            Ok(())
        }
    }
}
