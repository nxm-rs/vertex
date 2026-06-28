//! RPC provider implementations for Swarm nodes.

use std::sync::Arc;

use async_trait::async_trait;
use nectar_primitives::SwarmAddress;
use tracing::warn;
use vertex_swarm_api::{
    ChunkAddress, ChunkRetrievalResult, PeerReporter, PushReceipt, ReportSource, StampedChunk,
    SwarmChunkProvider, SwarmChunkSender, SwarmError, SwarmIdentity, SwarmResult,
    SwarmScoringEvent, SwarmTopologyRouting, SwarmTopologyState,
};
use vertex_swarm_net_pushsync::{DepthVerdict, Receipt};
use vertex_swarm_topology::TopologyHandle;
// `Instant` is portable (the browser performance clock on wasm); only timer
// sleeps are `!Send`, and the gated-set wait awaits a settle completion instead.
use vertex_tasks::time::{Duration, Instant};

use crate::{
    ClientHandle, PeerInflightLimiter, PeerSelector, RETRIEVAL_STAGGER, RaceFailure,
    race_candidates,
};

/// Report source for shallow/malformed receipts caught on the origin upload
/// path.
const PUSHSYNC_SOURCE: ReportSource = ReportSource::Protocol("pushsync");

/// Number of closest peers to try when pushing a chunk before giving up.
const PUSH_CANDIDATE_COUNT: usize = 5;

/// Pool of closest connected peers scanned for a free retrieval slot before
/// skip-busy filtering.
///
/// Intentionally wider than [`RETRIEVE_MAX_ATTEMPTS`] so that when address
/// proximity clusters onto a few close peers and one is at its in-flight cap,
/// skip-busy still has next-closest alternatives with a free slot rather than
/// overrunning the hot peer's per-connection substream budget. This is only a
/// selection pool: the race is bounded separately by [`RETRIEVE_MAX_ATTEMPTS`].
const RETRIEVE_CANDIDATE_WIDTH: usize = 8;

/// Maximum retrieval legs raced per chunk, so widening the skip-busy pool
/// does not amplify paid bandwidth: the wider pool only supplies free-slot
/// alternatives, the race still meters at most this many legs (the prior bound).
const RETRIEVE_MAX_ATTEMPTS: usize = 3;

/// Bound on the settle-and-await for a fully gated close set: the request's own
/// retrieval lifetime, matching the client-behaviour outbound retrieval timeout.
/// Accounting-timing back-pressure blocks within the request rather than failing
/// early to the consumer, and only a genuine lifetime expiry falls through to the
/// generic transient failure. The wait is progress-aware, so it paces on
/// settlement RTT and returns at once when no settle is in flight to drain debt.
const GATE_SETTLE_BUDGET: Duration = Duration::from_secs(30);

/// Chunk provider using ClientHandle for network retrieval.
#[derive(Clone)]
pub struct NetworkChunkProvider<I: SwarmIdentity> {
    client_handle: ClientHandle,
    topology: TopologyHandle<I>,
    selector: Option<Arc<PeerSelector>>,
    inflight: Option<Arc<PeerInflightLimiter>>,
}

impl<I: SwarmIdentity> NetworkChunkProvider<I> {
    pub fn new(client_handle: ClientHandle, topology: TopologyHandle<I>) -> Self {
        Self {
            client_handle,
            topology,
            selector: None,
            inflight: None,
        }
    }

    /// Order retrieval and pushsync candidates with `selector` (score- and
    /// affordability-aware) instead of plain proximity order.
    pub fn with_selector(mut self, selector: Arc<PeerSelector>) -> Self {
        self.selector = Some(selector);
        self
    }

    /// Cap concurrent outbound retrieval substreams per peer, skipping a peer at
    /// its cap in favour of the next-closest candidate with a free slot.
    pub fn with_inflight_limiter(mut self, inflight: Arc<PeerInflightLimiter>) -> Self {
        self.inflight = Some(inflight);
        self
    }

