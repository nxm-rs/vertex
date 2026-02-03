//! HTTP server for metrics

use crate::{Hooks, PrometheusRecorder};
use axum::{
    extract::State,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use metrics_exporter_prometheus::PrometheusHandle;
use std::{net::SocketAddr, sync::Arc};

use parking_lot::Mutex;
use tokio::sync::oneshot;
use tower::ServiceBuilder;
use tower_http::trace::TraceLayer;

/// Metrics server that exposes a prometheus endpoint
#[derive(Debug)]
pub struct MetricsServer {
    /// Server address
    addr: SocketAddr,
    /// Prometheus handle
    handle: PrometheusHandle,
    /// Metrics hooks
    hooks: Hooks,
    /// Shutdown signal
    shutdown_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl MetricsServer {
    /// Create a new metrics server
    pub fn new(
        addr: SocketAddr,
        handle: PrometheusHandle,
        hooks: Hooks,
    ) -> Self {
        Self {
            addr,
            handle,
            hooks,
            shutdown_tx: Arc::new(Mutex::new(None)),
        }
    }

    /// Start the metrics server
    pub async fn start(&self) -> eyre::Result<()> {
        let shared_state = Arc::new(ServerState {
            handle: self.handle.clone(),
            hooks: self.hooks.clone(),
        });

        // Build our application with a route
        let app = Router::new()
            .route("/", get(root))
            .route("/metrics", get(metrics_handler))
            .route("/health", get(health_handler))
            .with_state(shared_state)
            .layer(ServiceBuilder::new().layer(TraceLayer::new_for_http()));

        // Create a shutdown channel
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        {
            let mut lock = self.shutdown_tx.lock();
            *lock = Some(shutdown_tx);
        }

        // Start the server
        let server = axum::Server::bind(&self.addr)
            .serve(app.into_make_service())
            .with_graceful_shutdown(async {
                shutdown_rx.await.ok();
            });

        tracing::info!("Metrics server listening on {}", self.addr);

        // Spawn the server task
        tokio::spawn(async move {
            if let Err(err) = server.await {
                tracing::error!("Metrics server error: {}", err);
            }
        });

        Ok(())
    }

    /// Shutdown the metrics server
    pub async fn shutdown(&self) -> eyre::Result<()> {
        let tx = {
            let mut lock = self.shutdown_tx.lock();
            lock.take()
        };

        if let Some(tx) = tx {
            let _ = tx.send(());
            tracing::info!("Metrics server shutdown signal sent");
        }

        Ok(())
    }
}

/// Shared state for the metrics server
#[derive(Debug, Clone)]
struct ServerState {
    /// Prometheus handle
    handle: PrometheusHandle,
    /// Metrics hooks
    hooks: Hooks,
}

/// Root endpoint handler
async fn root() -> Html<&'static str> {
    Html(
        r#"
        <!DOCTYPE html>
        <html>
            <head>
                <title>Vertex Swarm Metrics</title>
                <style>
                    body {
                        font-family: sans-serif;
                        max-width: 800px;
                        margin: 0 auto;
                        padding: 2em;
                    }
                    h1 {
                        color: #333;
                    }
                    a {
                        color: #0066cc;
                        text-decoration: none;
                    }
                    a:hover {
                        text-decoration: underline;
                    }
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
        </html>
        "#,
    )
}

/// Metrics endpoint handler
async fn metrics_handler(State(state): State<Arc<ServerState>>) -> impl IntoResponse {
    // Execute all hooks to refresh metrics
    state.hooks.execute_all();

    // Render prometheus metrics
    state.handle.render()
}

/// Health check endpoint handler
async fn health_handler() -> impl IntoResponse {
    "OK"
}
