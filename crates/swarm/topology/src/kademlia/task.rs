//! Background task for Kademlia connection evaluation.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, broadcast};
use tracing::debug;
use vertex_swarm_api::SwarmIdentity;
use vertex_tasks::{GracefulShutdown, TaskExecutor};

use super::routing::KademliaRouting;
use crate::events::TopologyEvent;

/// Handle for triggering evaluation from the topology behaviour.
#[derive(Clone)]
pub(crate) struct RoutingEvaluatorHandle {
    notify: Arc<Notify>,
}

impl RoutingEvaluatorHandle {
    /// Create a handle whose evaluator task has not started yet.
    ///
    /// The shared [`Notify`] stores a single permit, so a trigger issued
    /// before [`spawn_evaluator`] wires the task is observed on startup.
    pub(crate) fn new() -> Self {
        Self {
            notify: Arc::new(Notify::new()),
        }
    }

    /// Signal the evaluator task to run an evaluation cycle.
    pub(crate) fn trigger_evaluation(&self) {
        self.notify.notify_one();
    }
}

/// Background task that evaluates Kademlia connection candidates.
struct RoutingEvaluatorTask<I: SwarmIdentity> {
    routing: Arc<KademliaRouting<I>>,
    notify: Arc<Notify>,
    event_tx: broadcast::Sender<TopologyEvent>,
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

            // The periodic tick is the only place that observes the
            // time-driven Converging -> Stable settle: the behaviour
            // re-derives the phase on connect/disconnect, but a quiet
            // table changes phase purely by the stability window passing.
            if let Some(transition) = self.routing.evaluate_phase() {
                let _ = self.event_tx.send(TopologyEvent::PhaseChanged {
                    from: transition.from,
                    to: transition.to,
                    depth: transition.depth.get(),
                });
            }
        }
    }
}

/// Spawn the routing evaluator task driven by the given handle's trigger.
///
/// `event_tx` is the topology event channel; the task broadcasts
/// [`TopologyEvent::PhaseChanged`] for phase transitions its periodic
/// evaluation commits.
pub(crate) fn spawn_evaluator<I: SwarmIdentity + 'static>(
    routing: Arc<KademliaRouting<I>>,
    handle: &RoutingEvaluatorHandle,
    event_tx: broadcast::Sender<TopologyEvent>,
    executor: &TaskExecutor,
) {
    let task = RoutingEvaluatorTask {
        routing,
        notify: Arc::clone(&handle.notify),
        event_tx,
    };

    executor.spawn_critical_with_graceful_shutdown_signal("topology.evaluator", move |shutdown| {
        task.run(shutdown)
    });
}
