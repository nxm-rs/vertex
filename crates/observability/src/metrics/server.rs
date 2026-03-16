//! HTTP server for prometheus metrics and profiling endpoints.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Json,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use metrics_exporter_prometheus::PrometheusHandle;
use serde::Deserialize;
use tower::ServiceBuilder;
use tower_http::trace::TraceLayer;
use vertex_tasks::TaskExecutor;

use super::Hooks;
use crate::MetricsServerConfig;
use crate::profiling;

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
            .route("/debug/pprof/profile", get(pprof_handler))
            .route("/debug/memory", get(memory_handler))
            .route("/debug/heap/dump", get(heap_dump_handler))
            .with_state(shared_state)
            .layer(ServiceBuilder::new().layer(TraceLayer::new_for_http()));

        let listener = tokio::net::TcpListener::bind(self.addr).await?;
        let addr = listener.local_addr()?;
        tracing::info!("Metrics server listening on {addr}");

        // Use the executor's shutdown signal for graceful shutdown
        executor.spawn_critical_with_graceful_shutdown_signal("metrics.server", move |shutdown| async move {
            tracing::debug!("Metrics server task started, beginning to serve");
            let server = axum::serve(listener, app).with_graceful_shutdown(shutdown.ignore_guard());

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
        <li><a href="/debug/pprof/profile?seconds=30">CPU Profile (flamegraph)</a></li>
        <li><a href="/debug/memory">Memory Stats (JSON)</a></li>
        <li><a href="/debug/heap/dump">Heap Profile Dump</a> (requires <code>heap-profiling</code> feature + <code>MALLOC_CONF=prof:true</code>)</li>
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

/// Query parameters for CPU profile endpoint.
#[derive(Debug, Deserialize)]
struct ProfileParams {
    /// Profile duration in seconds (default: 30).
    #[serde(default = "default_profile_seconds")]
    seconds: u64,
}

fn default_profile_seconds() -> u64 {
    30
}

/// CPU profile endpoint - generates flamegraph SVG.
async fn pprof_handler(Query(params): Query<ProfileParams>) -> Response {
    if !profiling::cpu_profiling_available() {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "CPU profiling requires the 'profiling' feature",
        )
            .into_response();
    }

    let duration = Duration::from_secs(params.seconds);

    // Run profiling in a blocking task to avoid blocking the async runtime
    let result = tokio::task::spawn_blocking(move || profiling::cpu_profile(duration)).await;

    match result {
        Ok(Ok(svg)) => ([(header::CONTENT_TYPE, "image/svg+xml")], svg).into_response(),
        Ok(Err(e)) => {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Profiling failed: {e}")).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Task failed: {e}")).into_response()
        }
    }
}

/// Memory stats endpoint - returns JSON with allocator statistics.
async fn memory_handler() -> Response {
    match profiling::memory_stats() {
        Ok(stats) => Json(stats).into_response(),
        Err(e) => (StatusCode::NOT_IMPLEMENTED, e.to_string()).into_response(),
    }
}

/// Heap profile dump endpoint.
///
/// Dumps a jemalloc heap profile to /tmp and returns the path.
/// Requires: `--features heap-profiling` AND `MALLOC_CONF=prof:true`.
/// Analyze with: `jeprof --svg /path/to/vertex /tmp/vertex_heap_<ts>.heap > heap.svg`
async fn heap_dump_handler() -> Response {
    if !profiling::heap_profiling_available() {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "Heap profiling requires the 'heap-profiling' feature. \
             Build with: cargo build --release --features heap-profiling",
        )
            .into_response();
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let path = std::path::PathBuf::from(format!("/tmp/vertex_heap_{timestamp}.heap"));

    match profiling::heap_dump(&path) {
        Ok(()) => {
            let msg = format!(
                "Heap profile dumped to: {}\n\n\
                 Analyze with:\n  \
                 jeprof --svg /path/to/vertex {} > heap.svg\n  \
                 jeprof --text /path/to/vertex {}\n",
                path.display(),
                path.display(),
                path.display(),
            );
            (StatusCode::OK, msg).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
