//! The concrete network forwarder: the accounting-carrying facade over the
//! dispatch engine's relay profile.
//!
//! The accounting-free forwarder seam (the [`Forwarder`] trait, [`StubForwarder`],
//! the [`ForwardedChunk`] / [`ForwardedReceipt`] carriers, [`ForwardError`], and
//! the [`closer_candidates`] selector) lives in `vertex-swarm-client-behaviour`;
//! the relay walk itself (strictly-closer candidates, sequential legs,
//! commit-on-verify two-leg accounting, the shared per-peer in-flight ledger)
//! is the engine's relay role in `crate::dispatch`. [`NetworkForwarder`] stays
//! here because it couples the engine to the client accounting instance, which
//! neither the behaviour crate nor the engine may name.
//!
//! [`closer_candidates`]: vertex_swarm_client_behaviour::closer_candidates

use std::sync::Arc;

use futures::future::BoxFuture;
use nectar_primitives::ChunkAddress;
use vertex_swarm_api::{CommitOnWrite, SwarmClientAccounting};
use vertex_swarm_client_behaviour::{ForwardError, ForwardedChunk, ForwardedReceipt, Forwarder};
use vertex_swarm_primitives::{OverlayAddress, StampedChunk};

use crate::dispatch::{CandidateOrdering, DispatchEngine, InflightLimit, LatencyHint};

/// The relay facade: the engine's relay role bound to the client accounting.
///
/// Delegates the strictly-closer walks to
/// [`DispatchEngine::relay_retrieve`]/[`DispatchEngine::relay_push`] and boxes
/// the returned un-applied provide action for the handler's deferred commit.
pub(crate) struct NetworkForwarder<A, O, G, L>
where
    O: CandidateOrdering,
    G: InflightLimit,
    L: LatencyHint,
{
    engine: DispatchEngine<O, G, L>,
    /// Two-leg prepare/apply accounting for the relay and terminal serves.
    accounting: Arc<A>,
}

impl<A, O, G, L> NetworkForwarder<A, O, G, L>
where
    O: CandidateOrdering,
    G: InflightLimit,
    L: LatencyHint,
{
    /// Bind the engine's relay role to the client accounting.
    pub(crate) fn new(engine: DispatchEngine<O, G, L>, accounting: Arc<A>) -> Self {
        Self { engine, accounting }
    }
}

