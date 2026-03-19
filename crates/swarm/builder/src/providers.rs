//! RPC provider implementations for Swarm nodes.

use async_trait::async_trait;
use nectar_primitives::SwarmAddress;
use vertex_swarm_api::{
    ChunkAddress, ChunkRetrievalResult, SwarmChunkProvider, SwarmError, SwarmIdentity, SwarmResult,
    SwarmTopologyRouting,
};
use vertex_swarm_node::ClientHandle;
use vertex_swarm_topology::TopologyHandle;

/// Chunk provider using ClientHandle for network retrieval.
#[derive(Clone)]
pub struct NetworkChunkProvider<I: SwarmIdentity> {
    client_handle: ClientHandle,
    topology: TopologyHandle<I>,
}

impl<I: SwarmIdentity> NetworkChunkProvider<I> {
    pub fn new(client_handle: ClientHandle, topology: TopologyHandle<I>) -> Self {
        Self {
            client_handle,
            topology,
        }
    }
}

#[async_trait]
impl<I: SwarmIdentity> SwarmChunkProvider for NetworkChunkProvider<I> {
    async fn retrieve_chunk(&self, address: &ChunkAddress) -> SwarmResult<ChunkRetrievalResult> {
        let chunk_address = SwarmAddress::new(address.0.into());

        // Get the closest peers to the chunk address (up to 5 candidates)
        let closest_peers = self.topology.closest_to(&chunk_address, 5);

        if closest_peers.is_empty() {
            return Err(SwarmError::Network {
                message: "No connected peers available for retrieval".to_string(),
            });
        }

        // Try each peer in order until one succeeds
        let mut last_error = None;
        for peer_overlay in closest_peers.into_iter().take(3) {
            // Try to retrieve from this peer
            match self
                .client_handle
                .retrieve_chunk(peer_overlay, chunk_address)
                .await
            {
                Ok(result) => {
                    return Ok(ChunkRetrievalResult {
                        data: result.data,
                        stamp: result.stamp,
                        served_by: result.peer,
                    });
                }
                Err(e) => {
                    last_error = Some(e);
                    // Continue to next peer
                }
            }
        }

        // All peers failed
        match last_error {
            Some(e) => Err(SwarmError::Network {
                message: e.to_string(),
            }),
            None => Err(SwarmError::ChunkNotFound { address: *address }),
        }
    }

    fn has_chunk(&self, _address: &ChunkAddress) -> bool {
        // Client nodes don't have local storage
        false
    }
}
