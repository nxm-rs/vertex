//! HTTP server for prometheus metrics endpoint.

use axum::{
    extract::State,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use metrics_exporter_prometheus::PrometheusHandle;
use std::{net::SocketAddr, sync::Arc};
use tower::ServiceBuilder;
use tower_http::trace::TraceLayer;
use vertex_tasks::TaskExecutor;

use super::Hooks;
use crate::MetricsServerConfig;

/// Metrics server exposing a prometheus endpoint.
#[derive(Debug)]
pub struct MetricsServer {
    addr: SocketAddr,
    handle: PrometheusHandle,
    hooks: Hooks,
}

impl MetricsServer {
    pub fn new(addr: SocketAddr, handle: PrometheusHandle, hooks: Hooks) -> Self {
        Self { addr, handle, hooks }
    }

    /// Create from configuration.
    pub fn from_config(config: &MetricsServerConfig, handle: PrometheusHandle, hooks: Hooks) -> Self {
        Self::new(config.addr(), handle, hooks)
    }

    /// Start the metrics server using the provided TaskExecutor.
    ///
    /// The server runs until the executor's shutdown signal fires.
    pub async fn start(self, executor: &TaskExecutor) -> eyre::Result<()> {
        let shared_state = Arc::new(ServerState {
            handle: self.handle,
            hooks: self.hooks,
        });

        let app = Router::new()
            .route("/", get(root))
            .route("/metrics", get(metrics_handler))
            .route("/health", get(health_handler))
            .with_state(shared_state)
            .layer(ServiceBuilder::new().layer(TraceLayer::new_for_http()));

        let listener = tokio::net::TcpListener::bind(self.addr).await?;
        let addr = listener.local_addr()?;
        tracing::info!("Metrics server listening on {addr}");

        // Use the executor's shutdown signal for graceful shutdown
        executor.spawn_critical_with_shutdown_signal("metrics_server", move |shutdown| async move {
            tracing::debug!("Metrics server task started, beginning to serve");
            let server = axum::serve(listener, app).with_graceful_shutdown(async move {
                shutdown.await;
                tracing::debug!("Metrics server shutdown signal received");
            });

            match server.await {
                Ok(()) => tracing::debug!("Metrics server stopped gracefully"),
                Err(err) => tracing::error!("Metrics server error: {err}"),
            }
            tracing::debug!("Metrics server task exiting");
        });

        Ok(())
    }
}

#[derive(Debug, Clone)]
struct ServerState {
    handle: PrometheusHandle,
    hooks: Hooks,
}

async fn root() -> Html<&'static str> {
    Html(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Vertex Swarm Metrics</title>
    <style>
        body { font-family: sans-serif; max-width: 800px; margin: 0 auto; padding: 2em; }
        h1 { color: #333; }
        a { color: #0066cc; text-decoration: none; }
        a:hover { text-decoration: underline; }
    </style>
</head>
<body>
    <h1>Vertex Swarm Metrics</h1>
    <p>Available endpoints:</p>
    <ul>
        <li><a href="/metrics">Prometheus Metrics</a></li>
        <li><a href="/health">Health Check</a></li>
    </ul>
</body>
</html>"#,
    )
}

async fn metrics_handler(State(state): State<Arc<ServerState>>) -> impl IntoResponse {
    state.hooks.execute_all();
    state.handle.render()
}

async fn health_handler() -> impl IntoResponse {
    "OK"
}
