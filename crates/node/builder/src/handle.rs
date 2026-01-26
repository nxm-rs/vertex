//! Node handle for managing a running node.

use eyre::Result;
use vertex_rpc_server::GrpcServerHandle;
use vertex_tasks::{Shutdown, TaskExecutor};

/// Handle to a running node.
///
/// Provides access to protocol components and methods for managing
/// the node lifecycle.
pub struct NodeHandle<C> {
    /// Protocol components (identity, topology, etc.)
    components: C,
    /// Task executor for the node.
    executor: TaskExecutor,
    /// gRPC server handle.
    grpc_handle: GrpcServerHandle,
}

impl<C> NodeHandle<C> {
    /// Create a new node handle.
    pub fn new(components: C, executor: TaskExecutor, grpc_handle: GrpcServerHandle) -> Self {
        Self {
            components,
            executor,
            grpc_handle,
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

    /// Get the shutdown signal receiver.
    pub fn shutdown_signal(&self) -> Shutdown {
        self.executor.on_shutdown_signal().clone()
    }

    /// Wait for the node to exit (shutdown signal or critical task panic).
    ///
    /// This runs the gRPC server and waits for either:
    /// - A shutdown signal (Ctrl+C)
    /// - A critical task panic
    pub async fn wait_for_exit(self) -> Result<()> {
        let shutdown = self.executor.on_shutdown_signal().clone();

        // Run the gRPC server until shutdown
        tokio::select! {
            result = self.grpc_handle.serve_with_shutdown(shutdown.clone()) => {
                if let Err(e) = result {
                    tracing::error!(error = %e, "gRPC server error");
                    return Err(e.into());
                }
            }
            _ = shutdown => {
                tracing::info!("Received shutdown signal");
            }
        }

        Ok(())
    }
}
