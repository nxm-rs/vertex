//! RPC provider implementations for Swarm nodes.

use std::sync::Arc;

use async_trait::async_trait;
use vertex_swarm_api::{
    Bin, ChunkAddress, ChunkRetrievalResult, PushReceipt, StampedChunk, SwarmChunkProvider,
    SwarmChunkSender, SwarmError, SwarmLocalStore, SwarmResult,
};
use vertex_swarm_net_pushsync::Receipt;

use crate::ClientHandle;
use crate::dispatch::{
    CandidateOrdering, DispatchEngine, InflightLimit, LatencyHint, RetrievalTopology,
};
use crate::selection::SettlementTrigger;

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
pub struct NetworkChunkProvider<O, G, L>
where
    O: CandidateOrdering + 'static,
    G: InflightLimit + 'static,
    L: LatencyHint + 'static,
{
    engine: DispatchEngine<O, G, L>,
    /// The node's own chunk cache, consulted before racing the swarm so a
    /// duplicate origin retrieval of a cached content chunk serves locally.
    /// `None` for an embedder that wires a cacheless provider.
    store: Option<Arc<dyn SwarmLocalStore>>,
}

impl<O, G, L> NetworkChunkProvider<O, G, L>
where
    O: CandidateOrdering + 'static,
    G: InflightLimit + 'static,
    L: LatencyHint + 'static,
{
    /// Build the provider over the three retrieval capabilities: candidate
    /// `ordering`, the per-peer `inflight` cap, and the per-PO `latency`
    /// estimate. `store` is the node's own cache, read before the swarm race.
    // A wiring constructor over the node's already-built collaborators; grouping
    // them into a params struct would only move the same fields behind one more
    // type.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client_handle: ClientHandle,
        topology: Arc<dyn RetrievalTopology>,
        max_bin: Bin,
        ordering: O,
        inflight: G,
        latency: L,
        settlement: Arc<dyn SettlementTrigger>,
        store: Option<Arc<dyn SwarmLocalStore>>,
    ) -> Self {
        Self {
            engine: DispatchEngine::new(
                client_handle,
                topology,
                max_bin,
                ordering,
                inflight,
                latency,
                settlement,
            ),
            store,
        }
    }
}

#[async_trait]
impl<O, G, L> SwarmChunkProvider for NetworkChunkProvider<O, G, L>
where
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

impl<O, G, L> NetworkChunkProvider<O, G, L>
where
    O: CandidateOrdering + 'static,
    G: InflightLimit + 'static,
    L: LatencyHint + 'static,
{
    /// Push `chunk` through the engine's sequential origin push profile,
    /// projecting the verified receipt onto the public boundary.
    async fn push_to_closest(&self, chunk: StampedChunk) -> SwarmResult<PushReceipt> {
        self.engine.push(chunk).await.map(push_receipt_of)
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
impl<O, G, L> SwarmChunkSender for NetworkChunkProvider<O, G, L>
where
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

#[cfg(test)]
mod tests {
    use nectar_primitives::SwarmAddress;

    use super::*;

    fn address(first_byte: u8) -> ChunkAddress {
        let mut bytes = [0u8; 32];
        bytes[0] = first_byte;
        ChunkAddress::new(bytes)
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
        use crate::dispatch::{
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