    /// Order proximity-sorted `candidates` for a request on `chunk`, settling
    /// and awaiting a fully gated close set.
    ///
    /// With a selector this delegates to the score- and affordability-aware
    /// ordering. A non-empty order returns at once (the fast path); a fully
    /// gated close set awaits its in-flight settles up to the retrieval-lifetime
    /// [`GATE_SETTLE_BUDGET`], returning at the deadline or promptly once no
    /// settle is in flight to drain debt, with whatever is admissible (possibly
    /// empty). An empty result falls through to the caller's generic transient
    /// failure, never an accounting-specific error. The wait is `Send` on every
    /// platform. With no selector the proximity order is returned unchanged.
    async fn select_or_wait(
        &self,
        candidates: Vec<SwarmAddress>,
        chunk: &ChunkAddress,
    ) -> Vec<SwarmAddress> {
        match &self.selector {
            Some(selector) => {
                let deadline = Instant::now() + GATE_SETTLE_BUDGET;
                selector.order_or_wait(candidates, chunk, deadline).await
            }
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
            .closest_to(&chunk_address, RETRIEVE_CANDIDATE_WIDTH);
        // Settle-and-await when every close peer is gated, so a fully gated set
        // recovers instead of failing; if still gated at the budget the empty
        // result falls through to the no-connected-peers path below, the same
        // generic transient error a no-peers failure yields.
        let closest_peers = self.select_or_wait(closest_peers, &chunk_address).await;

        // Skip-busy: prefer close peers with a free retrieval slot so a hot peer
        // at its in-flight cap is skipped at selection time rather than
        // overrun. When every close candidate is at its cap, fall through to the
        // full list rather than failing the request: degraded service beats
        // failure. The cap is non-economic and composed after the economic
        // selector above; the throttle paces each chosen request below.
        let mut candidates = skip_busy(closest_peers, self.inflight.as_deref());
        // Bound the raced legs so the wider skip-busy pool only supplies
        // free-slot alternatives and never amplifies metered bandwidth: the
        // survivor list is proximity-ordered and skip-busy preserves order, so
        // this keeps the closest free-slot peers (or, in the all-busy
        // fall-through, the closest few, the prior bound).
        candidates.truncate(RETRIEVE_MAX_ATTEMPTS);
        let attempts = candidates.len();

        // Race the candidates with a staggered start, resolving on the first
        // success, so a withholding head candidate is overtaken by the next one
        // within the stagger instead of stalling this slot for a full
        // per-attempt deadline and blocking every later chunk behind it. Each
        // attempt carries its own pacing (the outbound self-throttle and
        // affordability check run inside `retrieve_chunk` before it dispatches),
        // so the staggered starts preserve the per-peer pacing. The per-peer
        // in-flight permit is reserved lazily as each leg starts and rides its
        // request future, releasing the slot on drop including a cancelled
        // losing leg.
        match race_candidates(candidates, RETRIEVAL_STAGGER, |peer_overlay| {
            let permit = self
                .inflight
                .as_ref()
                .and_then(|limiter| limiter.try_acquire(&peer_overlay));
            // `originated = true`: our own retrieval, so the client service
            // debits the serving peer on delivery.
            let request = self
                .client_handle
                .retrieve_chunk(peer_overlay, chunk_address, true);
            async move {
                let _permit = permit;
                request.await
            }
        })
        .await
        {
            Ok(result) => Ok(ChunkRetrievalResult {
                chunk: result.chunk,
                stamp: result.stamp,
                served_by: result.peer,
            }),
            Err(RaceFailure::NoCandidates) => Err(SwarmError::network_msg(
                "no connected peers available for retrieval",
            )),
            Err(RaceFailure::AllFailed(e)) => Err(SwarmError::AllPeersFailed {
                address: *address,
                attempts,
                source: Box::new(e),
            }),
        }
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
        // Settle-and-await an all-gated closest set rather than immediately
        // falling through to a farther peer or failing; if still gated at the
        // budget the empty result yields the generic no-storer outcome below.
        let closest = self.select_or_wait(closest, &address).await;
        let attempts = closest.len();

        // The required custody depth is derived from our locally observed
        // neighbourhood depth (the trusted authority) and trust-but-verified
        // against the receipt's own claimed `storage_radius`. The check is gated
        // on that depth being credible (the neighbourhood has saturated); a
        // non-credible view cannot anchor the floor and yields an unverifiable
        // verdict. The receipt's signer was already recovered at the decode
        // boundary; a malformed receipt never reaches here (it surfaces as a push
        // error below).
        let local_depth = self.topology.depth();
        let neighbourhood_credible = self.topology.neighbourhood_credible();
        let reporter = self.topology.peer_manager();

        // Try each closest peer in order and return the first receipt that
        // verifies. A shallow receipt is rejected, the responding peer scored
        // adversely, and the walk continues to the next candidate: this is the
        // retry-via-different-route dynamic the depth check exists to engage (a
        // fabricated shallow receipt no longer convinces the uploader the push
        // succeeded). An unverifiable receipt (non-credible local view) is also
        // not trusted, but the responder is NOT penalised: it may be honest, we
        // just cannot judge custody depth. If no candidate verifies and at least
        // one was unverifiable, the push is reported as unconfirmed custody
        // rather than a hard failure. The seed error covers the no-candidates
        // case; each attempt replaces it, so the value after the loop is the last
        // failure.
        let mut outcome = Err(SwarmError::NoStorer {
            chunk_address: address,
        });
        for peer in closest {
            // `originated = true`: our own push, so the client service debits
            // the storer on receipt.
            match self
                .client_handle
                .push_chunk(peer, chunk.clone(), true)
                .await
            {
                Ok(receipt) => {
                    match accept_origin_receipt(
                        &receipt,
                        peer,
                        local_depth,
                        neighbourhood_credible,
                        reporter,
                    ) {
                        DepthVerdict::Verified => return Ok(push_receipt_of(receipt)),
                        DepthVerdict::Shallow(err) => {
                            outcome = Err(SwarmError::InvalidSignature {
                                chunk_address: address,
                                reason: err.to_string(),
                            });
                        }
                        DepthVerdict::Unverifiable => {
                            // Surface unconfirmed custody distinctly from a hard
                            // invalid-signature failure. A later shallow verdict
                            // (a proven finding) takes precedence over this; an
                            // earlier one is not downgraded.
                            if !matches!(outcome, Err(SwarmError::InvalidSignature { .. })) {
                                outcome = Err(SwarmError::UnconfirmedCustody {
                                    chunk_address: address,
                                });
                            }
                        }
                    }
                }
                Err(e) => {
                    // A transport-level failure is the weakest signal: it does
                    // not overwrite a depth verdict (shallow misbehaviour or
                    // unconfirmed custody) already recorded for an earlier
                    // candidate.
                    if !matches!(
                        outcome,
                        Err(SwarmError::InvalidSignature { .. })
                            | Err(SwarmError::UnconfirmedCustody { .. })
                    ) {
                        outcome = Err(SwarmError::AllPeersFailed {
                            address,
                            attempts,
                            source: Box::new(e),
                        });
                    }
                }
            }
        }

        outcome
    }
}

/// Filter proximity-ordered `candidates` to those with a free retrieval slot.
///
/// With no limiter the list is unchanged. When every close candidate is at its
/// in-flight cap the full list is returned (fall through, since degraded service
/// beats failing the request). Skip-busy happens here, at selection time, so a
/// busy head peer is never raced and the next-closest free peer leads instead.
fn skip_busy(
    candidates: Vec<SwarmAddress>,
    inflight: Option<&PeerInflightLimiter>,
) -> Vec<SwarmAddress> {
    let Some(limiter) = inflight else {
        return candidates;
    };
    let survivors: Vec<SwarmAddress> = candidates
        .iter()
        .copied()
        .filter(|peer| limiter.has_free_slot(peer))
        .collect();
    if survivors.is_empty() {
        candidates
    } else {
        survivors
    }
}

/// Project the internal domain [`Receipt`] onto the public boundary
/// [`PushReceipt`] returned to operators and embedders.
fn push_receipt_of(receipt: Receipt) -> PushReceipt {
    PushReceipt {
        storer: receipt.storer,
        signature: receipt.signature,
        nonce: receipt.nonce,
        storage_radius: receipt.storage_radius,
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
/// The receipt is a [`Receipt`]: its storer was recovered and verified at the
/// decode boundary (a malformed receipt never reaches here). This checks the
/// custody depth against the locally observed neighbourhood depth,
/// trust-but-verified against the receipt's own declared radius, gated on that
/// depth being credible (`neighbourhood_credible`).
///
/// The verdict drives the caller:
/// - [`DepthVerdict::Verified`]: the receipt is trusted; the push succeeded.
/// - [`DepthVerdict::Shallow`]: the storer is provably too shallow. The
///   responding peer is scored adversely for invalid data through the supplied
///   reporter (the same path #287 uses), and the caller retries via a different
///   route instead of believing a fabricated shallow receipt.
/// - [`DepthVerdict::Unverifiable`]: the local view is not credible enough to
///   judge custody depth. The peer is NOT penalised (it may be honest); the
///   caller treats the push as unconfirmed.
fn accept_origin_receipt(
    receipt: &Receipt,
    peer: SwarmAddress,
    local_depth: vertex_swarm_api::NeighborhoodDepth,
    neighbourhood_credible: bool,
    reporter: &dyn PeerReporter,
) -> DepthVerdict {
    let verdict = receipt.verify_depth(local_depth, neighbourhood_credible);
    if let DepthVerdict::Shallow(err) = &verdict {
        warn!(
            %peer,
            address = %receipt.address,
            error = <&'static str>::from(err),
            "rejected shallow custody receipt; retrying another route"
        );
        reporter.report_peer(&peer, SwarmScoringEvent::InvalidData, PUSHSYNC_SOURCE);
    }
    verdict
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use nectar_primitives::{Bin, NetworkId, Nonce, compute_overlay};
    use vertex_swarm_api::{NeighborhoodDepth, ReportSource, StorageRadius, SwarmScoringEvent};
    use vertex_swarm_net_pushsync::WireReceipt;

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

    /// A storer-verified receipt as the decode boundary produces it, with the
    /// storer ground to sit at least `min_depth` bits deep relative to `address`.
    fn signed_receipt(
        signer: &PrivateKeySigner,
        address: &ChunkAddress,
        min_depth: u8,
        storage_radius: StorageRadius,
    ) -> Receipt {
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
                let wire = WireReceipt::new(*address, signature, nonce, storage_radius);
                return Receipt::reconstruct(wire, NET).expect("reconstructs");
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

        assert_eq!(
            accept_origin_receipt(&receipt, peer, depth(8), true, &reporter),
            DepthVerdict::Verified,
            "deep receipt accepted"
        );
        assert!(reporter.reports.lock().unwrap().is_empty());
    }

    #[test]
    fn origin_rejects_a_shallow_receipt_and_reports_the_peer() {
        let signer = PrivateKeySigner::random();
        let addr = address(0xff);
        // Shallow signer; against a credible local view the floor (depth 12)
        // rejects it regardless of the claimed radius.
        let receipt = signed_receipt(&signer, &addr, 0, radius(8));
        let reporter = RecordingReporter::default();
        let peer = SwarmAddress::from([0x22; 32]);

        let DepthVerdict::Shallow(_) =
            accept_origin_receipt(&receipt, peer, depth(12), true, &reporter)
        else {
            panic!("shallow receipt rejected");
        };

        let (reported_peer, event, source) = reporter.single();
        assert_eq!(reported_peer, peer, "the responding peer is scored");
        assert_eq!(event, SwarmScoringEvent::InvalidData);
        assert_eq!(source, ReportSource::Protocol("pushsync"));
    }

    #[test]
    fn origin_rejects_a_shallow_receipt_claiming_radius_zero() {
        // Regression: against a credible local view an attacker setting
        // storage_radius == 0 must not bypass the local floor at the origin
        // uploader.
        let signer = PrivateKeySigner::random();
        let addr = address(0xff);
        let receipt = signed_receipt(&signer, &addr, 0, radius(0));
        let reporter = RecordingReporter::default();
        let peer = SwarmAddress::from([0x55; 32]);

        assert!(
            matches!(
                accept_origin_receipt(&receipt, peer, depth(12), true, &reporter),
                DepthVerdict::Shallow(_)
            ),
            "radius 0 does not bypass the local floor"
        );
        assert_eq!(reporter.count(), 1);
    }

    #[test]
    fn origin_treats_an_unverifiable_receipt_as_unconfirmed_without_reporting() {
        // Regression for #316: with a non-credible local view (a fresh or sparse
        // node, local_depth == 0) a shallow receipt declaring radius 0 must NOT
        // be accepted, and the responder must NOT be penalised: the verdict is
        // unverifiable, not a finding of misbehaviour.
        let signer = PrivateKeySigner::random();
        let addr = address(0xff);
        let receipt = signed_receipt(&signer, &addr, 0, radius(0));
        let reporter = RecordingReporter::default();
        let peer = SwarmAddress::from([0x66; 32]);

        assert_eq!(
            accept_origin_receipt(&receipt, peer, depth(0), false, &reporter),
            DepthVerdict::Unverifiable,
            "non-credible view yields an unverifiable verdict"
        );
        assert_eq!(
            reporter.count(),
            0,
            "an unverifiable receipt does not penalise the peer"
        );
    }

    mod staggered_race {
        use std::time::{Duration, Instant};

        use crate::{ChunkTransferError, ClientCommand, ClientHandle, RetrievalResult};
        use nectar_primitives::ContentChunk;
        use tokio::sync::mpsc;

        use super::super::{RETRIEVAL_STAGGER, RaceFailure, race_candidates};
        use super::*;

        fn test_chunk() -> nectar_primitives::AnyChunk {
            ContentChunk::new(&b"provider-race-chunk"[..])
                .expect("valid content chunk")
                .into()
        }

        /// Drive the exact future the provider builds per candidate: each
        /// attempt is `client_handle.retrieve_chunk(peer, address)`, raced with a
        /// staggered start. The per-candidate pacing (the outbound self-throttle
        /// and affordability check) lives inside that call, so this exercises the
        /// provider's retrieval leg and race wiring without standing up a
        /// topology mock.
        async fn race_over_handle(
            handle: ClientHandle,
            candidates: Vec<SwarmAddress>,
            address: ChunkAddress,
        ) -> Result<RetrievalResult, RaceFailure<ChunkTransferError>> {
            race_candidates(candidates, RETRIEVAL_STAGGER, move |peer| {
                let handle = handle.clone();
                async move { handle.retrieve_chunk(peer, address, true).await }
            })
            .await
        }

        #[tokio::test]
        async fn withholding_head_is_overtaken_by_the_second_candidate() {
            let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
            let handle = ClientHandle::new(tx);

            let address = address(0xaa);
            let peer_a = SwarmAddress::from([1u8; 32]);
            let peer_b = SwarmAddress::from([2u8; 32]);

            let start = Instant::now();
            let race = tokio::spawn(race_over_handle(handle, vec![peer_a, peer_b], address));

            // The head request arrives first; leave it unanswered so it
            // withholds. The stagger must bring in the second candidate, whose
            // response resolves the race well under the per-attempt deadline.
            let head = match rx.recv().await.expect("head command") {
                ClientCommand::RetrieveChunk { peer, response, .. } => {
                    assert_eq!(peer, peer_a);
                    response
                }
                other => panic!("unexpected command: {other:?}"),
            };
            match rx.recv().await.expect("second command after stagger") {
                ClientCommand::RetrieveChunk { peer, response, .. } => {
                    assert_eq!(peer, peer_b);
                    response
                        .send(Ok(RetrievalResult {
                            chunk: test_chunk(),
                            stamp: None,
                            peer: peer_b,
                        }))
                        .expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            }

            let result = race.await.unwrap().expect("race resolves");
            assert_eq!(result.peer, peer_b, "the staggered second wins");
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "overtaken within the stagger, well under the per-attempt deadline"
            );

            // The losing head request's response channel was dropped when the
            // race resolved: the handler observes the closed receiver and
            // releases any reservation the in-flight attempt held. Sending on it
            // now fails, proving the loser was dropped (not run to completion).
            assert!(
                head.send(Ok(RetrievalResult {
                    chunk: test_chunk(),
                    stamp: None,
                    peer: peer_a,
                }))
                .is_err(),
                "the losing head response channel is dropped on resolve"
            );
        }

        #[tokio::test]
        async fn all_candidates_failing_yields_the_last_error() {
            // The handle's command channel is closed, so every retrieval attempt
            // fails immediately and the race exhausts every candidate.
            let (tx, rx) = mpsc::channel::<ClientCommand>(16);
            drop(rx);
            let handle = ClientHandle::new(tx);

            let address = address(0xbb);
            let candidates = vec![SwarmAddress::from([1u8; 32]), SwarmAddress::from([2u8; 32])];

            let outcome = race_over_handle(handle, candidates, address).await;
            assert!(
                matches!(
                    outcome,
                    Err(RaceFailure::AllFailed(ChunkTransferError::ChannelClosed))
                ),
                "all candidates failing surfaces the last attempt's error"
            );
        }

        #[tokio::test]
        async fn no_candidates_yields_no_candidates() {
            let (tx, _rx) = mpsc::channel::<ClientCommand>(16);
            let handle = ClientHandle::new(tx);

            let outcome = race_over_handle(handle, Vec::new(), address(0xcc)).await;
            assert!(matches!(outcome, Err(RaceFailure::NoCandidates)));
        }
    }

    mod skip_busy_scheduler {
        use std::num::NonZeroUsize;
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        use nectar_primitives::ContentChunk;
        use tokio::sync::mpsc;

        use crate::{
            ChunkTransferError, ClientCommand, ClientHandle, PeerInflightLimiter, RetrievalResult,
        };

        use super::super::{
            RETRIEVAL_STAGGER, RETRIEVE_MAX_ATTEMPTS, RaceFailure, race_candidates, skip_busy,
        };
        use super::*;

        const CAP_ONE: NonZeroUsize = match NonZeroUsize::new(1) {
            Some(cap) => cap,
            None => unreachable!(),
        };

        fn overlay(n: u8) -> SwarmAddress {
            SwarmAddress::from([n; 32])
        }

        fn test_chunk() -> nectar_primitives::AnyChunk {
            ContentChunk::new(&b"skip-busy-chunk"[..])
                .expect("valid content chunk")
                .into()
        }

        /// Drive the exact composition the provider builds: skip-busy filtering
        /// at selection time, then the staggered race whose legs reserve an
        /// in-flight permit that rides the request future and releases on drop.
        async fn race_with_limiter(
            handle: ClientHandle,
            limiter: Arc<PeerInflightLimiter>,
            candidates: Vec<SwarmAddress>,
            address: ChunkAddress,
        ) -> Result<RetrievalResult, RaceFailure<ChunkTransferError>> {
            let candidates = skip_busy(candidates, Some(&limiter));
            race_candidates(candidates, RETRIEVAL_STAGGER, move |peer| {
                let permit = limiter.try_acquire(&peer);
                let handle = handle.clone();
                async move {
                    let _permit = permit;
                    handle.retrieve_chunk(peer, address, true).await
                }
            })
            .await
        }

        #[test]
        fn skip_busy_without_a_limiter_keeps_every_candidate() {
            let candidates = vec![overlay(1), overlay(2), overlay(3)];
            assert_eq!(skip_busy(candidates.clone(), None), candidates);
        }

        #[test]
        fn skip_busy_drops_a_capped_head() {
            let limiter = PeerInflightLimiter::new(CAP_ONE);
            let busy = overlay(1);
            let _held = limiter.try_acquire(&busy).expect("first slot");

            let survivors = skip_busy(vec![busy, overlay(2), overlay(3)], Some(&limiter));
            assert_eq!(
                survivors,
                vec![overlay(2), overlay(3)],
                "the capped head is skipped, the next-closest free peers remain"
            );
        }

        #[test]
        fn skip_busy_falls_through_when_every_candidate_is_capped() {
            let limiter = PeerInflightLimiter::new(CAP_ONE);
            let candidates = vec![overlay(1), overlay(2)];
            let _h1 = limiter.try_acquire(&overlay(1)).expect("slot a");
            let _h2 = limiter.try_acquire(&overlay(2)).expect("slot b");

            assert_eq!(
                skip_busy(candidates.clone(), Some(&limiter)),
                candidates,
                "all-busy falls through to the full list rather than failing"
            );
        }

        #[test]
        fn skip_busy_pool_is_truncated_to_the_attempts_bound() {
            // A wide pool of free-slot peers must not race every leg: the
            // production path truncates the skip-busy survivors to the attempts
            // bound, so the wider pool only supplies free-slot alternatives
            // without amplifying metered bandwidth.
            let limiter = PeerInflightLimiter::new(CAP_ONE);
            let pool: Vec<SwarmAddress> = (1..=8).map(overlay).collect();

            let mut candidates = skip_busy(pool, Some(&limiter));
            candidates.truncate(RETRIEVE_MAX_ATTEMPTS);

            assert_eq!(candidates.len(), RETRIEVE_MAX_ATTEMPTS);
            assert_eq!(
                candidates,
                vec![overlay(1), overlay(2), overlay(3)],
                "the closest attempts-bound free-slot peers are kept in order"
            );
        }

        #[tokio::test]
        async fn capped_head_is_skipped_for_the_next_free_peer() {
            // The closest peer is at its cap; the race must dispatch to the
            // next-closest peer with a free slot, never blocking on or contacting
            // the capped head.
            let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
            let handle = ClientHandle::new(tx);
            let limiter = Arc::new(PeerInflightLimiter::new(CAP_ONE));

            let head = overlay(1);
            let next = overlay(2);
            // Saturate the head so it has no free slot at selection time.
            let _held = limiter.try_acquire(&head).expect("saturate the head");

            let address = address(0xab);
            let race = tokio::spawn(race_with_limiter(
                handle,
                Arc::clone(&limiter),
                vec![head, next],
                address,
            ));

            // The only command dispatched is to the next-closest peer: the capped
            // head was skipped at selection time, not contacted.
            match rx.recv().await.expect("a command for the free peer") {
                ClientCommand::RetrieveChunk { peer, response, .. } => {
                    assert_eq!(peer, next, "the skipped head is not contacted");
                    response
                        .send(Ok(RetrievalResult {
                            chunk: test_chunk(),
                            stamp: None,
                            peer: next,
                        }))
                        .expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            }

            let result = race.await.unwrap().expect("race resolves");
            assert_eq!(result.peer, next, "the free next-closest peer serves");
        }

        #[tokio::test]
        async fn losing_leg_releases_its_permit_on_drop() {
            // The head leg reserves a permit and then withholds; the staggered
            // second wins and the head leg is dropped. Dropping it must release
            // the head's in-flight slot, so the head is reservable again.
            let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
            let handle = ClientHandle::new(tx);
            let limiter = Arc::new(PeerInflightLimiter::new(CAP_ONE));

            let head = overlay(1);
            let second = overlay(2);
            let address = address(0xcd);

            let start = Instant::now();
            let race = tokio::spawn(race_with_limiter(
                handle,
                Arc::clone(&limiter),
                vec![head, second],
                address,
            ));

            // The head leg dispatches first and reserves the head's only slot.
            let _head_response = match rx.recv().await.expect("head command") {
                ClientCommand::RetrieveChunk { peer, response, .. } => {
                    assert_eq!(peer, head);
                    assert!(
                        !limiter.has_free_slot(&head),
                        "the in-flight head leg holds the head's slot"
                    );
                    response
                }
                other => panic!("unexpected command: {other:?}"),
            };
            // After the stagger the second candidate joins and resolves the race.
            match rx.recv().await.expect("second command after stagger") {
                ClientCommand::RetrieveChunk { peer, response, .. } => {
                    assert_eq!(peer, second);
                    response
                        .send(Ok(RetrievalResult {
                            chunk: test_chunk(),
                            stamp: None,
                            peer: second,
                        }))
                        .expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            }

            let result = race.await.unwrap().expect("race resolves");
            assert_eq!(result.peer, second, "the staggered second wins");
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "resolved within the stagger, not a per-attempt deadline"
            );
            // The losing head leg was dropped when the race resolved, releasing
            // its permit: the head's slot is free again.
            assert!(
                limiter.has_free_slot(&head),
                "the cancelled head leg released its in-flight slot on drop"
            );
            assert!(
                limiter.try_acquire(&head).is_some(),
                "the freed head slot is reservable again"
            );
        }
    }

    mod gated_fallback {
        use std::future::Future;
        use std::pin::Pin;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        use nectar_primitives::SwarmAddress;
        use vertex_swarm_api::{Au, ChunkAddress, Ledger, SwarmError, SwarmPricing, Threshold};
        use vertex_tasks::time::Instant;

        use crate::{PeerScores, PeerSelector, SettlementTrigger};

        use super::address;

        fn overlay(n: u8) -> SwarmAddress {
            SwarmAddress::from([n; 32])
        }

        struct NoScores;
        impl PeerScores for NoScores {
            fn peer_score(&self, _overlay: &SwarmAddress) -> Option<f64> {
                None
            }
        }

        struct UnitPricer;
        impl SwarmPricing for UnitPricer {
            fn price(&self, _chunk: &ChunkAddress) -> Au {
                Au::from_amount(1)
            }
            fn peer_price(&self, _peer: &SwarmAddress, _chunk: &ChunkAddress) -> Au {
                Au::from_amount(1)
            }
        }

        /// Refuses every peer at the unit price while `gated`; admits once clear.
        struct GatedLedger(Arc<AtomicBool>);
        impl Ledger for GatedLedger {
            fn balance(&self, _o: &SwarmAddress) -> Au {
                Au::ZERO
            }
            fn reserved(&self, _o: &SwarmAddress) -> Au {
                Au::ZERO
            }
            fn headroom(&self, _o: &SwarmAddress, _t: Threshold) -> Au {
                Au::from_amount(1000)
            }
            fn disconnect_line(&self, _o: &SwarmAddress) -> Au {
                if self.0.load(Ordering::SeqCst) {
                    Au::ZERO
                } else {
                    Au::from_amount(1000)
                }
            }
            fn settle_trigger(&self, _o: &SwarmAddress) -> Au {
                Au::from_amount(1000)
            }
        }

        /// `settled` resolves at once; when `drains` it clears the gate first
        /// (modelling a completed in-flight settle that drops the peer's debt
        /// under its line) and reports `true`. Otherwise it reports `false`: no
        /// settle is draining debt, so the wait loop stops without spinning.
        struct DrainSettlement {
            gate: Arc<AtomicBool>,
            drains: bool,
        }
        impl SettlementTrigger for DrainSettlement {
            fn trigger_settlement(&self, _peer: SwarmAddress) {}
            fn settled(
                &self,
                _peers: &[SwarmAddress],
            ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
                if self.drains {
                    self.gate.store(false, Ordering::SeqCst);
                }
                Box::pin(std::future::ready(self.drains))
            }
        }

        fn selector(gate: Arc<AtomicBool>, drains: bool) -> PeerSelector {
            PeerSelector::new(
                Arc::new(NoScores),
                Arc::new(GatedLedger(Arc::clone(&gate))),
                Arc::new(UnitPricer),
                Arc::new(DrainSettlement { gate, drains }),
            )
        }

        #[tokio::test]
        async fn a_recovering_gated_set_returns_the_admissible_peers() {
            // Every close peer is gated on the first order; the awaited settle
            // drains the debt (the gate clears) and the re-order returns them.
            let gate = Arc::new(AtomicBool::new(true));
            let sel = selector(Arc::clone(&gate), true);
            let deadline = Instant::now() + std::time::Duration::from_secs(5);
            let ordered = sel
                .order_or_wait(
                    vec![overlay(1), overlay(2)],
                    &ChunkAddress::zero(),
                    deadline,
                )
                .await;
            assert_eq!(
                ordered,
                vec![overlay(1), overlay(2)],
                "recovers once the settle drains"
            );
        }

        #[tokio::test]
        async fn a_fully_gated_set_with_no_draining_settle_returns_empty() {
            // The gate never clears and no settle is draining debt: the
            // no-progress guard terminates the wait with an empty result, so the
            // dispatch falls through to its generic transient error rather than
            // hanging or spinning to the far deadline.
            let gate = Arc::new(AtomicBool::new(true));
            let sel = selector(Arc::clone(&gate), false);
            let deadline = Instant::now() + std::time::Duration::from_secs(30);
            let ordered = sel
                .order_or_wait(
                    vec![overlay(1), overlay(2)],
                    &ChunkAddress::zero(),
                    deadline,
                )
                .await;
            assert!(ordered.is_empty(), "still gated, no settle draining");
        }

        #[test]
        fn an_empty_close_set_surfaces_a_generic_transient_error() {
            // What a fully gated (empty) selection falls through to is the same
            // generic transient failure a genuine no-peers/no-storer case yields:
            // retrieval a `Network` error, push a `NoStorer` error. Neither is an
            // accounting-specific variant, so the accounting concern never reaches
            // the consumer.
            let retrieval = SwarmError::network_msg("no connected peers available for retrieval");
            assert!(matches!(retrieval, SwarmError::Network { .. }));
            assert!(
                retrieval.is_retryable(),
                "a no-peers retrieval is transient"
            );

            let push = SwarmError::NoStorer {
                chunk_address: address(0xaa),
            };
            assert!(matches!(push, SwarmError::NoStorer { .. }));
            assert!(push.is_retryable(), "a no-storer push is transient");
        }
    }
}
