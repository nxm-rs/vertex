//! The forwarder seam: relay a retrieval or a pushsync to a closer peer.
//!
//! Inbound serving is handler-inline: each inbound retrieval or pushsync request
//! becomes one self-contained future, with the substream itself as the
//! correlation (mirroring the outbound model). When the local cache cannot
//! answer a retrieval, or for every pushsync, the handler hands off to a
//! [`Forwarder`] that relays to a closer peer and returns the result.
//!
//! Two implementations live here:
//!
//! - [`StubForwarder`] always returns [`ForwardError::NoCloserPeer`], so a cache
//!   miss and every pushsync reset the inbound substream. It is the right
//!   behaviour for a node that holds no reserve and takes no custody and never
//!   wants to relay (and is the model the behaviour-level tests drive).
//! - [`NetworkForwarder`] is the real multi-hop relay: it selects the closest
//!   peer to the target excluding the requester (and ourselves), enforces the
//!   forwarding-Kademlia loop rule (never relay to a peer that is not strictly
//!   closer to the target, by XOR distance, than both the requester and this
//!   node), reuses the existing self-contained outbound futures
//!   ([`ClientHandle::retrieve_chunk`](crate::ClientHandle::retrieve_chunk) /
//!   [`push_chunk`](crate::ClientHandle::push_chunk)) for the upstream leg, and
//!   accounts both legs through the prepare/apply reservation actions so a
//!   forwarder earns the spread. A failed forward drops both reservation actions
//!   (release-on-drop), so no accounting leaks. Termination comes from the
//!   strictly-closer rule (XOR distance decreases monotonically, bounded by the
//!   address width), not from a hop/TTL counter, which the protocol does not
//!   carry; [`MAX_FORWARD_CANDIDATES`] only caps per-node retry fan-out.
//!
//! Address verification lives at both edges. The downstream chunk returned by an
//! upstream retrieval is verified against the requested address here
//! ([`StampedChunk::verify_answers`] -> [`VerifiedStampedChunk`]) before it is
//! cached or relayed; the handler additionally verifies before it writes the
//! chunk to the responder, so a chunk that does not hash to the requested
//! address can never travel back to the requester. `verify_answers` is an
//! address-equality check (BMT integrity for content chunks, signature-recovered
//! owner for single-owner chunks); it does **not** validate the postage stamp's
//! funding or expiry, which is a separate postage concern. A relayed pushsync
//! receipt is verified for custody depth before it is relayed: the signer
//! overlay is recovered from the receipt signature (never the off-wire
//! `storer` field) and `PO(signer, chunk)` must reach a depth derived from the
//! locally observed neighbourhood depth, so a forwarder never launders a
//! malformed or shallow receipt upstream and scores the downstream signer
//! adversely when it does.
//!
//! # Upstream credit is deferred to the wire write
//!
//! The two legs are not both committed inside the forwarder. The downstream
//! `receive` leg is genuinely complete the moment a verified chunk/receipt is in
//! hand, so it is applied here. The upstream `provide` leg (the requester or
//! pusher paying us) is *not* applied here: it is returned to the handler as a
//! boxed [`AccountingAction`] and committed only after the chunk or receipt is
//! successfully written back to the requester's substream. If that wire write
//! fails, the handler drops the action, releasing the reservation, so the
//! requester is never charged for a delivery it did not receive.

use std::sync::Arc;

use futures::future::BoxFuture;
use nectar_primitives::{ChunkAddress, NetworkId};
use tracing::{debug, warn};
use vertex_swarm_api::{
    AccountingAction, PeerReporter, PushReceipt, ReceiptDepthError, ReportSource,
    SwarmClientAccounting, SwarmScoringEvent, SwarmTopologyRouting, SwarmTopologyState,
    verify_receipt_depth,
};
use vertex_swarm_primitives::{OverlayAddress, StampedChunk};

use crate::ClientHandle;

/// Report source for shallow/malformed receipts caught on the relay path.
const PUSHSYNC_SOURCE: ReportSource = ReportSource::Protocol("pushsync");

