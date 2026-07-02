//! HTTP server for prometheus metrics and profiling endpoints.

use std::{net::SocketAddr, sync::Arc};

use axum::{
    Router,
    extract::State,
    response::{Html, IntoResponse},
    routing::get,
};
use metrics_exporter_prometheus::PrometheusHandle;
use tower::ServiceBuilder;
use tower_http::trace::TraceLayer;
use vertex_tasks::TaskExecutor;

use super::Hooks;
use super::task::spawn_server_task;
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
        Self {
            addr,
            handle,
            hooks,
        }
    }

    /// Create from configuration.
    pub fn from_config(
        config: &MetricsServerConfig,
        handle: PrometheusHandle,
        hooks: Hooks,
    ) -> Self {
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
            .route("/health", get(health_handler));

        // The unauthenticated debug routes mount only in a build carrying a
        // profiling capability. A default build never exposes them, so an
        // operator scraping /metrics cannot be reached through pprof (a
        // blocking-pool exhaustion lever) or the allocator-stats fingerprint;
        // within a profiling build the pprof duration stays bounded and
        // single-flight, and the default bind stays on localhost regardless.
        #[cfg(any(feature = "profiling", feature = "jemalloc"))]
        let app = app
            .route("/debug/pprof/profile", get(debug_routes::pprof_handler))
            .route("/debug/memory", get(debug_routes::memory_handler))
            .route("/debug/heap/dump", get(debug_routes::heap_dump_handler));

        let app = app
            .with_state(shared_state)
            .layer(ServiceBuilder::new().layer(TraceLayer::new_for_http()));

        let listener = tokio::net::TcpListener::bind(self.addr).await?;
        let addr = listener.local_addr()?;
        tracing::info!("Metrics server listening on {addr}");

        spawn_server_task(executor, listener, app);

        Ok(())
    }
}

#[derive(Debug, Clone)]
struct ServerState {
    handle: PrometheusHandle,
    hooks: Hooks,
}

async fn root() -> Html<&'static str> {
    // The debug endpoints are advertised only when they are mounted (a
    // profiling build), so the index never leaks routes that are not served.
    // The two pages are identical apart from the debug list block.
    #[cfg(any(feature = "profiling", feature = "jemalloc"))]
    let page = r#"<!DOCTYPE html>
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
</html>"#;
    #[cfg(not(any(feature = "profiling", feature = "jemalloc")))]
    let page = r#"<!DOCTYPE html>
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
</html>"#;

    Html(page)
}

async fn metrics_handler(State(state): State<Arc<ServerState>>) -> impl IntoResponse {
    state.hooks.execute_all();
    state.handle.render()
}

async fn health_handler() -> impl IntoResponse {
    "OK"
}

/// The unauthenticated profiling and allocator-inspection endpoints. Compiled
/// only for a build that carries a profiling capability, so a default build
/// neither mounts nor references them.
#[cfg(any(feature = "profiling", feature = "jemalloc"))]
mod debug_routes {
    use std::time::Duration;

    use axum::{
        Json,
        extract::Query,
        http::{StatusCode, header},
        response::{IntoResponse, Response},
    };
    use serde::Deserialize;

    use crate::profiling;

    /// Query parameters for CPU profile endpoint.
    #[derive(Debug, Deserialize)]
    pub(super) struct ProfileParams {
        /// Profile duration in seconds (default: 30, capped at [`MAX_PROFILE_SECONDS`]).
        #[serde(default = "default_profile_seconds")]
        seconds: u64,
    }

    fn default_profile_seconds() -> u64 {
        30
    }

    /// Upper bound on a requested profile duration. A caller-controlled duration
    /// parks a blocking-pool thread for its whole length, so an unbounded value is
    /// a denial-of-service lever; clamp it.
    const MAX_PROFILE_SECONDS: u64 = 60;

    /// Clamp a requested profile duration into `[1, MAX_PROFILE_SECONDS]`.
    fn clamp_profile_seconds(requested: u64) -> u64 {
        requested.clamp(1, MAX_PROFILE_SECONDS)
    }

    /// One CPU profile at a time. The profiler is process-global, so concurrent
    /// runs would collide and each parks a blocking thread; a second request is
    /// refused rather than queued.
    static PROFILE_IN_FLIGHT: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// Releases the single-flight guard on drop so an early return or a panic in the
    /// blocking task cannot wedge the endpoint.
    struct ProfileGuard;

    impl Drop for ProfileGuard {
        fn drop(&mut self) {
            PROFILE_IN_FLIGHT.store(false, std::sync::atomic::Ordering::Release);
        }
    }

    /// CPU profile endpoint - generates flamegraph SVG.
    pub(super) async fn pprof_handler(Query(params): Query<ProfileParams>) -> Response {
        if !profiling::cpu_profiling_available() {
            return (
                StatusCode::NOT_IMPLEMENTED,
                "CPU profiling requires the 'profiling' feature",
            )
                .into_response();
        }

        if PROFILE_IN_FLIGHT
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                "A CPU profile is already running; retry when it completes",
            )
                .into_response();
        }
        let _guard = ProfileGuard;

        let duration = Duration::from_secs(clamp_profile_seconds(params.seconds));

        // Run profiling in a blocking task to avoid blocking the async runtime
        let result = tokio::task::spawn_blocking(move || profiling::cpu_profile(duration)).await;

        match result {
            Ok(Ok(svg)) => ([(header::CONTENT_TYPE, "image/svg+xml")], svg).into_response(),
            Ok(Err(e)) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Profiling failed: {e}"),
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Task failed: {e}"),
            )
                .into_response(),
        }
    }

    /// Memory stats endpoint - returns JSON with allocator statistics.
    pub(super) async fn memory_handler() -> Response {
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
    pub(super) async fn heap_dump_handler() -> Response {
        if !profiling::heap_profiling_available() {
            return (
                StatusCode::NOT_IMPLEMENTED,
                "Heap profiling requires the 'heap-profiling' feature. \
             Build with: cargo build --release --features heap-profiling",
            )
                .into_response();
        }

        let timestamp = vertex_util_runtime::time::now_unix_secs();
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

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn profile_duration_is_clamped_to_the_bound() {
            assert_eq!(clamp_profile_seconds(0), 1, "a zero duration floors to one");
            assert_eq!(
                clamp_profile_seconds(30),
                30,
                "an in-range duration is kept"
            );
            assert_eq!(
                clamp_profile_seconds(u64::MAX),
                MAX_PROFILE_SECONDS,
                "an unbounded duration is capped, so pprof cannot park a blocking thread for longer"
            );
        }
    }
} // mod debug_routes

#[cfg(test)]
mod tests {
    use super::*;

    /// A default (non-profiling) build must not advertise the debug endpoints on
    /// the index, so an operator serving /metrics leaks no unauthenticated
    /// routes. The route registration is gated by the same cfg, so absence on
    /// the index mirrors absence on the listener.
    #[cfg(not(any(feature = "profiling", feature = "jemalloc")))]
    #[tokio::test]
    async fn default_build_index_advertises_no_debug_routes() {
        let Html(page) = root().await;
        assert!(!page.contains("/debug/"), "index leaks a debug route");
        assert!(page.contains("/metrics"), "index still serves metrics");
    }

    /// A profiling build advertises the debug endpoints, confirming the gate
    /// selects the populated page rather than always hiding them.
    #[cfg(any(feature = "profiling", feature = "jemalloc"))]
    #[tokio::test]
    async fn profiling_build_index_advertises_debug_routes() {
        let Html(page) = root().await;
        assert!(
            page.contains("/debug/pprof/profile"),
            "profiling index omits pprof"
        );
    }
}
