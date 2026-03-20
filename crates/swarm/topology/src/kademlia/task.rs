//! Background task for Kademlia connection evaluation.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tracing::debug;
use vertex_swarm_api::SwarmIdentity;
use vertex_tasks::{GracefulShutdown, TaskExecutor};

use super::routing::KademliaRouting;

/// Handle for triggering evaluation from the topology behaviour.
#[derive(Clone)]
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
    async fn run(self, shutdown: GracefulShutdown) {
        let debounce = Duration::from_millis(100);
        let periodic = Duration::from_secs(5);
        let mut shutdown = std::pin::pin!(shutdown);

        loop {
            tokio::select! {
                guard = &mut shutdown => {
                    debug!("routing evaluator shutting down");
                    drop(guard);
                    return;
                }
                _ = self.notify.notified() => {
                    tokio::time::sleep(debounce).await;
                }
                _ = tokio::time::sleep(periodic) => {}
            }
            self.routing.evaluate_connections();
        }
    }
}

/// Spawn the routing evaluator task. Returns a handle for triggering evaluation.
pub(crate) fn spawn_evaluator<I: SwarmIdentity + 'static>(
    routing: Arc<KademliaRouting<I>>,
    executor: &TaskExecutor,
) -> RoutingEvaluatorHandle {
    let notify = Arc::new(Notify::new());
    let handle = RoutingEvaluatorHandle {
        notify: notify.clone(),
    };

    let task = RoutingEvaluatorTask { routing, notify };

    executor.spawn_critical_with_graceful_shutdown_signal("topology.evaluator", move |shutdown| {
        task.run(shutdown)
    });

    handle
}
