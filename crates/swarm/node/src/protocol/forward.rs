//! The concrete network forwarder: the real multi-hop relay for retrieval and
//! pushsync.
//!
//! The accounting-free forwarder seam (the [`Forwarder`] trait, [`StubForwarder`],
//! the [`ForwardedChunk`] / [`ForwardedReceipt`] carriers, [`ForwardError`], and
//! the [`closer_candidates`] selector) lives in `vertex-swarm-client-behaviour`.
//! [`NetworkForwarder`] stays here because it couples to client accounting and
//! the outbound [`ClientHandle`], which the behaviour crate must not depend on.
//!
//! Address verification lives at both edges. The downstream chunk returned by an
//! upstream retrieval is verified against the requested address here (an
//! address-equality check) before it is cached or relayed; the handler
//! additionally verifies before it writes the chunk to the responder, so a chunk
//! that does not hash to the requested address can never travel back to the
//! requester. The check is BMT integrity for content chunks and a
//! signature-recovered owner for single-owner chunks; it does **not** validate
//! the postage stamp's
//! funding or expiry, which is a separate postage concern. A relayed pushsync
//! receipt arrives as a [`Receipt`](vertex_swarm_net_pushsync::Receipt) whose
//! storer was already recovered and
//! verified at the decode boundary; the forwarder checks its custody depth
//! before relaying, so `PO(storer, chunk)` must reach a depth derived from the
//! locally observed neighbourhood depth. A forwarder never launders a shallow
//! receipt upstream and scores the downstream peer adversely when it tries.

use std::sync::Arc;

use futures::future::BoxFuture;
use nectar_primitives::ChunkAddress;
use tracing::{debug, warn};
use vertex_swarm_api::{
    Commit, PeerReporter, ReportSource, SwarmClientAccounting, SwarmScoringEvent,
    SwarmTopologyRouting, SwarmTopologyState,
};
use vertex_swarm_client_behaviour::{
    ForwardError, ForwardedChunk, ForwardedReceipt, Forwarder, closer_candidates,
};
use vertex_swarm_net_pushsync::DepthVerdict;
use vertex_swarm_primitives::{OverlayAddress, StampedChunk};

use crate::ClientHandle;

/// Report source for shallow/malformed receipts caught on the relay path.
const PUSHSYNC_SOURCE: ReportSource = ReportSource::Protocol("pushsync");

/// The real multi-hop relay: forwarding Kademlia for retrieval and pushsync.
///
/// Generic over the topology routing surface `T` (closest-peer selection) and
/// the client accounting surface `A` (two-leg prepare/apply). Holds a
/// [`ClientHandle`] so the upstream leg reuses the same self-contained outbound
/// futures the origin path uses; no separate dial machinery exists.
///
/// # Loop prevention and termination
///
/// Forwarding Kademlia routes a request strictly toward the target's
/// neighbourhood: each hop must hand the request to a peer that is *strictly
/// closer* to the target than the peer that asked **and** strictly closer than
/// this node itself, measured by full XOR distance. [`closer_candidates`] filters
/// the topology's proximity-ordered candidates down to exactly those, also
/// excluding the requester and ourselves. Because XOR distance to the target
/// strictly decreases along the chain, a request can never cycle back to a peer
/// it has already visited, so no per-request visited set or hop counter is
/// needed; the chain is bounded by the address width.
///
/// # Two-leg accounting
///
/// A forwarder sits between an *upstream* peer (the requester or pusher, the one
/// we provide service to) and a *downstream* peer (the closer peer we relay to,
/// the one that provides service to us). It credits the upstream leg
/// (`prepare_provide_chunk(exclude)`) and debits the downstream leg
/// (`prepare_receive_chunk(closer)`) so it earns the price spread between the
/// two. Both actions reserve on creation; on a successful relay both are applied,
/// committing the balance changes. On any failure both actions are dropped, which
/// releases the reservations (the receive/provide [`Reservation`] legs release on
/// drop), so a failed forward never leaks an accounting reservation.
///
/// [`Reservation`]: vertex_swarm_accounting::Reservation
pub(crate) struct NetworkForwarder<T, A> {
    /// Our own overlay: excluded from candidates and used as the
    /// strictly-closer reference for the loop bound alongside the requester.
    local: OverlayAddress,
    /// Proximity-ordered closest-peer selection and locally observed depth.
    topology: Arc<T>,
    /// Two-leg prepare/apply accounting.
    accounting: Arc<A>,
    /// Reuses the origin outbound futures for the upstream relay leg.
    handle: ClientHandle,
    /// The single sanctioned scoring path: a shallow relayed receipt scores the
    /// downstream signer adversely through it.
    reporter: Arc<dyn PeerReporter>,
}

