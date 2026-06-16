//! Wasm-buildable proximity-routing chunk provider/sender for the browser client.

use std::sync::Arc;

use nectar_primitives::SwarmAddress;
use vertex_swarm_api::{
    ChunkAddress, ChunkRetrievalResult, PushReceipt, StampedChunk, SwarmChunkProvider,
    SwarmChunkSender, SwarmError, SwarmResult, SwarmTopologyRouting,
};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::{
    ChunkTransferError, ClientHandle, RETRIEVAL_STAGGER, RaceFailure, race_candidates,
};
use vertex_swarm_primitives::OverlayAddress;
use vertex_swarm_topology::TopologyHandle;

/// Closest peers to try when pushing a chunk before giving up.
const PUSH_CANDIDATE_COUNT: usize = 5;

/// Connected peers raced together in the first retrieval wave.
const RETRIEVE_INITIAL_CANDIDATES: usize = 8;

/// Additional connected peers admitted to the race on each later fallback wave.
const RETRIEVE_WAVE_STEP: usize = 8;

/// Hard ceiling on connected peers a retrieval races before it stops widening.
///
/// A retrieval never dials: it relies on the serving peer to forward the request
/// toward the chunk's neighbourhood (the retrieval protocol is forwarding-based).
/// Racing the closest connected peers is enough, so this is the only budget.
const RETRIEVE_CLOSE_BUDGET: usize = 24;

/// A proximity-routing chunk provider/sender over the browser client node.
#[derive(Clone)]
pub struct BrowserChunkProvider {
    client: ClientHandle,
    topology: TopologyHandle<Arc<Identity>>,
}

impl BrowserChunkProvider {
    /// Build the provider from the launched client's handle and topology.
    pub fn new(client: ClientHandle, topology: TopologyHandle<Arc<Identity>>) -> Self {
        Self { client, topology }
    }
}

#[async_trait::async_trait]
impl SwarmChunkProvider for BrowserChunkProvider {
    /// Retrieve a chunk by racing the closest connected peers in widening waves.
    ///
    /// The browser never dials on a retrieval. The retrieval protocol is
    /// forwarding-based: a connected peer that does not hold the chunk relays the
    /// request to a peer strictly closer to the chunk and returns the answer, so
    /// racing the closest connected peers reaches the chunk's neighbourhood
    /// without the client opening any new connection. A terminal absence surfaces
    /// as `NotFound`.
    async fn retrieve_chunk(&self, address: &ChunkAddress) -> SwarmResult<ChunkRetrievalResult> {
        let chunk_address = SwarmAddress::new(address.0.into());

        // Rank the connected peers by proximity to the chunk, but only the
        // closest `RETRIEVE_CLOSE_BUDGET` of them.
        let ranked = self
            .topology
            .closest_to(&chunk_address, RETRIEVE_CLOSE_BUDGET);

        if ranked.is_empty() {
            tracing::warn!(
                chunk = %chunk_address,
                "retrieval: no connected peers available"
            );
            return Err(SwarmError::network_msg(
                "no connected peers available for retrieval",
            ));
        }

        match self.race_connected_waves(&ranked, chunk_address).await {
            WaveOutcome::Hit(result) => {
                tracing::debug!(
                    chunk = %chunk_address,
                    served_by = %result.served_by,
                    server_po = chunk_address.proximity(&result.served_by).get(),
                    close_peers = ranked.len(),
                    "retrieval: succeeded over connected peers (forwarding)"
                );
                Ok(result)
            }
            WaveOutcome::NotFound => {
                tracing::warn!(
                    chunk = %chunk_address,
                    close_peers = ranked.len(),
                    "retrieval: not found across connected peers (forwarded-but-absent)"
                );
                Err(SwarmError::ChunkNotFound { address: *address })
            }
            WaveOutcome::Failed(e) => {
                tracing::warn!(
                    chunk = %chunk_address,
                    close_peers = ranked.len(),
                    error = %e,
                    "retrieval: all connected peers failed"
                );
                Err(SwarmError::AllPeersFailed {
                    address: *address,
                    attempts: ranked.len(),
                    source: Box::new(e),
                })
            }
            WaveOutcome::NoCandidates => Err(SwarmError::network_msg(
                "no connected peers available for retrieval",
            )),
        }
    }

