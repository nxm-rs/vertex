//! RPC server trait definitions for Vertex nodes.
//!
//! This crate defines the [`RpcServer`] trait that abstracts over different
//! RPC implementations (gRPC, JSON-RPC, etc.).

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use async_trait::async_trait;

/// RPC server capability for nodes.
///
/// This trait defines the interface for RPC servers that expose node
/// functionality to external clients. Implementations can use different
/// protocols (gRPC, JSON-RPC, etc.) while maintaining a consistent interface.
#[async_trait]
#[auto_impl::auto_impl(&, Arc)]
pub trait RpcServer: Send + Sync {
    /// Start the RPC server and begin accepting connections.
    ///
    /// This method should be called in a spawned task as it will run
    /// until the server is stopped.
    async fn start(&self) -> eyre::Result<()>;

    /// Stop the RPC server gracefully.
    ///
    /// This signals the server to stop accepting new connections and
    /// wait for existing requests to complete.
    async fn stop(&self) -> eyre::Result<()>;

    /// Get the address the server is listening on.
    fn address(&self) -> SocketAddr;

    /// Check if the server is running.
    fn is_running(&self) -> bool;
}

/// No-op RPC server for nodes that don't expose RPC.
///
/// This is useful for testing or when running nodes in embedded mode
/// without external API access.
#[derive(Debug, Clone, Default)]
pub struct NoRpcServer;

#[async_trait]
impl RpcServer for NoRpcServer {
    async fn start(&self) -> eyre::Result<()> {
        Ok(())
    }

    async fn stop(&self) -> eyre::Result<()> {
        Ok(())
    }

    fn address(&self) -> SocketAddr {
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
    }

    fn is_running(&self) -> bool {
        false
    }
}
