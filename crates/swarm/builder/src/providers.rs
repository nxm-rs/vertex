//! RPC provider implementations for Swarm nodes.

use std::sync::Arc;

use async_trait::async_trait;
use nectar_primitives::SwarmAddress;
use tracing::warn;
use vertex_swarm_api::{
    ChunkAddress, ChunkRetrievalResult, PeerReporter, PushReceipt, ReportSource, StampedChunk,
    SwarmChunkProvider, SwarmChunkSender, SwarmError, SwarmIdentity, SwarmResult,
    SwarmScoringEvent, SwarmSpec, SwarmTopologyRouting, SwarmTopologyState, verify_receipt_depth,
};
use vertex_swarm_node::{ClientHandle, PeerSelector};
use vertex_swarm_topology::TopologyHandle;

/// Report source for shallow/malformed receipts caught on the origin upload
/// path.
const PUSHSYNC_SOURCE: ReportSource = ReportSource::Protocol("pushsync");

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

        // The required custody depth is derived from our locally observed
        // neighbourhood depth (the trusted authority) and trust-but-verified
        // against the receipt's own claimed `storage_radius`. The signer is
        // recovered from the receipt signature over (address, nonce).
        let network_id = self.topology.identity().spec().network_id();
        let local_depth = self.topology.depth();
        let reporter = self.topology.peer_manager();

        // Try each closest peer in order and return the first receipt that
        // verifies. A shallow or malformed receipt is rejected, the responding
        // peer scored adversely, and the walk continues to the next candidate:
        // this is the retry-via-different-route dynamic the depth check exists
        // to engage (a fabricated shallow receipt no longer convinces the
        // uploader the push succeeded). The seed error covers the no-candidates
        // case; each failed attempt replaces it, so the value after the loop is
        // always the last failure.
        let mut outcome = Err(SwarmError::NoStorer {
            chunk_address: address,
        });
        for peer in closest {
            match self.client_handle.push_chunk(peer, chunk.clone()).await {
                Ok(receipt) => {
                    match accept_origin_receipt(
                        &receipt,
                        &address,
                        network_id,
                        local_depth,
                        peer,
                        reporter,
                    ) {
                        Ok(()) => return Ok(receipt),
                        Err(err) => {
                            outcome = Err(SwarmError::InvalidSignature {
                                chunk_address: address,
                                reason: err.to_string(),
                            });
                        }
                    }
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

/// Decide whether an origin uploader accepts a custody receipt from `peer`.
///
/// Recovers the signer overlay from the receipt signature (NOT the off-wire
/// `storer` field) and checks `PO(signer, chunk)` against a depth derived from
/// the locally observed neighbourhood depth, trust-but-verified against the
/// receipt's own `storage_radius`. On rejection the responding peer is scored
/// adversely for invalid data through the supplied reporter (the same path
/// #287 uses), so the uploader's caller retries via a different route instead
/// of believing a fabricated shallow receipt that the push succeeded.
fn accept_origin_receipt(
    receipt: &PushReceipt,
    address: &ChunkAddress,
    network_id: nectar_primitives::NetworkId,
    local_depth: vertex_swarm_api::NeighborhoodDepth,
    peer: SwarmAddress,
    reporter: &dyn PeerReporter,
) -> Result<(), vertex_swarm_api::ReceiptDepthError> {
    match verify_receipt_depth(receipt, address, network_id, local_depth) {
        Ok(_signer) => Ok(()),
        Err(err) => {
            warn!(
                %peer,
                %address,
                error = <&'static str>::from(&err),
                "rejected unverifiable custody receipt; retrying another route"
            );
            reporter.report_peer(&peer, SwarmScoringEvent::InvalidData, PUSHSYNC_SOURCE);
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use nectar_primitives::{Bin, NetworkId, Nonce, compute_overlay};
    use vertex_swarm_api::{
        NeighborhoodDepth, ReceiptDepthError, ReportSource, StorageRadius, SwarmScoringEvent,
    };

    use super::*;

    const NET: NetworkId = NetworkId::MAINNET;

    #[derive(Default)]
    struct RecordingReporter {
        reports: Mutex<Vec<(SwarmAddress, SwarmScoringEvent, ReportSource)>>,
    }

    impl PeerReporter for RecordingReporter {
        fn report_peer(
            &self,
            overlay: &SwarmAddress,
            event: SwarmScoringEvent,
            source: ReportSource,
        ) {
            self.reports.lock().unwrap().push((*overlay, event, source));
        }
    }

    impl RecordingReporter {
        /// Return the single recorded report, asserting exactly one exists.
        fn single(&self) -> (SwarmAddress, SwarmScoringEvent, ReportSource) {
            let reports = self.reports.lock().unwrap();
            assert_eq!(reports.len(), 1, "expected exactly one report");
            *reports.first().expect("one report")
        }

        fn count(&self) -> usize {
            self.reports.lock().unwrap().len()
        }
    }

    fn address(first_byte: u8) -> ChunkAddress {
        let mut bytes = [0u8; 32];
        bytes[0] = first_byte;
        ChunkAddress::new(bytes)
    }

    fn signed_receipt(
        signer: &PrivateKeySigner,
        address: &ChunkAddress,
        min_depth: u8,
        storage_radius: StorageRadius,
    ) -> PushReceipt {
        let eth = signer.address();
        // The signature is over the 32-byte address only (the wire format) and
        // is independent of the nonce, so sign once and grind for overlay depth.
        let signature = signer.sign_message_sync(address.as_bytes()).expect("sign");
        let mut counter = 0u64;
        loop {
            let mut nonce_bytes = [0u8; 32];
            nonce_bytes[..8].copy_from_slice(&counter.to_le_bytes());
            let nonce = Nonce::from(nonce_bytes);
            let overlay = compute_overlay(&eth, NET, &nonce);
            if address.proximity(&overlay).get() >= min_depth {
                return PushReceipt {
                    storer: SwarmAddress::from([0xff; 32]),
                    signature,
                    nonce,
                    storage_radius,
                };
            }
            counter += 1;
        }
    }

    fn depth(n: u8) -> NeighborhoodDepth {
        NeighborhoodDepth::new(Bin::new(n).unwrap())
    }

    fn radius(n: u8) -> StorageRadius {
        StorageRadius::new(Bin::new(n).unwrap())
    }

    #[test]
    fn origin_accepts_a_deep_receipt_without_reporting() {
        let signer = PrivateKeySigner::random();
        let addr = address(0xff);
        let receipt = signed_receipt(&signer, &addr, 8, radius(8));
        let reporter = RecordingReporter::default();
        let peer = SwarmAddress::from([0x11; 32]);

        accept_origin_receipt(&receipt, &addr, NET, depth(8), peer, &reporter)
            .expect("deep receipt accepted");
        assert!(reporter.reports.lock().unwrap().is_empty());
    }

    #[test]
    fn origin_rejects_a_shallow_receipt_and_reports_the_peer() {
        let signer = PrivateKeySigner::random();
        let addr = address(0xff);
        // Shallow signer; the local floor (depth 12) rejects it regardless of the
        // claimed radius.
        let receipt = signed_receipt(&signer, &addr, 0, radius(8));
        let reporter = RecordingReporter::default();
        let peer = SwarmAddress::from([0x22; 32]);

        let err = accept_origin_receipt(&receipt, &addr, NET, depth(12), peer, &reporter)
            .expect_err("shallow receipt rejected");
        assert!(matches!(err, ReceiptDepthError::Shallow { .. }));

        let (reported_peer, event, source) = reporter.single();
        assert_eq!(reported_peer, peer, "the responding peer is scored");
        assert_eq!(event, SwarmScoringEvent::InvalidData);
        assert_eq!(source, ReportSource::Protocol("pushsync"));
    }

    #[test]
    fn origin_rejects_a_shallow_receipt_claiming_radius_zero() {
        // Regression: an attacker setting storage_radius == 0 must not bypass the
        // local floor at the origin uploader.
        let signer = PrivateKeySigner::random();
        let addr = address(0xff);
        let receipt = signed_receipt(&signer, &addr, 0, radius(0));
        let reporter = RecordingReporter::default();
        let peer = SwarmAddress::from([0x55; 32]);

        let err = accept_origin_receipt(&receipt, &addr, NET, depth(12), peer, &reporter)
            .expect_err("radius 0 does not bypass the local floor");
        assert!(matches!(err, ReceiptDepthError::Shallow { .. }));
        assert_eq!(reporter.count(), 1);
    }

    #[test]
    fn origin_rejects_a_malformed_receipt() {
        let addr = address(0xff);
        let receipt = PushReceipt {
            storer: SwarmAddress::from([0x33; 32]),
            signature: alloy_primitives::Signature::from_raw(&[0u8; 65]).expect("zero signature"),
            nonce: Nonce::from([1u8; 32]),
            storage_radius: radius(8),
        };
        let reporter = RecordingReporter::default();
        let peer = SwarmAddress::from([0x44; 32]);

        let err = accept_origin_receipt(&receipt, &addr, NET, depth(8), peer, &reporter)
            .expect_err("malformed receipt rejected");
        assert_eq!(err, ReceiptDepthError::MalformedSignature);
        assert_eq!(reporter.count(), 1);
    }
}