impl<A, O, G, L> Forwarder for NetworkForwarder<A, O, G, L>
where
    A: SwarmClientAccounting + Send + Sync + 'static,
    O: CandidateOrdering + Clone + 'static,
    G: InflightLimit + Clone + 'static,
    // The relay leg holds the shared per-peer in-flight permit across its
    // await, so the boxed `Send` future requires a `Send` permit.
    <G as InflightLimit>::Permit: Send,
    L: LatencyHint + Clone + 'static,
{
    fn retrieve(
        &self,
        address: ChunkAddress,
        exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<ForwardedChunk, ForwardError>> {
        let engine = self.engine.clone();
        let accounting = Arc::clone(&self.accounting);

        Box::pin(async move {
            let (result, provide) = engine
                .relay_retrieve(&*accounting, address, exclude)
                .await?;
            Ok(ForwardedChunk {
                chunk: result.chunk,
                stamp: result.stamp,
                provide: Box::new(provide),
            })
        })
    }

    fn push(
        &self,
        chunk: StampedChunk,
        exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<ForwardedReceipt, ForwardError>> {
        let engine = self.engine.clone();
        let accounting = Arc::clone(&self.accounting);

        Box::pin(async move {
            let (receipt, provide) = engine.relay_push(&*accounting, chunk, exclude).await?;
            Ok(ForwardedReceipt {
                receipt,
                provide: Box::new(provide),
            })
        })
    }

    fn prepare_serve(
        &self,
        peer: OverlayAddress,
        address: &ChunkAddress,
    ) -> Result<Box<dyn CommitOnWrite>, ForwardError> {
        let provide = self
            .accounting
            .prepare_provide_chunk(peer, address)
            .map_err(|_| ForwardError::AccountingRefused)?;
        Ok(Box::new(provide))
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::Arc;

    use std::sync::Mutex;

    use alloy_primitives::{B256, Signature};
    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use nectar_postage::Stamp;
    use nectar_primitives::{AnyChunk, ContentChunk, NetworkId, Nonce, compute_overlay};
    use tokio::sync::mpsc;
    use vertex_swarm_accounting::{
        Accounting, ClientAccounting, DefaultAccountingConfig, FixedPricer,
    };
    use vertex_swarm_api::{
        Au, Bin, PeerReporter, ReportSource, StorageRadius, SwarmAccounting, SwarmPeerAccounting,
        SwarmPricing, SwarmScoringEvent,
    };
    use vertex_swarm_identity::Identity;
    use vertex_swarm_net_pushsync::{Receipt, WireReceipt};
    use vertex_swarm_spec::Spec;
    use vertex_swarm_test_utils::{MockTopology, test_identity_arc};

    use super::*;
    use crate::dispatch::{NoLatencyHint, ProximityOnly, RetrievalTopology};
    use crate::inflight::PeerInflightLimiter;
    use crate::selection::SettlementTrigger;
    use crate::{ClientCommand, ClientHandle, RetrievalResult};

    const TEST_NET: NetworkId = NetworkId::MAINNET;

    /// A reporter that records every report so tests can assert the scoring side
    /// effect of a rejected receipt.
    #[derive(Default)]
    struct RecordingReporter {
        reports: Mutex<Vec<(OverlayAddress, SwarmScoringEvent, ReportSource)>>,
    }

    impl PeerReporter for RecordingReporter {
        fn report_peer(
            &self,
            overlay: &OverlayAddress,
            event: SwarmScoringEvent,
            source: ReportSource,
        ) {
            self.reports.lock().unwrap().push((*overlay, event, source));
        }
    }

    impl RecordingReporter {
        /// Return the single recorded report, asserting exactly one exists.
        fn single(&self) -> (OverlayAddress, SwarmScoringEvent, ReportSource) {
            let reports = self.reports.lock().unwrap();
            assert_eq!(reports.len(), 1, "expected exactly one report");
            *reports.first().expect("one report")
        }

        /// True when no report was recorded.
        fn is_empty(&self) -> bool {
            self.reports.lock().unwrap().is_empty()
        }
    }

    /// Settlement trigger that ignores every drive: relay walks never settle.
    struct NoSettle;

    impl SettlementTrigger for NoSettle {
        fn trigger_settlement(&self, _peer: OverlayAddress) {}
    }

    /// The local overlay every test pins on its mock topology, so the
    /// strictly-closer gate is deterministic against ground candidates.
    const LOCAL: [u8; 32] = [0xee; 32];

    /// Build the engine's relay role over `topo` for the facade under test.
    fn engine_over(
        topo: MockTopology,
        handle: ClientHandle,
    ) -> DispatchEngine<ProximityOnly, Arc<PeerInflightLimiter>, NoLatencyHint> {
        DispatchEngine::new(
            handle,
            Arc::new(topo.with_overlay(OverlayAddress::from(LOCAL))) as Arc<dyn RetrievalTopology>,
            Bin::new(31).unwrap(),
            ProximityOnly,
            Arc::new(PeerInflightLimiter::new(NonZeroUsize::new(4).unwrap())),
            NoLatencyHint,
            Arc::new(NoSettle),
        )
    }

    /// Sign a custody receipt over the 32-byte chunk address (the wire format)
    /// with `signer`, grinding the nonce so the storer's derived overlay shares
    /// at least `min_depth` leading bits with `address` (i.e.
    /// `PO(storer, address) >= min_depth`). Returns the wire receipt and the
    /// storer's overlay, so a test controls exactly how deep the storer sits
    /// relative to the chunk. The downstream handler's decode boundary recovers
    /// the storer; tests resolve the push command with this wire receipt.
    fn signed_receipt_at_depth(
        signer: &PrivateKeySigner,
        address: &ChunkAddress,
        min_depth: u8,
        storage_radius: StorageRadius,
    ) -> (WireReceipt, OverlayAddress) {
        let eth = signer.address();
        // The signature is over the address only and is independent of the
        // nonce, so sign once and grind the nonce purely for overlay depth.
        // Depths used in tests are small, so this terminates quickly.
        let signature = signer.sign_message_sync(address.as_bytes()).expect("sign");
        let mut counter = 0u64;
        loop {
            let mut nonce_bytes = [0u8; 32];
            nonce_bytes[..8].copy_from_slice(&counter.to_le_bytes());
            let nonce = Nonce::from(nonce_bytes);
            let overlay = compute_overlay(&eth, TEST_NET, &nonce);
            if address.proximity(&overlay).get() >= min_depth {
                return (
                    WireReceipt::new(*address, signature, nonce, storage_radius),
                    overlay,
                );
            }
            counter += 1;
        }
    }

    /// Reconstruct a [`Receipt`] from a wire receipt, as the decode boundary
    /// does, so a test can build the value a forwarder relays.
    fn reconstructed(wire: WireReceipt) -> Receipt {
        Receipt::reconstruct(wire, TEST_NET).expect("test receipt reconstructs")
    }

    /// A stamped content chunk and its content-derived address.
    fn stamped() -> StampedChunk {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        let stamp = Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig);
        let chunk: AnyChunk = ContentChunk::new(&b"forwarded payload"[..])
            .expect("valid content chunk")
            .into();
        StampedChunk::new(chunk, stamp)
    }

    /// Build an overlay sharing `leading_bits` leading bits with `address`, so
    /// its proximity to the address is exactly `leading_bits` (the next bit is
    /// flipped). Used to place a peer at a controlled distance from the target.
    fn overlay_at_proximity(address: &ChunkAddress, leading_bits: usize) -> OverlayAddress {
        let mut bytes = address.0.0;
        // Flip the bit immediately after the shared prefix so the proximity is
        // exactly `leading_bits`: the first differing bit caps proximity.
        let byte = leading_bits / 8;
        let bit = 7 - (leading_bits % 8);
        if let Some(b) = bytes.get_mut(byte) {
            *b ^= 1 << bit;
        }
        OverlayAddress::from(bytes)
    }

    type TestAccounting = ClientAccounting<
        Arc<Accounting<DefaultAccountingConfig, Arc<Identity>>>,
        FixedPricer<Spec>,
    >;

    fn accounting() -> Arc<TestAccounting> {
        let bandwidth = Arc::new(Accounting::new(
            DefaultAccountingConfig::default(),
            test_identity_arc(),
        ));
        let pricer = FixedPricer::new(10_000, vertex_swarm_spec::init_mainnet());
        Arc::new(ClientAccounting::new(bandwidth, pricer))
    }

    /// Drive a forwarder future to completion while answering the single
    /// outbound command it emits with `answer`.
    async fn drive_one_command<F, T>(
        mut rx: mpsc::Receiver<ClientCommand>,
        fut: F,
        answer: impl FnOnce(ClientCommand),
    ) -> T
    where
        F: std::future::Future<Output = T>,
    {
        let driver = async {
            if let Some(cmd) = rx.recv().await {
                answer(cmd);
            }
        };
        let (result, ()) = tokio::join!(fut, driver);
        result
    }

    #[tokio::test]
    async fn retrieve_relays_verifies_and_accounts_both_legs() {
        let chunk = stamped();
        let address = *chunk.address();
        let requester = overlay_at_proximity(&address, 2);
        let closer = overlay_at_proximity(&address, 16);

        let acct = accounting();
        let topo = MockTopology::default().with_closest(vec![closer]);
        let (tx, rx) = mpsc::channel::<ClientCommand>(4);

        let provide_price = acct.pricing().peer_price(&requester, &address);
        let receive_price = acct.pricing().peer_price(&closer, &address);
        assert!(
            provide_price > receive_price,
            "the requester is farther than the closer peer, so the forwarder earns the spread"
        );

        let forwarder =
            NetworkForwarder::new(engine_over(topo, ClientHandle::new(tx)), Arc::clone(&acct));

        let (chunk_for_answer, stamp_for_answer) = chunk.clone().into_parts();
        let got = drive_one_command(
            rx,
            forwarder.retrieve(address, requester),
            move |cmd| match cmd {
                ClientCommand::RetrieveChunk {
                    peer,
                    address: requested,
                    response,
                    originated,
                } => {
                    assert!(!originated, "a relay leg is never an origin request");
                    assert_eq!(peer, closer, "the upstream leg targets the closer peer");
                    assert_eq!(requested, address);
                    response
                        .send(Ok(RetrievalResult {
                            chunk: chunk_for_answer,
                            stamp: Some(stamp_for_answer),
                            peer: closer,
                        }))
                        .expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            },
        )
        .await;

        let forwarded = got.expect("relay succeeds");
        assert_eq!(
            *forwarded.chunk.address(),
            address,
            "the relayed chunk is verified"
        );

        // The downstream leg is committed inside the walk, so the closer peer
        // is already owed receive_price. The upstream `provide` is returned
        // un-applied: until it is committed (which the handler does after a
        // successful wire write) the requester owes nothing.
        assert_eq!(
            acct.accounting().for_peer(requester).balance(),
            Au::ZERO,
            "the upstream credit is deferred until the wire write"
        );
        assert_eq!(
            acct.accounting().for_peer(closer).balance(),
            Au::ZERO - receive_price
        );

        // Commit the upstream leg as the handler would after writing the chunk
        // back: now the requester owes us provide_price.
        forwarded.provide.apply_boxed();
        assert_eq!(
            acct.accounting().for_peer(requester).balance(),
            provide_price
        );
        assert_eq!(
            acct.accounting().for_peer(closer).balance(),
            Au::ZERO - receive_price
        );
    }

    #[tokio::test]
    async fn retrieve_dropping_provide_releases_upstream_and_keeps_downstream() {
        // A wire-write failure: the handler drops the un-applied provide action
        // instead of committing it. The requester must not be charged, and the
        // downstream leg (already committed) must remain.
        let chunk = stamped();
        let address = *chunk.address();
        let requester = overlay_at_proximity(&address, 2);
        let closer = overlay_at_proximity(&address, 16);

        let acct = accounting();
        let topo = MockTopology::default().with_closest(vec![closer]);
        let (tx, rx) = mpsc::channel::<ClientCommand>(4);

        let receive_price = acct.pricing().peer_price(&closer, &address);
        let forwarder =
            NetworkForwarder::new(engine_over(topo, ClientHandle::new(tx)), Arc::clone(&acct));

        let (chunk_for_answer, stamp_for_answer) = chunk.clone().into_parts();
        let forwarded =
            drive_one_command(
                rx,
                forwarder.retrieve(address, requester),
                move |cmd| match cmd {
                    ClientCommand::RetrieveChunk { response, .. } => {
                        response
                            .send(Ok(RetrievalResult {
                                chunk: chunk_for_answer,
                                stamp: Some(stamp_for_answer),
                                peer: closer,
                            }))
                            .expect("receiver alive");
                    }
                    other => panic!("unexpected command: {other:?}"),
                },
            )
            .await
            .expect("relay succeeds");

        // Simulate the handler's wire-write failure: drop the provide action.
        drop(forwarded.provide);

        // The requester was never charged; the downstream leg stands.
        assert_eq!(
            acct.accounting().for_peer(requester).balance(),
            Au::ZERO,
            "dropping the un-applied provide leg charges the requester nothing"
        );
        assert_eq!(
            acct.accounting().for_peer(closer).balance(),
            Au::ZERO - receive_price
        );
    }

    #[tokio::test]
    async fn push_relays_receipt_verbatim_and_accounts_both_legs() {
        let chunk = stamped();
        let address = *chunk.address();
        let pusher = overlay_at_proximity(&address, 2);
        let closer = overlay_at_proximity(&address, 16);

        let acct = accounting();
        let reporter = Arc::new(RecordingReporter::default());
        let topo = MockTopology::default()
            .with_closest(vec![closer])
            .with_reporter(Arc::clone(&reporter) as Arc<dyn PeerReporter>);
        let (tx, rx) = mpsc::channel::<ClientCommand>(4);

        let provide_price = acct.pricing().peer_price(&pusher, &address);
        let receive_price = acct.pricing().peer_price(&closer, &address);

        let forwarder =
            NetworkForwarder::new(engine_over(topo, ClientHandle::new(tx)), Arc::clone(&acct));

        // A real storer receipt signed by a key whose overlay sits 8 bits deep
        // relative to the chunk; the mock depth is 0 so any deep-enough receipt
        // passes. The receipt must be relayed VERBATIM. The downstream decode
        // boundary turns the wire receipt into a `Receipt`; we model that here by
        // reconstructing it before answering the push command.
        let signer = PrivateKeySigner::random();
        let (storer_receipt, storer_overlay) = signed_receipt_at_depth(
            &signer,
            &address,
            8,
            StorageRadius::new(Bin::new(8).unwrap()),
        );
        let expected = storer_receipt.clone();
        let answer = reconstructed(storer_receipt);
        let got = drive_one_command(
            rx,
            forwarder.push(chunk.clone(), pusher),
            move |cmd| match cmd {
                ClientCommand::PushChunk {
                    peer,
                    address: requested,
                    chunk: pushed,
                    response,
                    originated,
                } => {
                    assert!(!originated, "a relay leg is never an origin push");
                    assert_eq!(peer, closer);
                    assert_eq!(requested, address);
                    assert_eq!(*pushed.address(), address);
                    response.send(Ok(answer)).expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            },
        )
        .await;

        let forwarded = got.expect("relay succeeds");
        // The receipt is relayed verbatim: the recovered storer matches and the
        // wire bytes reproduce the storer's own signature, nonce, and radius.
        assert_eq!(forwarded.receipt.storer, storer_overlay);
        assert_eq!(forwarded.receipt.to_wire(), expected);

        // Downstream committed; upstream deferred until the wire write.
        assert_eq!(acct.accounting().for_peer(pusher).balance(), Au::ZERO);
        assert_eq!(
            acct.accounting().for_peer(closer).balance(),
            Au::ZERO - receive_price
        );

        forwarded.provide.apply_boxed();
        assert_eq!(acct.accounting().for_peer(pusher).balance(), provide_price);
        assert_eq!(
            acct.accounting().for_peer(closer).balance(),
            Au::ZERO - receive_price
        );

        // A verified receipt is not a violation: nothing was reported.
        assert!(reporter.is_empty());
    }

    #[tokio::test]
    async fn push_rejects_shallow_receipt_reports_signer_and_leaks_no_reservation() {
        // A forwarder must never launder a shallow custody receipt. The
        // downstream peer returns a receipt whose signer is too shallow for the
        // chunk; the forward fails with `ShallowReceipt`, the downstream peer is
        // scored adversely for invalid data, and neither leg commits.
        let chunk = stamped();
        let address = *chunk.address();
        let pusher = overlay_at_proximity(&address, 2);
        let closer = overlay_at_proximity(&address, 16);

        let acct = accounting();
        // Require depth 12: local depth 12 and a deep wire radius below.
        let reporter = Arc::new(RecordingReporter::default());
        let topo = MockTopology::default()
            .with_closest(vec![closer])
            .with_depth(12)
            .with_reporter(Arc::clone(&reporter) as Arc<dyn PeerReporter>);
        let (tx, rx) = mpsc::channel::<ClientCommand>(4);

        let forwarder =
            NetworkForwarder::new(engine_over(topo, ClientHandle::new(tx)), Arc::clone(&acct));

        // The signer's overlay is only 0..a few bits deep. The receipt claims a
        // shallow radius (8) that does not raise the bar, so the local floor
        // (depth 12 minus tolerance) is what rejects it. The signer is far too
        // shallow.
        let signer = PrivateKeySigner::random();
        let (shallow, _signer_overlay) = signed_receipt_at_depth(
            &signer,
            &address,
            0,
            StorageRadius::new(Bin::new(8).unwrap()),
        );
        let answer = reconstructed(shallow);

        let err = drive_one_command(
            rx,
            forwarder.push(chunk.clone(), pusher),
            move |cmd| match cmd {
                ClientCommand::PushChunk { response, .. } => {
                    response.send(Ok(answer)).expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            },
        )
        .await
        .expect_err("a shallow receipt is never relayed");
        assert!(matches!(err, ForwardError::ShallowReceipt));

        // The downstream peer that handed us the shallow receipt is scored as
        // invalid data through the same reporter the inbound edge uses.
        let (reported_peer, event, source) = reporter.single();
        assert_eq!(reported_peer, closer);
        assert_eq!(event, SwarmScoringEvent::InvalidData);
        assert_eq!(source, ReportSource::Protocol("pushsync"));

        // Both reservations released on drop: nothing was charged.
        assert_eq!(acct.accounting().for_peer(pusher).balance(), Au::ZERO);
        assert_eq!(acct.accounting().for_peer(closer).balance(), Au::ZERO);
    }

    #[tokio::test]
    async fn push_rejects_shallow_receipt_claiming_radius_zero() {
        // Regression: a forwarder must not relay a shallow receipt just because
        // the attacker set storage_radius == 0. The local floor (depth 12) is
        // authoritative and a zero wire radius cannot lower it.
        let chunk = stamped();
        let address = *chunk.address();
        let pusher = overlay_at_proximity(&address, 2);
        let closer = overlay_at_proximity(&address, 16);

        let acct = accounting();
        let reporter = Arc::new(RecordingReporter::default());
        let topo = MockTopology::default()
            .with_closest(vec![closer])
            .with_depth(12)
            .with_reporter(Arc::clone(&reporter) as Arc<dyn PeerReporter>);
        let (tx, rx) = mpsc::channel::<ClientCommand>(4);

        let forwarder =
            NetworkForwarder::new(engine_over(topo, ClientHandle::new(tx)), Arc::clone(&acct));

        let signer = PrivateKeySigner::random();
        let (shallow, _signer_overlay) = signed_receipt_at_depth(
            &signer,
            &address,
            0,
            StorageRadius::new(Bin::new(0).unwrap()),
        );
        let answer = reconstructed(shallow);

        let err = drive_one_command(
            rx,
            forwarder.push(chunk.clone(), pusher),
            move |cmd| match cmd {
                ClientCommand::PushChunk { response, .. } => {
                    response.send(Ok(answer)).expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            },
        )
        .await
        .expect_err("radius 0 does not bypass the local floor");
        assert!(matches!(err, ForwardError::ShallowReceipt));

        let (reported_peer, event, _) = reporter.single();
        assert_eq!(reported_peer, closer);
        assert_eq!(event, SwarmScoringEvent::InvalidData);

        assert_eq!(acct.accounting().for_peer(pusher).balance(), Au::ZERO);
        assert_eq!(acct.accounting().for_peer(closer).balance(), Au::ZERO);
    }

    #[tokio::test]
    async fn push_with_non_credible_view_is_unverifiable_and_does_not_penalise() {
        // With a non-credible local view (the neighbourhood has not saturated)
        // the forwarder cannot judge custody depth, so even a shallow receipt
        // declaring radius 0 must not be relayed AND the downstream peer must not
        // be penalised. The forward fails with `UnverifiableReceipt`, nothing is
        // scored, and no reservation leaks.
        let chunk = stamped();
        let address = *chunk.address();
        let pusher = overlay_at_proximity(&address, 2);
        let closer = overlay_at_proximity(&address, 16);

        let acct = accounting();
        // Non-credible view: a fresh node at depth 0, neighbourhood unsaturated.
        let reporter = Arc::new(RecordingReporter::default());
        let topo = MockTopology::default()
            .with_closest(vec![closer])
            .with_depth(0)
            .with_credible(false)
            .with_reporter(Arc::clone(&reporter) as Arc<dyn PeerReporter>);
        let (tx, rx) = mpsc::channel::<ClientCommand>(4);

        let forwarder =
            NetworkForwarder::new(engine_over(topo, ClientHandle::new(tx)), Arc::clone(&acct));

        let signer = PrivateKeySigner::random();
        let (shallow, _signer_overlay) = signed_receipt_at_depth(
            &signer,
            &address,
            0,
            StorageRadius::new(Bin::new(0).unwrap()),
        );
        let answer = reconstructed(shallow);

        let err = drive_one_command(
            rx,
            forwarder.push(chunk.clone(), pusher),
            move |cmd| match cmd {
                ClientCommand::PushChunk { response, .. } => {
                    response.send(Ok(answer)).expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            },
        )
        .await
        .expect_err("an unverifiable receipt is never relayed");
        assert!(matches!(err, ForwardError::UnverifiableReceipt));

        // The downstream peer is NOT penalised: the receipt may be honest.
        assert!(reporter.is_empty());

        // Both reservations released on drop: nothing was charged.
        assert_eq!(acct.accounting().for_peer(pusher).balance(), Au::ZERO);
        assert_eq!(acct.accounting().for_peer(closer).balance(), Au::ZERO);
    }

    #[tokio::test]
    async fn push_failure_from_decode_rejected_receipt_relays_nothing() {
        // A malformed (unrecoverable) downstream receipt is rejected at the
        // downstream decode boundary, scoring that peer there, and the push
        // resolves as a remote failure. At the forwarder the failure surfaces as
        // a push error, so nothing is relayed and no reservation leaks. (The
        // malformed-receipt rejection itself is covered at the decode boundary in
        // `vertex-swarm-net-pushsync` and the handler.)
        let chunk = stamped();
        let address = *chunk.address();
        let pusher = overlay_at_proximity(&address, 2);
        let closer = overlay_at_proximity(&address, 16);

        let acct = accounting();
        let reporter = Arc::new(RecordingReporter::default());
        let topo = MockTopology::default()
            .with_closest(vec![closer])
            .with_reporter(Arc::clone(&reporter) as Arc<dyn PeerReporter>);
        let (tx, rx) = mpsc::channel::<ClientCommand>(4);

        let forwarder =
            NetworkForwarder::new(engine_over(topo, ClientHandle::new(tx)), Arc::clone(&acct));

        let err = drive_one_command(
            rx,
            forwarder.push(chunk.clone(), pusher),
            move |cmd| match cmd {
                ClientCommand::PushChunk { response, .. } => {
                    response
                        .send(Err(crate::ChunkTransferError::Remote))
                        .expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            },
        )
        .await
        .expect_err("a push failure is never relayed as a receipt");
        assert!(matches!(err, ForwardError::AllPeersFailed));

        // The forwarder did not relay and did not double-score: the decode
        // boundary already scored the malformed downstream peer.
        assert!(reporter.is_empty());
        assert_eq!(acct.accounting().for_peer(pusher).balance(), Au::ZERO);
        assert_eq!(acct.accounting().for_peer(closer).balance(), Au::ZERO);
    }

    #[tokio::test]
    async fn retrieve_without_closer_peer_fails_and_leaks_no_reservation() {
        let chunk = stamped();
        let address = *chunk.address();
        // The requester is already in the neighbourhood: nothing is closer.
        let requester = overlay_at_proximity(&address, 20);
        let sideways = overlay_at_proximity(&address, 8);

        let acct = accounting();
        let topo = MockTopology::default().with_closest(vec![sideways]);
        let (tx, _rx) = mpsc::channel::<ClientCommand>(4);

        let forwarder =
            NetworkForwarder::new(engine_over(topo, ClientHandle::new(tx)), Arc::clone(&acct));
        let err = forwarder
            .retrieve(address, requester)
            .await
            .expect_err("no strictly-closer peer");
        assert!(matches!(err, ForwardError::NoCloserPeer));

        // No leg was attempted, so no reservation is held or committed.
        assert_eq!(acct.accounting().for_peer(requester).balance(), Au::ZERO);
        assert_eq!(acct.accounting().for_peer(sideways).balance(), Au::ZERO);
    }

    #[tokio::test]
    async fn failed_upstream_releases_both_reservations() {
        let chunk = stamped();
        let address = *chunk.address();
        let requester = overlay_at_proximity(&address, 2);
        let closer = overlay_at_proximity(&address, 16);

        let acct = accounting();
        let topo = MockTopology::default().with_closest(vec![closer]);
        let (tx, rx) = mpsc::channel::<ClientCommand>(4);

        let forwarder =
            NetworkForwarder::new(engine_over(topo, ClientHandle::new(tx)), Arc::clone(&acct));

        // The upstream peer reports a failure: no chunk comes back.
        let err = drive_one_command(
            rx,
            forwarder.retrieve(address, requester),
            |cmd| match cmd {
                ClientCommand::RetrieveChunk { response, .. } => {
                    response
                        .send(Err(crate::ChunkTransferError::Remote))
                        .expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            },
        )
        .await
        .expect_err("relay fails when the upstream leg fails");
        assert!(matches!(err, ForwardError::AllPeersFailed));

        // Both reservations were released on drop: balances are untouched.
        assert_eq!(acct.accounting().for_peer(requester).balance(), Au::ZERO);
        assert_eq!(acct.accounting().for_peer(closer).balance(), Au::ZERO);
    }

    #[tokio::test]
    async fn prepare_serve_bills_only_after_the_wire_write() {
        let chunk = stamped();
        let address = *chunk.address();
        let requester = overlay_at_proximity(&address, 2);

        let acct = accounting();
        let (tx, _rx) = mpsc::channel::<ClientCommand>(4);
        let forwarder = NetworkForwarder::new(
            engine_over(MockTopology::default(), ClientHandle::new(tx)),
            Arc::clone(&acct),
        );

        let price = acct.pricing().peer_price(&requester, &address);
        let provide = forwarder
            .prepare_serve(requester, &address)
            .expect("within the settle line");

        // Reserved, not billed, until the handler commits after the write.
        assert_eq!(acct.accounting().for_peer(requester).balance(), Au::ZERO);
        provide.apply_boxed();
        assert_eq!(acct.accounting().for_peer(requester).balance(), price);
    }

    #[tokio::test]
    async fn concurrent_prepared_serves_exhaust_the_settle_line_and_release_on_drop() {
        // In-flight serves count against the settle line via the shadow
        // reservation, so a peer holding many concurrent requests cannot run
        // its exposure past the payment threshold; dropping them restores the
        // headroom.
        let chunk = stamped();
        let address = *chunk.address();
        let requester = overlay_at_proximity(&address, 2);

        let acct = accounting();
        let (tx, _rx) = mpsc::channel::<ClientCommand>(4);
        let forwarder = NetworkForwarder::new(
            engine_over(MockTopology::default(), ClientHandle::new(tx)),
            Arc::clone(&acct),
        );

        let mut held = Vec::new();
        let refused = loop {
            match forwarder.prepare_serve(requester, &address) {
                Ok(provide) => held.push(provide),
                Err(err) => break err,
            }
            assert!(held.len() <= 20_000, "the settle line never engaged");
        };
        assert!(matches!(refused, ForwardError::AccountingRefused));
        assert_eq!(
            acct.accounting().for_peer(requester).balance(),
            Au::ZERO,
            "held serves reserve, never commit"
        );

        held.clear();
        assert!(forwarder.prepare_serve(requester, &address).is_ok());
    }

    #[tokio::test]
    async fn refused_deliveries_accrue_ghost_debt_and_starve_the_serve_gate() {
        // A peer that requests answers and never takes delivery: every
        // forfeited serve leaves a ghost trace, so the gate refuses long
        // before any balance commits, and unlike a released in-flight serve
        // the ghost persists.
        let chunk = stamped();
        let address = *chunk.address();
        let requester = overlay_at_proximity(&address, 2);

        let acct = accounting();
        let (tx, _rx) = mpsc::channel::<ClientCommand>(4);
        let forwarder = NetworkForwarder::new(
            engine_over(MockTopology::default(), ClientHandle::new(tx)),
            Arc::clone(&acct),
        );

        let mut refusals = 0usize;
        let refused = loop {
            match forwarder.prepare_serve(requester, &address) {
                Ok(provide) => provide.forfeit_boxed(),
                Err(err) => break err,
            }
            refusals += 1;
            assert!(refusals <= 20_000, "the serve gate never engaged");
        };
        assert!(matches!(refused, ForwardError::AccountingRefused));
        assert_eq!(
            acct.accounting().for_peer(requester).balance(),
            Au::ZERO,
            "forfeits never commit"
        );

        // The ghost persists: the peer stays starved.
        assert!(forwarder.prepare_serve(requester, &address).is_err());
    }
}
