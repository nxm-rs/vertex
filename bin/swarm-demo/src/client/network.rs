//! Wasm-buildable proximity-routing chunk provider/sender for the browser client.

use std::sync::Arc;

use vertex_swarm_api::{
    ChunkAddress, ChunkRetrievalResult, PushReceipt, StampedChunk, SwarmChunkProvider,
    SwarmChunkSender, SwarmError, SwarmResult, SwarmTopologyRouting,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::{
    ClientHandle, NoInflightLimit, NoLatencyHint, ProximityOnly, RetrievalEngine,
};
use vertex_swarm_topology::TopologyHandle;

/// Closest peers to try when pushing a chunk before giving up.
const PUSH_CANDIDATE_COUNT: usize = 5;

/// A proximity-routing chunk provider/sender over the browser client node.
///
/// Drives the shared [`RetrievalEngine`] with the null-object capabilities
/// ([`ProximityOnly`], [`NoInflightLimit`], [`NoLatencyHint`]): no economic
/// ordering, no per-peer cap, and the constant stagger. Every retrieval terminal
/// surfaces as `RetrievalExhausted`.
#[derive(Clone)]
pub struct BrowserChunkProvider {
    engine: RetrievalEngine<Arc<Identity>, ProximityOnly, NoInflightLimit, NoLatencyHint>,
    client: ClientHandle,
    topology: TopologyHandle<Arc<Identity>>,
}

impl BrowserChunkProvider {
    /// Build the provider from the launched client's handle and topology.
    pub fn new(client: ClientHandle, topology: TopologyHandle<Arc<Identity>>) -> Self {
        Self {
            engine: RetrievalEngine::new(
                client.clone(),
                topology.clone(),
                ProximityOnly,
                NoInflightLimit,
                NoLatencyHint,
            ),
            client,
            topology,
        }
    }
}

#[async_trait::async_trait]
impl SwarmChunkProvider for BrowserChunkProvider {
    async fn retrieve_chunk(&self, address: &ChunkAddress) -> SwarmResult<ChunkRetrievalResult> {
        self.engine.retrieve(address).await
    }

    fn has_chunk(&self, _address: &ChunkAddress) -> bool {
        // Client nodes have no local storage.
        false
    }
}

#[async_trait::async_trait]
impl SwarmChunkSender for BrowserChunkProvider {
    async fn send_chunk_unchecked(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt> {
        push_to_closest(&self.client, &self.topology, chunk).await
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
        push_to_closest(&self.client, &self.topology, chunk).await
    }
}

/// Push `chunk` to the closest storers sequentially, returning the first receipt.
async fn push_to_closest(
    client: &ClientHandle,
    topology: &TopologyHandle<Arc<Identity>>,
    chunk: StampedChunk,
) -> SwarmResult<PushReceipt> {
    let address = *chunk.address();
    let closest = topology.closest_to(&address, PUSH_CANDIDATE_COUNT);
    let attempts = closest.len();

    let mut outcome = Err(SwarmError::NoStorer {
        chunk_address: address,
    });
    for peer in closest {
        match client.push_chunk(peer, chunk.clone(), true).await {
            Ok(receipt) => {
                // The receipt's storer was already recovered at the pushsync
                // decode boundary. The browser provider does not run the
                // full neighbourhood-depth verdict (it has no credible local
                // depth early in a session); a returned receipt is accepted as
                // proof the chunk reached a storer.
                return Ok(PushReceipt {
                    storer: receipt.storer,
                    signature: receipt.signature,
                    nonce: receipt.nonce,
                    storage_radius: receipt.storage_radius,
                });
            }
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