/// Maximum number of closer peers this node tries, in order, for a single
/// inbound request before giving up.
///
/// This is a per-node fan-out cap, **not** a per-request hop/TTL counter: it
/// bounds how many downstream candidates this one node retries for one inbound
/// request, not the length of the overall A->B->C->... relay chain. Termination
/// of the chain comes from the strictly-closer rule (every hop must hand the
/// request to a peer strictly closer to the target by XOR distance than both the
/// requester and this node), which makes proximity monotonically increase toward
/// the target and is bounded by the address width, so no per-request hop counter
/// or visited set is needed. The reference also walks a small fixed number of
/// closer peers per hop; we mirror that with a bounded candidate set.
const MAX_FORWARD_CANDIDATES: usize = 3;

/// Why a forward could not complete.
///
/// The reason is intentionally coarse: the handler only needs to know the
/// forward did not produce a chunk or receipt so it can reset the inbound
/// substream. A real forwarder carries richer diagnostics for its own metrics,
/// but the inbound serving path treats every failure as a reset.
#[derive(Debug, thiserror::Error, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub(crate) enum ForwardError {
    /// No peer strictly closer to the target than both the requester and this
    /// node is available to relay to. Covers both the no-candidate case and the
    /// loop-prevention case (every candidate would forward sideways or backwards
    /// in distance).
    #[error("no closer peer to forward to")]
    NoCloserPeer,

    /// Every strictly-closer candidate we tried failed to answer.
    #[error("all closer peers failed to relay")]
    AllPeersFailed,

    /// The upstream leg returned a chunk that does not answer the request, or a
    /// receipt that is structurally malformed. Never relayed.
    #[error("upstream relay returned unverified data")]
    UnverifiedRelay,

    /// The upstream leg returned a custody receipt whose signer is not deep
    /// enough for the chunk (`PO(signer, chunk) < required`). A forwarder must
    /// never launder a shallow receipt upstream: it is dropped here and the
    /// downstream signer is scored adversely.
    #[error("upstream relay returned a shallow custody receipt")]
    ShallowReceipt,

    /// Accounting refused one of the two legs (over the disconnect threshold),
    /// so the relay was not attempted. Any reservation already taken is released
    /// on drop.
    #[error("accounting refused the relay")]
    AccountingRefused,
}

/// Relays a retrieval or a pushsync to a closer peer on behalf of an inbound
/// request.
///
/// `exclude` is the requester or pusher, passed so the forwarder never relays
/// back to the peer that asked (loop prevention) and so it can account the
/// inbound leg against it. The returned futures are `'static`, boxed, and `Send`
/// so the handler can hold them in its inbound set: a libp2p `ConnectionHandler`
/// is `Send` on both native and wasm (the browser `Stream` is itself `Send`), so
/// the inbound serving futures are `Send` too.
pub(crate) trait Forwarder: Send + Sync {
    /// Retrieve `address` from a closer peer, excluding `exclude`.
    ///
    /// On success the downstream `receive` leg is already committed (we did
    /// receive the chunk), and the un-applied upstream `provide` action is
    /// returned alongside the chunk: the handler commits it only after the chunk
    /// is written back to the requester, and drops it (releasing the
    /// reservation) if that wire write fails.
    fn retrieve(
        &self,
        address: ChunkAddress,
        exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<ForwardedChunk, ForwardError>>;

    /// Push `chunk` to a closer peer, excluding `exclude`, returning the
    /// storer's receipt to relay verbatim.
    ///
    /// The upstream `provide` action is returned un-applied for the same
    /// deferred-commit reason as [`retrieve`](Self::retrieve).
    fn push(
        &self,
        chunk: StampedChunk,
        exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<ForwardedReceipt, ForwardError>>;
}

/// A relayed chunk together with the un-applied upstream credit.
///
/// The forwarder hands this to the handler, which writes `chunk` back to the
/// requester and only then commits `provide` (or drops it on a wire-write
/// failure, releasing the reservation).
pub(crate) struct ForwardedChunk {
    /// The verified chunk to write back to the requester.
    pub(crate) chunk: StampedChunk,
    /// The un-applied upstream credit; commit after a successful wire write.
    pub(crate) provide: Box<dyn AccountingAction>,
}

impl std::fmt::Debug for ForwardedChunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForwardedChunk")
            .field("chunk", &self.chunk)
            .finish_non_exhaustive()
    }
}

