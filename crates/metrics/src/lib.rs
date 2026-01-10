//! Metrics and observability for Vertex Swarm
//!
//! This crate provides metrics collection, tracing, and logging for the Vertex Swarm node.

use std::sync::Arc;

mod config;
mod hooks;
mod logging;
mod prometheus;
mod recorder;
mod server;
mod tracing;

pub use config::*;
pub use hooks::*;
pub use logging::*;
pub use prometheus::*;
pub use recorder::*;
pub use server::*;
pub use tracing::*;

/// Re-export metrics crate for convenience
pub use metrics;

/// Main entry point for configuring and initializing metrics
#[derive(Debug, Clone)]
pub struct MetricsSystem {
    /// Configuration for metrics
    config: MetricsConfig,
    /// Recorder handle for prometheus
    prometheus_recorder: Option<Arc<PrometheusRecorder>>,
    /// The metrics server handle
    server: Option<Arc<MetricsServer>>,
    /// Metrics hooks
    hooks: Hooks,
}

impl MetricsSystem {
    /// Create a new metrics system with the given configuration
    pub fn new(config: MetricsConfig) -> Self {
        Self {
            config,
            prometheus_recorder: None,
            server: None,
            hooks: Hooks::default(),
        }
    }

    /// Build and start the metrics system
    pub async fn start(mut self) -> eyre::Result<Self> {
        // Initialize logging
        if self.config.logging.enabled {
            initialize_logging(&self.config.logging)?;
        }

        // Initialize tracing
        if self.config.tracing.enabled {
            initialize_tracing(&self.config.tracing)?;
        }

        // Initialize metrics
        if self.config.prometheus.enabled {
            let recorder = install_prometheus_recorder();
            self.prometheus_recorder = Some(Arc::new(recorder));

            // Start the metrics server if configured
            if let Some(addr) = self.config.prometheus.endpoint {
                let server = MetricsServer::new(
                    addr,
                    recorder.handle().clone(),
                    self.hooks.clone(),
                );
                server.start().await?;
                self.server = Some(Arc::new(server));
            }
        }

        Ok(self)
    }

    /// Add a metrics hook
    pub fn with_hook<F>(mut self, hook: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.hooks = self.hooks.with_hook(hook);
        self
    }

    /// Get the prometheus handle for recording metrics
    pub fn prometheus_handle(&self) -> Option<&metrics_exporter_prometheus::PrometheusHandle> {
        self.prometheus_recorder.as_ref().map(|r| r.handle())
    }

    /// Shut down the metrics system
    pub async fn shutdown(self) -> eyre::Result<()> {
        if let Some(server) = self.server {
            server.shutdown().await?;
        }
        Ok(())
    }
}
