//! RPC providers container for Swarm protocol.

/// RPC providers container for the Swarm protocol.
#[derive(Debug, Clone)]
pub struct RpcProviders<Topo, Chunk> {
    topology: Topo,
    chunk: Chunk,
}

impl<Topo, Chunk> RpcProviders<Topo, Chunk> {
    /// Create new RPC providers.
    pub fn new(topology: Topo, chunk: Chunk) -> Self {
        Self { topology, chunk }
    }

    /// Get reference to the topology provider.
    pub fn topology(&self) -> &Topo {
        &self.topology
    }

    /// Get reference to the chunk provider.
    pub fn chunk(&self) -> &Chunk {
        &self.chunk
    }
}
