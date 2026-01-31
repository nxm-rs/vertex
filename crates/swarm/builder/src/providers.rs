//! RPC provider implementations for Swarm nodes.
//!
//! This module provides concrete implementations of the provider traits
//! defined in `vertex-swarm-api`.

use std::sync::Arc;

use alloy_primitives::hex::FromHex;
use async_trait::async_trait;
use nectar_primitives::SwarmAddress;
use vertex_swarm_kademlia::KademliaTopology;
use vertex_swarm_api::{SwarmChunkProvider, ChunkRetrievalError, ChunkRetrievalResult, SwarmTopology};
use vertex_swarm_core::ClientHandle;
use vertex_swarm_identity::Identity;

/// Chunk provider implementation that uses a ClientHandle for network retrieval.
///
/// This wraps the network layer's ClientHandle to provide chunk retrieval
/// via the gRPC API.
#[derive(Clone)]
pub struct NetworkChunkProvider {
    client_handle: ClientHandle,
    topology: Arc<KademliaTopology<Arc<Identity>>>,
}

impl std::fmt::Debug for NetworkChunkProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkChunkProvider")
            .field("client_handle", &"ClientHandle")
            .field("topology", &"KademliaTopology")
            .finish()
    }
}

impl NetworkChunkProvider {
    /// Create a new network chunk provider.
    pub fn new(
        client_handle: ClientHandle,
        topology: Arc<KademliaTopology<Arc<Identity>>>,
    ) -> Self {
        Self {
            client_handle,
            topology,
        }
    }
}

#[async_trait]
impl SwarmChunkProvider for NetworkChunkProvider {
    async fn retrieve_chunk(&self, address: &str) -> Result<ChunkRetrievalResult, ChunkRetrievalError> {
        // Parse the hex address into a SwarmAddress
        let addr_bytes = <[u8; 32]>::from_hex(address)
            .map_err(|_| ChunkRetrievalError::InvalidAddress(address.to_string()))?;
        let chunk_address = SwarmAddress::new(addr_bytes);

        // Get the closest peers to the chunk address (up to 5 candidates)
        let closest_peers = self.topology.closest_to(&chunk_address, 5);

        if closest_peers.is_empty() {
            return Err(ChunkRetrievalError::Network(
                "No connected peers available for retrieval".to_string(),
            ));
        }

        // Try each peer in order until one succeeds
        let mut last_error = None;
        for peer_overlay in closest_peers.into_iter().take(3) {
            // Try to retrieve from this peer
            match self.client_handle.retrieve_chunk(peer_overlay, chunk_address).await {
                Ok(result) => {
                    return Ok(ChunkRetrievalResult {
                        data: result.data,
                        stamp: result.stamp,
                        served_by: result.peer.to_string(),
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
            Some(e) => Err(ChunkRetrievalError::Network(e.to_string())),
            None => Err(ChunkRetrievalError::NotFound(address.to_string())),
        }
    }

    fn has_chunk(&self, _address: &str) -> bool {
        // Client nodes don't have local storage
        false
    }
}