/// A relayed receipt together with the un-applied upstream credit.
pub(crate) struct ForwardedReceipt {
    /// The storer's receipt to relay verbatim to the pusher.
    pub(crate) receipt: PushReceipt,
    /// The un-applied upstream credit; commit after a successful wire write.
    pub(crate) provide: Box<dyn AccountingAction>,
}

impl std::fmt::Debug for ForwardedReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForwardedReceipt")
            .field("receipt", &self.receipt)
            .finish_non_exhaustive()
    }
}

/// A forwarder that never relays: every relay fails with
/// [`ForwardError::NoCloserPeer`].
///
/// A cache miss therefore resets the inbound retrieval substream and every
/// inbound pushsync resets too, which is the correct behaviour for a node that
/// holds no reserve, takes no custody, and does not participate as a relay.
pub(crate) struct StubForwarder;

impl Forwarder for StubForwarder {
    fn retrieve(
        &self,
        _address: ChunkAddress,
        _exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<ForwardedChunk, ForwardError>> {
        Box::pin(async { Err(ForwardError::NoCloserPeer) })
    }

    fn push(
        &self,
        _chunk: StampedChunk,
        _exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<ForwardedReceipt, ForwardError>> {
        Box::pin(async { Err(ForwardError::NoCloserPeer) })
    }
}

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
/// this node itself, measured by full XOR distance (the same `closer` rule the
/// reference uses, not capped proximity order). [`closer_candidates`] filters the
/// topology's proximity-ordered candidates down to exactly those, also excluding
/// the requester and ourselves. Because XOR distance to the target strictly
/// decreases along the chain, a request can never cycle back to a peer it has
/// already visited, so no per-request visited set or hop counter is needed; the
/// chain is bounded by the address width. [`MAX_FORWARD_CANDIDATES`] only caps
/// the per-node retry fan-out, not the chain length.
///
/// # Two-leg accounting
///
/// A forwarder sits between an *upstream* peer (the requester or pusher, the one
/// we provide service to) and a *downstream* peer (the closer peer we relay to,
/// the one that provides service to us). It credits the upstream leg
/// (`prepare_provide_chunk(exclude)`) and debits the downstream leg
/// (`prepare_receive_chunk(closer)`), exactly as the reference does, so it earns
/// the price spread between the two. Both actions reserve on creation; on a
/// successful relay both are applied, committing the balance changes. On any
/// failure both actions are dropped, which releases the reservations
/// (`ReceiveAction`/`ProvideAction` release on drop), so a failed forward never
/// leaks an accounting reservation.
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
    /// Network id for recovering the signer overlay of a custody receipt
    /// (`compute_overlay(eth, network_id, nonce)`).
    network_id: NetworkId,
    /// The single sanctioned scoring path: a shallow or malformed relayed
    /// receipt scores the downstream signer adversely through it.
    reporter: Arc<dyn PeerReporter>,
}

impl<T, A> NetworkForwarder<T, A> {
    /// Build a network forwarder from the local overlay, topology, accounting,
    /// the outbound client handle, the network id (for signer recovery), and
    /// the peer reporter (for scoring shallow receipts).
    pub(crate) fn new(
        local: OverlayAddress,
        topology: Arc<T>,
        accounting: Arc<A>,
        handle: ClientHandle,
        network_id: NetworkId,
        reporter: Arc<dyn PeerReporter>,
    ) -> Self {
        Self {
            local,
            topology,
            accounting,
            handle,
            network_id,
            reporter,
        }
    }
}

/// Select the peers strictly closer to `target` than both `requester` and
/// `local`, excluding the requester and `local`, in proximity order, capped at
/// [`MAX_FORWARD_CANDIDATES`].
///
/// This is the loop-prevention core, and it mirrors the reference's `closer`
/// rule using **full XOR distance**, not capped proximity order. A candidate is
/// kept only when it is strictly closer to the target than the requester (so the
/// request never routes sideways or backwards and can never cycle) **and**
/// strictly closer than this node (so a node already in the chunk's
/// neighbourhood does not relay sideways to an equally deep peer, matching the
/// reference's "closer than me" gate and avoiding the capped-PO plateau where all
/// deep peers compare equal). Using XOR distance rather than capped proximity
/// also lets the strict comparison distinguish peers inside the deepest band.
fn closer_candidates(
    topology: &impl SwarmTopologyRouting,
    target: &ChunkAddress,
    requester: OverlayAddress,
    local: OverlayAddress,
) -> Vec<OverlayAddress> {
    topology
        .closest_to(target, MAX_FORWARD_CANDIDATES * 2)
        .into_iter()
        .filter(|peer| *peer != requester && *peer != local)
        // `target.closer(peer, other)` is true iff `peer` is strictly closer to
        // `target` than `other` by full XOR distance. The candidate must beat
        // both the requester (loop prevention) and this node (the reference's
        // self-relative "closer than me" gate).
        .filter(|peer| target.closer(peer, &requester) && target.closer(peer, &local))
        .take(MAX_FORWARD_CANDIDATES)
        .collect()
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

                match handle.retrieve_chunk(closer, address).await {
                    Ok(result) => {
                        // Edge verification (#287): the relayed chunk must
                        // answer the requested address before we account, cache,
                        // or relay it. The type-state proves it; we keep the
                        // inner chunk to hand back to the handler, which verifies
                        // again before the wire.
                        match result.chunk.verify_answers(address) {
                            Ok(verified) => {
                                // The downstream leg is genuinely complete (we
                                // received the chunk), so commit it now. The
                                // upstream `provide` is returned un-applied for
                                // the handler to commit after the wire write.
                                receive.apply();
                                debug!(%closer, %address, "relayed retrieval");
                                return Ok(ForwardedChunk {
                                    chunk: verified.into_inner(),
                                    provide: Box::new(provide),
                                });
                            }
                            Err(_) => {
                                // The downstream peer served the wrong chunk:
                                // drop `receive` (release) and try the next.
                                drop(receive);
                                last = ForwardError::UnverifiedRelay;
                            }
                        }
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
        let network_id = self.network_id;
        // Snapshot the locally observed neighbourhood depth now: it is the
        // trusted authority for the required receipt depth. Reading it here
        // keeps the future `'static`.
        let local_depth = self.topology.depth();
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

                match handle.push_chunk(closer, chunk.clone()).await {
                    Ok(receipt) => {
                        // Verify the receipt BEFORE relaying upstream (#287 +
                        // #293): a forwarder must never launder a malformed or
                        // shallow custody receipt. The signer overlay is
                        // recovered from `receipt.signature` over (address,
                        // nonce) - NOT from the off-wire `receipt.storer`, which
                        // the handler set to `closer` (the immediate downstream
                        // peer, several hops from the real signer on a multi-hop
                        // relay). The required depth is derived dynamically from
                        // our locally observed neighbourhood depth, trust-but-
                        // verified against the receipt's own `storage_radius`.
                        // The receipt is relayed VERBATIM by the handler; we
                        // never mint or re-sign it.
                        match verify_receipt_depth(&receipt, &address, network_id, local_depth) {
                            Ok(_signer) => {
                                // Downstream leg complete; commit it. Upstream
                                // `provide` returned un-applied for the handler.
                                receive.apply();
                                debug!(%closer, %address, "relayed pushsync");
                                return Ok(ForwardedReceipt {
                                    receipt,
                                    provide: Box::new(provide),
                                });
                            }
                            Err(err) => {
                                // Drop the downstream leg (release the
                                // reservation) and never relay this receipt. The
                                // downstream peer that handed us a shallow or
                                // malformed receipt is scored adversely as
                                // invalid data, so it loses reputation and we do
                                // not take the reputational hit for laundering
                                // it. Try the next candidate.
                                drop(receive);
                                warn!(
                                    %closer,
                                    %address,
                                    error = <&'static str>::from(&err),
                                    "rejected unverifiable relayed receipt"
                                );
                                reporter.report_peer(
                                    &closer,
                                    SwarmScoringEvent::InvalidData,
                                    PUSHSYNC_SOURCE,
                                );
                                last = match err {
                                    ReceiptDepthError::Shallow { .. } => {
                                        ForwardError::ShallowReceipt
                                    }
                                    ReceiptDepthError::MalformedSignature => {
                                        ForwardError::UnverifiedRelay
                                    }
                                };
                            }
                        }
                    }
                    Err(_) => {
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
    use vertex_swarm_api::{
        Au, ReportSource, SwarmBandwidthAccounting, SwarmPeerBandwidth, SwarmPricing,
        SwarmScoringEvent,
    };
    use vertex_swarm_bandwidth::{
        Accounting, ClientAccounting, DefaultBandwidthConfig, FixedPricer,
    };
    use vertex_swarm_identity::Identity;
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
    /// with `signer`, grinding the nonce so the signer's derived overlay shares
    /// at least `min_depth` leading bits with `address` (i.e.
    /// `PO(signer, address) >= min_depth`). Returns the receipt and the signer's
    /// overlay, so a test controls exactly how deep the signer sits relative to
    /// the chunk.
    fn signed_receipt_at_depth(
        signer: &PrivateKeySigner,
        address: &ChunkAddress,
        min_depth: u8,
        storage_radius: StorageRadius,
        storer: OverlayAddress,
    ) -> (PushReceipt, OverlayAddress) {
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
                    PushReceipt {
                        storer,
                        signature,
                        nonce,
                        storage_radius,
                    },
                    overlay,
                );
            }
            counter += 1;
        }
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

    #[test]
    fn closer_candidates_keeps_only_strictly_closer_peers() {
        let address = *stamped().address();
        // Requester shares 4 leading bits with the target.
        let requester = overlay_at_proximity(&address, 4);
        let closer = overlay_at_proximity(&address, 10);
        let sideways = overlay_at_proximity(&address, 4);
        let farther = overlay_at_proximity(&address, 1);
        let local = OverlayAddress::from([0xee; 32]);

        let topo = MockTopology::default().with_closest(vec![closer, sideways, farther]);
        let got = closer_candidates(&topo, &address, requester, local);
        assert_eq!(got, vec![closer], "only the strictly-closer peer survives");
    }

    #[test]
    fn closer_candidates_excludes_requester_and_local() {
        let address = *stamped().address();
        let requester = overlay_at_proximity(&address, 4);
        // Local is farther from the target than the candidate, so the
        // self-relative gate does not reject the candidate; this isolates the
        // exclusion behaviour (local must be dropped because it is us, not
        // because of the closer-than-me gate).
        let local = overlay_at_proximity(&address, 2);
        let closer = overlay_at_proximity(&address, 10);

        // The topology returns local and requester among the closest; both must
        // be filtered out as self/requester.
        let topo = MockTopology::default().with_closest(vec![local, closer, requester]);
        let got = closer_candidates(&topo, &address, requester, local);
        assert_eq!(got, vec![closer]);
    }

    #[test]
    fn closer_candidates_drops_peers_farther_than_local() {
        // The reference's self-relative gate: a node already deeper in the
        // target's neighbourhood than a candidate must not relay sideways/back to
        // that candidate, even when the candidate is still closer than the
        // requester. This is the divergence-from-reference fix.
        let address = *stamped().address();
        let requester = overlay_at_proximity(&address, 2);
        // We (local) share 12 bits with the target.
        let local = overlay_at_proximity(&address, 12);
        // The candidate is closer than the requester (8 > 2) but farther than us
        // (8 < 12), so it must be rejected.
        let farther_than_local = overlay_at_proximity(&address, 8);
        // A candidate deeper than us survives.
        let deeper = overlay_at_proximity(&address, 20);

        let topo = MockTopology::default().with_closest(vec![deeper, farther_than_local]);
        let got = closer_candidates(&topo, &address, requester, local);
        assert_eq!(
            got,
            vec![deeper],
            "only a peer strictly closer than this node survives"
        );
    }

    #[test]
    fn closer_candidates_empty_when_nothing_is_closer() {
        let address = *stamped().address();
        let requester = overlay_at_proximity(&address, 12);
        let local = OverlayAddress::from([0xee; 32]);
        let sideways = overlay_at_proximity(&address, 8);

        let topo = MockTopology::default().with_closest(vec![sideways]);
        assert!(closer_candidates(&topo, &address, requester, local).is_empty());
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
            TEST_NET,
            Arc::new(RecordingReporter::default()) as Arc<dyn PeerReporter>,
        );

        let chunk_for_answer = chunk.clone();
        let got = drive_one_command(
            rx,
            forwarder.retrieve(address, requester),
            move |cmd| match cmd {
                ClientCommand::RetrieveChunk {
                    peer,
                    address: requested,
                    response,
                } => {
                    assert_eq!(peer, closer, "the upstream leg targets the closer peer");
                    assert_eq!(requested, address);
                    response
                        .send(Ok(RetrievalResult {
                            chunk: chunk_for_answer,
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
            TEST_NET,
            Arc::new(RecordingReporter::default()) as Arc<dyn PeerReporter>,
        );

        let chunk_for_answer = chunk.clone();
        let forwarded =
            drive_one_command(
                rx,
                forwarder.retrieve(address, requester),
                move |cmd| match cmd {
                    ClientCommand::RetrieveChunk { response, .. } => {
                        response
                            .send(Ok(RetrievalResult {
                                chunk: chunk_for_answer,
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
            TEST_NET,
            Arc::clone(&reporter) as Arc<dyn PeerReporter>,
        );

        // A real storer receipt signed by a signer whose overlay sits 8 bits
        // deep relative to the chunk; the mock depth is 0 so any deep-enough
        // receipt passes. The receipt must be relayed VERBATIM.
        let signer = PrivateKeySigner::random();
        let (storer_receipt, _signer_overlay) = signed_receipt_at_depth(
            &signer,
            &address,
            8,
            StorageRadius::new(Bin::new(8).unwrap()),
            closer,
        );
        let expected = storer_receipt.clone();
        let got = drive_one_command(
            rx,
            forwarder.push(chunk.clone(), pusher),
            move |cmd| match cmd {
                ClientCommand::PushChunk {
                    peer,
                    address: requested,
                    chunk: pushed,
                    response,
                } => {
                    assert_eq!(peer, closer);
                    assert_eq!(requested, address);
                    assert_eq!(*pushed.address(), address);
                    response.send(Ok(storer_receipt)).expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            },
        )
        .await;

        let forwarded = got.expect("relay succeeds");
        // The receipt is relayed verbatim: every field is the storer's own.
        assert_eq!(forwarded.receipt.storer, expected.storer);
        assert_eq!(forwarded.receipt.signature, expected.signature);
        assert_eq!(forwarded.receipt.nonce, expected.nonce);
        assert_eq!(forwarded.receipt.storage_radius, expected.storage_radius);

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
            TEST_NET,
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
            closer,
        );

        let err = drive_one_command(
            rx,
            forwarder.push(chunk.clone(), pusher),
            move |cmd| match cmd {
                ClientCommand::PushChunk { response, .. } => {
                    response.send(Ok(shallow)).expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            },
        )
        .await
        .expect_err("a shallow receipt is never relayed");
        assert!(matches!(err, ForwardError::ShallowReceipt));

        // The downstream peer that handed us the shallow receipt is scored as
        // invalid data through the same reporter #287 uses.
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
            TEST_NET,
            Arc::clone(&reporter) as Arc<dyn PeerReporter>,
        );

        let signer = PrivateKeySigner::random();
        let (shallow, _signer_overlay) = signed_receipt_at_depth(
            &signer,
            &address,
            0,
            StorageRadius::new(Bin::new(0).unwrap()),
            closer,
        );

        let err = drive_one_command(
            rx,
            forwarder.push(chunk.clone(), pusher),
            move |cmd| match cmd {
                ClientCommand::PushChunk { response, .. } => {
                    response.send(Ok(shallow)).expect("receiver alive");
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
    async fn push_rejects_malformed_receipt_signature() {
        // An all-zero signature is the structural failure signal; it must never
        // be relayed and the sender is scored for invalid data.
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
            TEST_NET,
            Arc::clone(&reporter) as Arc<dyn PeerReporter>,
        );

        let malformed = PushReceipt {
            storer: closer,
            signature: Signature::from_raw(&[0u8; 65]).expect("zero signature"),
            nonce: Nonce::from([3u8; 32]),
            storage_radius: StorageRadius::new(Bin::new(5).unwrap()),
        };

        let err = drive_one_command(
            rx,
            forwarder.push(chunk.clone(), pusher),
            move |cmd| match cmd {
                ClientCommand::PushChunk { response, .. } => {
                    response.send(Ok(malformed)).expect("receiver alive");
                }
                other => panic!("unexpected command: {other:?}"),
            },
        )
        .await
        .expect_err("a malformed receipt is never relayed");
        assert!(matches!(err, ForwardError::UnverifiedRelay));

        let (_, event, _) = reporter.single();
        assert_eq!(event, SwarmScoringEvent::InvalidData);

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
            TEST_NET,
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
            TEST_NET,
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