impl<T, A> NetworkForwarder<T, A> {
    /// Build a network forwarder from the local overlay, topology, accounting,
    /// the outbound client handle, and the peer reporter (for scoring shallow
    /// receipts).
    pub(crate) fn new(
        local: OverlayAddress,
        topology: Arc<T>,
        accounting: Arc<A>,
        handle: ClientHandle,
        reporter: Arc<dyn PeerReporter>,
    ) -> Self {
        Self {
            local,
            topology,
            accounting,
            handle,
            reporter,
        }
    }
}

impl<T, A> Forwarder for NetworkForwarder<T, A>
where
    T: SwarmTopologyRouting + SwarmTopologyState + Send + Sync + 'static,
    A: SwarmClientAccounting + Send + Sync + 'static,
{
    fn retrieve(
        &self,
        address: ChunkAddress,
        exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<ForwardedChunk, ForwardError>> {
        let candidates = closer_candidates(&*self.topology, &address, exclude, self.local);
        let accounting = Arc::clone(&self.accounting);
        let handle = self.handle.clone();

        Box::pin(async move {
            if candidates.is_empty() {
                return Err(ForwardError::NoCloserPeer);
            }

            // Credit the upstream leg: the requester pays us for serving the
            // chunk on. Held across the whole relay; released on drop if the
            // relay fails. It is NOT committed here: a verified chunk in hand
            // does not yet mean the requester received it, so the action is
            // handed back un-applied and the handler commits it only after the
            // chunk is on the wire.
            let provide = accounting
                .prepare_provide_chunk(exclude, &address)
                .map_err(|_| ForwardError::AccountingRefused)?;

            let mut last = ForwardError::AllPeersFailed;
            for closer in candidates {
                // Debit the downstream leg: we pay the closer peer for the
                // chunk it serves us. `originated = false`: this is a relay, not
                // our own request. Released on drop if this attempt fails.
                let receive = match accounting.prepare_receive_chunk(closer, &address, false) {
                    Ok(action) => action,
                    Err(_) => {
                        // Cannot afford this downstream peer; try the next.
                        last = ForwardError::AccountingRefused;
                        continue;
                    }
                };

                // `originated = false`: this is a relay leg, so the service must
                // not debit the completion event. The forwarder debits this leg
                // itself via `prepare_receive_chunk` above.
                match handle.retrieve_chunk(closer, address, false).await {
                    Ok(result) => {
                        // Edge verification: the relayed chunk must answer the
                        // requested address before we account, cache, or relay
                        // it. The chunk is address-derived (BMT hash or owner
                        // plus signature), so equality proves it answers the
                        // request, independent of the stamp. The handler
                        // re-checks the same equality before the wire.
                        if *result.chunk.address() == address {
                            // The downstream leg is genuinely complete (we
                            // received the chunk), so commit it now. The
                            // upstream `provide` is returned un-applied for
                            // the handler to commit after the wire write.
                            receive.apply();
                            debug!(%closer, %address, "relayed retrieval");
                            return Ok(ForwardedChunk {
                                chunk: result.chunk,
                                stamp: result.stamp,
                                provide: Box::new(provide),
                            });
                        }
                        // The downstream peer served the wrong chunk:
                        // drop `receive` (release) and try the next.
                        drop(receive);
                        last = ForwardError::UnverifiedRelay;
                    }
                    Err(_) => {
                        // Downstream attempt failed: `receive` drops here,
                        // releasing its reservation. The upstream `provide`
                        // reservation stays held for the next candidate.
                        drop(receive);
                        last = ForwardError::AllPeersFailed;
                    }
                }
            }

            // Every candidate failed: `provide` drops here, releasing the
            // upstream reservation so nothing leaks.
            Err(last)
        })
    }

    fn push(
        &self,
        chunk: StampedChunk,
        exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<ForwardedReceipt, ForwardError>> {
        let address = *chunk.address();
        let candidates = closer_candidates(&*self.topology, &address, exclude, self.local);
        let accounting = Arc::clone(&self.accounting);
        let handle = self.handle.clone();
        // Snapshot the locally observed neighbourhood depth now: it is the
        // trusted authority for the required receipt depth. Snapshot whether that
        // depth is credible alongside it (the neighbourhood has saturated); a
        // non-credible depth cannot anchor the check. Reading both here keeps the
        // future `'static`.
        let local_depth = self.topology.depth();
        let neighbourhood_credible = self.topology.neighbourhood_credible();
        let reporter = Arc::clone(&self.reporter);

        Box::pin(async move {
            if candidates.is_empty() {
                return Err(ForwardError::NoCloserPeer);
            }

            // Credit the upstream leg: the pusher pays us for relaying the chunk
            // toward its neighbourhood. Returned un-applied; the handler commits
            // it only after the receipt is written back to the pusher.
            let provide = accounting
                .prepare_provide_chunk(exclude, &address)
                .map_err(|_| ForwardError::AccountingRefused)?;

            let mut last = ForwardError::AllPeersFailed;
            for closer in candidates {
                // Debit the downstream leg: we pay the storer (or next hop) for
                // accepting the chunk.
                let receive = match accounting.prepare_receive_chunk(closer, &address, false) {
                    Ok(action) => action,
                    Err(_) => {
                        last = ForwardError::AccountingRefused;
                        continue;
                    }
                };

                // `originated = false`: a relay leg, debited by the forwarder
                // above, so the service must not debit the completion event.
                match handle.push_chunk(closer, chunk.clone(), false).await {
                    Ok(receipt) => {
                        // The receipt's storer was recovered and verified at the
                        // decode boundary, so a malformed receipt never reaches
                        // here (it surfaces as a push error on the arm below). The
                        // forwarder's remaining duty is the depth policy: it must
                        // never launder a SHALLOW custody receipt. The check runs
                        // against the recovered storer (NOT the immediate
                        // downstream peer) and our locally observed depth, and is
                        // gated on that depth being credible. The receipt is
                        // relayed VERBATIM by the handler; we never re-sign it.
                        match receipt.verify_depth(local_depth, neighbourhood_credible) {
                            DepthVerdict::Verified => {
                                // Downstream leg complete; commit it. Upstream
                                // `provide` returned un-applied for the handler.
                                receive.apply();
                                debug!(%closer, %address, "relayed pushsync");
                                return Ok(ForwardedReceipt {
                                    receipt,
                                    provide: Box::new(provide),
                                });
                            }
                            DepthVerdict::Shallow(err) => {
                                // Drop the downstream leg (release the
                                // reservation) and never relay this receipt. The
                                // downstream peer that handed us a shallow receipt
                                // is scored adversely as invalid data, so it loses
                                // reputation and we do not take the reputational
                                // hit for laundering it. Try the next candidate.
                                drop(receive);
                                warn!(
                                    %closer,
                                    %address,
                                    error = <&'static str>::from(&err),
                                    "rejected shallow relayed receipt"
                                );
                                reporter.report_peer(
                                    &closer,
                                    SwarmScoringEvent::InvalidData,
                                    PUSHSYNC_SOURCE,
                                );
                                last = ForwardError::ShallowReceipt;
                            }
                            DepthVerdict::Unverifiable => {
                                // The local view is not credible enough to judge
                                // custody depth, so we cannot relay this receipt,
                                // but the downstream peer may be honest: drop the
                                // reservation, do NOT penalise it, and try the
                                // next candidate.
                                drop(receive);
                                debug!(
                                    %closer,
                                    %address,
                                    "relayed receipt unverifiable: neighbourhood view not credible"
                                );
                                last = ForwardError::UnverifiableReceipt;
                            }
                        }
                    }
                    Err(_) => {
                        // A push failure also covers the malformed-receipt case:
                        // the downstream handler rejects an unrecoverable receipt
                        // at decode (scoring that peer) and resolves the push as
                        // a remote failure, so a malformed receipt never reaches
                        // the relay seam.
                        drop(receive);
                        last = ForwardError::AllPeersFailed;
                    }
                }
            }

            Err(last)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use std::sync::Mutex;

    use alloy_primitives::{B256, Signature};
    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use nectar_postage::Stamp;
    use nectar_primitives::{AnyChunk, ContentChunk, NetworkId, Nonce, compute_overlay};
    use tokio::sync::mpsc;
    use vertex_swarm_accounting::{
        Accounting, ClientAccounting, DefaultBandwidthConfig, FixedPricer,
    };
    use vertex_swarm_api::{
        Au, ReportSource, SwarmBandwidthAccounting, SwarmPeerBandwidth, SwarmPricing,
        SwarmScoringEvent,
    };
    use vertex_swarm_identity::Identity;
    use vertex_swarm_net_pushsync::{Receipt, WireReceipt};
    use vertex_swarm_primitives::{Bin, StorageRadius};
    use vertex_swarm_spec::Spec;
    use vertex_swarm_test_utils::{MockTopology, test_identity_arc};

    use super::*;
    use crate::{ClientCommand, RetrievalResult};

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

    type TestAccounting =
        ClientAccounting<Arc<Accounting<DefaultBandwidthConfig, Arc<Identity>>>, FixedPricer<Spec>>;

    fn accounting() -> Arc<TestAccounting> {
        let bandwidth = Arc::new(Accounting::new(
            DefaultBandwidthConfig::default(),
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
        let local = OverlayAddress::from([0xee; 32]);

        let acct = accounting();
        let topo = Arc::new(MockTopology::default().with_closest(vec![closer]));
        let (tx, rx) = mpsc::channel::<ClientCommand>(4);
        let handle = ClientHandle::new(tx);

        let provide_price = acct.pricing().peer_price(&requester, &address);
        let receive_price = acct.pricing().peer_price(&closer, &address);
        assert!(
            provide_price > receive_price,
            "the requester is farther than the closer peer, so the forwarder earns the spread"
        );

        let forwarder = NetworkForwarder::new(
            local,
            topo,
            Arc::clone(&acct),
            handle,
            Arc::new(RecordingReporter::default()) as Arc<dyn PeerReporter>,
        );

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

        // The downstream leg is committed inside the forwarder, so the closer
        // peer is already owed receive_price. The upstream `provide` is returned
        // un-applied: until it is committed (which the handler does after a
        // successful wire write) the requester owes nothing.
        assert_eq!(
            acct.bandwidth().for_peer(requester).balance(),
            Au::ZERO,
            "the upstream credit is deferred until the wire write"
        );
        assert_eq!(
            acct.bandwidth().for_peer(closer).balance(),
            Au::ZERO - receive_price
        );

        // Commit the upstream leg as the handler would after writing the chunk
        // back: now the requester owes us provide_price.
        forwarded.provide.apply_boxed();
        assert_eq!(
            acct.bandwidth().for_peer(requester).balance(),
            provide_price
        );
        assert_eq!(
            acct.bandwidth().for_peer(closer).balance(),
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
        let local = OverlayAddress::from([0xee; 32]);

        let acct = accounting();
        let topo = Arc::new(MockTopology::default().with_closest(vec![closer]));
        let (tx, rx) = mpsc::channel::<ClientCommand>(4);
        let handle = ClientHandle::new(tx);

        let receive_price = acct.pricing().peer_price(&closer, &address);
        let forwarder = NetworkForwarder::new(
            local,
            topo,
            Arc::clone(&acct),
            handle,
            Arc::new(RecordingReporter::default()) as Arc<dyn PeerReporter>,
        );

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
            acct.bandwidth().for_peer(requester).balance(),
            Au::ZERO,
            "dropping the un-applied provide leg charges the requester nothing"
        );
        assert_eq!(
            acct.bandwidth().for_peer(closer).balance(),
            Au::ZERO - receive_price
        );
    }

    #[tokio::test]
    async fn push_relays_receipt_verbatim_and_accounts_both_legs() {
        let chunk = stamped();
        let address = *chunk.address();
        let pusher = overlay_at_proximity(&address, 2);
        let closer = overlay_at_proximity(&address, 16);
        let local = OverlayAddress::from([0xee; 32]);

        let acct = accounting();
        let topo = Arc::new(MockTopology::default().with_closest(vec![closer]));
        let (tx, rx) = mpsc::channel::<ClientCommand>(4);
        let handle = ClientHandle::new(tx);

        let provide_price = acct.pricing().peer_price(&pusher, &address);
        let receive_price = acct.pricing().peer_price(&closer, &address);

        let reporter = Arc::new(RecordingReporter::default());
        let forwarder = NetworkForwarder::new(
            local,
            topo,
            Arc::clone(&acct),
            handle,
            Arc::clone(&reporter) as Arc<dyn PeerReporter>,
        );

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
        assert_eq!(acct.bandwidth().for_peer(pusher).balance(), Au::ZERO);
        assert_eq!(
            acct.bandwidth().for_peer(closer).balance(),
            Au::ZERO - receive_price
        );

        forwarded.provide.apply_boxed();
        assert_eq!(acct.bandwidth().for_peer(pusher).balance(), provide_price);
        assert_eq!(
            acct.bandwidth().for_peer(closer).balance(),
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
        let local = OverlayAddress::from([0xee; 32]);

        let acct = accounting();
        // Require depth 12: local depth 12 and a deep wire radius below.
        let topo = Arc::new(
            MockTopology::default()
                .with_closest(vec![closer])
                .with_depth(12),
        );
        let (tx, rx) = mpsc::channel::<ClientCommand>(4);
        let handle = ClientHandle::new(tx);

        let reporter = Arc::new(RecordingReporter::default());
        let forwarder = NetworkForwarder::new(
            local,
            topo,
            Arc::clone(&acct),
            handle,
            Arc::clone(&reporter) as Arc<dyn PeerReporter>,
        );

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
        assert_eq!(acct.bandwidth().for_peer(pusher).balance(), Au::ZERO);
        assert_eq!(acct.bandwidth().for_peer(closer).balance(), Au::ZERO);
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
        let local = OverlayAddress::from([0xee; 32]);

        let acct = accounting();
        let topo = Arc::new(
            MockTopology::default()
                .with_closest(vec![closer])
                .with_depth(12),
        );
        let (tx, rx) = mpsc::channel::<ClientCommand>(4);
        let handle = ClientHandle::new(tx);

        let reporter = Arc::new(RecordingReporter::default());
        let forwarder = NetworkForwarder::new(
            local,
            topo,
            Arc::clone(&acct),
            handle,
            Arc::clone(&reporter) as Arc<dyn PeerReporter>,
        );

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

        assert_eq!(acct.bandwidth().for_peer(pusher).balance(), Au::ZERO);
        assert_eq!(acct.bandwidth().for_peer(closer).balance(), Au::ZERO);
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
        let local = OverlayAddress::from([0xee; 32]);

        let acct = accounting();
        // Non-credible view: a fresh node at depth 0, neighbourhood unsaturated.
        let topo = Arc::new(
            MockTopology::default()
                .with_closest(vec![closer])
                .with_depth(0)
                .with_credible(false),
        );
        let (tx, rx) = mpsc::channel::<ClientCommand>(4);
        let handle = ClientHandle::new(tx);

        let reporter = Arc::new(RecordingReporter::default());
        let forwarder = NetworkForwarder::new(
            local,
            topo,
            Arc::clone(&acct),
            handle,
            Arc::clone(&reporter) as Arc<dyn PeerReporter>,
        );

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
        assert_eq!(acct.bandwidth().for_peer(pusher).balance(), Au::ZERO);
        assert_eq!(acct.bandwidth().for_peer(closer).balance(), Au::ZERO);
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
        let local = OverlayAddress::from([0xee; 32]);

        let acct = accounting();
        let topo = Arc::new(MockTopology::default().with_closest(vec![closer]));
        let (tx, rx) = mpsc::channel::<ClientCommand>(4);
        let handle = ClientHandle::new(tx);

        let reporter = Arc::new(RecordingReporter::default());
        let forwarder = NetworkForwarder::new(
            local,
            topo,
            Arc::clone(&acct),
            handle,
            Arc::clone(&reporter) as Arc<dyn PeerReporter>,
        );

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
        assert_eq!(acct.bandwidth().for_peer(pusher).balance(), Au::ZERO);
        assert_eq!(acct.bandwidth().for_peer(closer).balance(), Au::ZERO);
    }

    #[tokio::test]
    async fn retrieve_without_closer_peer_fails_and_leaks_no_reservation() {
        let chunk = stamped();
        let address = *chunk.address();
        // The requester is already in the neighbourhood: nothing is closer.
        let requester = overlay_at_proximity(&address, 20);
        let sideways = overlay_at_proximity(&address, 8);
        let local = OverlayAddress::from([0xee; 32]);

        let acct = accounting();
        let topo = Arc::new(MockTopology::default().with_closest(vec![sideways]));
        let (tx, _rx) = mpsc::channel::<ClientCommand>(4);
        let handle = ClientHandle::new(tx);

        let forwarder = NetworkForwarder::new(
            local,
            topo,
            Arc::clone(&acct),
            handle,
            Arc::new(RecordingReporter::default()) as Arc<dyn PeerReporter>,
        );
        let err = forwarder
            .retrieve(address, requester)
            .await
            .expect_err("no strictly-closer peer");
        assert!(matches!(err, ForwardError::NoCloserPeer));

        // No leg was attempted, so no reservation is held or committed.
        assert_eq!(acct.bandwidth().for_peer(requester).balance(), Au::ZERO);
        assert_eq!(acct.bandwidth().for_peer(sideways).balance(), Au::ZERO);
    }

    #[tokio::test]
    async fn failed_upstream_releases_both_reservations() {
        let chunk = stamped();
        let address = *chunk.address();
        let requester = overlay_at_proximity(&address, 2);
        let closer = overlay_at_proximity(&address, 16);
        let local = OverlayAddress::from([0xee; 32]);

        let acct = accounting();
        let topo = Arc::new(MockTopology::default().with_closest(vec![closer]));
        let (tx, rx) = mpsc::channel::<ClientCommand>(4);
        let handle = ClientHandle::new(tx);

        let forwarder = NetworkForwarder::new(
            local,
            topo,
            Arc::clone(&acct),
            handle,
            Arc::new(RecordingReporter::default()) as Arc<dyn PeerReporter>,
        );

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
        assert_eq!(acct.bandwidth().for_peer(requester).balance(), Au::ZERO);
        assert_eq!(acct.bandwidth().for_peer(closer).balance(), Au::ZERO);
    }
}
