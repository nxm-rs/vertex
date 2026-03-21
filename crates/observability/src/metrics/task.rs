//! Background tasks for metrics infrastructure.

use axum::Router;
use metrics_exporter_prometheus::PrometheusHandle;
use tokio::net::TcpListener;
use vertex_tasks::TaskExecutor;

/// Spawn the metrics HTTP server as a critical task with graceful shutdown.
pub(super) fn spawn_server_task(executor: &TaskExecutor, listener: TcpListener, app: Router) {
    executor.spawn_critical_with_graceful_shutdown_signal(
        "metrics.server",
        move |shutdown| async move {
            tracing::debug!("Metrics server task started, beginning to serve");
            let server = axum::serve(listener, app).with_graceful_shutdown(shutdown.ignore_guard());

            match server.await {
                Ok(()) => tracing::debug!("Metrics server stopped gracefully"),
                Err(err) => tracing::error!("Metrics server error: {err}"),
            }
            tracing::debug!("Metrics server task exiting");
        },
    );
}

/// Spawn the metrics upkeep task that periodically flushes idle/expired metrics.
pub(super) fn spawn_upkeep_task(
    executor: &TaskExecutor,
    handle: PrometheusHandle,
    interval_secs: u64,
) {
    executor.spawn_periodic(
        "metrics.upkeep",
        std::time::Duration::from_secs(interval_secs),
        move || {
            handle.run_upkeep();
        },
    );
}
