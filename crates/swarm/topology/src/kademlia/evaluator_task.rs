//! Background task for Kademlia connection evaluation.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tracing::debug;
use vertex_swarm_api::SwarmIdentity;

use super::routing::KademliaRouting;

/// Handle for triggering evaluation from the topology behaviour.
pub(crate) struct RoutingEvaluatorHandle {
    notify: Arc<Notify>,
}

impl RoutingEvaluatorHandle {
    /// Signal the evaluator task to run an evaluation cycle.
    pub(crate) fn trigger_evaluation(&self) {
        self.notify.notify_one();
    }
}

/// Background task that evaluates Kademlia connection candidates.
struct RoutingEvaluatorTask<I: SwarmIdentity> {
    routing: Arc<KademliaRouting<I>>,
    notify: Arc<Notify>,
}

impl<I: SwarmIdentity + 'static> RoutingEvaluatorTask<I> {
    async fn run(self) {
        let debounce = Duration::from_millis(100);
        let periodic = Duration::from_secs(5);
        let mut interval = tokio::time::interval(periodic);
        // First tick completes immediately — consume it so we don't
        // double-evaluate on startup.
        interval.tick().await;

        loop {
            tokio::select! {
                _ = self.notify.notified() => {
                    tokio::time::sleep(debounce).await;
                    self.routing.evaluate_connections();
                }
                _ = interval.tick() => {
                    self.routing.evaluate_connections();
                }
            }
        }
    }
}

/// Spawn the routing evaluator task. Returns a handle for triggering evaluation.
pub(crate) fn spawn_evaluator<I: SwarmIdentity + 'static>(
    routing: Arc<KademliaRouting<I>>,
) -> Result<RoutingEvaluatorHandle, String> {
    let notify = Arc::new(Notify::new());
    let handle = RoutingEvaluatorHandle {
        notify: notify.clone(),
    };

    let task = RoutingEvaluatorTask { routing, notify };

    let executor = vertex_tasks::TaskExecutor::try_current()
        .map_err(|e| format!("No task executor available: {e}"))?;

    executor.spawn_critical_with_graceful_shutdown_signal(
        "routing_evaluator",
        |shutdown| async move {
            tokio::select! {
                _ = task.run() => {}
                guard = shutdown => {
                    debug!("routing evaluator shutting down");
                    drop(guard);
                }
            }
        },
    );

    Ok(handle)
}
