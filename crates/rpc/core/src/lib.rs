//! RPC server trait definitions for Vertex nodes.
//!
//! This crate defines the [`RpcServer`] trait that abstracts over different
//! RPC implementations (gRPC, JSON-RPC, etc.).

use std::net::SocketAddr;

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

/// Provider trait for topology information.
///
/// Implement this trait to expose topology data to the RPC server.
/// The KademliaTopology can implement this directly.
#[auto_impl::auto_impl(&, Arc)]
pub trait TopologyProvider: Send + Sync {
    /// Get the overlay address as hex string.
    fn overlay_address(&self) -> String;

    /// Get the current depth.
    fn depth(&self) -> u8;

    /// Get the number of connected peers.
    fn connected_peers_count(&self) -> usize;

    /// Get the number of known peers.
    fn known_peers_count(&self) -> usize;

    /// Get the number of pending connections.
    fn pending_connections_count(&self) -> usize;

    /// Get bin sizes as (connected, known) for each PO 0-31.
    fn bin_sizes(&self) -> Vec<(usize, usize)>;

    /// Get connected peer addresses in a specific bin as hex strings.
    fn connected_peers_in_bin(&self, po: u8) -> Vec<String>;
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
        "0.0.0.0:0".parse().unwrap()
    }

    fn is_running(&self) -> bool {
        false
    }
}
