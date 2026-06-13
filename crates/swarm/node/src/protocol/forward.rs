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
//!   forwarding-Kademlia loop bound (never relay to a peer that is not strictly
//!   closer to the target than the requester) and a hop/TTL bound, reuses the
//!   existing self-contained outbound futures
//!   ([`ClientHandle::retrieve_chunk`](crate::ClientHandle::retrieve_chunk) /
//!   [`push_chunk`](crate::ClientHandle::push_chunk)) for the upstream leg, and
//!   accounts both legs through the prepare/apply reservation actions so a
//!   forwarder earns the spread. A failed forward drops both reservation actions
//!   (release-on-drop), so no accounting leaks.
//!
//! Verification lives at both edges. The downstream chunk returned by an
//! upstream retrieval is verified against the requested address here
//! ([`StampedChunk::verify_answers`] -> [`VerifiedStampedChunk`]) before it is
//! cached or relayed; the handler additionally verifies before it writes the
//! chunk to the responder, so an unverified chunk can never travel back to the
//! requester. A relayed pushsync receipt is checked for structural validity (a
//! non-empty signature) before it is relayed.

use std::sync::Arc;

use futures::future::BoxFuture;
use nectar_primitives::ChunkAddress;
use tracing::debug;
use vertex_swarm_api::{
    AccountingAction, PushReceipt, SwarmClientAccounting, SwarmTopologyRouting,
};
use vertex_swarm_primitives::{OverlayAddress, StampedChunk};

use crate::ClientHandle;

/// Maximum number of relay hops a single inbound request may traverse from this
/// node onward.
///
/// This is the local TTL bound: each forward selects only strictly-closer peers
/// (so proximity is monotonically increasing toward the target), which already
/// makes an infinite loop impossible, and the hop cap is the belt-and-braces
/// upper bound on the candidate walk this node performs for one request. The
/// reference walks a small fixed number of closer peers per hop; we mirror that
/// with a bounded candidate set.
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
    /// No peer strictly closer to the target than the requester is available to
    /// relay to. Covers both the no-candidate case and the loop-prevention
    /// case (every candidate would forward sideways or backwards in proximity).
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
    fn retrieve(
        &self,
        address: ChunkAddress,
        exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<StampedChunk, ForwardError>>;

    /// Push `chunk` to a closer peer, excluding `exclude`, returning the
    /// storer's receipt to relay verbatim.
    fn push(
        &self,
        chunk: StampedChunk,
        exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<PushReceipt, ForwardError>>;
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
    ) -> BoxFuture<'static, Result<StampedChunk, ForwardError>> {
        Box::pin(async { Err(ForwardError::NoCloserPeer) })
    }

    fn push(
        &self,
        _chunk: StampedChunk,
        _exclude: OverlayAddress,
    ) -> BoxFuture<'static, Result<PushReceipt, ForwardError>> {
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
/// # Loop prevention and the hop bound
///
/// Forwarding Kademlia routes a request strictly toward the target's
/// neighbourhood: each hop must hand the request to a peer that is *strictly
/// closer* to the target than the peer that asked. [`closer_candidates`] filters
/// the topology's proximity-ordered candidates down to exactly those, excluding
/// the requester and ourselves. Because proximity to the target is monotonically
/// increasing along the chain, a request can never cycle back to a peer it has
/// already visited, so no per-request visited set is needed. The candidate set
/// is additionally capped at [`MAX_FORWARD_CANDIDATES`] as the local TTL bound on
/// the walk this node performs for one inbound request.
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

/// Select the peers strictly closer to `target` than `requester`, excluding the
/// requester and `local`, in proximity order, capped at [`MAX_FORWARD_CANDIDATES`].
///
/// This is the loop-prevention core: a candidate is kept only when its proximity
/// to the target is strictly greater than the requester's proximity to the
/// target. A peer that is at the same or lower proximity than the requester
/// would route the request sideways or backwards and is dropped, so the request
/// always advances toward the neighbourhood and can never cycle.
fn closer_candidates(
    topology: &impl SwarmTopologyRouting,
    target: &ChunkAddress,
    requester: OverlayAddress,
    local: OverlayAddress,
) -> Vec<OverlayAddress> {
    // The requester's proximity to the target is the floor: we only forward to a
    // peer strictly closer than that. `target.proximity(&peer)` is the number of
    // leading bits shared with the target; higher means closer.
    let floor = target.proximity(&requester);
    topology
        .closest_to(target, MAX_FORWARD_CANDIDATES * 2)
        .into_iter()
        .filter(|peer| *peer != requester && *peer != local)
        .filter(|peer| target.proximity(peer) > floor)
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
    ) -> BoxFuture<'static, Result<StampedChunk, ForwardError>> {
        let candidates = closer_candidates(&*self.topology, &address, exclude, self.local);
        let accounting = Arc::clone(&self.accounting);
        let handle = self.handle.clone();

        Box::pin(async move {
            if candidates.is_empty() {
                return Err(ForwardError::NoCloserPeer);
            }

            // Credit the upstream leg: the requester pays us for serving the
            // chunk on. Held across the whole relay; released on drop if the
            // relay fails, applied only once a verified chunk is in hand.
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
                                // Both legs succeeded: commit the spread.
                                receive.apply();
                                provide.apply();
                                debug!(%closer, %address, "relayed retrieval");
                                return Ok(verified.into_inner());
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
    ) -> BoxFuture<'static, Result<PushReceipt, ForwardError>> {
        let address = *chunk.address();
        let candidates = closer_candidates(&*self.topology, &address, exclude, self.local);
        let accounting = Arc::clone(&self.accounting);
        let handle = self.handle.clone();

        Box::pin(async move {
            if candidates.is_empty() {
                return Err(ForwardError::NoCloserPeer);
            }

            // Credit the upstream leg: the pusher pays us for relaying the chunk
            // toward its neighbourhood.
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
                        // `storage_radius`; verifying `PO(receipt.storer, chunk)`
                        // against a required depth and scoring/rejecting a shallow
                        // receipt is filled in by #293 and is intentionally NOT
                        // implemented here.
                        if receipt_is_well_formed(&receipt) {
                            receive.apply();
                            provide.apply();
                            debug!(%closer, %address, "relayed pushsync");
                            return Ok(receipt);
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
        let local = overlay_at_proximity(&address, 12);
        let closer = overlay_at_proximity(&address, 10);

        // The topology returns local and requester among the closest; both must
        // be filtered out even though local is strictly closer.
        let topo = MockTopology::default().with_closest(vec![local, closer, requester]);
        let got = closer_candidates(&topo, &address, requester, local);
        assert_eq!(got, vec![closer]);
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

        let relayed = got.expect("relay succeeds");
        assert_eq!(*relayed.address(), address, "the relayed chunk is verified");

        // Both legs committed: the requester owes us provide_price (credit), and
        // we owe the closer peer receive_price (debit).
        let owed_by_requester = acct.bandwidth().for_peer(requester).balance();
        let owed_to_closer = acct.bandwidth().for_peer(closer).balance();
        assert_eq!(owed_by_requester, provide_price);
        assert_eq!(owed_to_closer, Au::ZERO - receive_price);
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

        let relayed = got.expect("relay succeeds");
        // The receipt is relayed verbatim: every field is the storer's own.
        assert_eq!(relayed.storer, expected.storer);
        assert_eq!(relayed.signature, expected.signature);
        assert_eq!(relayed.nonce, expected.nonce);
        assert_eq!(relayed.storage_radius, expected.storage_radius);

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
