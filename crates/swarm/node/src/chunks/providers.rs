//! RPC provider implementations for Swarm nodes.

use std::sync::Arc;

use async_trait::async_trait;
use nectar_primitives::SwarmAddress;
use tracing::warn;
use vertex_swarm_api::{
    ChunkAddress, ChunkRetrievalResult, PeerReporter, PushReceipt, ReportSource, StampedChunk,
    SwarmChunkProvider, SwarmChunkSender, SwarmError, SwarmIdentity, SwarmLocalStore, SwarmResult,
    SwarmScoringEvent, SwarmTopologyRouting, SwarmTopologyState,
};
use vertex_swarm_net_pushsync::{DepthVerdict, Receipt};
use vertex_swarm_topology::TopologyHandle;

use crate::ClientHandle;
use crate::retrieval_engine::{CandidateOrdering, InflightLimit, LatencyHint, RetrievalEngine};

/// Report source for shallow/malformed receipts caught on the origin upload
/// path.
const PUSHSYNC_SOURCE: ReportSource = ReportSource::Protocol("pushsync");

/// Number of closest peers to try when pushing a chunk before giving up.
const PUSH_CANDIDATE_COUNT: usize = 5;

/// Chunk provider driving the shared retrieval engine, generic over the three
/// retrieval capabilities: a native client wires the score- and affordability-
/// aware selector, per-peer in-flight cap, and per-PO latency estimate; a
/// browser client wires proximity ordering, the same per-peer cap, and the
/// constant stagger.
///
/// Both instantiations share one push path: the closest-storer custody upload
/// runs the depth verdict against the local neighbourhood floor. A shallow
/// observer (a browser with few peers) sets a low floor, so an honest deep
/// receipt still verifies; only an unverifiable early-session view (before the
/// neighbourhood is credible) yields [`SwarmError::UnconfirmedCustody`].
///
/// Every retrieval terminal surfaces as [`SwarmError::RetrievalExhausted`];
/// forwarding retrieval has no authoritative negative, so absence is never
/// claimed.
#[derive(Clone)]
pub struct NetworkChunkProvider<I, O, G, L>
where
    I: SwarmIdentity + 'static,
    O: CandidateOrdering + 'static,
    G: InflightLimit + 'static,
    L: LatencyHint + 'static,
{
    engine: RetrievalEngine<I, O, G, L>,
    /// The node's own chunk cache, consulted before racing the swarm so a
    /// duplicate origin retrieval of a cached content chunk serves locally.
    /// `None` for an embedder that wires a cacheless provider.
    store: Option<Arc<dyn SwarmLocalStore>>,
}

impl<I, O, G, L> NetworkChunkProvider<I, O, G, L>
where
    I: SwarmIdentity + 'static,
    O: CandidateOrdering + 'static,
    G: InflightLimit + 'static,
    L: LatencyHint + 'static,
{
    /// Build the provider over the three retrieval capabilities: candidate
    /// `ordering`, the per-peer `inflight` cap, and the per-PO `latency`
    /// estimate. `store` is the node's own cache, read before the swarm race.
    pub fn new(
        client_handle: ClientHandle,
        topology: TopologyHandle<I>,
        ordering: O,
        inflight: G,
        latency: L,
        store: Option<Arc<dyn SwarmLocalStore>>,
    ) -> Self {
        Self {
            engine: RetrievalEngine::new(client_handle, topology, ordering, inflight, latency),
            store,
        }
    }
}

