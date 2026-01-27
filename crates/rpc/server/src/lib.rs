//! gRPC server framework for Vertex nodes.
//!
//! This crate provides a gRPC-based RPC server framework with:
//!
//! - [`GrpcRegistry`] - Dynamic service registration during protocol build
//! - [`GrpcServer`] - Standalone server for simple use cases
//! - [`HealthService`] - gRPC health checking protocol
//!
//! # Registry Pattern (Recommended)
//!
//! The [`GrpcRegistry`] allows protocols to register their services during
//! the build phase, eliminating the need for a separate "providers" step:
//!
//! ```ignore
//! use vertex_rpc_server::GrpcRegistry;
//!
//! // During protocol build
//! let mut registry = GrpcRegistry::new();
//! registry.add_service(MyServiceServer::new(my_service));
//! registry.add_service(HealthServer::new(HealthService::default()));
//! registry.add_descriptor(MY_FILE_DESCRIPTOR_SET);
//!
//! // Launcher builds and serves
//! let handle = registry.into_server(addr)?;
//! handle.serve().await?;
//! ```
//!
//! # Standalone Server
//!
//! For simple use cases, use [`GrpcServer`] directly:
//!
//! ```ignore
//! use vertex_rpc_server::{GrpcServer, GrpcServerConfig};
//!
//! let server = GrpcServer::with_config(config);
//! server.start().await?;
//! ```

mod grpc_protocol;
mod health;
mod registry;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use tokio::sync::watch;
use tonic::transport::Server;
use tracing::{info, warn};
pub use vertex_rpc_core::RpcServer;

// Re-export the config trait for users
pub use vertex_node_api::RpcConfig;

pub use grpc_protocol::GrpcProtocol;
pub use health::HealthService;
pub use registry::{GrpcRegistry, GrpcServerHandle};

// Re-export generated types for external use
pub mod proto {
    pub mod health {
        tonic::include_proto!("vertex.health.v1");
    }

    /// File descriptor set for gRPC reflection.
    pub const FILE_DESCRIPTOR_SET: &[u8] = tonic::include_file_descriptor_set!("vertex_descriptor");
}

/// Configuration for the gRPC server.
#[derive(Clone, Debug)]
pub struct GrpcServerConfig {
    /// Address to bind to.
    pub addr: SocketAddr,
}

impl Default for GrpcServerConfig {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:1635".parse().unwrap(),
        }
    }
}

impl GrpcServerConfig {
    /// Create configuration from an RpcConfig trait implementation.
    pub fn from_config(config: &impl RpcConfig) -> Self {
        let addr = SocketAddr::new(
            config
                .grpc_addr()
                .parse()
                .unwrap_or(IpAddr::from([127, 0, 0, 1])),
            config.grpc_port(),
        );
        Self { addr }
    }
}

/// gRPC server framework for Vertex nodes.
///
/// Provides the base infrastructure for running a gRPC server. Protocol-specific
/// services can be added using the builder pattern or via `GrpcServiceProvider`.
///
/// Implements the [`RpcServer`] trait for lifecycle management.
pub struct GrpcServer {
    config: GrpcServerConfig,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    running: AtomicBool,
}

impl GrpcServer {
    /// Create a new gRPC server with the given address.
    pub fn new(addr: SocketAddr) -> Arc<Self> {
        Self::with_config(GrpcServerConfig { addr })
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

    /// Get a new server builder for adding custom services.
    ///
    /// Use this to add protocol-specific services before starting the server.
    pub fn builder() -> Server {
        Server::builder()
    }

    /// Get the health service proto file descriptor set.
    ///
    /// Can be combined with protocol-specific descriptors for gRPC reflection.
    pub fn file_descriptor_set() -> &'static [u8] {
        proto::FILE_DESCRIPTOR_SET
    }
}

// Implement node-types marker trait for NodeTypes compatibility
impl vertex_node_types::RpcServer for GrpcServer {}

#[async_trait]
impl RpcServer for GrpcServer {
    async fn start(&self) -> eyre::Result<()> {
        let health_service = HealthService::default();
        let health_server = proto::health::health_server::HealthServer::new(health_service);

        // Enable gRPC reflection for tools like grpcurl
        let reflection_service = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(proto::FILE_DESCRIPTOR_SET)
            .build_v1()?;

        info!(addr = %self.config.addr, "Starting gRPC server");
        self.running.store(true, Ordering::SeqCst);

        let mut shutdown_rx = self.shutdown_rx.clone();

        let result = Server::builder()
            .add_service(health_server)
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
