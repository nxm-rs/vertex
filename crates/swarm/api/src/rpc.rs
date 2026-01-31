//! RPC providers container for Swarm protocol.

use crate::{SwarmChunkProvider, SwarmTopologyProvider};

/// RPC providers container for the Swarm protocol.
#[derive(Debug, Clone)]
pub struct RpcProviders<Topo, Chunk> {
    /// Topology provider for network status information.
    pub topology: Topo,
    /// Chunk provider for retrieval operations.
    pub chunk: Chunk,
}

impl<Topo, Chunk> RpcProviders<Topo, Chunk> {
    /// Create new RPC providers.
    pub fn new(topology: Topo, chunk: Chunk) -> Self {
        Self { topology, chunk }
    }
}

impl<Topo: SwarmTopologyProvider, Chunk: SwarmChunkProvider> RpcProviders<Topo, Chunk> {
    /// Get reference to the topology provider.
    pub fn topology(&self) -> &Topo {
        &self.topology
    }

    /// Get reference to the chunk provider.
    pub fn chunk(&self) -> &Chunk {
        &self.chunk
    }
}
