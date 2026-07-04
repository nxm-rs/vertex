//! Node handle for managing a running node.

use std::net::SocketAddr;

use tracing::info;
use vertex_node_api::NodeProtocol;
use vertex_rpc_server::{GrpcTransport, ServeWith, Transport, TransportServer};
use vertex_tasks::{Shutdown, TaskExecutor};

use crate::InfrastructureError;

/// Handle to a running node with access to components and shutdown signal.
///
/// Serving is a post-launch step: `launch` returns the bare components and the
/// operator opts a transport in via [`serve_with`](Self::serve_with).
pub struct NodeHandle<P: NodeProtocol> {
    components: P::Components,
    executor: TaskExecutor,
    shutdown: Shutdown,
}

impl<P: NodeProtocol> NodeHandle<P> {
    /// Create a new node handle.
    pub fn new(components: P::Components, executor: TaskExecutor, shutdown: Shutdown) -> Self {
        Self {
            components,
            executor,
            shutdown,
        }
    }

    /// Get a reference to the protocol components.
    pub fn components(&self) -> &P::Components {
        &self.components
    }

    /// Get a mutable reference to the protocol components.
    pub fn components_mut(&mut self) -> &mut P::Components {
        &mut self.components
    }

    /// Consume the handle and return the components.
    pub fn into_components(self) -> P::Components {
        self.components
    }

    /// Get a clone of the shutdown signal.
    pub fn shutdown_signal(&self) -> Shutdown {
        self.shutdown.clone()
    }

    /// Serve the components over `Tr`, binding at `addr`.
    ///
    /// Registers the protocol's serve view into a per-launch registry and spawns
    /// the bound server as the critical `rpc.server` task. The server resolves on
    /// the graceful-shutdown signal; an exit for any other reason (a post-bind
    /// serve failure) requests graceful shutdown so the node does not linger
    /// without its RPC endpoint.
    pub fn serve_with<Tr: Transport>(&self, addr: SocketAddr) -> Result<(), InfrastructureError>
    where
        P::ServeView: ServeWith<Tr>,
    {
        let mut registry = Tr::Registry::default();
        P::serve_view(&self.components).register(&mut registry);

        let server = Tr::into_server(registry, addr)
            .map_err(|e| InfrastructureError::Transport(e.into()))?;

        let shutdown_executor = self.executor.clone();
        self.executor.spawn_critical_with_graceful_shutdown_signal(
            "rpc.server",
            move |shutdown| async move {
                if let Err(e) = server.serve_with_shutdown(shutdown.ignore_guard()).await {
                    tracing::error!(error = %e, "RPC server error");
                }
                let _ = shutdown_executor.initiate_graceful_shutdown();
            },
        );

        info!(%addr, "RPC server started");
        Ok(())
    }

    /// Serve the components over gRPC, binding at `addr`.
    pub fn serve_grpc(&self, addr: SocketAddr) -> Result<(), InfrastructureError>
    where
        P::ServeView: ServeWith<GrpcTransport>,
    {
        self.serve_with::<GrpcTransport>(addr)
    }

    /// Wait for the node to exit (shutdown signal or critical task panic).
    pub async fn wait_for_shutdown(self) {
        self.shutdown.await;
        tracing::info!("Node shutdown complete");
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::future::Future;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use vertex_node_api::{InfrastructureContext, NodeProtocol};
    use vertex_rpc_server::{ServeWith, Transport, TransportServer};
    use vertex_tasks::TaskManager;

    use super::NodeHandle;

    struct StubComponents {
        registered: Arc<AtomicUsize>,
    }

    struct StubView {
        registered: Arc<AtomicUsize>,
    }

    struct StubProtocol;

    impl NodeProtocol for StubProtocol {
        type Config = ();
        type Components = StubComponents;
        type ServeView = StubView;
        type BuildError = Infallible;

        async fn launch(
            _config: Self::Config,
            _ctx: &dyn InfrastructureContext,
        ) -> Result<Self::Components, Self::BuildError> {
            unreachable!("stub protocol is not launched in this test")
        }

        fn serve_view(components: &Self::Components) -> Self::ServeView {
            StubView {
                registered: components.registered.clone(),
            }
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("stub transport error")]
    struct StubError;

    #[derive(Default)]
    struct StubRegistry;

    struct StubServer;

    struct StubTransport;

    impl Transport for StubTransport {
        type Registry = StubRegistry;
        type Server = StubServer;
        type Error = StubError;

        fn into_server(
            _reg: Self::Registry,
            _addr: std::net::SocketAddr,
        ) -> Result<Self::Server, Self::Error> {
            Ok(StubServer)
        }
    }

    impl TransportServer for StubServer {
        type Error = StubError;

        async fn serve_with_shutdown(
            self,
            signal: impl Future<Output = ()> + Send + 'static,
        ) -> Result<(), Self::Error> {
            signal.await;
            Ok(())
        }
    }

    impl ServeWith<StubTransport> for StubView {
        fn register(&self, _reg: &mut StubRegistry) {
            self.registered.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn serve_with_registers_and_returns_ok() {
        let manager = TaskManager::current();
        let executor = manager.executor();
        let registered = Arc::new(AtomicUsize::new(0));

        let handle: NodeHandle<StubProtocol> = NodeHandle::new(
            StubComponents {
                registered: registered.clone(),
            },
            executor.clone(),
            executor.on_shutdown_signal().clone(),
        );

        let addr = "127.0.0.1:0".parse().unwrap();
        handle.serve_with::<StubTransport>(addr).unwrap();

        assert_eq!(registered.load(Ordering::SeqCst), 1);
    }
}
