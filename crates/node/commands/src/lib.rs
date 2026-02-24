//! Generic CLI infrastructure for Vertex nodes.

pub use vertex_node_builder::LaunchContext;
pub use vertex_node_core::args::{InfraArgs, LogArgs, TracingArgs};

use std::future::Future;
use std::time::Duration;

use clap::Parser;
use color_eyre::eyre;
use tracing::{error, info, warn};
use vertex_node_core::version;
use vertex_observability::VertexTracer;
use vertex_tasks::{TaskExecutor, TaskManager};

/// Timeout for graceful shutdown before forcing exit.
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// CLI types with logging configuration.
pub trait HasLogs {
    fn logs(&self) -> &LogArgs;
}

/// CLI types with tracing configuration.
pub trait HasTracing {
    fn tracing(&self) -> &TracingArgs;
}

/// Run a CLI with error handling, logging, task management, and graceful shutdown.
pub async fn run_cli<C, F, Fut>(runner: F) -> eyre::Result<()>
where
    C: Parser + HasLogs + HasTracing,
    F: FnOnce(C) -> Fut,
    Fut: Future<Output = eyre::Result<()>>,
{
    color_eyre::install()?;
    let cli = C::parse();

    // Build tracer from CLI args
    let mut tracer = VertexTracer::new();

    if let Some(stdout) = cli.logs().stdout_config() {
        tracer = tracer.with_stdout(stdout);
    }
    if let Some(file) = cli.logs().file_config_from_args() {
        tracer = tracer.with_file(file);
    }
    if let Some(otlp) = cli.tracing().otlp_config() {
        tracer = tracer.with_otlp(otlp);
    }
    if let Some(otlp_logs) = cli.tracing().otlp_logs_config() {
        tracer = tracer.with_otlp_logs(otlp_logs);
    }

    let _guard = tracer.init()?;
    info!("Starting Vertex {}", version::VERSION);

    // TaskManager must stay alive - dropping it fires shutdown signal to all tasks.
    // We spawn it as a separate task so it stays alive when other select! branches complete.
    // This allows initiate_graceful_shutdown() to send the event while TaskManager is still running.
    let task_manager = TaskManager::current();
    let executor = TaskExecutor::current();
    let manager_handle = tokio::spawn(task_manager);

    tokio::select! {
        result = manager_handle => {
            // TaskManager completed - either graceful shutdown or critical task panic
            match result {
                Ok(Ok(())) => {
                    info!("Shutdown complete");
                    Ok(())
                }
                Ok(Err(e)) => {
                    error!("Critical task panicked: {}", e);
                    Err(eyre::eyre!("Critical task panicked: {}", e))
                }
                Err(e) => {
                    error!("TaskManager task panicked: {}", e);
                    Err(eyre::eyre!("TaskManager task panicked: {}", e))
                }
            }
        }
        result = runner(cli) => {
            // Runner completed - initiate graceful shutdown
            if let Err(e) = executor.initiate_graceful_shutdown() {
                warn!("Failed to initiate graceful shutdown after runner completed: {}", e);
            }
            result
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Received Ctrl+C, initiating graceful shutdown...");
            match executor.initiate_graceful_shutdown() {
                Ok(graceful_shutdown) => {
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
