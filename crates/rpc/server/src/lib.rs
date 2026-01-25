//! gRPC server implementation for Vertex nodes.
//!
//! This crate provides a gRPC-based RPC server that exposes node functionality
//! to external clients. Currently implements:
//!
//! - Health check service (gRPC health checking protocol)
//! - Node service (topology and status information)
//!
//! # Usage
//!
//! ```ignore
//! use vertex_rpc_server::{GrpcServer, GrpcServerConfig};
//! use vertex_rpc_core::RpcServer;
//!
//! let config = GrpcServerConfig {
//!     addr: "127.0.0.1:1635".parse()?,
//!     topology_provider: Some(my_topology.clone()),
//! };
//! let server = GrpcServer::with_config(config);
//! server.start().await?;
//! ```

mod health;
mod node;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use tokio::sync::watch;
use tonic::transport::Server;
use tracing::{info, warn};
pub use vertex_rpc_core::{RpcServer, TopologyProvider};

// Re-export the config trait for users
pub use vertex_node_api::RpcConfig;

pub use health::HealthService;
pub use node::NodeService;

// Re-export generated types for external use
pub mod proto {
    pub mod health {
        tonic::include_proto!("vertex.health.v1");
    }

    pub mod node {
        tonic::include_proto!("vertex.node.v1");
    }

    /// File descriptor set for gRPC reflection.
    pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("vertex_descriptor");
}

/// Configuration for the gRPC server.
#[derive(Clone)]
pub struct GrpcServerConfig {
    /// Address to bind to.
    pub addr: SocketAddr,

    /// Optional topology provider for node status queries.
    pub topology_provider: Option<Arc<dyn TopologyProvider>>,
}

impl Default for GrpcServerConfig {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:1635".parse().unwrap(),
            topology_provider: None,
        }
    }
}

impl GrpcServerConfig {
    /// Create configuration from an RpcConfig trait implementation.
    pub fn from_config(config: &impl RpcConfig) -> Self {
        let addr = SocketAddr::new(
            config.grpc_addr().parse().unwrap_or(IpAddr::from([127, 0, 0, 1])),
            config.grpc_port(),
        );
        Self {
            addr,
            topology_provider: None,
        }
    }

    /// Set the topology provider.
    pub fn with_topology(mut self, provider: Arc<dyn TopologyProvider>) -> Self {
        self.topology_provider = Some(provider);
        self
    }
}

impl std::fmt::Debug for GrpcServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcServerConfig")
            .field("addr", &self.addr)
            .field("topology_provider", &self.topology_provider.is_some())
            .finish()
    }
}

/// gRPC server for Vertex nodes.
///
/// Implements the [`RpcServer`] trait and provides health check and node status services.
/// Additional services can be added as the node API expands.
pub struct GrpcServer {
    config: GrpcServerConfig,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    running: AtomicBool,
}

impl GrpcServer {
    /// Create a new gRPC server with the given address.
    pub fn new(addr: SocketAddr) -> Arc<Self> {
        Self::with_config(GrpcServerConfig {
            addr,
            topology_provider: None,
        })
    }

    /// Create a new gRPC server with the given configuration.
    pub fn with_config(config: GrpcServerConfig) -> Arc<Self> {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Arc::new(Self {
            config,
            shutdown_tx,
            shutdown_rx,
            running: AtomicBool::new(false),
        })
    }
}

// Implement node-types marker trait for NodeTypes compatibility
impl vertex_node_types::RpcServer for GrpcServer {}

#[async_trait]
impl RpcServer for GrpcServer {
    async fn start(&self) -> eyre::Result<()> {
        let health_service = HealthService::default();
        let health_server = proto::health::health_server::HealthServer::new(health_service);

        // Node service (with optional topology provider)
        let node_service = NodeService::new(self.config.topology_provider.clone());
        let node_server = proto::node::node_server::NodeServer::new(node_service);

        // Enable gRPC reflection for tools like grpcurl
        let reflection_service = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(proto::FILE_DESCRIPTOR_SET)
            .build_v1()?;

        info!(addr = %self.config.addr, "Starting gRPC server");
        self.running.store(true, Ordering::SeqCst);

        let mut shutdown_rx = self.shutdown_rx.clone();

        let result = Server::builder()
            .add_service(health_server)
            .add_service(node_server)
            .add_service(reflection_service)
            .serve_with_shutdown(self.config.addr, async move {
                shutdown_rx.changed().await.ok();
            })
            .await;

        self.running.store(false, Ordering::SeqCst);

        match result {
            Ok(()) => {
                info!("gRPC server stopped");
                Ok(())
            }
            Err(e) => {
                warn!(error = %e, "gRPC server error");
                Err(e.into())
            }
        }
    }

    async fn stop(&self) -> eyre::Result<()> {
        info!("Stopping gRPC server");
        self.shutdown_tx.send(true)?;
        Ok(())
    }

    fn address(&self) -> SocketAddr {
        self.config.addr
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}