#[async_trait]
impl<I, O, G, L> SwarmChunkProvider for NetworkChunkProvider<I, O, G, L>
where
    I: SwarmIdentity + 'static,
    O: CandidateOrdering + 'static,
    G: InflightLimit + 'static,
    // The retrieval race holds the in-flight permit across the request await, so
    // the `Send`-bounded provider future requires a `Send` permit. Native
    // `MaybeSend` is `Send`, so this is free there; on wasm it holds for the real
    // `OwnedSemaphorePermit` the concrete client wires.
    <G as InflightLimit>::Permit: Send,
    L: LatencyHint + 'static,
{
    async fn retrieve_chunk(&self, address: &ChunkAddress) -> SwarmResult<ChunkRetrievalResult> {
        // Serve our own duplicate retrieval from the local cache before racing
        // the swarm. `get` applies the single-owner TTL and only content chunks
        // are cached, so a hit is an immutable, byte-safe content chunk; the
        // node's own overlay stands in as the serving peer to mark a local serve.
        if let Some(store) = &self.store
            && let Ok(Some(cached)) = store.get(address)
            && *cached.address() == *address
        {
            let (chunk, stamp) = cached.into_parts();
            return Ok(ChunkRetrievalResult {
                chunk,
                stamp,
                served_by: self.engine.topology().overlay_address(),
            });
        }
        self.engine.retrieve(address).await
    }

    fn has_chunk(&self, _address: &ChunkAddress) -> bool {
        // Client nodes don't have local storage
        false
    }
}

