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
//! receipt is checked for structural validity (a non-empty signature) before it
//! is relayed.
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
use nectar_primitives::ChunkAddress;
use tracing::debug;
use vertex_swarm_api::{
    AccountingAction, PushReceipt, SwarmClientAccounting, SwarmTopologyRouting,
};
use vertex_swarm_primitives::{OverlayAddress, StampedChunk};

use crate::ClientHandle;

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
    /// Proximity-ordered closest-peer selection.
    topology: Arc<T>,
    /// Two-leg prepare/apply accounting.
    accounting: Arc<A>,
    /// Reuses the origin outbound futures for the upstream relay leg.
    handle: ClientHandle,
}

impl<T, A> NetworkForwarder<T, A> {
    /// Build a network forwarder from the local overlay, topology, accounting,
    /// and the outbound client handle.
    pub(crate) fn new(
        local: OverlayAddress,
        topology: Arc<T>,
        accounting: Arc<A>,
        handle: ClientHandle,
    ) -> Self {
        Self {
            local,
            topology,
            accounting,
            handle,
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
    T: SwarmTopologyRouting + Send + Sync + 'static,
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
                        // Edge verification (#287): never relay a structurally
                        // malformed receipt upstream. A storer signs over
                        // (address, nonce); the signature must be present. The
                        // receipt is relayed VERBATIM by the handler; we never
                        // mint or re-sign it.
                        //
                        // SHALLOW-RECEIPT SEAM (#293): a depth check on the
                        // receipt belongs here, on the relay path, so a forwarder
                        // never launders a receipt whose signer is not deep
                        // enough for the chunk. The receipt already carries
                        // `storage_radius`; the depth check must recover the
                        // signer overlay from `receipt.signature` (as the
                        // reference does), NOT trust the off-wire `receipt.storer`
                        // field, which the handler sets to the immediate
                        // downstream peer and is several hops from the real signer
                        // on a multi-hop relay. Filled in by #293; intentionally
                        // NOT implemented here.
                        if receipt_is_well_formed(&receipt) {
                            // Downstream leg complete; commit it. Upstream
                            // `provide` returned un-applied for the handler.
                            receive.apply();
                            debug!(%closer, %address, "relayed pushsync");
                            return Ok(ForwardedReceipt {
                                receipt,
                                provide: Box::new(provide),
                            });
                        }
                        drop(receive);
                        last = ForwardError::UnverifiedRelay;
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

/// A receipt is structurally well-formed when its signature is non-empty.
///
/// A storer signs a receipt over the chunk address and a nonce; an all-zero
/// signature is the on-the-wire signal for a failure, never a real receipt, so
/// it is never relayed. The full cryptographic recovery of the signer overlay
/// (and the #293 shallow-receipt depth check) attach on top of this gate.
fn receipt_is_well_formed(receipt: &PushReceipt) -> bool {
    receipt.signature.as_bytes() != [0u8; 65]
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use alloy_primitives::{B256, Signature};
    use nectar_postage::Stamp;
    use nectar_primitives::{AnyChunk, ContentChunk, Nonce};
    use tokio::sync::mpsc;
    use vertex_swarm_api::{Au, SwarmBandwidthAccounting, SwarmPeerBandwidth, SwarmPricing};
    use vertex_swarm_bandwidth::{
        Accounting, ClientAccounting, DefaultBandwidthConfig, FixedPricer,
    };
    use vertex_swarm_identity::Identity;
    use vertex_swarm_primitives::{Bin, StorageRadius};
    use vertex_swarm_spec::Spec;
    use vertex_swarm_test_utils::{MockTopology, test_identity_arc};

    use super::*;
    use crate::{ClientCommand, RetrievalResult};

    /// A stamped content chunk and its content-derived address.
    fn stamped() -> StampedChunk {
        let sig = Signature::from_raw(&[1u8; 65]).expect("valid signature");
        let stamp = Stamp::new(B256::repeat_byte(0xaa), 3, 7, 42, sig);
        let chunk: AnyChunk = ContentChunk::new(&b"forwarded payload"[..])
            .expect("valid content chunk")
            .into();
        StampedChunk::new(chunk, stamp)
    }

    fn good_signature() -> Signature {
        let mut raw = [0u8; 65];
        raw[..64].fill(1);
        raw[64] = 27;
        Signature::try_from(&raw[..]).expect("valid signature")
    }

    fn receipt(storer: OverlayAddress) -> PushReceipt {
        PushReceipt {
            storer,
            signature: good_signature(),
            nonce: Nonce::from([9u8; 32]),
            storage_radius: StorageRadius::new(Bin::new(5).unwrap()),
        }
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

        let forwarder = NetworkForwarder::new(local, topo, Arc::clone(&acct), handle);

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
        let forwarder = NetworkForwarder::new(local, topo, Arc::clone(&acct), handle);

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

        let forwarder = NetworkForwarder::new(local, topo, Arc::clone(&acct), handle);

        let storer_receipt = receipt(closer);
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

        let forwarder = NetworkForwarder::new(local, topo, Arc::clone(&acct), handle);
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

        let forwarder = NetworkForwarder::new(local, topo, Arc::clone(&acct), handle);

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
