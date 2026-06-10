//! RPC provider implementations for Swarm nodes.

use std::sync::Arc;

use async_trait::async_trait;
use nectar_primitives::SwarmAddress;
use vertex_swarm_api::{
    ChunkAddress, ChunkRetrievalResult, PushReceipt, StampedChunk, SwarmChunkProvider,
    SwarmChunkSender, SwarmError, SwarmIdentity, SwarmResult, SwarmTopologyRouting,
};
use vertex_swarm_node::{ClientHandle, PeerSelector};
use vertex_swarm_topology::TopologyHandle;

/// Number of closest peers to try when pushing a chunk before giving up.
const PUSH_CANDIDATE_COUNT: usize = 5;

/// Number of closest peers to try when retrieving a chunk before giving up.
const RETRIEVE_CANDIDATE_COUNT: usize = 3;

/// Chunk provider using ClientHandle for network retrieval.
#[derive(Clone)]
pub struct NetworkChunkProvider<I: SwarmIdentity> {
    client_handle: ClientHandle,
    topology: TopologyHandle<I>,
    selector: Option<Arc<PeerSelector>>,
}

impl<I: SwarmIdentity> NetworkChunkProvider<I> {
    pub fn new(client_handle: ClientHandle, topology: TopologyHandle<I>) -> Self {
        Self {
            client_handle,
            topology,
            selector: None,
        }
    }

    /// Order retrieval and pushsync candidates with `selector` (score- and
    /// affordability-aware) instead of plain proximity order.
    pub fn with_selector(mut self, selector: Arc<PeerSelector>) -> Self {
        self.selector = Some(selector);
        self
    }

    /// Order proximity-sorted `candidates` for a request on `chunk`.
    fn select(&self, candidates: Vec<SwarmAddress>, chunk: &ChunkAddress) -> Vec<SwarmAddress> {
        match &self.selector {
            Some(selector) => selector.order(candidates, chunk),
            None => candidates,
        }
    }
}

#[async_trait]
impl<I: SwarmIdentity> SwarmChunkProvider for NetworkChunkProvider<I> {
    async fn retrieve_chunk(&self, address: &ChunkAddress) -> SwarmResult<ChunkRetrievalResult> {
        let chunk_address = SwarmAddress::new(address.0.into());
        let closest_peers = self
            .topology
            .closest_to(&chunk_address, RETRIEVE_CANDIDATE_COUNT);
        let closest_peers = self.select(closest_peers, &chunk_address);
        let attempts = closest_peers.len();

        // Try each closest peer in order and return the first success. The
        // seed error covers the no-candidates case; each failed attempt
        // replaces it, so the value after the loop is always the last failure.
        let mut outcome = Err(SwarmError::network_msg(
            "no connected peers available for retrieval",
        ));
        for peer_overlay in closest_peers {
            match self
                .client_handle
                .retrieve_chunk(peer_overlay, chunk_address)
                .await
            {
                Ok(result) => {
                    return Ok(ChunkRetrievalResult {
                        chunk: result.chunk,
                        served_by: result.peer,
                    });
                }
                Err(e) => {
                    outcome = Err(SwarmError::AllPeersFailed {
                        address: *address,
                        attempts,
                        source: Box::new(e),
                    });
                }
            }
        }

        outcome
    }

    fn has_chunk(&self, _address: &ChunkAddress) -> bool {
        // Client nodes don't have local storage
        false
    }
}

impl<I: SwarmIdentity> NetworkChunkProvider<I> {
    /// Push `chunk` to the storer peers closest to its address, returning the
    /// first receipt.
    ///
    /// Walks the closest candidates in order and returns the first storer that
    /// accepts the chunk. The client handle correlates a push response to its
    /// request by chunk address alone, so the candidates are tried sequentially
    /// rather than raced.
    async fn push_to_closest(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt> {
        let address = *chunk.address();
        let closest = self.topology.closest_to(&address, PUSH_CANDIDATE_COUNT);
        let closest = self.select(closest, &address);
        let attempts = closest.len();

        // Try each closest peer in order and return the first receipt. The
        // seed error covers the no-candidates case; each failed attempt
        // replaces it, so the value after the loop is always the last failure.
        let mut outcome = Err(SwarmError::NoStorer {
            chunk_address: address,
        });
        for peer in closest {
            match self.client_handle.push_chunk(peer, chunk.clone()).await {
                Ok(receipt) => return Ok(receipt),
                Err(e) => {
                    outcome = Err(SwarmError::AllPeersFailed {
                        address,
                        attempts,
                        source: Box::new(e),
                    });
                }
            }
        }

        outcome
    }
}

#[async_trait]
impl<I: SwarmIdentity> SwarmChunkSender for NetworkChunkProvider<I> {
    async fn send_chunk_unchecked(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt> {
        self.push_to_closest(chunk).await
    }

    async fn send_chunk(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt> {
        let address = *chunk.address();
        chunk
            .stamp()
            .recover_signer(&address)
            .map_err(|err| SwarmError::InvalidSignature {
                chunk_address: address,
                reason: err.to_string(),
            })?;

        self.push_to_closest(chunk).await
    }
}