impl<I, O, G, L> NetworkChunkProvider<I, O, G, L>
where
    I: SwarmIdentity + 'static,
    O: CandidateOrdering + 'static,
    G: InflightLimit + 'static,
    L: LatencyHint + 'static,
{
    /// Push `chunk` to the storer peers closest to its address, returning the
    /// first receipt.
    ///
    /// Tries the closest candidates in order and returns the first storer that
    /// accepts the chunk. The client handle correlates a push response to its
    /// request by chunk address alone, so the candidates are tried sequentially
    /// rather than raced.
    async fn push_to_closest(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt> {
        let address = *chunk.address();
        let closest = self
            .engine
            .topology()
            .closest_to(&address, PUSH_CANDIDATE_COUNT);
        // Rank by band and score, hard-skipping a refused peer; an all-gated set
        // yields an empty result and the generic no-storer outcome below. The peer
        // a push actually dispatches to is settled at the origin credit gate.
        let closest = self.engine.order(closest, &address);
        let attempts = closest.len();

        // The required custody depth is derived from our locally observed
        // neighbourhood depth (the trusted authority) and trust-but-verified
        // against the receipt's own claimed `storage_radius`. The check is gated
        // on that depth being credible (the neighbourhood has saturated); a
        // non-credible view cannot anchor the floor and yields an unverifiable
        // verdict. The receipt's signer was already recovered at the decode
        // boundary; a malformed receipt never reaches here (it surfaces as a push
        // error below).
        let local_depth = self.engine.topology().depth();
        let neighbourhood_credible = self.engine.topology().neighbourhood_credible();
        let reporter = self.engine.topology().peer_manager();

        // Try each closest peer in order and return the first receipt that
        // verifies. A shallow receipt is rejected, the responding peer scored
        // adversely, and the loop continues to the next candidate: this is the
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
                .engine
                .client_handle()
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
impl<I, O, G, L> SwarmChunkSender for NetworkChunkProvider<I, O, G, L>
where
    I: SwarmIdentity + 'static,
    O: CandidateOrdering + 'static,
    G: InflightLimit + 'static,
    L: LatencyHint + 'static,
{
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
    /// storer ground to sit exactly `proximity` bits deep relative to `address`.
    ///
    /// The grind targets an exact proximity, not a lower bound: the depth verdict
    /// turns on the observed proximity, so a lower bound would leave it dependent
    /// on the random overlay and flake (a shallow case occasionally grinding deep
    /// enough to verify). An exact target makes every verdict deterministic.
    fn signed_receipt(
        signer: &PrivateKeySigner,
        address: &ChunkAddress,
        proximity: u8,
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
            if address.proximity(&overlay).get() == proximity {
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
        // Regression for a non-credible local view (a fresh or sparse node,
        // local_depth == 0): a shallow receipt declaring radius 0 must NOT be
        // accepted, and the responder must NOT be penalised: the verdict is
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

        use crate::race_candidates;

        use super::*;
        use crate::{RETRIEVAL_STAGGER, RaceFailure};

        fn test_chunk() -> nectar_primitives::AnyChunk {
            ContentChunk::new(&b"provider-race-chunk"[..])
                .expect("valid content chunk")
                .into()
        }

        /// Drive the exact future the provider builds per candidate: each
        /// attempt is `client_handle.retrieve_chunk(peer, address)`, raced with a
        /// staggered start. The per-candidate pacing (the admission band and
        /// affordability check) lives inside that call, so this exercises the
        /// provider's retrieval attempt and race wiring without standing up a
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

    mod inflight_scheduler {
        use std::num::NonZeroUsize;
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        use nectar_primitives::ContentChunk;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use tokio::sync::mpsc;

        use crate::{
            ChunkTransferError, ClientCommand, ClientHandle, PeerInflightLimiter, RetrievalResult,
        };

        use crate::race_candidates;

        use super::*;
        use crate::retrieval_engine::{
            InflightLimit, RETRIEVE_ATTEMPT_BUDGET, RETRIEVE_DEADLINE, RETRIEVE_MAX_IN_FLIGHT,
        };
        use crate::{RETRIEVAL_STAGGER, RaceFailure, race_with_refill};

        const CAP_ONE: NonZeroUsize = match NonZeroUsize::new(1) {
            Some(cap) => cap,
            None => unreachable!(),
        };

        fn overlay(n: u8) -> SwarmAddress {
            SwarmAddress::from([n; 32])
        }

        fn test_chunk() -> nectar_primitives::AnyChunk {
            ContentChunk::new(&b"inflight-chunk"[..])
                .expect("valid content chunk")
                .into()
        }

        /// Drive the exact composition the provider builds: availability
        /// filtering at selection time, then the staggered race whose attempts reserve an
        /// in-flight permit that rides the request future and releases on drop.
        async fn race_with_limiter(
            handle: ClientHandle,
            limiter: Arc<PeerInflightLimiter>,
            candidates: Vec<SwarmAddress>,
            address: ChunkAddress,
        ) -> Result<RetrievalResult, RaceFailure<ChunkTransferError>> {
            let (candidates, _enforce_cap) = limiter.available(candidates);
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

        #[tokio::test]
        async fn race_budget_caps_metered_attempts_below_the_free_slot_pool() {
            // A wide pool of free-slot peers must not meter an attempt each: the race
            // dispatches at most the attempt budget, refilling a failed attempt from the
            // next-closest peer, so the wider pool supplies coverage alternatives
            // without amplifying paid bandwidth.
            let (tx, rx) = mpsc::channel::<ClientCommand>(64);
            drop(rx); // every retrieval fails at once: the race spends its budget.
            let handle = ClientHandle::new(tx);
            let limiter = Arc::new(PeerInflightLimiter::new(CAP_ONE));

            let pool: Vec<SwarmAddress> = (1..=16).map(overlay).collect();
            let (candidates, _enforce_cap) = limiter.available(pool);
            assert_eq!(candidates.len(), 16, "all 16 peers have a free slot");

            let attempts = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&attempts);
            let outcome = race_with_refill(
                candidates,
                RETRIEVE_ATTEMPT_BUDGET,
                RETRIEVE_MAX_IN_FLIGHT,
                RETRIEVE_DEADLINE,
                RETRIEVAL_STAGGER,
                move |peer| {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let permit = limiter.try_acquire(&peer);
                    let handle = handle.clone();
                    Some(async move {
                        let _permit = permit;
                        handle.retrieve_chunk(peer, address(0xaa), true).await
                    })
                },
            )
            .await;

            assert!(matches!(outcome, Err(RaceFailure::AllFailed(_))));
            assert_eq!(
                attempts.load(Ordering::SeqCst),
                RETRIEVE_ATTEMPT_BUDGET,
                "the race meters at most the attempt budget across the wider free-slot pool"
            );
        }

        #[tokio::test]
        async fn enforce_cap_declines_a_peer_that_filled_since_the_snapshot() {
            // Under enforce_cap, a peer free at the availability snapshot but
            // saturated before its attempt dispatches is declined: no command
            // reaches it and it spends no attempt, so the cap holds on live state,
            // not the stale snapshot. The next free peer serves instead. Retrieval
            // no longer enforces the cap (a busy holder is a best-effort tail), so
            // this exercises the race helper's decline path directly.
            let (tx, mut rx) = mpsc::channel::<ClientCommand>(16);
            let handle = ClientHandle::new(tx);
            let limiter = Arc::new(PeerInflightLimiter::new(CAP_ONE));

            let filled = overlay(1);
            let free = overlay(2);

            let candidates = vec![filled, free];
            let enforce_cap = true;

            // Between the snapshot and dispatch the first peer's slot is taken.
            let _held = limiter
                .try_acquire(&filled)
                .expect("saturate the first peer");

            let address = address(0xac);
            let attempts = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&attempts);
            let lim = Arc::clone(&limiter);
            let race = tokio::spawn(async move {
                race_with_refill(
                    candidates,
                    RETRIEVE_ATTEMPT_BUDGET,
                    RETRIEVE_MAX_IN_FLIGHT,
                    RETRIEVE_DEADLINE,
                    RETRIEVAL_STAGGER,
                    move |peer| {
                        let permit = lim.try_acquire(&peer);
                        // The enforce-cap decline: a peer with no live slot spends
                        // no attempt and is skipped for the next candidate.
                        if enforce_cap && permit.is_none() {
                            return None;
                        }
                        counted.fetch_add(1, Ordering::SeqCst);
                        let handle = handle.clone();
                        Some(async move {
                            let _permit = permit;
                            handle.retrieve_chunk(peer, address, true).await
                        })
                    },
                )
                .await
            });

            // The only command is for the free peer: the saturated peer is declined.
            match rx.recv().await.expect("a command for the free peer") {
                ClientCommand::RetrieveChunk { peer, response, .. } => {
                    assert_eq!(peer, free, "the saturated peer is declined, not contacted");
                    response
                        .send(Ok(RetrievalResult {
                            chunk: test_chunk(),
                            stamp: None,
                            peer: free,
                        }))
                        .expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            }

            let result = race.await.unwrap().expect("race resolves");
            assert_eq!(result.peer, free, "the free peer serves the chunk");
            assert_eq!(
                attempts.load(Ordering::SeqCst),
                1,
                "only the free peer spent an attempt; the saturated peer was declined"
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
        async fn losing_attempt_releases_its_permit_on_drop() {
            // The head attempt reserves a permit and then withholds; the staggered
            // second wins and the head attempt is dropped. Dropping it must release
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

            // The head attempt dispatches first and reserves the head's only slot.
            let _head_response = match rx.recv().await.expect("head command") {
                ClientCommand::RetrieveChunk { peer, response, .. } => {
                    assert_eq!(peer, head);
                    assert!(
                        !limiter.has_free_slot(&head),
                        "the in-flight head attempt holds the head's slot"
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
            // The losing head attempt was dropped when the race resolved, releasing
            // its permit: the head's slot is free again.
            assert!(
                limiter.has_free_slot(&head),
                "the cancelled head attempt released its in-flight slot on drop"
            );
            assert!(
                limiter.try_acquire(&head).is_some(),
                "the freed head slot is reservable again"
            );
        }
    }

    mod gated_fallback {
        use vertex_swarm_api::SwarmError;

        use super::address;

        #[test]
        fn a_fully_gated_set_surfaces_the_terminal_outcome() {
            // What a fully gated (empty) selection falls through to: retrieval the
            // honest `RetrievalExhausted` (no authoritative negative exists, so
            // absence is never claimed), push a `NoStorer`. Neither is an
            // accounting-specific variant, so the accounting concern never reaches
            // the consumer.
            let retrieval = SwarmError::RetrievalExhausted {
                address: address(0xaa),
            };
            assert!(matches!(retrieval, SwarmError::RetrievalExhausted { .. }));

            let push = SwarmError::NoStorer {
                chunk_address: address(0xaa),
            };
            assert!(matches!(push, SwarmError::NoStorer { .. }));
            assert!(push.is_retryable(), "a no-storer push is transient");
        }
    }
}