    fn has_chunk(&self, _address: &ChunkAddress) -> bool {
        // Client nodes have no local storage.
        false
    }
}

/// Outcome of the close-peer wave phase.
enum WaveOutcome {
    /// A connected peer served the chunk.
    Hit(ChunkRetrievalResult),
    /// Every attempted wave failed and the terminal failure was `NotFound`.
    NotFound,
    /// Every attempted wave failed with a retryable (non-`NotFound`) error.
    Failed(ChunkTransferError),
    /// The candidate slice was empty.
    NoCandidates,
}

impl BrowserChunkProvider {
    /// Race the proximity-ranked connected peers in widening waves to the first answer.
    ///
    /// Each attempt logs the serving peer's proximity to the chunk at debug: a
    /// connected peer serving a chunk far from itself is the observable signature
    /// of the remote forwarding the request onward.
    async fn race_connected_waves(
        &self,
        ranked: &[OverlayAddress],
        chunk_address: SwarmAddress,
    ) -> WaveOutcome {
        let available = ranked.len();
        let mut attempted = 0usize;
        let mut wave = 0usize;
        let mut last_failure: Option<ChunkTransferError> = None;

        while attempted < available {
            let wave_len = if wave == 0 {
                RETRIEVE_INITIAL_CANDIDATES
            } else {
                RETRIEVE_WAVE_STEP
            };
            let wave_end = (attempted + wave_len).min(available);
            let slice: Vec<_> = ranked[attempted..wave_end].to_vec();
            let wave_size = slice.len();
            if wave_size == 0 {
                break;
            }

            match race_candidates(slice, RETRIEVAL_STAGGER, |peer| {
                let client = self.client.clone();
                async move {
                    let peer_po = chunk_address.proximity(&peer).get();
                    let started = js_sys::Date::now();
                    let outcome = client.retrieve_chunk(peer, chunk_address, true).await;
                    let latency_ms = js_sys::Date::now() - started;
                    match &outcome {
                        Ok(_) => tracing::debug!(
                            %peer,
                            chunk = %chunk_address,
                            peer_po,
                            latency_ms,
                            "retrieval attempt: Hit"
                        ),
                        Err(ChunkTransferError::NotFound(_)) => tracing::debug!(
                            %peer,
                            chunk = %chunk_address,
                            peer_po,
                            latency_ms,
                            "retrieval attempt: NotFound"
                        ),
                        Err(ChunkTransferError::TimedOut) => tracing::debug!(
                            %peer,
                            chunk = %chunk_address,
                            peer_po,
                            latency_ms,
                            "retrieval attempt: Timeout"
                        ),
                        Err(e) => tracing::debug!(
                            %peer,
                            chunk = %chunk_address,
                            peer_po,
                            latency_ms,
                            error = %e,
                            "retrieval attempt: transport-error"
                        ),
                    }
                    outcome
                }
            })
            .await
            {
                Ok(result) => {
                    return WaveOutcome::Hit(ChunkRetrievalResult {
                        chunk: result.chunk,
                        stamp: result.stamp,
                        served_by: result.peer,
                    });
                }
                Err(RaceFailure::NoCandidates) => break,
                Err(RaceFailure::AllFailed(e)) => {
                    attempted += wave_size;
                    last_failure = Some(e);
                    wave += 1;
                }
            }
        }

        match last_failure {
            Some(ChunkTransferError::NotFound(_)) => WaveOutcome::NotFound,
            Some(e) => WaveOutcome::Failed(e),
            None => WaveOutcome::NoCandidates,
        }
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
