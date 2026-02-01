//! Configuration traits for Swarm protocol components.

use core::time::Duration;
use std::future::Future;
use std::pin::Pin;

use vertex_node_api::NodeContext;

use crate::components::{SwarmAccountingConfig, SwarmLocalStoreConfig};
use crate::{SwarmBootnodeTypes, SwarmClientTypes, SwarmStorerTypes};

/// A boxed future representing the node's main event loop.
pub type NodeTask = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Configuration for storage incentives (redistribution, postage).
pub trait SwarmStorageConfig {
    /// Whether this node participates in redistribution.
    fn redistribution_enabled(&self) -> bool;
}

/// Configuration for P2P networking.
pub trait SwarmNetworkConfig {
    /// Get listen addresses as multiaddr strings.
    fn listen_addrs(&self) -> Vec<String>;

    /// Get bootnode addresses as multiaddr strings.
    fn bootnodes(&self) -> Vec<String>;

    /// Whether peer discovery is enabled.
    fn discovery_enabled(&self) -> bool;

    /// Maximum number of peer connections.
    fn max_peers(&self) -> usize;

    /// Connection idle timeout.
    fn idle_timeout(&self) -> Duration;

    /// Get external/NAT addresses to advertise.
    fn nat_addrs(&self) -> Vec<String> {
        Vec::new()
    }

    /// Whether auto-NAT discovery from observed addresses is enabled.
    fn nat_auto_enabled(&self) -> bool {
        false
    }
}

/// Configuration for Swarm node identity.
pub trait SwarmIdentityConfig {
    /// Whether to use ephemeral identity (random key, not persisted).
    fn ephemeral(&self) -> bool;
}

/// Combined Swarm protocol configuration.
pub trait SwarmConfig:
    SwarmAccountingConfig + SwarmLocalStoreConfig + SwarmStorageConfig + SwarmNetworkConfig + SwarmIdentityConfig
{
}

impl<T> SwarmConfig for T where
    T: SwarmAccountingConfig + SwarmLocalStoreConfig + SwarmStorageConfig + SwarmNetworkConfig + SwarmIdentityConfig
{
}

/// Default storage incentive configuration (redistribution disabled).
#[derive(Debug, Clone, Copy)]
pub struct DefaultStorageConfig;

impl SwarmStorageConfig for DefaultStorageConfig {
    fn redistribution_enabled(&self) -> bool {
        false
    }
}

/// Estimated metadata overhead as a fraction of chunk data.
pub const METADATA_OVERHEAD_FACTOR: f64 = 0.20;

/// Estimate the total disk space required for storing a given number of chunks.
///
/// # Example
/// ```
/// use vertex_swarm_api::estimate_storage_bytes;
///
/// let bytes = estimate_storage_bytes(1_000_000, 4096);
/// assert_eq!(bytes, 4_915_200_000); // ~4.58 GB
/// ```
pub fn estimate_storage_bytes(num_chunks: u64, chunk_size: usize) -> u64 {
    let chunk_data = num_chunks * chunk_size as u64;
    let overhead = (chunk_data as f64 * METADATA_OVERHEAD_FACTOR) as u64;
    chunk_data + overhead
}

/// Estimate the number of chunks that can fit in a given disk space.
///
/// # Example
/// ```
/// use vertex_swarm_api::estimate_chunks_for_bytes;
///
/// let chunks = estimate_chunks_for_bytes(10 * 1024 * 1024 * 1024, 4096);
/// assert_eq!(chunks, 2_184_533);
/// ```
pub fn estimate_chunks_for_bytes(available_bytes: u64, chunk_size: usize) -> u64 {
    let effective_chunk_size = chunk_size as f64 * (1.0 + METADATA_OVERHEAD_FACTOR);
    (available_bytes as f64 / effective_chunk_size) as u64
}

/// Configuration that knows how to launch a Swarm node.
///
/// Build produces a task (the main event loop) and providers for RPC.
/// The provider type varies by node capability (bootnode vs client vs storer).
#[async_trait::async_trait]
pub trait SwarmLaunchConfig: Send + Sync + 'static {
    /// The Swarm types for this configuration.
    type Types: SwarmBootnodeTypes;

    /// Providers for RPC services (node-type specific).
    type Providers: Send + Sync + 'static;

    /// Error type for build failures.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Build the node's main event loop and RPC providers.
    async fn build(
        self,
        ctx: &NodeContext,
    ) -> Result<(NodeTask, Self::Providers), Self::Error>;
}

/// Marker for configs that launch client nodes.
pub trait SwarmClientLaunchConfig: SwarmLaunchConfig
where
    Self::Types: SwarmClientTypes,
{
}
impl<T: SwarmLaunchConfig> SwarmClientLaunchConfig for T where T::Types: SwarmClientTypes {}

/// Marker for configs that launch storer nodes.
pub trait SwarmStorerLaunchConfig: SwarmLaunchConfig
where
    Self::Types: SwarmStorerTypes,
{
}
impl<T: SwarmLaunchConfig> SwarmStorerLaunchConfig for T where T::Types: SwarmStorerTypes {}
