//! Node handle for managing a running node.

use vertex_tasks::Shutdown;

/// Handle to a running node.
///
/// Provides access to protocol components and the shutdown signal.
/// All services (protocol, gRPC) are spawned as critical tasks and
/// managed by the `TaskManager`.
pub struct NodeHandle<C> {
    /// Protocol components (identity, topology, etc.)
    components: C,
    /// Shutdown signal for waiting.
    shutdown: Shutdown,
}

impl<C> NodeHandle<C> {
    /// Create a new node handle.
    pub fn new(components: C, shutdown: Shutdown) -> Self {
        Self {
            components,
            shutdown,
        }
    }

    /// Get a reference to the protocol components.
    pub fn components(&self) -> &C {
        &self.components
    }

    /// Get a mutable reference to the protocol components.
    pub fn components_mut(&mut self) -> &mut C {
        &mut self.components
    }

    /// Consume the handle and return the components.
    pub fn into_components(self) -> C {
        self.components
    }

    /// Get a clone of the shutdown signal.
    pub fn shutdown_signal(&self) -> Shutdown {
        self.shutdown.clone()
    }

    /// Wait for the node to exit (shutdown signal or critical task panic).
    ///
    /// This simply waits for the shutdown signal. All services are already
    /// running as spawned critical tasks.
    pub async fn wait_for_shutdown(self) {
        self.shutdown.await;
        tracing::info!("Node shutdown complete");
    }
}
